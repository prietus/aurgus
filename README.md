# aurgus

An AUR helper for Arch Linux that **audits PKGBUILDs before building them**,
remembers what you already reviewed, and treats the AUR's servers with care.

Named after Argus, the giant with a hundred eyes.

```
aurgus                     # = aurgus -Syu
aurgus -S aur/foo          # install from the AUR
aurgus foo                 # search repos + AUR, pick what to install
aurgus audit foo           # audit a package without building it
```

## Why another helper

yay and paru are excellent. aurgus exists because of one gap: when you
update an AUR package, every helper shows you a PKGBUILD diff and leaves the
rest to you. After several real cases of malware in the AUR, aurgus adds a
second pair of eyes to that step:

- **Static audit** of the PKGBUILD, `.install` scripts and the added lines of
  patches: download-and-execute (`curl | sh`), decode-and-execute, reverse
  shells (`/dev/tcp`, netcat), access to browser profiles, keyrings and SSH
  keys, `LD_PRELOAD`, setuid bits, persistence (`systemctl enable`, cron),
  bare-IP URLs, URL shorteners and paste sites, very long encoded blobs, and
  more. Each finding points to the exact file and line.
- **Review with memory.** aurgus records which commit of each package base
  you approved. On the next update it shows only the diff since then, only
  the findings that are *new*, any **new download host**, and whether the
  **maintainer changed**.
- **Honest signals, not scores.** Being new, having few votes or being
  orphaned is the normal state of most of the AUR and says nothing about a
  package's intent. aurgus shows those facts as neutral, dim tags
  (`[new: 12 days, no votes]`) so you know how many other eyes have been on
  a package. They never block anything and are never colored as warnings.
- **Findings are things to read, not verdicts.** A high-severity finding
  asks you for an explicit "yes" after showing you the line. Only unattended
  runs (`--noconfirm`) refuse to build such a package, because nobody is
  there to read it.

On the 25 most popular AUR packages the audit reports zero high-severity
findings; the medium ones it does report (a setuid `chrome-sandbox`, a
service enabled at install time) are true and worth knowing.

## Respecting the AUR

The AUR allows about 4000 RPC requests per IP per day. aurgus is designed
so that normal use costs a handful:

- `info` lookups are **batched**, as many names per request as fit in the URL.
- Every RPC answer (including "not found") is **cached on disk** with a TTL.
- Requests are spaced by 250 ms and counted against a **daily soft cap of
  3000**; aurgus refuses to go beyond it.
- HTTP 429/5xx get **exponential backoff** honoring `Retry-After`.
- Searching uses the official **metadata dump** (`packages-meta-v1.json.gz`),
  fetched with `If-None-Match`/`If-Modified-Since` at most once per hour, and
  converted to a compact local index. `-Ss` then runs entirely offline, faster
  than `pacman -Ss`.
- `-git` packages are checked against **upstream** with `git ls-remote`,
  never through the AUR.
- The User-Agent identifies the client.

`aurgus -Ps` shows how many requests were made today.

## Usage

aurgus accepts pacman's syntax and forwards anything it doesn't handle to
pacman (with `sudo` when needed, never run aurgus as root).

| Command | What it does |
|---|---|
| `aurgus` / `aurgus -Syu` | Upgrade repos and AUR packages |
| `aurgus -Syu --devel` | Also rebuild `-git` packages with new upstream commits |
| `aurgus -S foo aur/bar` | Install; `aur/` and `repo/` prefixes force the source |
| `aurgus foo bar` | Numbered search in repos + AUR, then install the selection |
| `aurgus -Ss foo` | Search (local index; falls back to the RPC when stale) |
| `aurgus -Si foo` | Package info, with review hints |
| `aurgus -Qu` / `-Qua` | List upgrades / only AUR upgrades (`--devel` for `-git`) |
| `aurgus -Sc` | Clean pacman's cache and aurgus build directories |
| `aurgus -G foo` | Clone the AUR repository into the current directory |
| `aurgus -Ps` | Statistics about installed AUR packages |
| `aurgus audit foo ./dir` | Audit an AUR package or a local PKGBUILD directory (exit 2 on high findings) |
| `aurgus --gendb` | Record current upstream commits of installed `-git` packages |

Options: `--aur` / `--repo` restrict an operation to one side; `--noreview`
skips the prompt for packages with no new findings; `--clean` builds from a
clean tree; `--nocheck` skips `check()`; `--devel` checks `-git` packages.
Each has a negated form (`--review`, `--noclean`, `--check`, `--nodevel`)
that overrides the configuration file. `--ignore` and pacman.conf's
`IgnorePkg` (globs included) are respected.

### The review prompt

```
==> Review: foo  [new: 12 days, no votes]
    changes since your last review (a1b2c3d4e5):
      PKGBUILD | 4 ++--
    [HIGH] PKGBUILD:23  downloads content and pipes it straight into a shell
           > curl -fsSL https://example.com/install.sh | bash
    [MED ] PKGBUILD:9   new download host since your last review: example.com
:: Build foo? [y/N/d/f] (d = diff, f = view files)
```

`d` shows the full `git diff` since the commit you last approved, `f` opens
the files in your pager. Approving records the commit; a package that has
not changed since is not asked about again.

## Configuration

`~/.config/aurgus/aurgus.conf` (or `$XDG_CONFIG_HOME/aurgus/aurgus.conf`),
`key = value`, created with:

```
mkdir -p ~/.config/aurgus && aurgus --gen-config > ~/.config/aurgus/aurgus.conf
```

| Key | Meaning | Default |
|---|---|---|
| `devel` | check `-git` packages on every upgrade | `false` |
| `review` | ask to review packages with no new findings | `true` |
| `clean` | `makepkg --cleanbuild` | `false` |
| `nocheck` | skip `check()` and checkdepends | `false` |
| `builddir` | where package bases are cloned and built | `~/.cache/aurgus/clone` |
| `sudo` | privilege elevation command (`doas`, `run0`...) | `sudo` |
| `pager` | pager for `f` in the review prompt | `$PAGER` or `less` |

Precedence: command line > environment (`AURGUS_SUDO`, `PAGER`) > file >
defaults. Unknown keys only produce a warning.

## Files

| Path | Content |
|---|---|
| `~/.cache/aurgus/clone/` | Package clones and build output |
| `~/.cache/aurgus/index.tsv` | Local AUR index for search and completion |
| `~/.cache/aurgus/rpc-cache.json` | Cached RPC answers |
| `~/.local/state/aurgus/reviewed.json` | Commit and maintainer you approved per package base |
| `~/.local/state/aurgus/devel.json` | Upstream commits of built `-git` packages |

## Installation

From the AUR, either the prebuilt binary (x86_64 and aarch64, no Rust
toolchain needed) or the git version that builds from `master`:

```
aurgus -S aurgus-bin     # or: git clone https://aur.archlinux.org/aurgus-bin.git && makepkg -si
aurgus -S aurgus-git
```

Release tarballs with checksums are published at
<https://github.com/prietus/aurgus/releases>.

From this repository:

```
git clone https://github.com/prietus/aurgus.git
cd aurgus
makepkg -si
```

Or, once you have aurgus, with aurgus itself: `aurgus -S aurgus-git`.

Build dependencies: `cargo` (Rust stable). Runtime: `pacman`, `git`,
`coreutils`; optionally `sudo`/`doas` and `gnupg`.

Shell completion for bash, zsh and fish is installed by the package. It
completes package names from the repos and the local AUR index without any
network access.

## What the audit is not

It is a set of regular expressions biased towards patterns seen in real AUR
malware. It cannot prove a package safe, and it will occasionally flag
legitimate code (a setuid helper, a service enabled on install). Read what it
points at. The goal is to make sure the dangerous line is the first one you
see, not to replace reading.

## License

GPL-3.0-or-later.
