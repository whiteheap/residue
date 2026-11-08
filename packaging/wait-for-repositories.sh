#!/usr/bin/env bash
# Wait until a URL serves the package repositories just published from DIR.
#
#   packaging/wait-for-repositories.sh DIR URL SECONDS
#
# DIR is the folder packaging/publish-repositories.sh pushed. The script
# fetches the file each client reads first, apt's InRelease, dnf's
# repomd.xml and pacman's residue.db for every architecture, from URL every
# 15 seconds until each one matches DIR byte for byte. It fails after
# SECONDS and names the files that still differ. Needs curl and cmp.
set -euo pipefail

if [ $# -ne 3 ]; then
  echo "usage: $0 DIR URL SECONDS" >&2
  exit 2
fi

dir=$(cd "$1" && pwd)
url=${2%/}
limit=$3

pending=()
for file in "$dir"/apt/dists/stable/InRelease "$dir"/rpm/*/repodata/repomd.xml "$dir"/arch/*/residue.db; do
  [ -f "$file" ] || { echo "$0: $file is missing" >&2; exit 1; }
  pending+=("${file#"$dir"/}")
done

got=$(mktemp)
trap 'rm -f "$got"' EXIT
deadline=$((SECONDS + limit))
while :; do
  left=()
  for file in "${pending[@]}"; do
    if curl -fs --max-time 30 -o "$got" "$url/$file" && cmp -s "$got" "$dir/$file"; then
      echo "served: $url/$file"
    else
      left+=("$file")
    fi
  done
  [ ${#left[@]} -gt 0 ] || exit 0
  pending=("${left[@]}")
  echo "not yet: ${pending[*]}"
  if [ "$SECONDS" -ge "$deadline" ]; then
    echo "$0: after ${limit}s, $url still does not serve the published bytes of: ${pending[*]}" >&2
    exit 1
  fi
  sleep 15
done
