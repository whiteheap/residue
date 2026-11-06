#!/usr/bin/env bash
# Build a signed dnf repository from .rpm files.
#
#   packaging/rpm-repo.sh RPMS OUT KEY
#
# RPMS holds the .rpm files. OUT receives one directory per architecture
# (x86_64, aarch64) with the packages signed by KEY, the fingerprint of a
# secret key in the current GnuPG home, and createrepo_c metadata whose
# repomd.xml has a detached signature. packaging/rpm/residue.repo points dnf
# at it. Needs rpm, rpmsign, createrepo_c and gpg.
set -euo pipefail

if [ $# -ne 3 ]; then
  echo "usage: $0 RPMS OUT KEY" >&2
  exit 2
fi

rpms=$(cd "$1" && pwd)
mkdir -p "$2"
out=$(cd "$2" && pwd)
key=$3

for rpm in "$rpms"/*.rpm; do
  arch=$(rpm -qp --qf '%{ARCH}' "$rpm")
  file="$out/$arch/$(rpm -qp --qf '%{NAME}-%{VERSION}-%{RELEASE}.%{ARCH}' "$rpm").rpm"
  mkdir -p "$out/$arch"
  cp "$rpm" "$file"
  rpmsign --define "_gpg_name $key" --define "__gpg $(command -v gpg)" --addsign "$file" >/dev/null 2>&1
done

for dir in "$out"/*/; do
  createrepo_c --quiet "$dir"
  gpg --batch --yes --local-user "$key" --armor --detach-sign \
    --output "$dir/repodata/repomd.xml.asc" "$dir/repodata/repomd.xml"
done
