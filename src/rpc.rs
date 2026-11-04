//! AUR RPC v5 client designed to be gentle with aur.archlinux.org:
//!
//! * `info` lookups are batched (many `arg[]` per request, bounded by URL length)
//! * every response is cached on disk with a TTL (negative results too)
//! * requests are spaced by a minimum interval and counted against a daily
//!   soft cap below the AUR's per-IP limit
//! * 429/5xx responses are retried with exponential backoff honoring Retry-After
//! * a descriptive User-Agent identifies the client

use crate::util::{self, cache_dir, now};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::thread::sleep;
use std::time::{Duration, Instant};

pub const AUR_URL: &str = "https://aur.archlinux.org";
pub const USER_AGENT: &str = concat!(
    "aurgus/",
    env!("CARGO_PKG_VERSION"),
    " (AUR helper; +https://wiki.archlinux.org/title/AUR_helpers)"
);
/// Keep request URLs comfortably below common server limits.
const MAX_URL: usize = 4000;
const INFO_TTL: u64 = 5 * 60;
const SEARCH_TTL: u64 = 10 * 60;
/// The AUR allows 4000 RPC requests per IP per day; stay well below that.
pub const DAILY_SOFT_CAP: u32 = 3000;
const MIN_INTERVAL: Duration = Duration::from_millis(250);
const MAX_RETRIES: u32 = 4;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
pub struct AurPkg {
    pub name: String,
    pub package_base: String,
    pub version: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(rename = "URL", default)]
    pub url: Option<String>,
    #[serde(default)]
    pub num_votes: u32,
    #[serde(default)]
    pub popularity: f64,
    #[serde(default)]
    pub out_of_date: Option<u64>,
    #[serde(default)]
    pub maintainer: Option<String>,
    #[serde(default)]
    pub submitter: Option<String>,
    #[serde(default)]
    pub first_submitted: u64,
    #[serde(default)]
    pub last_modified: u64,
    #[serde(default)]
    pub depends: Vec<String>,
    #[serde(default)]
    pub make_depends: Vec<String>,
    #[serde(default)]
    pub check_depends: Vec<String>,
    #[serde(default)]
    pub opt_depends: Vec<String>,
    #[serde(default)]
    pub provides: Vec<String>,
    #[serde(default)]
    pub conflicts: Vec<String>,
    #[serde(default)]
    pub replaces: Vec<String>,
    #[serde(default)]
    pub license: Vec<String>,
    #[serde(default)]
    pub keywords: Vec<String>,
    #[serde(default)]
    pub co_maintainers: Vec<String>,
}

#[derive(Deserialize)]
struct RpcResponse {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    results: Vec<AurPkg>,
}

#[derive(Serialize, Deserialize, Default)]
struct DiskCache {
    #[serde(default)]
    info: HashMap<String, (u64, Option<AurPkg>)>,
    #[serde(default)]
    search: HashMap<String, (u64, Vec<AurPkg>)>,
}

#[derive(Serialize, Deserialize, Default)]
struct RateState {
    day: u64,
    count: u32,
}

pub struct Client {
    agent: ureq::Agent,
    last: Option<Instant>,
    cache: DiskCache,
    rate: RateState,
    dirty: bool,
}

/// Percent-encodes everything except RFC 3986 unreserved characters.
pub fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

impl Client {
    pub fn new() -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(15))
            .timeout(Duration::from_secs(120))
            .build();
        let cache = util::read_json(&cache_dir().join("rpc-cache.json")).unwrap_or_default();
        let mut rate: RateState =
            util::read_json(&cache_dir().join("ratelimit.json")).unwrap_or_default();
        let today = now() / 86400;
        if rate.day != today {
            rate = RateState {
                day: today,
                count: 0,
            };
        }
        Client {
            agent,
            last: None,
            cache,
            rate,
            dirty: false,
        }
    }

    pub fn requests_today(&self) -> u32 {
        self.rate.count
    }

    fn throttle(&mut self) -> Result<()> {
        if self.rate.count >= DAILY_SOFT_CAP {
            bail!(
                "aurgus already made {} AUR requests today (soft cap {DAILY_SOFT_CAP}); \
                 refusing to make more to respect the AUR's rate limit",
                self.rate.count
            );
        }
        if let Some(t) = self.last {
            let elapsed = t.elapsed();
            if elapsed < MIN_INTERVAL {
                sleep(MIN_INTERVAL - elapsed);
            }
        }
        self.last = Some(Instant::now());
        self.rate.count += 1;
        self.dirty = true;
        Ok(())
    }

    /// Rate-limited GET with retries on 429/5xx and transport errors.
    pub fn get(&mut self, url: &str, headers: &[(&str, String)]) -> Result<ureq::Response> {
        let mut delay = Duration::from_secs(1);
        for attempt in 0..=MAX_RETRIES {
            self.throttle()?;
            let mut req = self.agent.get(url).set("User-Agent", USER_AGENT);
            for (k, v) in headers {
                req = req.set(k, v);
            }
            match req.call() {
                Ok(r) => return Ok(r),
                Err(ureq::Error::Status(code, r))
                    if (code == 429 || code >= 500) && attempt < MAX_RETRIES =>
                {
                    let wait = r
                        .header("Retry-After")
                        .and_then(|v| v.trim().parse::<u64>().ok())
                        .map(Duration::from_secs)
                        .unwrap_or(delay)
                        .min(Duration::from_secs(120));
                    util::warn(&format!(
                        "AUR answered HTTP {code}, retrying in {}s",
                        wait.as_secs()
                    ));
                    sleep(wait);
                    delay *= 2;
                }
                Err(ureq::Error::Status(code, _)) => bail!("AUR returned HTTP {code} for {url}"),
                Err(ureq::Error::Transport(t)) if attempt < MAX_RETRIES => {
                    util::warn(&format!(
                        "network error ({t}), retrying in {}s",
                        delay.as_secs()
                    ));
                    sleep(delay);
                    delay *= 2;
                }
                Err(e) => return Err(e).context("request to the AUR failed"),
            }
        }
        bail!("giving up on {url} after {MAX_RETRIES} retries")
    }

    fn rpc(&mut self, url: &str) -> Result<Vec<AurPkg>> {
        let r: RpcResponse = self
            .get(url, &[])?
            .into_json()
            .context("invalid JSON from the AUR RPC")?;
        if r.kind == "error" {
            bail!(
                "AUR RPC: {}",
                r.error.unwrap_or_else(|| "unknown error".into())
            );
        }
        Ok(r.results)
    }

    /// Looks up packages by exact pkgname. Missing packages are simply absent
    /// from the returned map.
    pub fn info(&mut self, names: &[String]) -> Result<HashMap<String, AurPkg>> {
        let mut out = HashMap::new();
        let mut missing = Vec::new();
        let t = now();
        let mut seen = HashSet::new();
        for n in names {
            if !seen.insert(n.as_str()) {
                continue;
            }
            match self.cache.info.get(n) {
                Some((at, pkg)) if t.saturating_sub(*at) < INFO_TTL => {
                    if let Some(p) = pkg {
                        out.insert(n.clone(), p.clone());
                    }
                }
                _ => missing.push(n.clone()),
            }
        }

        let base = format!("{AUR_URL}/rpc/v5/info?");
        let mut chunks: Vec<Vec<String>> = Vec::new();
        let mut chunk = Vec::new();
        let mut len = base.len();
        for n in missing {
            let part = encode(&n).len() + "arg[]=&".len();
            if !chunk.is_empty() && len + part > MAX_URL {
                chunks.push(std::mem::take(&mut chunk));
                len = base.len();
            }
            len += part;
            chunk.push(n);
        }
        if !chunk.is_empty() {
            chunks.push(chunk);
        }

        for chunk in chunks {
            let query: Vec<String> = chunk
                .iter()
                .map(|n| format!("arg[]={}", encode(n)))
                .collect();
            let results = self.rpc(&format!("{base}{}", query.join("&")))?;
            let t = now();
            let mut found: HashMap<String, AurPkg> =
                results.into_iter().map(|p| (p.name.clone(), p)).collect();
            for n in chunk {
                let p = found.remove(&n);
                if let Some(p) = &p {
                    out.insert(n.clone(), p.clone());
                }
                self.cache.info.insert(n, (t, p));
            }
            self.dirty = true;
        }
        Ok(out)
    }

    /// RPC search. `by` is one of name, name-desc, maintainer, depends,
    /// makedepends, optdepends, checkdepends, provides, ...
    pub fn search(&mut self, term: &str, by: &str) -> Result<Vec<AurPkg>> {
        let key = format!("{by}:{term}");
        if let Some((at, v)) = self.cache.search.get(&key)
            && now().saturating_sub(*at) < SEARCH_TTL
        {
            return Ok(v.clone());
        }
        if term.chars().count() < 2 {
            bail!("AUR search terms must be at least 2 characters long");
        }
        let url = format!("{AUR_URL}/rpc/v5/search/{}?by={by}", encode(term));
        let v = self.rpc(&url).map_err(|e| {
            if e.to_string().contains("Too many") {
                anyhow::anyhow!(
                    "'{term}' matches too many AUR packages for the RPC; use a more specific term, \
                     or run `aurgus -Sy` once to search a local index instead"
                )
            } else {
                e
            }
        })?;
        self.cache.search.insert(key, (now(), v.clone()));
        self.dirty = true;
        Ok(v)
    }

    fn save(&mut self) -> Result<()> {
        if !self.dirty {
            return Ok(());
        }
        let t = now();
        self.cache
            .info
            .retain(|_, (at, _)| t.saturating_sub(*at) < INFO_TTL);
        self.cache
            .search
            .retain(|_, (at, _)| t.saturating_sub(*at) < SEARCH_TTL);
        util::write_json(&cache_dir().join("rpc-cache.json"), &self.cache)?;
        util::write_json(&cache_dir().join("ratelimit.json"), &self.rate)?;
        self.dirty = false;
        Ok(())
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        if let Err(e) = self.save() {
            util::warn(&format!("could not save the AUR cache: {e:#}"));
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn encoding() {
        assert_eq!(super::encode("gtk+3"), "gtk%2B3");
        assert_eq!(super::encode("lib32-foo_bar.x"), "lib32-foo_bar.x");
        assert_eq!(super::encode("a b"), "a%20b");
    }
}
