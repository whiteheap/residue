#!/usr/bin/env bash
# Build a signed pacman repository named `residue` from Arch packages.
#
#   packaging/arch-repo.sh PKGS OUT KEY
#
# PKGS holds the .pkg.tar.zst files. OUT receives one directory per
# architecture (x86_64, aarch64) with the packages, a detached signature for
# each, and residue.db and residue.files signed by KEY, the fingerprint of a
# secret key in the current GnuPG home. Everything is signed here. Only
# repo-add, which exists only on Arch, runs in an Arch container: with no
# key and no network, the packages read-only, and one empty directory it may
# write. Needs bsdtar, gpg and Docker.
set -euo pipefail

# archlinux:base as of 2026-11-09 (built 2026-11-05), the image the 0.0.10 to
# 0.0.12 repositories were built with. Pinned by digest, so a moved tag never
# runs here. To update it, run `docker buildx imagetools inspect
# archlinux:base`, put the Digest it prints here, and check a packages.yml
# dry run, which builds the repositories with a key made for the run.
image=archlinux:base@sha256:b1c2fe6e717bfd5673af8fc217e1e1c3ce0dcbb1d53c3414c8425e7ce79fc6d9

if [ $# -ne 3 ]; then
  echo "usage: $0 PKGS OUT KEY" >&2
  exit 2
fi

pkgs=$(cd "$1" && pwd)
mkdir -p "$2"
out=$(cd "$2" && pwd)
key=$3
sign() { gpg --batch --yes --local-user "$key" --detach-sign --output "$1.sig" "$1"; }

# Each package under the name pacman asks for, with its signature beside it,
# which pacman downloads with it.
for pkg in "$pkgs"/*.pkg.tar.zst; do
  info=$(bsdtar -xOf "$pkg" .PKGINFO)
  field() { printf '%s\n' "$info" | sed -n "s/^$1 = //p"; }
  arch=$(field arch)
  file="$out/$arch/$(field pkgname)-$(field pkgver)-$arch.pkg.tar.zst"
  mkdir -p "$out/$arch"
  cp "$pkg" "$file"
  sign "$file"
done

db=$(mktemp -d)
trap 'rm -rf "$db"' EXIT
docker run --rm --network none --platform linux/amd64 --user "$(id -u):$(id -g)" -e HOME=/tmp \
  -v "$out:/repo:ro" -v "$db:/db" "$image" bash -euc '
    for dir in /repo/*/; do
      arch=$(basename "$dir")
      mkdir "/db/$arch"
      repo-add --quiet "/db/$arch/residue.db.tar.gz" "$dir"*.pkg.tar.zst
    done'

# Only the two databases of each architecture leave that directory, as
# regular files, and are signed here. repo-add makes residue.db and
# residue.files symbolic links, and GitHub Pages does not serve links, so
# each is a copy of the file it would point at.
for dir in "$out"/*/; do
  arch=$(basename "$dir")
  for name in residue.db residue.files; do
    made="$db/$arch/$name.tar.gz"
    if [ -L "$db/$arch" ] || [ -L "$made" ] || [ ! -f "$made" ]; then
      echo "repo-add made no $arch/$name.tar.gz" >&2
      exit 1
    fi
    cp "$made" "$dir/$name.tar.gz"
    sign "$dir/$name.tar.gz"
    cp "$dir/$name.tar.gz" "$dir/$name"
    cp "$dir/$name.tar.gz.sig" "$dir/$name.sig"
  done
done
