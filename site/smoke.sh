#!/bin/sh
# Checks that the live site serves this checkout's site/public, byte for byte.
# Run from anywhere: sh site/smoke.sh [URL]. URL defaults to
# https://residue.whiteheap.com. It only reads. A new version can take a few
# seconds to reach every Cloudflare location, so it tries for about a minute
# before it fails.
#
# /           200, the bytes of index.html, and the Content-Security-Policy
#             header of public/_headers, exactly
# /NAME       200, the bytes of site/public/NAME, for every other served file;
#             with nosniff, a script or a font must also carry its own type
# a missing   404, the bytes of 404.html
#   path
set -u

url=${1:-https://residue.whiteheap.com}
url=${url%/}
root=$(cd "$(dirname "$0")/.." && pwd)
public="$root/site/public"
# The policy public/_headers sets, the one line check.sh holds to its value.
csp=$(sed -n 's/^  Content-Security-Policy: //p' "$public/_headers")
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT

# The value of one response header, by its lower-case name.
header() {
  tr -d '\r' <"$scratch/headers" | grep -i "^$1:" | sed 's/^[^:]*: *//'
}

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
    else
      case $name in
        index.html) check=content-security-policy expect=$csp ;;
        *.js) check=content-type expect='text/javascript*' ;;
        *.woff2) check=content-type expect='font/woff2*' ;;
        *) check= ;;
      esac
      if [ -n "$check" ]; then
        got=$(header "$check")
        # expect is a case pattern; the policy has no pattern character.
        case $got in
          $expect) ;;
          *) printf 'FAIL %s: %s is "%s", expected "%s"\n' "$target" "$check" "$got" "$expect" >>"$scratch/failures" ;;
        esac
      fi
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
