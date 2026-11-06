#!/usr/bin/env bash
# Build a signed apt repository from .deb files.
#
#   packaging/apt-repo.sh DEBS OUT KEY
#
# DEBS holds the .deb files. OUT receives the repository: pool/ and the
# `stable` suite under dists/, with InRelease and Release.gpg signed by KEY,
# the fingerprint of a secret key in the current GnuPG home.
# Needs dpkg-deb, apt-ftparchive (apt-utils) and gpg.
set -euo pipefail

if [ $# -ne 3 ]; then
  echo "usage: $0 DEBS OUT KEY" >&2
  exit 2
fi

debs=$(cd "$1" && pwd)
mkdir -p "$2"
out=$(cd "$2" && pwd)
key=$3

pool="$out/pool/main/r/residue"
mkdir -p "$pool"
for deb in "$debs"/*.deb; do
  field() { dpkg-deb --field "$deb" "$1"; }
  cp "$deb" "$pool/$(field Package)_$(field Version)_$(field Architecture).deb"
done

cd "$out"
for arch in amd64 arm64; do
  dir="dists/stable/main/binary-$arch"
  mkdir -p "$dir"
  apt-ftparchive --arch "$arch" packages pool > "$dir/Packages"
  gzip -9nkf "$dir/Packages"
done

apt-ftparchive \
  -o APT::FTPArchive::Release::Origin=Whiteheap \
  -o APT::FTPArchive::Release::Label=residue \
  -o APT::FTPArchive::Release::Suite=stable \
  -o APT::FTPArchive::Release::Codename=stable \
  -o "APT::FTPArchive::Release::Architectures=amd64 arm64" \
  -o APT::FTPArchive::Release::Components=main \
  release dists/stable > Release.tmp
mv Release.tmp dists/stable/Release

gpg --batch --yes --local-user "$key" --clearsign --output dists/stable/InRelease dists/stable/Release
gpg --batch --yes --local-user "$key" --armor --detach-sign --output dists/stable/Release.gpg dists/stable/Release
