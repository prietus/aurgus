//! Small shared helpers: paths, colors, prompts, JSON persistence.

use anyhow::{Context, Result};
use serde::{Serialize, de::DeserializeOwned};
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static NOCONFIRM: AtomicBool = AtomicBool::new(false);

pub fn set_noconfirm(v: bool) {
    NOCONFIRM.store(v, Ordering::Relaxed);
}

pub fn noconfirm() -> bool {
    NOCONFIRM.load(Ordering::Relaxed)
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
}

fn xdg(var: &str, fallback: &str) -> PathBuf {
    std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home().join(fallback))
        .join("aurgus")
}

pub fn cache_dir() -> PathBuf {
    xdg("XDG_CACHE_HOME", ".cache")
}

pub fn state_dir() -> PathBuf {
    xdg("XDG_STATE_HOME", ".local/state")
}

pub fn clone_dir(base: &str) -> PathBuf {
    crate::config::get()
        .builddir
        .clone()
        .unwrap_or_else(|| cache_dir().join("clone"))
        .join(base)
}

pub fn read_json<T: DeserializeOwned>(path: &Path) -> Option<T> {
    let data = std::fs::read(path).ok()?;
    serde_json::from_slice(&data).ok()
}

/// Atomically writes `value` as JSON (write to temp file, then rename).
pub fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&tmp, serde_json::to_vec(value)?)?;
    std::fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))
}

fn color_enabled() -> bool {
    static C: OnceLock<bool> = OnceLock::new();
    *C.get_or_init(|| std::env::var_os("NO_COLOR").is_none() && io::stdout().is_terminal())
}

pub fn paint(code: &str, s: impl AsRef<str>) -> String {
    if color_enabled() {
        format!("\x1b[{code}m{}\x1b[0m", s.as_ref())
    } else {
        s.as_ref().to_string()
    }
}

pub fn bold(s: impl AsRef<str>) -> String {
    paint("1", s)
}
pub fn red(s: impl AsRef<str>) -> String {
    paint("1;31", s)
}
pub fn green(s: impl AsRef<str>) -> String {
    paint("1;32", s)
}
pub fn yellow(s: impl AsRef<str>) -> String {
    paint("1;33", s)
}
pub fn magenta(s: impl AsRef<str>) -> String {
    paint("1;35", s)
}
pub fn cyan(s: impl AsRef<str>) -> String {
    paint("1;36", s)
}
pub fn dim(s: impl AsRef<str>) -> String {
    paint("2", s)
}

pub fn msg(s: &str) {
    println!("{} {}", paint("1;34", "::"), bold(s));
}

pub fn warn(s: &str) {
    eprintln!("{} {s}", yellow("warning:"));
}

pub fn error(s: &str) {
    eprintln!("{} {s}", red("error:"));
}

pub fn read_line(prompt: &str) -> Result<String> {
    print!("{prompt}");
    io::stdout().flush()?;
    let mut s = String::new();
    if io::stdin().lock().read_line(&mut s)? == 0 {
        println!();
        // EOF: nobody is there to answer, so never take a default silently.
        anyhow::bail!(
            "stdin is closed and a question needs an answer (use --noconfirm for unattended runs)"
        );
    }
    Ok(s.trim().to_string())
}

/// Yes/no question. Under --noconfirm the default answer is taken.
pub fn ask(question: &str, default_yes: bool) -> Result<bool> {
    let hint = if default_yes { "[Y/n]" } else { "[y/N]" };
    if noconfirm() {
        let ans = if default_yes { "y" } else { "n" };
        println!("{} {} {hint} {ans}", paint("1;34", "::"), bold(question));
        return Ok(default_yes);
    }
    let ans = read_line(&format!(
        "{} {} {hint} ",
        paint("1;34", "::"),
        bold(question)
    ))?;
    Ok(match ans.to_lowercase().as_str() {
        "" => default_yes,
        "y" | "yes" | "s" | "si" | "sí" => true,
        _ => false,
    })
}

/// Formats a unix timestamp as YYYY-MM-DD (UTC).
pub fn fmt_date(ts: u64) -> String {
    // Howard Hinnant's civil_from_days.
    let z = (ts / 86400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    #[test]
    fn dates() {
        assert_eq!(super::fmt_date(0), "1970-01-01");
        assert_eq!(super::fmt_date(1_709_164_800), "2024-02-29");
    }
}
