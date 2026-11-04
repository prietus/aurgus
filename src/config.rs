//! Optional configuration file: `$XDG_CONFIG_HOME/aurgus/aurgus.conf`
//! (`~/.config/aurgus/aurgus.conf`), in pacman.conf-like `key = value`
//! form. Command-line flags always win over the file.

use std::path::PathBuf;
use std::sync::OnceLock;

#[derive(Debug, Default)]
pub struct Config {
    /// Check upstream of -git packages on every upgrade (--devel).
    pub devel: bool,
    /// Ask to review clean packages (false = --noreview).
    pub review: Option<bool>,
    /// makepkg --cleanbuild on every build (--clean).
    pub clean: bool,
    /// Skip check() (--nocheck).
    pub nocheck: bool,
    /// Where package bases are cloned and built.
    pub builddir: Option<PathBuf>,
    /// Privilege elevation command (sudo, doas, run0...).
    pub sudo: Option<String>,
    /// Pager used to view files during review.
    pub pager: Option<String>,
}

pub const EXAMPLE: &str = "\
# aurgus configuration. Lines starting with # are comments.
# Command-line flags always override these values.

# Check upstream of -git packages on every -Syu (same as --devel)
#devel = false

# Ask to review every package before building it, even without findings
# (false is the same as --noreview; high-severity findings are always shown)
#review = true

# Always build from a clean source tree (makepkg --cleanbuild)
#clean = false

# Skip check() and checkdepends
#nocheck = false

# Directory where AUR repositories are cloned and built
#builddir = ~/.cache/aurgus/clone

# Command used to run pacman as root (the AURGUS_SUDO variable overrides it)
#sudo = sudo

# Pager used to view PKGBUILDs during review (the PAGER variable overrides it)
#pager = less
";

pub fn path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/"))
                .join(".config")
        });
    base.join("aurgus").join("aurgus.conf")
}

fn expand_tilde(s: &str) -> PathBuf {
    match s.strip_prefix("~/") {
        Some(rest) => std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/"))
            .join(rest),
        None => PathBuf::from(s),
    }
}

/// Parses the config text. Unknown keys and bad values produce warnings in
/// the returned list rather than errors, so a typo never blocks an upgrade.
pub fn parse(text: &str) -> (Config, Vec<String>) {
    let mut c = Config::default();
    let mut warnings = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            warnings.push(format!("line {}: expected `key = value`", i + 1));
            continue;
        };
        let (k, v) = (k.trim().to_lowercase(), v.trim());
        let as_bool = |v: &str| match v.to_lowercase().as_str() {
            "true" | "yes" | "on" | "1" => Some(true),
            "false" | "no" | "off" | "0" => Some(false),
            _ => None,
        };
        let mut bad = || warnings.push(format!("line {}: invalid value for {k}: {v}", i + 1));
        match k.as_str() {
            "devel" => match as_bool(v) {
                Some(b) => c.devel = b,
                None => bad(),
            },
            "review" => match as_bool(v) {
                Some(b) => c.review = Some(b),
                None => bad(),
            },
            "clean" | "cleanbuild" => match as_bool(v) {
                Some(b) => c.clean = b,
                None => bad(),
            },
            "nocheck" => match as_bool(v) {
                Some(b) => c.nocheck = b,
                None => bad(),
            },
            "builddir" if !v.is_empty() => c.builddir = Some(expand_tilde(v)),
            "sudo" if !v.is_empty() => c.sudo = Some(v.to_string()),
            "pager" if !v.is_empty() => c.pager = Some(v.to_string()),
            "builddir" | "sudo" | "pager" => bad(),
            _ => warnings.push(format!("line {}: unknown option {k}", i + 1)),
        }
    }
    (c, warnings)
}

/// The loaded configuration (read once; warnings printed on first use).
pub fn get() -> &'static Config {
    static C: OnceLock<Config> = OnceLock::new();
    C.get_or_init(|| {
        let Ok(text) = std::fs::read_to_string(path()) else {
            return Config::default();
        };
        let (c, warnings) = parse(&text);
        for w in warnings {
            crate::util::warn(&format!("{}: {w}", path().display()));
        }
        c
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsing() {
        let (c, w) = parse(
            "# c\ndevel = yes\nReview=off\nbuilddir = /tmp/b\nsudo = doas\nbogus = 1\nclean = maybe\n",
        );
        assert!(c.devel);
        assert_eq!(c.review, Some(false));
        assert_eq!(c.builddir.as_deref(), Some(std::path::Path::new("/tmp/b")));
        assert_eq!(c.sudo.as_deref(), Some("doas"));
        assert!(!c.clean);
        assert_eq!(w.len(), 2, "{w:?}");
    }

    #[test]
    fn example_is_valid() {
        let (_, w) = parse(EXAMPLE);
        assert!(w.is_empty(), "{w:?}");
        let uncommented: String = EXAMPLE
            .lines()
            .map(|l| l.strip_prefix('#').filter(|r| r.contains('=')).unwrap_or(l))
            .collect::<Vec<_>>()
            .join("\n");
        let (c, w) = parse(&uncommented);
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(c.pager.as_deref(), Some("less"));
    }
}
