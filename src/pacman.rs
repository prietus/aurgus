//! Thin wrappers around pacman. Output parsing always runs with LC_ALL=C.

use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet};
use std::os::unix::fs::MetadataExt;
use std::process::{Command, Stdio};

pub fn is_root() -> bool {
    std::fs::metadata("/proc/self")
        .map(|m| m.uid() == 0)
        .unwrap_or(false)
}

fn sudo() -> String {
    std::env::var("AURGUS_SUDO")
        .ok()
        .or_else(|| crate::config::get().sudo.clone())
        .unwrap_or_else(|| "sudo".into())
}

/// Runs pacman with inherited stdio; `root` prefixes sudo when needed.
pub fn run(args: &[String], root: bool) -> Result<i32> {
    let mut cmd = if root && !is_root() {
        let mut c = Command::new(sudo());
        c.arg("pacman");
        c
    } else {
        Command::new("pacman")
    };
    let st = cmd.args(args).status().context("failed to run pacman")?;
    Ok(st.code().unwrap_or(1))
}

fn capture(args: &[&str]) -> Result<(i32, String)> {
    let out = Command::new("pacman")
        .args(args)
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .context("failed to run pacman")?;
    Ok((
        out.status.code().unwrap_or(1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    ))
}

fn pairs(out: &str) -> impl Iterator<Item = (String, String)> + '_ {
    out.lines().filter_map(|l| {
        let mut it = l.split_whitespace();
        Some((it.next()?.to_string(), it.next()?.to_string()))
    })
}

/// name -> version of every installed package.
pub fn installed() -> Result<HashMap<String, String>> {
    Ok(pairs(&capture(&["-Q"])?.1).collect())
}

/// Installed packages that are not in any sync repository.
pub fn foreign() -> Result<Vec<(String, String)>> {
    Ok(pairs(&capture(&["-Qm"])?.1).collect())
}

pub fn explicit_count() -> Result<usize> {
    Ok(capture(&["-Qeq"])?.1.lines().count())
}

/// Names of every package in the sync repositories. `pacman -Slq` takes
/// ~0.5 s, so the list is cached and keyed on the sync databases' mtimes.
pub fn sync_names() -> Result<HashSet<String>> {
    let dbpath = Command::new("pacman-conf")
        .arg("DBPath")
        .stderr(Stdio::null())
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/var/lib/pacman/".into());
    let mut stamp = Vec::new();
    if let Ok(rd) = std::fs::read_dir(std::path::Path::new(&dbpath).join("sync")) {
        for e in rd.flatten() {
            if e.path().extension().is_some_and(|x| x == "db") {
                if let Ok(m) = e.metadata() {
                    stamp.push(format!(
                        "{}:{}:{}",
                        e.file_name().to_string_lossy(),
                        m.mtime(),
                        m.len()
                    ));
                }
            }
        }
    }
    stamp.sort();
    let stamp = stamp.join(" ");
    let cache = crate::util::cache_dir().join("sync-names.txt");
    if let Ok(text) = std::fs::read_to_string(&cache) {
        let mut lines = text.lines();
        if lines.next() == Some(stamp.as_str()) {
            return Ok(lines.map(str::to_string).collect());
        }
    }
    let out = capture(&["-Slq"])?.1;
    let names: HashSet<String> = out.lines().map(str::to_string).collect();
    if let Some(dir) = cache.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&cache, format!("{stamp}\n{out}"));
    Ok(names)
}

pub fn group_names() -> Result<HashSet<String>> {
    Ok(capture(&["-Sgq"])?.1.lines().map(str::to_string).collect())
}

/// Returns the dependencies that are NOT satisfied by installed packages.
pub fn unsatisfied(deps: &[String]) -> Result<Vec<String>> {
    if deps.is_empty() {
        return Ok(Vec::new());
    }
    let mut args = vec!["-T"];
    args.extend(deps.iter().map(String::as_str));
    Ok(capture(&args)?.1.lines().map(str::to_string).collect())
}

/// Whether a sync repository can satisfy `dep` (by name or provides).
pub fn repo_satisfies(dep: &str) -> bool {
    capture(&["-Spdd", "--noconfirm", "--print-format", "%n", "--", dep])
        .map(|(code, out)| code == 0 && !out.trim().is_empty())
        .unwrap_or(false)
}

pub struct RepoHit {
    pub repo: String,
    pub name: String,
    pub version: String,
    pub desc: String,
}

/// Parses `pacman -Ss` output.
pub fn search(terms: &[String]) -> Result<Vec<RepoHit>> {
    let mut args = vec!["-Ss", "--"];
    args.extend(terms.iter().map(String::as_str));
    let (_, out) = capture(&args)?;
    let mut hits: Vec<RepoHit> = Vec::new();
    for line in out.lines() {
        if line.starts_with(' ') {
            if let Some(h) = hits.last_mut() {
                h.desc = line.trim().to_string();
            }
            continue;
        }
        let mut it = line.split_whitespace();
        let (Some(full), Some(version)) = (it.next(), it.next()) else {
            continue;
        };
        let Some((repo, name)) = full.split_once('/') else {
            continue;
        };
        hits.push(RepoHit {
            repo: repo.into(),
            name: name.into(),
            version: version.into(),
            desc: String::new(),
        });
    }
    Ok(hits)
}

/// `IgnorePkg` entries from pacman.conf (Include directives expanded by
/// pacman-conf). `config` is an alternative --config path.
pub fn conf_ignorepkg(config: Option<&str>) -> Vec<String> {
    let mut cmd = Command::new("pacman-conf");
    if let Some(c) = config {
        cmd.arg("--config").arg(c);
    }
    cmd.arg("IgnorePkg")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(str::to_string)
                .filter(|l| !l.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Whether `name` matches any pattern (pacman allows shell globs in IgnorePkg).
pub fn is_ignored(name: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|p| {
        if p == name {
            return true;
        }
        if !p.contains(['*', '?', '[']) {
            return false;
        }
        match (
            std::ffi::CString::new(p.as_str()),
            std::ffi::CString::new(name),
        ) {
            // SAFETY: both are valid NUL-terminated C strings for the call's duration.
            (Ok(p), Ok(n)) => unsafe { libc::fnmatch(p.as_ptr(), n.as_ptr(), 0) == 0 },
            _ => false,
        }
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn ignore_globs() {
        let pats = ["linux", "python-*", "foo?bar"].map(String::from);
        assert!(super::is_ignored("linux", &pats));
        assert!(super::is_ignored("python-requests", &pats));
        assert!(super::is_ignored("foo-bar", &pats));
        assert!(!super::is_ignored("linux-lts", &pats));
        assert!(!super::is_ignored("python", &pats));
    }
}
