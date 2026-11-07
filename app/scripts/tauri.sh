#!/bin/sh
# The Tauri CLI, with the app's version set from the repository's Cargo.toml.
# The app and the residue it bundles have one version, so a release bumps one
# file. `pnpm tauri ...` runs this script.
#
# tauri.conf.json has no version: `tauri dev`, `build` and `bundle` get it here
# as a --config merge, which sets the bundle's CFBundleShortVersionString and
# the version the window shows. Other subcommands run unchanged.
set -eu

app=$(cd "$(dirname "$0")/.." && pwd)
version=$(sed -n 's/^version = "\(.*\)"$/\1/p' "$app/../Cargo.toml" | head -1)
[ -n "$version" ] || { echo "tauri.sh: no version in $app/../Cargo.toml" >&2; exit 1; }
tauri="$app/node_modules/.bin/tauri"
[ -x "$tauri" ] || { echo "tauri.sh: run pnpm install first" >&2; exit 1; }

case "${1:-}" in
  dev | build | bundle)
    command=$1
    shift
    exec "$tauri" "$command" --config "{\"version\":\"$version\"}" "$@"
    ;;
  *)
    exec "$tauri" "$@"
    ;;
esac
