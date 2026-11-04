#!/usr/bin/env bash
# Point the aurgus-bin PKGBUILD at a published release and regenerate .SRCINFO.
# usage: packaging/aurgus-bin/update.sh 0.1.2
set -euo pipefail
ver="${1:?usage: $0 <version without v>}"
dir="$(cd "$(dirname "$0")" && pwd)"
sums="$(curl -fsSL -A aurgus-bin-update "https://github.com/prietus/aurgus/releases/download/v$ver/SHA256SUMS")"
x86="$(grep -- "-x86_64-linux.tar.gz" <<<"$sums" | cut -d' ' -f1)"
arm="$(grep -- "-aarch64-linux.tar.gz" <<<"$sums" | cut -d' ' -f1)"
[[ ${#x86} -eq 64 && ${#arm} -eq 64 ]] || { echo "checksums not found for v$ver" >&2; exit 1; }
sed -i -e "s/^pkgver=.*/pkgver=$ver/" -e "s/^pkgrel=.*/pkgrel=1/" \
    -e "s/^sha256sums_x86_64=.*/sha256sums_x86_64=('$x86')/" \
    -e "s/^sha256sums_aarch64=.*/sha256sums_aarch64=('$arm')/" "$dir/PKGBUILD"
(cd "$dir" && makepkg --printsrcinfo > .SRCINFO)
echo "aurgus-bin updated to $ver; commit and push $dir to the AUR"
