//! Local AUR index built from the official metadata dump
//! (`packages-meta-v1.json.gz`). This is what the AUR asks heavy users to use
//! instead of hammering the RPC. It is fetched with conditional requests
//! (ETag / If-Modified-Since) and at most once per `MIN_AGE`.
//!
//! The 55 MB JSON dump is converted once into a compact tab-separated index
//! (`index.tsv`) holding only the fields search needs, so a search parses a
//! few MB of plain text instead of the whole dump.

use crate::rpc::{AUR_URL, AurPkg, Client};
use crate::util::{self, cache_dir, now};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::io::{BufReader, Read, Write};
use std::path::PathBuf;

const MIN_AGE: u64 = 60 * 60;
/// Older than this, search falls back to the RPC.
pub const MAX_AGE_FOR_SEARCH: u64 = 24 * 60 * 60;
/// Bump when the index format changes; old indexes are then rebuilt.
const INDEX_FORMAT: &str = "aurgus-index-1";

#[derive(Serialize, Deserialize, Default)]
struct MetaState {
    etag: Option<String>,
    last_modified: Option<String>,
    checked: u64,
}

fn dump_path() -> PathBuf {
    cache_dir().join("packages-meta-v1.json.gz")
}

fn index_path() -> PathBuf {
    cache_dir().join("index.tsv")
}

fn state_path() -> PathBuf {
    cache_dir().join("packages-meta.json")
}

/// Seconds since the index was last confirmed fresh, if there is one.
pub fn age() -> Option<u64> {
    if !dump_path().exists() {
        return None;
    }
    let st: MetaState = util::read_json(&state_path())?;
    Some(now().saturating_sub(st.checked))
}

/// Refreshes the index. Returns true when a new dump was downloaded.
pub fn refresh(client: &mut Client, force: bool) -> Result<bool> {
    let mut st: MetaState = util::read_json(&state_path()).unwrap_or_default();
    let have = dump_path().exists();
    if have && !force && now().saturating_sub(st.checked) < MIN_AGE {
        return Ok(false);
    }
    let mut headers = Vec::new();
    if have {
        if let Some(e) = &st.etag {
            headers.push(("If-None-Match", e.clone()));
        }
        if let Some(lm) = &st.last_modified {
            headers.push(("If-Modified-Since", lm.clone()));
        }
    }
    let resp = client.get(&format!("{AUR_URL}/packages-meta-v1.json.gz"), &headers)?;
    if resp.status() == 304 {
        st.checked = now();
        util::write_json(&state_path(), &st)?;
        return Ok(false);
    }
    st.etag = resp.header("ETag").map(str::to_string);
    st.last_modified = resp.header("Last-Modified").map(str::to_string);
    let mut body = Vec::new();
    resp.into_reader()
        .take(256 * 1024 * 1024)
        .read_to_end(&mut body)
        .context("downloading the AUR metadata dump")?;
    std::fs::create_dir_all(cache_dir())?;
    let tmp = dump_path().with_extension("part");
    std::fs::write(&tmp, &body)?;
    std::fs::rename(&tmp, dump_path())?;
    build_index()?;
    st.checked = now();
    util::write_json(&state_path(), &st)?;
    Ok(true)
}

/// Parses the full dump (slow: ~1 s).
fn parse_dump() -> Result<Vec<AurPkg>> {
    let bytes = std::fs::read(dump_path())?;
    // The server may or may not apply transfer compression; accept both.
    let pkgs = if bytes.starts_with(&[0x1f, 0x8b]) {
        serde_json::from_reader(BufReader::new(flate2::read::GzDecoder::new(&bytes[..])))
    } else {
        serde_json::from_slice(&bytes)
    }
    .context("the local AUR index is corrupt; run `aurgus -Syy` to download it again")?;
    Ok(pkgs)
}

/// Case-insensitive `contains` for a lowercase needle, allocation-free for ASCII.
fn contains_ci(hay: &str, needle_lower: &str) -> bool {
    if needle_lower.is_ascii() {
        let (h, n) = (hay.as_bytes(), needle_lower.as_bytes());
        n.is_empty() || h.windows(n.len()).any(|w| w.eq_ignore_ascii_case(n))
    } else {
        hay.to_lowercase().contains(needle_lower)
    }
}

fn clean(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c == '\t' || c == '\n' || c == '\r' {
                ' '
            } else {
                c
            }
        })
        .collect()
}

fn build_index() -> Result<()> {
    let pkgs = parse_dump()?;
    let tmp = index_path().with_extension("part");
    let mut w = std::io::BufWriter::new(std::fs::File::create(&tmp)?);
    writeln!(w, "{INDEX_FORMAT}")?;
    for p in &pkgs {
        writeln!(
            w,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            p.name,
            p.package_base,
            p.version,
            p.num_votes,
            p.popularity,
            p.maintainer.as_deref().unwrap_or(""),
            p.out_of_date.unwrap_or(0),
            p.first_submitted,
            p.last_modified,
            clean(p.description.as_deref().unwrap_or(""))
        )?;
    }
    w.flush()?;
    drop(w);
    std::fs::rename(&tmp, index_path())?;
    Ok(())
}

/// Parses the index, keeping only entries accepted by `keep(name, description)`.
fn parse_index(keep: impl Fn(&str, &str) -> bool) -> Result<Option<Vec<AurPkg>>> {
    let Ok(text) = std::fs::read_to_string(index_path()) else {
        return Ok(None);
    };
    let mut lines = text.lines();
    if lines.next() != Some(INDEX_FORMAT) {
        return Ok(None);
    }
    let mut out = Vec::new();
    let mut f: Vec<&str> = Vec::with_capacity(10);
    for line in lines {
        f.clear();
        f.extend(line.splitn(10, '\t'));
        if f.len() != 10 || !keep(f[0], f[9]) {
            continue;
        }
        let num = |s: &str| s.parse::<u64>().unwrap_or(0);
        out.push(AurPkg {
            name: f[0].into(),
            package_base: f[1].into(),
            version: f[2].into(),
            num_votes: num(f[3]) as u32,
            popularity: f[4].parse().unwrap_or(0.0),
            maintainer: (!f[5].is_empty()).then(|| f[5].to_string()),
            out_of_date: Some(num(f[6])).filter(|&t| t > 0),
            first_submitted: num(f[7]),
            last_modified: num(f[8]),
            description: (!f[9].is_empty()).then(|| f[9].to_string()),
            ..Default::default()
        });
    }
    Ok(Some(out))
}

/// Names in the local index starting with `prefix` (for shell completion).
pub fn names_with_prefix(prefix: &str) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(index_path()) else {
        return Vec::new();
    };
    text.lines()
        .skip(1)
        .filter_map(|l| l.split('\t').next())
        .filter(|n| n.starts_with(prefix))
        .map(str::to_string)
        .collect()
}

/// Packages whose name or description contain every (lowercase) term.
/// Returns None when there is no local index. The index has no
/// dependency information. Builds the compact index from the dump when
/// it is missing or in an old format.
pub fn search(terms_lower: &[String]) -> Result<Option<Vec<AurPkg>>> {
    let keep = |name: &str, desc: &str| {
        terms_lower
            .iter()
            .all(|t| contains_ci(name, t) || contains_ci(desc, t))
    };
    if let Some(v) = parse_index(&keep)? {
        return Ok(Some(v));
    }
    if !dump_path().exists() {
        return Ok(None);
    }
    build_index()?;
    parse_index(&keep)
}

#[cfg(test)]
mod tests {
    #[test]
    fn ci_contains() {
        assert!(super::contains_ci("Feature packed AUR helper", "aur"));
        assert!(super::contains_ci("Ünïcode Ñame", "ñame"));
        assert!(!super::contains_ci("abc", "abd"));
    }
}
