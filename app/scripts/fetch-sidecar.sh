#!/bin/sh
# Download the residue release pinned in sidecar.lock, verify it, and place it
# where Tauri's externalBin expects it: src-tauri/binaries/residue-<target>.
#
# Each archive must match the checksum pinned in sidecar.lock, appear with that
# checksum in the release's SHA256SUMS, and carry a build-provenance attestation
# from whiteheap/residue's release workflow for the pinned tag
# (`gh attestation verify`, which needs `gh auth login` or GH_TOKEN).
# Nothing is committed: src-tauri/binaries/ is gitignored.
#
# Usage: sh scripts/fetch-sidecar.sh [--force]
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
lock="$root/sidecar.lock"
out="$root/src-tauri/binaries"
repo=whiteheap/residue
force=0
[ "${1:-}" = "--force" ] && force=1

value() {
  v=$(sed -n "s/^$1=//p" "$lock")
  [ -n "$v" ] || { echo "fetch-sidecar: $1 is missing from sidecar.lock" >&2; exit 1; }
  printf '%s' "$v"
}

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

version=$(value version)
tag="v$version"

case "$(uname -s)" in
  Darwin) targets="aarch64-apple-darwin x86_64-apple-darwin" ;;
  *) echo "fetch-sidecar: only macOS is packaged so far" >&2; exit 1 ;;
esac

# The stamp records what was verified; a matching stamp skips the download.
stamp="$version"
for t in $targets; do stamp="$stamp $(value "sha256_$t")"; done
if [ "$force" = 0 ] && [ -f "$out/.verified" ] && [ "$(cat "$out/.verified")" = "$stamp" ]; then
  present=1
  for t in $targets universal-apple-darwin; do
    [ -x "$out/residue-$t" ] || present=0
  done
  if [ "$present" = 1 ]; then
    echo "fetch-sidecar: residue $version already verified in src-tauri/binaries"
    exit 0
  fi
fi

command -v gh >/dev/null 2>&1 || { echo "fetch-sidecar: gh is required to verify the attestation" >&2; exit 1; }

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT INT TERM

base="https://github.com/$repo/releases/download/$tag"
curl -fsSL --proto '=https' --tlsv1.2 -o "$work/SHA256SUMS" "$base/SHA256SUMS"

mkdir -p "$out"
rm -f "$out/.verified"
for t in $targets; do
  name="residue-$tag-$t.tar.gz"
  want=$(value "sha256_$t")
  curl -fsSL --proto '=https' --tlsv1.2 -o "$work/$name" "$base/$name"

  got=$(sha256 "$work/$name")
  if [ "$got" != "$want" ]; then
    echo "fetch-sidecar: $name has checksum $got, sidecar.lock pins $want" >&2
    exit 1
  fi
  if ! grep -qx "$want  $name" "$work/SHA256SUMS"; then
    echo "fetch-sidecar: the release's SHA256SUMS does not list $name with the pinned checksum" >&2
    exit 1
  fi
  gh attestation verify "$work/$name" \
    --repo "$repo" \
    --signer-workflow "$repo/.github/workflows/release.yml" \
    --source-ref "refs/tags/$tag" \
    --deny-self-hosted-runners >/dev/null
  echo "fetch-sidecar: $name: checksum and provenance attestation verified"

  mkdir "$work/$t"
  tar -xzf "$work/$name" -C "$work/$t" "residue-$tag-$t/residue"
  bin="$work/$t/residue-$tag-$t/residue"
  case "$t" in
    aarch64-*) arch=arm64 ;;
    x86_64-*) arch=x86_64 ;;
  esac
  if [ "$(lipo -archs "$bin")" != "$arch" ]; then
    echo "fetch-sidecar: $name does not hold a $arch binary" >&2
    exit 1
  fi
  install -m 0755 "$bin" "$out/residue-$t"
done

# `tauri build --target universal-apple-darwin` bundles residue-universal-apple-darwin.
lipo -create -output "$out/residue-universal-apple-darwin" \
  "$out/residue-aarch64-apple-darwin" "$out/residue-x86_64-apple-darwin"
chmod 0755 "$out/residue-universal-apple-darwin"

reported=$("$out/residue-universal-apple-darwin" --version)
if [ "$reported" != "residue $version" ]; then
  echo "fetch-sidecar: the binary reports '$reported', expected 'residue $version'" >&2
  exit 1
fi

printf '%s\n' "$stamp" > "$out/.verified"
echo "fetch-sidecar: placed residue $version in src-tauri/binaries ($targets, universal)"
