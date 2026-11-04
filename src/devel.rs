//! Updates for VCS (`-git`) packages: compare the upstream HEAD with what
//! was last built, using `git ls-remote` against upstream — never the AUR.
//!
//! A package is up to date when the remote commit equals the one recorded in
//! `devel.json` after its last build, or when the installed version string
//! already contains the remote commit's short hash (as most `-git` pkgver()
//! functions embed it), which makes a seed database optional.

use crate::install;
use crate::util::{self, clone_dir, state_dir};
use anyhow::Result;
use std::collections::{BTreeMap, HashMap};
use std::process::{Command, Stdio};
use std::sync::Mutex;

/// base -> (source url#ref -> commit)
type Db = BTreeMap<String, BTreeMap<String, String>>;

static OBSERVED: Mutex<Option<Db>> = Mutex::new(None);

fn db_path() -> std::path::PathBuf {
    state_dir().join("devel.json")
}

fn load_db() -> Db {
    util::read_json(&db_path()).unwrap_or_default()
}

pub fn is_vcs(name: &str) -> bool {
    name.ends_with("-git")
}

/// Git sources of a package base, from its .SRCINFO: (key, url, ref).
/// Sources pinned to a commit (`#commit=`) are skipped.
fn git_sources(base: &str) -> Vec<(String, String, String)> {
    let Ok(srcinfo) = std::fs::read_to_string(clone_dir(base).join(".SRCINFO")) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for raw in srcinfo.lines() {
        let Some(src) = raw.trim().strip_prefix("source") else {
            continue;
        };
        // "source = x" or "source_x86_64 = x"
        let Some((_, val)) = src.split_once('=') else {
            continue;
        };
        let val = val.trim();
        let val = val.rsplit("::").next().unwrap_or(val);
        let Some(rest) = val.strip_prefix("git+") else {
            continue;
        };
        let (url, frag) = rest.split_once('#').unwrap_or((rest, ""));
        let r = match frag.split_once('=') {
            Some(("branch", b)) => format!("refs/heads/{b}"),
            Some(("tag", t)) => format!("refs/tags/{t}"),
            Some(("commit", _)) => continue,
            _ => "HEAD".to_string(),
        };
        out.push((format!("{url}#{r}"), url.to_string(), r));
    }
    out
}

fn ls_remote(url: &str, r: &str) -> Option<String> {
    let out = Command::new("timeout")
        .args(["30", "git", "ls-remote", "--", url, r])
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    // Prefer the peeled tag commit ("^{}") when present.
    let mut best = None;
    for line in s.lines() {
        let (sha, name) = line.split_once('\t')?;
        if best.is_none() || name.ends_with("^{}") {
            best = Some(sha.to_string());
        }
    }
    best
}

/// Checks the installed `-git` packages in `installed` (name -> version)
/// whose AUR base is given in `bases` (name -> base). Returns the names
/// that need rebuilding. Clones are fetched first (git, not RPC).
pub fn updates(
    installed: &HashMap<String, String>,
    bases: &HashMap<String, String>,
) -> Result<Vec<String>> {
    let names: Vec<&String> = installed
        .keys()
        .filter(|n| is_vcs(n) && bases.contains_key(*n))
        .collect();
    if names.is_empty() {
        return Ok(Vec::new());
    }
    let mut todo: Vec<String> = names.iter().map(|n| bases[*n].clone()).collect();
    todo.sort();
    todo.dedup();
    install::fetch_all(&todo)?;

    let db = load_db();
    let results = Mutex::new(Vec::new());
    let observed = Mutex::new(Db::new());
    for chunk in todo.chunks(8) {
        std::thread::scope(|s| {
            for base in chunk {
                let (results, observed, db, names) = (&results, &observed, &db, &names);
                s.spawn(move || {
                    let mut seen = BTreeMap::new();
                    let mut changed = false;
                    for (key, url, r) in git_sources(base) {
                        let Some(sha) = ls_remote(&url, &r) else {
                            util::warn(&format!("{base}: could not reach {url}"));
                            continue;
                        };
                        let recorded = db.get(base).and_then(|m| m.get(&key));
                        let in_version = names
                            .iter()
                            .filter(|n| bases[**n] == *base)
                            .all(|n| installed[*n].contains(&sha[..7.min(sha.len())]));
                        if recorded != Some(&sha) && !in_version {
                            changed = true;
                        }
                        seen.insert(key, sha);
                    }
                    observed.lock().unwrap().insert(base.clone(), seen);
                    if changed {
                        results.lock().unwrap().push(base.clone());
                    }
                });
            }
        });
    }
    *OBSERVED.lock().unwrap() = Some(observed.into_inner().unwrap());
    let changed = results.into_inner().unwrap();
    let mut out: Vec<String> = names
        .into_iter()
        .filter(|n| changed.contains(&bases[*n]))
        .cloned()
        .collect();
    out.sort();
    Ok(out)
}

/// Records the commits observed for `base` as "built"; called after a
/// successful build so the next `--devel` check sees it as up to date.
pub fn record(base: &str) -> Result<()> {
    let observed = OBSERVED.lock().unwrap();
    let Some(shas) = observed.as_ref().and_then(|o| o.get(base)) else {
        return Ok(());
    };
    let mut db = load_db();
    db.insert(base.to_string(), shas.clone());
    util::write_json(&db_path(), &db)
}

/// `--gendb`: records the current upstream commits of every installed
/// `-git` package without building anything.
pub fn gendb(
    installed: &HashMap<String, String>,
    bases: &HashMap<String, String>,
) -> Result<usize> {
    updates(installed, bases)?;
    let observed = OBSERVED.lock().unwrap().take().unwrap_or_default();
    let n = observed.len();
    let mut db = load_db();
    db.extend(observed);
    util::write_json(&db_path(), &db)?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    #[test]
    fn vcs_names() {
        assert!(super::is_vcs("foo-git"));
        assert!(!super::is_vcs("foo-gitlab"));
    }
}
