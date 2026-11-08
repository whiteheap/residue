#!/usr/bin/env bash
# Point the AUR package at a published release.
#
#   packaging/aur/update.sh TAG
#
# Sets pkgver and the two checksums in PKGBUILD from the release's SHA256SUMS,
# resets pkgrel to 1, then regenerates .SRCINFO with makepkg in an
# archlinux:base-devel container. Needs gh and Docker.
set -euo pipefail

if [ $# -ne 1 ]; then
  echo "usage: $0 TAG" >&2
  exit 2
fi

tag=$1
version=${tag#v}
dir=$(cd "$(dirname "$0")" && pwd)
sums=$(gh release download "$tag" --repo whiteheap/residue-cli --pattern SHA256SUMS --output -)
sum() {
  local line
  line=$(printf '%s\n' "$sums" | grep " residue-$tag-$1-unknown-linux-musl.tar.gz\$")
  printf '%s' "${line%% *}"
}
x86_64=$(sum x86_64)
aarch64=$(sum aarch64)

sed -i.bak \
  -e "s/^pkgver=.*/pkgver=$version/" \
  -e "s/^pkgrel=.*/pkgrel=1/" \
  -e "s/^sha256sums_x86_64=.*/sha256sums_x86_64=('$x86_64')/" \
  -e "s/^sha256sums_aarch64=.*/sha256sums_aarch64=('$aarch64')/" \
  "$dir/PKGBUILD"
rm "$dir/PKGBUILD.bak"

docker run --rm --platform linux/amd64 -v "$dir:/pkg:ro" archlinux:base-devel \
  bash -c 'useradd -m build && cp /pkg/PKGBUILD /home/build/ && cd /home/build && runuser -u build -- makepkg --printsrcinfo' \
  > "$dir/.SRCINFO"

echo "PKGBUILD and .SRCINFO now point at $tag"
