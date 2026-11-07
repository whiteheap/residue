#!/bin/sh
# Checks that the live site serves this checkout's site/public, byte for byte.
# Run from anywhere: sh site/smoke.sh [URL]. URL defaults to
# https://residue.whiteheap.com. It only reads. A new version can take a few
# seconds to reach every Cloudflare location, so it tries for about a minute
# before it fails.
#
# /           200, the bytes of index.html, a Content-Security-Policy header
# /NAME       200, the bytes of site/public/NAME, for every other served file
# a missing   404, the bytes of 404.html
#   path
set -u

url=${1:-https://residue.whiteheap.com}
url=${url%/}
root=$(cd "$(dirname "$0")/.." && pwd)
public="$root/site/public"
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT

# One pass over every file. Writes a line per mismatch to $scratch/failures.
pass() {
  : >"$scratch/failures"
  for file in "$public"/*; do
    name=$(basename "$file")
    want=200
    case $name in
      _headers) continue ;;
      index.html) target=/ ;;
      404.html) target=/residue-smoke-missing-page want=404 ;;
      *) target=/$name ;;
    esac
    status=$(curl -sS -o "$scratch/body" -D "$scratch/headers" -w '%{http_code}' "$url$target") ||
      status=error
    if [ "$status" != "$want" ]; then
      printf 'FAIL %s: HTTP %s, expected %s\n' "$target" "$status" "$want" >>"$scratch/failures"
    elif ! cmp -s "$file" "$scratch/body"; then
      printf 'FAIL %s: differs from site/public/%s\n' "$target" "$name" >>"$scratch/failures"
    elif [ "$name" = index.html ] && ! grep -qi '^content-security-policy:' "$scratch/headers"; then
      printf 'FAIL %s: no Content-Security-Policy header\n' "$target" >>"$scratch/failures"
    fi
  done
  [ ! -s "$scratch/failures" ]
}

attempt=1
while ! pass; do
  if [ "$attempt" -ge 12 ]; then
    cat "$scratch/failures"
    printf '%s does not serve site/public after %s attempts\n' "$url" "$attempt"
    exit 1
  fi
  attempt=$((attempt + 1))
  sleep 5
done
printf 'ok   %s serves site/public byte for byte (attempt %s)\n' "$url" "$attempt"
