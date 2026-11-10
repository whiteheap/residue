#!/usr/bin/env bash
# Build the signed apt, dnf and pacman repositories, then check every
# signature against the public key alone.
#
#   packaging/repositories.sh PKGS OUT KEYRING
#
# PKGS holds the .deb, .rpm and .pkg.tar.zst files. KEYRING is the armored
# public key; its secret half must be in the current GnuPG home. OUT
# receives the tree publish-repositories.sh pushes: apt/, rpm/, arch/ and
# the public key as residue-signing-key.asc. packages.yml runs it with the
# signing key, and in a dry run with a key made for the run. Needs what
# apt-repo.sh, rpm-repo.sh and arch-repo.sh need, and gpgv.
set -euo pipefail

if [ $# -ne 3 ]; then
  echo "usage: $0 PKGS OUT KEYRING" >&2
  exit 2
fi

here=$(cd "$(dirname "$0")" && pwd)
pkgs=$1
mkdir -p "$2"
out=$(cd "$2" && pwd)
keyring=$3
fpr=$(gpg --show-keys --with-colons "$keyring" | awk -F: '/^fpr/ { print $10; exit }')

"$here/apt-repo.sh" "$pkgs" "$out/apt" "$fpr"
"$here/rpm-repo.sh" "$pkgs" "$out/rpm" "$fpr"
"$here/arch-repo.sh" "$pkgs" "$out/arch" "$fpr"
cp "$keyring" "$out/residue-signing-key.asc"
gpg --dearmor < "$keyring" > "$out/apt/residue-archive-keyring.gpg"
cp "$here/rpm/residue.repo" "$out/rpm/residue.repo"

# gpgv with an empty home, so only the public key counts.
empty=$(mktemp -d)
trap 'rm -rf "$empty"' EXIT
check() { gpgv --homedir "$empty" --keyring "$out/apt/residue-archive-keyring.gpg" "$@"; }
check "$out/apt/dists/stable/InRelease"
check "$out/apt/dists/stable/Release.gpg" "$out/apt/dists/stable/Release"
for sig in "$out"/arch/*/*.sig "$out"/rpm/*/repodata/repomd.xml.asc; do
  check "$sig" "${sig%.*}"
done
