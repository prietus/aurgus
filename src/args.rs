//! pacman-compatible argument parsing (`-Syu`, `-Ss foo`, `--needed`, ...).

use anyhow::{Result, bail};

/// Operations: pacman's plus G (getpkgbuild) and P (show/stats).
const OPS: &str = "DFQRSTUVGP";
/// Long options handled by aurgus and never forwarded to pacman.
const OWN_LONGS: &[&str] = &["aur", "repo", "noreview", "clean", "nocheck"];
/// pacman long options that take a separate value.
const VALUE_LONGS: &[&str] = &[
    "config",
    "dbpath",
    "root",
    "cachedir",
    "gpgdir",
    "hookdir",
    "logfile",
    "arch",
    "sysroot",
    "color",
    "overwrite",
    "assume-installed",
    "ignore",
    "ignoregroup",
    "print-format",
];

#[derive(Debug, Default)]
pub struct Args {
    pub op: Option<char>,
    pub shorts: Vec<char>,
    /// Forwarded to pacman, e.g. "--needed", "--ignore=foo".
    pub longs: Vec<String>,
    own: Vec<String>,
    pub targets: Vec<String>,
}

impl Args {
    pub fn has(&self, c: char) -> bool {
        self.shorts.contains(&c)
    }

    pub fn count(&self, c: char) -> usize {
        self.shorts.iter().filter(|&&x| x == c).count()
    }

    pub fn has_long(&self, name: &str) -> bool {
        let flag = format!("--{name}");
        self.longs
            .iter()
            .any(|l| *l == flag || l.starts_with(&format!("{flag}=")))
    }

    pub fn own(&self, name: &str) -> bool {
        self.own.iter().any(|o| o == name)
    }

    /// `-<op><shorts>` plus forwarded long options, without targets.
    pub fn pacman_args(&self) -> Vec<String> {
        self.pacman_args_without(&[])
    }

    pub fn pacman_args_without(&self, drop: &[char]) -> Vec<String> {
        let mut v = Vec::new();
        if let Some(op) = self.op {
            let s: String = std::iter::once(op)
                .chain(self.shorts.iter().copied().filter(|c| !drop.contains(c)))
                .collect();
            v.push(format!("-{s}"));
        }
        v.extend(self.longs.iter().cloned());
        v
    }
}

fn set_op(a: &mut Args, c: char) -> Result<()> {
    if a.op.is_some_and(|o| o != c) {
        bail!("only one operation may be used at a time");
    }
    a.op = Some(c);
    Ok(())
}

pub fn parse(raw: &[String]) -> Result<Args> {
    let mut a = Args::default();
    let mut it = raw.iter();
    let mut only_targets = false;
    while let Some(arg) = it.next() {
        if only_targets || !arg.starts_with('-') || arg == "-" {
            a.targets.push(arg.clone());
            continue;
        }
        if arg == "--" {
            only_targets = true;
            continue;
        }
        if let Some(long) = arg.strip_prefix("--") {
            let name = long.split('=').next().unwrap_or(long);
            let op = match name {
                "sync" => Some('S'),
                "query" => Some('Q'),
                "remove" => Some('R'),
                "upgrade" => Some('U'),
                "database" => Some('D'),
                "files" => Some('F'),
                "deptest" => Some('T'),
                "version" => Some('V'),
                "getpkgbuild" => Some('G'),
                "show" => Some('P'),
                _ => None,
            };
            if let Some(op) = op {
                set_op(&mut a, op)?;
            } else if OWN_LONGS.contains(&name) {
                a.own.push(name.to_string());
            } else if VALUE_LONGS.contains(&name) && !long.contains('=') {
                let Some(v) = it.next() else {
                    bail!("option --{name} requires a value")
                };
                a.longs.push(format!("--{name}={v}"));
            } else {
                a.longs.push(arg.clone());
            }
            continue;
        }
        for c in arg[1..].chars() {
            if OPS.contains(c) {
                set_op(&mut a, c)?;
            } else {
                a.shorts.push(c);
            }
        }
    }
    Ok(a)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Args {
        parse(&s.split_whitespace().map(String::from).collect::<Vec<_>>()).unwrap()
    }

    #[test]
    fn parsing() {
        let a = p("-Syu --needed --ignore foo --noreview bar");
        assert_eq!(a.op, Some('S'));
        assert!(a.has('y') && a.has('u'));
        assert!(a.has_long("needed") && a.own("noreview") && !a.has_long("noreview"));
        assert_eq!(a.targets, ["bar"]);
        assert_eq!(a.pacman_args(), ["-Syu", "--needed", "--ignore=foo"]);
        assert_eq!(p("-Syy").count('y'), 2);
        assert!(parse(&["-S".into(), "-R".into()]).is_err());
        assert_eq!(p("-Ss -- -weird").targets, ["-weird"]);
    }
}
