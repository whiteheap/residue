#!/bin/sh
# Place the residue CLI where Tauri's externalBin expects it:
# src-tauri/binaries/residue-<target>, for aarch64-apple-darwin,
# x86_64-apple-darwin and universal-apple-darwin (the two joined with lipo).
# Nothing here is committed: src-tauri/binaries/ is ignored.
#
#   sh scripts/sidecar.sh
#       Build residue from this repository's root for both macOS
#       architectures. `pnpm tauri dev` runs this first, and so does CI.
#   sh scripts/sidecar.sh --from ARM64_BINARY X86_64_BINARY
#       Use these two binaries instead. The release passes the binaries of
#       its two macOS archives, so the app carries the residue it publishes.
#   sh scripts/sidecar.sh --check
#       Only check what is placed. `tauri build` runs this, so it never
#       bundles a missing binary or one of another version.
#
# Each way ends with the same checks: each binary holds its one architecture,
# the universal one holds both, and it reports the version in ../Cargo.toml,
# which is the app's version too.
set -eu

app=$(cd "$(dirname "$0")/.." && pwd)
root=$(cd "$app/.." && pwd)
out="$app/src-tauri/binaries"

version=$(sed -n 's/^version = "\(.*\)"$/\1/p' "$root/Cargo.toml" | head -1)
[ -n "$version" ] || { echo "sidecar: no version in $root/Cargo.toml" >&2; exit 1; }

arch_is() {
  got=$(lipo -archs "$1" 2>/dev/null) || got="(not a Mach-O binary)"
  if [ "$got" != "$2" ]; then
    echo "sidecar: $1 holds $got, expected $2" >&2
    exit 1
  fi
}

check() {
  for t in aarch64-apple-darwin x86_64-apple-darwin universal-apple-darwin; do
    if [ ! -x "$out/residue-$t" ]; then
      echo "sidecar: src-tauri/binaries/residue-$t is missing; run sh scripts/sidecar.sh" >&2
      exit 1
    fi
  done
  arch_is "$out/residue-aarch64-apple-darwin" arm64
  arch_is "$out/residue-x86_64-apple-darwin" x86_64
  arch_is "$out/residue-universal-apple-darwin" "x86_64 arm64"
  # The universal binary runs its native slice on either kind of Mac.
  reported=$("$out/residue-universal-apple-darwin" --version)
  if [ "$reported" != "residue $version" ]; then
    echo "sidecar: the bundled binary reports '$reported', and Cargo.toml says $version; run sh scripts/sidecar.sh" >&2
    exit 1
  fi
}

place() {
  arch_is "$1" arm64
  arch_is "$2" x86_64
  mkdir -p "$out"
  install -m 0755 "$1" "$out/residue-aarch64-apple-darwin"
  install -m 0755 "$2" "$out/residue-x86_64-apple-darwin"
  rm -f "$out/residue-universal-apple-darwin"
  lipo -create -output "$out/residue-universal-apple-darwin" "$1" "$2"
  chmod 0755 "$out/residue-universal-apple-darwin"
}

case "${1:-}" in
  --check)
    check
    echo "sidecar: residue $version is in src-tauri/binaries"
    ;;
  --from)
    [ "$#" -eq 3 ] || { echo "usage: sh scripts/sidecar.sh --from ARM64_BINARY X86_64_BINARY" >&2; exit 2; }
    place "$2" "$3"
    check
    echo "sidecar: placed residue $version from $2 and $3"
    ;;
  "")
    # cargo runs in app/, so rustup takes the toolchain from
    # app/rust-toolchain.toml, which has both macOS targets. The CLI builds
    # as its own package: nothing in app/ is part of it.
    (cd "$app" && cargo build --locked --release \
      --manifest-path "$root/Cargo.toml" --target-dir "$root/target" \
      --target aarch64-apple-darwin --target x86_64-apple-darwin)
    place "$root/target/aarch64-apple-darwin/release/residue" \
      "$root/target/x86_64-apple-darwin/release/residue"
    check
    echo "sidecar: built residue $version from $root and placed it in src-tauri/binaries"
    ;;
  *)
    echo "usage: sh scripts/sidecar.sh [--from ARM64_BINARY X86_64_BINARY | --check]" >&2
    exit 2
    ;;
esac
