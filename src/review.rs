//! Diff-aware review: remembers which commit of each package base you
//! approved, and on the next update shows only what changed — plus audit
//! findings that are *new* since then, new download hosts and maintainer
//! changes.

use crate::audit::{self, Finding, Severity};
use crate::rpc::AurPkg;
use crate::signals;
use crate::util::{self, bold, dim, green, paint, state_dir};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::{Command, Stdio};

#[derive(Serialize, Deserialize, Clone)]
pub struct Reviewed {
    pub commit: String,
    pub maintainer: Option<String>,
    pub at: u64,
    /// AUR `LastModified` of the package base when aurgus last built it
    /// successfully. While the AUR entry is unchanged, a version mismatch
    /// (typical of -git packages with a stale static pkgver) is not an update.
    #[serde(default)]
    pub built_last_modified: Option<u64>,
}

/// Records that `base` was just built from the AUR entry last modified at
/// `last_modified`.
pub fn mark_built(base: &str, last_modified: u64) -> Result<()> {
    let mut state = load();
    if let Some(r) = state.get_mut(base) {
        r.built_last_modified = Some(last_modified);
        save(&state)?;
    }
    Ok(())
}

pub fn load() -> HashMap<String, Reviewed> {
    util::read_json(&state_dir().join("reviewed.json")).unwrap_or_default()
}

fn save(state: &HashMap<String, Reviewed>) -> Result<()> {
    util::write_json(&state_dir().join("reviewed.json"), state)
}

fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .context("failed to run git")?;
    if !out.status.success() {
        bail!("git {} failed", args.join(" "));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

pub struct Report {
    /// Empty when `dir` is not a git checkout.
    pub head: String,
    pub files: Vec<String>,
    pub findings: Vec<Finding>,
    /// Review-attention tags from AUR metadata (never a verdict).
    pub tags: Vec<String>,
    pub few_eyes: bool,
    /// Last reviewed commit, if it still exists in the repository.
    pub prev_commit: Option<String>,
    pub diffstat: String,
}

impl Report {
    pub fn new_count(&self, sev: Severity) -> usize {
        self.findings
            .iter()
            .filter(|f| f.is_new && f.severity == sev)
            .count()
    }
}

fn list_files(dir: &Path, is_git: bool) -> Result<Vec<String>> {
    let mut files: Vec<String> = if is_git {
        git(dir, &["ls-files", "-z"])?
            .split('\0')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect()
    } else {
        std::fs::read_dir(dir)?
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect()
    };
    files.retain(|f| f != ".SRCINFO" && f != ".gitignore");
    files.sort_by_key(|f| (f != "PKGBUILD", f.clone()));
    Ok(files)
}

pub fn analyze(dir: &Path, pkg: Option<&AurPkg>, prev: Option<&Reviewed>) -> Result<Report> {
    let is_git = dir.join(".git").exists();
    let head = if is_git {
        git(dir, &["rev-parse", "HEAD"])?.trim().to_string()
    } else {
        String::new()
    };
    let files = list_files(dir, is_git)?;
    let prev_commit = prev
        .filter(|p| {
            is_git
                && git(
                    dir,
                    &["cat-file", "-e", &format!("{}^{{commit}}", p.commit)],
                )
                .is_ok()
        })
        .map(|p| p.commit.clone());

    let old = |f: &str| -> Option<String> {
        let c = prev_commit.as_ref()?;
        git(dir, &["show", &format!("{c}:{f}")]).ok()
    };

    let mut findings = Vec::new();
    let mut text_files = Vec::new();
    for f in &files {
        let Ok(bytes) = std::fs::read(dir.join(f)) else {
            continue;
        };
        if bytes.len() > 4 * 1024 * 1024 || bytes.contains(&0) {
            continue; // binary or huge; not reviewable as text
        }
        text_files.push(f.clone());
        let new = String::from_utf8_lossy(&bytes);
        let mut found = audit::scan(f, &new);
        if prev_commit.is_some() {
            let seen: HashSet<(&str, String)> = old(f)
                .map(|o| {
                    audit::scan(f, &o)
                        .into_iter()
                        .map(|x| (x.rule, x.snippet))
                        .collect()
                })
                .unwrap_or_default();
            for x in &mut found {
                x.is_new = !seen.contains(&(x.rule, x.snippet.clone()));
            }
        }
        findings.extend(found);

        if f == "PKGBUILD"
            && let Some(o) = old(f)
        {
            let before = audit::hosts(&o);
            for h in audit::hosts(&new).difference(&before) {
                findings.push(Finding {
                    severity: Severity::Medium,
                    rule: "new-host",
                    message: format!("new download host since your last review: {h}"),
                    file: f.clone(),
                    line: 0,
                    snippet: String::new(),
                    is_new: true,
                });
            }
        }
    }

    if let (Some(p), Some(pkg)) = (prev, pkg)
        && p.maintainer != pkg.maintainer
    {
        let name = |m: &Option<String>| m.clone().unwrap_or_else(|| "(orphan)".into());
        findings.push(Finding {
            severity: Severity::High,
            rule: "maintainer-changed",
            message: format!(
                "maintainer changed since your last review: {} -> {}",
                name(&p.maintainer),
                name(&pkg.maintainer)
            ),
            file: "AUR".into(),
            line: 0,
            snippet: String::new(),
            is_new: true,
        });
    }

    findings.sort_by(|a, b| {
        b.severity
            .cmp(&a.severity)
            .then(a.file.cmp(&b.file))
            .then(a.line.cmp(&b.line))
    });
    let diffstat = match &prev_commit {
        Some(c) if *c != head => git(
            dir,
            &[
                "diff",
                "--stat",
                &format!("{c}..HEAD"),
                "--",
                ".",
                ":(exclude).SRCINFO",
            ],
        )
        .unwrap_or_default(),
        _ => String::new(),
    };
    Ok(Report {
        head,
        files: text_files,
        findings,
        tags: pkg.map(signals::tags).unwrap_or_default(),
        few_eyes: pkg.is_some_and(signals::few_eyes),
        prev_commit,
        diffstat,
    })
}

pub fn print_report(name: &str, r: &Report) {
    println!();
    let tags = if r.tags.is_empty() {
        String::new()
    } else {
        dim(format!("  [{}]", r.tags.join(", ")))
    };
    println!(
        "{} {}{tags}",
        paint("1;36", "==>"),
        bold(format!("Review: {name}"))
    );
    match &r.prev_commit {
        Some(c) if *c == r.head => println!("    unchanged since your last review"),
        Some(c) => {
            println!(
                "    changes since your last review ({}):",
                &c[..c.len().min(10)]
            );
            for l in r.diffstat.lines() {
                println!("      {l}");
            }
        }
        None if !r.head.is_empty() => {
            println!(
                "    {}",
                bold("first review of this package: read the files")
            );
            if r.few_eyes {
                println!(
                    "    {}",
                    dim(
                        "few people have reviewed this package yet, so read it fully rather than skim"
                    )
                );
            }
        }
        None => {}
    }

    let shown: Vec<&Finding> = r.findings.iter().filter(|f| f.is_new).collect();
    let old = r.findings.len() - shown.len();
    if shown.is_empty() {
        println!("    {} audit: no new suspicious patterns", green("✓"));
    }
    for f in shown {
        let loc = if f.line > 0 {
            format!("{}:{}", f.file, f.line)
        } else {
            f.file.clone()
        };
        println!("    [{}] {}  {}", f.severity.label(), bold(&loc), f.message);
        if !f.snippet.is_empty() {
            println!("           {}", dim(format!("> {}", f.snippet)));
        }
    }
    if old > 0 {
        println!(
            "    {}",
            dim(format!(
                "({old} finding(s) already present at your last review not shown)"
            ))
        );
    }
}

fn show_files(dir: &Path, files: &[String]) -> Result<()> {
    let pager = std::env::var("PAGER")
        .ok()
        .or_else(|| crate::config::get().pager.clone())
        .unwrap_or_else(|| "less".into());
    Command::new("sh")
        .arg("-c")
        .arg(format!("{pager} \"$@\""))
        .arg("sh")
        .args(files)
        .current_dir(dir)
        .status()
        .context("failed to start the pager")?;
    Ok(())
}

fn show_diff(dir: &Path, r: &Report) -> Result<()> {
    let Some(c) = &r.prev_commit else {
        println!("No previous review to diff against; showing the files instead.");
        return show_files(dir, &r.files);
    };
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "diff",
            &format!("{c}..HEAD"),
            "--",
            ".",
            ":(exclude).SRCINFO",
        ])
        .status()
        .context("failed to run git diff")?;
    Ok(())
}

/// Interactive review of one package base. Returns whether it may be built.
/// High-severity findings always require an explicit "yes" and make
/// --noconfirm abort, even with --noreview.
pub fn review(base: &str, pkg: &AurPkg, interactive: bool) -> Result<bool> {
    let dir = util::clone_dir(base);
    let mut state = load();
    let prev = state.get(base).cloned();
    let report = analyze(&dir, Some(pkg), prev.as_ref())?;
    let high = report.new_count(Severity::High);
    let unchanged = prev.as_ref().is_some_and(|p| p.commit == report.head);

    if unchanged && high == 0 {
        println!("{} {base}: unchanged since your last review", green("✓"));
        return Ok(true);
    }
    print_report(base, &report);

    let accept = if high > 0 && util::noconfirm() {
        util::error(&format!(
            "{base} has {high} high-severity audit finding(s); refusing to build it unattended"
        ));
        false
    } else if !interactive && high == 0 {
        true
    } else {
        let hint = if high > 0 { "[y/N/d/f]" } else { "[Y/n/d/f]" };
        loop {
            let ans = util::read_line(&format!(
                "{} {} {hint} {} ",
                paint("1;34", "::"),
                bold(format!("Build {base}?")),
                dim("(d = diff, f = view files)")
            ))?;
            match ans.to_lowercase().as_str() {
                "" => break high == 0,
                "y" | "yes" | "s" | "si" | "sí" => break true,
                "n" | "no" => break false,
                "d" => show_diff(&dir, &report)?,
                "f" => show_files(&dir, &report.files)?,
                _ => {}
            }
        }
    };

    if accept {
        state.insert(
            base.to_string(),
            Reviewed {
                commit: report.head.clone(),
                maintainer: pkg.maintainer.clone(),
                at: util::now(),
                built_last_modified: None,
            },
        );
        save(&state)?;
    }
    Ok(accept)
}
