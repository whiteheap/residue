#!/bin/sh
# Checks that the live site serves this checkout's site/public, byte for byte.
# Run from anywhere: sh site/smoke.sh [URL]. URL defaults to
# https://residue.whiteheap.com. It only reads. A new version can take a few
# seconds to reach every Cloudflare location, so it tries for about a minute
# before it fails.
#
# /           200, the bytes of index.html, the Content-Security-Policy
#             header of public/_headers, exactly, and over https a
#             Strict-Transport-Security header with includeSubDomains and a
#             max-age of at least 2592000 (30 days), which the whiteheap.com
#             zone sends
# /NAME       200, the bytes of site/public/NAME, for every other served file,
#             and /.well-known/NAME for each file in public/.well-known; with
#             nosniff, a script or a font must also carry its own type;
#             install.sh is text/plain, each .txt file text/plain with
#             charset=utf-8 (security.txt and llms.txt require it), the
#             sitemap XML, and each .png image/png
# a missing   404, the bytes of 404.html
#   path
# crawlers    200 for /, robots.txt, sitemap.xml and llms.txt as each of
#             Googlebot, bingbot, OAI-SearchBot, Claude-SearchBot and
#             PerplexityBot, by user agent, so no bot rule turns them away
#
# It asks for each file with Accept: */*, never text/html: for a client that
# accepts HTML, Cloudflare adds its Web Analytics script to the page, and the
# bytes would no longer match.
set -u

url=${1:-https://residue.whiteheap.com}
url=${url%/}
# A browser ignores Strict-Transport-Security over plain http, so a local
# http:// run does not ask for it.
case $url in
  https://*) secure=yes ;;
  *) secure=no ;;
esac
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
  for agent in Googlebot/2.1 bingbot/2.0 OAI-SearchBot/1.0 Claude-SearchBot/1.0 PerplexityBot/1.0; do
    for target in / /robots.txt /sitemap.xml /llms.txt; do
      status=$(curl -sS -A "$agent" -o /dev/null -w '%{http_code}' "$url$target") || status=error
      [ "$status" = 200 ] ||
        printf 'FAIL %s as %s: HTTP %s, expected 200\n' "$target" "$agent" "$status" >>"$scratch/failures"
    done
  done
  for file in "$public"/* "$public"/.well-known/*; do
    [ -f "$file" ] || continue
    name=${file#"$public"/}
    want=200
    case $name in
      _headers) continue ;;
      index.html) target=/ ;;
      404.html) target=/residue-smoke-missing-page want=404 ;;
      *) target=/$name ;;
    esac
    # curl sends Accept: */* by default; this keeps it so.
    status=$(curl -sS -H 'Accept: */*' -o "$scratch/body" -D "$scratch/headers" -w '%{http_code}' "$url$target") ||
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
        # Text, so a browser shows the script instead of downloading it.
        install.sh) check=content-type expect='text/plain*' ;;
        *.txt) check=content-type expect='text/plain*charset=utf-8*' ;;
        sitemap.xml) check=content-type expect='*/xml*' ;;
        *.png) check=content-type expect='image/png*' ;;
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
      # HSTS comes from the whiteheap.com zone's setting, not from _headers
      # (site/README.md, "HTTPS").
      if [ "$name" = index.html ] && [ "$secure" = yes ]; then
        hsts=$(header strict-transport-security)
        lower=$(printf '%s\n' "$hsts" | tr '[:upper:]' '[:lower:]')
        age=$(printf '%s\n' "$lower" | tr ';' '\n' | sed -n 's/^ *max-age *= *"\{0,1\}\([0-9][0-9]*\)"\{0,1\} *$/\1/p')
        # No max-age, or more than one header, counts as none.
        case $age in
          '' | *[!0-9]*) age=0 ;;
        esac
        case $lower in
          *includesubdomains*) subdomains=yes ;;
          *) subdomains=no ;;
        esac
        if [ "$subdomains" = no ] || [ "$age" -lt 2592000 ]; then
          printf 'FAIL %s: strict-transport-security is "%s", expected includeSubDomains and a max-age of at least 2592000\n' "$target" "$hsts" >>"$scratch/failures"
        fi
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
printf 'ok   %s serves site/public byte for byte, to every crawler too (attempt %s)\n' "$url" "$attempt"
[ "$secure" = yes ] || printf 'note %s is not https, so Strict-Transport-Security was not checked\n' "$url"
