#!/usr/bin/env bash
# Build .deb, .rpm and Arch packages from the static Linux release archives.
#
#   packaging/linux-packages.sh TAG ARCHIVES OUT
#
# ARCHIVES holds residue-TAG-{x86_64,aarch64}-unknown-linux-musl.tar.gz.
# The packages are written to OUT with names that do not change between
# releases, so releases/latest/download/NAME always points at the newest one.
# Needs nfpm on PATH.
set -euo pipefail

if [ $# -ne 3 ]; then
  echo "usage: $0 TAG ARCHIVES OUT" >&2
  exit 2
fi

tag=$1
version=${tag#v}
archives=$(cd "$2" && pwd)
mkdir -p "$3"
out=$(cd "$3" && pwd)
config="$(cd "$(dirname "$0")" && pwd)/nfpm.yaml"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

for cpu in x86_64 aarch64; do
  case $cpu in
    x86_64) arch=amd64 ;;
    aarch64) arch=arm64 ;;
  esac
  name="residue-$tag-$cpu-unknown-linux-musl"
  tar -xzf "$archives/$name.tar.gz" -C "$work"
  for packager in deb rpm archlinux; do
    case $packager in
      deb) file="residue_$arch.deb" ;;
      rpm) file="residue.$cpu.rpm" ;;
      archlinux) file="residue-$cpu.pkg.tar.zst" ;;
    esac
    (cd "$work/$name" && VERSION=$version ARCH=$arch \
      nfpm package --config "$config" --packager "$packager" --target "$out/$file")
  done
done
