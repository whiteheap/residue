#!/usr/bin/env bash
# Build a signed pacman repository named `residue` from Arch packages.
#
#   packaging/arch-repo.sh PKGS OUT KEY
#
# PKGS holds the .pkg.tar.zst files. OUT receives one directory per
# architecture (x86_64, aarch64) with the packages, a detached signature for
# each, and residue.db and residue.files signed by KEY, the fingerprint of a
# secret key in the current GnuPG home. Needs repo-add and bsdtar (pacman)
# and gpg.
set -euo pipefail

if [ $# -ne 3 ]; then
  echo "usage: $0 PKGS OUT KEY" >&2
  exit 2
fi

pkgs=$(cd "$1" && pwd)
mkdir -p "$2"
out=$(cd "$2" && pwd)
key=$3
sign() { gpg --batch --yes --local-user "$key" --detach-sign --output "$1.sig" "$1"; }

for pkg in "$pkgs"/*.pkg.tar.zst; do
  info=$(bsdtar -xOf "$pkg" .PKGINFO)
  field() { printf '%s\n' "$info" | sed -n "s/^$1 = //p"; }
  arch=$(field arch)
  file="$out/$arch/$(field pkgname)-$(field pkgver)-$arch.pkg.tar.zst"
  mkdir -p "$out/$arch"
  cp "$pkg" "$file"
  sign "$file"
  repo-add --quiet "$out/$arch/residue.db.tar.gz" "$file"
done

# repo-add makes residue.db and residue.files symbolic links, and GitHub Pages
# does not serve links, so replace each with the file it points at.
for dir in "$out"/*/; do
  for db in residue.db residue.files; do
    sign "$dir/$db.tar.gz"
    rm -f "$dir/$db" "$dir/$db.sig"
    cp "$dir/$db.tar.gz" "$dir/$db"
    cp "$dir/$db.tar.gz.sig" "$dir/$db.sig"
  done
done
