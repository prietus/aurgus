//! Local AUR index built from the official metadata dump
//! (`packages-meta-v1.json.gz`). This is what the AUR asks heavy users to use
//! instead of hammering the RPC. It is fetched with conditional requests
//! (ETag / If-Modified-Since) and at most once per `MIN_AGE`.

use crate::rpc::{AUR_URL, AurPkg, Client};
use crate::util::{self, cache_dir, now};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::io::{BufReader, Read};
use std::path::PathBuf;

const MIN_AGE: u64 = 60 * 60;
/// Older than this, search falls back to the RPC.
pub const MAX_AGE_FOR_SEARCH: u64 = 24 * 60 * 60;

#[derive(Serialize, Deserialize, Default)]
struct MetaState {
    etag: Option<String>,
    last_modified: Option<String>,
    checked: u64,
}

fn dump_path() -> PathBuf {
    cache_dir().join("packages-meta-v1.json.gz")
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
    st.checked = now();
    util::write_json(&state_path(), &st)?;
    Ok(true)
}

/// Loads every package in the local index (name, version, votes, ...;
/// the dump has no dependency information).
pub fn load() -> Result<Option<Vec<AurPkg>>> {
    let Ok(bytes) = std::fs::read(dump_path()) else {
        return Ok(None);
    };
    // The server may or may not apply transfer compression; accept both.
    let pkgs = if bytes.starts_with(&[0x1f, 0x8b]) {
        serde_json::from_reader(BufReader::new(flate2::read::GzDecoder::new(&bytes[..])))
    } else {
        serde_json::from_slice(&bytes)
    }
    .context("the local AUR index is corrupt; run `aurgus -Syy` to download it again")?;
    Ok(Some(pkgs))
}
