#!/bin/sh
# Static checks for the residue site. Run from anywhere: sh site/check.sh
# Reads files only; needs no network and no Cloudflare account.
#
# 1. site/public holds only the files the site serves.
# 2. The logo and the chart, light and dark, are byte-identical to
#    docs/assets, their owner.
# 3. Every command on the page appears verbatim in README.md, and so does the
#    minimum Git version.
# 4. Every local path and #fragment the pages link to exists.
# 5. Every README anchor the pages link to is a README heading.
# 6. No script, no third-party resource, no form.
set -u

root=$(cd "$(dirname "$0")/.." && pwd)
public="$root/site/public"
readme="$root/README.md"
failures=0
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT

fail() {
  printf 'FAIL %s\n' "$*"
  failures=$((failures + 1))
}

ok() {
  printf 'ok   %s\n' "$*"
}

# 1. The publish directory allowlist.
expected='404.html
_headers
how-it-works-dark.svg
how-it-works.svg
index.html
logo-dark.svg
logo.svg
style.css
whiteheap-mark-black.svg
whiteheap-mark-white.svg'
actual=$(cd "$public" && find . -type f | sed 's|^\./||' | LC_ALL=C sort)
if [ "$actual" = "$expected" ]; then
  ok "site/public holds only the allowlisted files"
else
  fail "site/public differs from the allowlist in site/check.sh:"
  printf "%s\n" "$expected" > "$scratch/expected"
  printf "%s\n" "$actual" | diff -u "$scratch/expected" - || true
fi

# 2. Copies of the README artwork.
for asset in logo.svg logo-dark.svg how-it-works.svg how-it-works-dark.svg; do
  if cmp -s "$root/docs/assets/$asset" "$public/$asset"; then
    ok "$asset matches docs/assets/$asset"
  else
    fail "$asset differs from docs/assets/$asset; copy it again"
  fi
done

# 3. Commands and facts come from the README.
commands=$(sed -n 's|.*<pre[^>]*><code>\(.*\)</code></pre>.*|\1|p' "$public/index.html")
if [ -z "$commands" ]; then
  fail "no <pre><code> command found in index.html"
fi
printf '%s\n' "$commands" | while IFS= read -r command; do
  [ -n "$command" ] || continue
  case $command in
    *'&'*) printf 'FAIL command has an HTML entity, check it by hand: %s\n' "$command" ;;
    *)
      if grep -Fq -- "$command" "$readme"; then
        printf 'ok   command is in README.md: %s\n' "$command"
      else
        printf 'FAIL command is not in README.md: %s\n' "$command"
      fi
      ;;
  esac
done | tee "$scratch/commands"
failures=$((failures + $(grep -c "^FAIL" "$scratch/commands")))

git_version=$(sed -n 's/.*Git \([0-9][0-9.]*[0-9]\) or later.*/\1/p' "$public/index.html")
if [ -n "$git_version" ] && grep -Eq "\`git\` $git_version or later" "$readme"; then
  ok "minimum Git version $git_version matches README.md"
else
  fail "minimum Git version on the page ('$git_version') does not match README.md"
fi

# 4. Local links and fragments.
for page in "$public"/*.html; do
  name=$(basename "$page")
  for ref in $(grep -oE '(href|src)="/[^"]*"' "$page" | sed 's/^[a-z]*="\(.*\)"$/\1/' | sort -u); do
    path=${ref%%#*}
    case $path in
      /) path=/index.html ;;
    esac
    if [ -f "$public$path" ]; then
      ok "$name: $ref exists"
    else
      fail "$name: $ref has no file in site/public"
    fi
  done
  for fragment in $(grep -o 'href="#[^"]*"' "$page" | sed 's/^href="#\(.*\)"$/\1/' | sort -u); do
    if grep -q "id=\"$fragment\"" "$page"; then
      ok "$name: #$fragment exists"
    else
      fail "$name: #$fragment has no element with that id"
    fi
  done
done

# 5. README anchors, slugged the way GitHub does it: lowercase, punctuation
#    other than hyphens dropped, spaces to hyphens.
slugs=$(sed -n 's/^#\{1,6\} \(.*\)$/\1/p' "$readme" |
  tr '[:upper:]' '[:lower:]' |
  sed 's/[^a-z0-9 _-]//g; s/ /-/g')
for anchor in $(grep -oh 'https://github.com/whiteheap/residue#[^"]*' "$public"/*.html | sed 's/.*#//' | sort -u); do
  if [ "$anchor" = readme ] || printf '%s\n' "$slugs" | grep -qx -- "$anchor"; then
    ok "README anchor #$anchor exists"
  else
    fail "README anchor #$anchor is not a README heading"
  fi
done

# 6. Nothing runs and nothing loads from elsewhere.
if grep -Eil '<(script|form|iframe|object|embed)' "$public"/*.html "$public"/*.svg; then
  fail "a page or image above has a script, form or embed"
else
  ok "no script, form or embed"
fi
# rel="canonical" names the page's own address; it loads nothing.
if grep -Ei '(src|srcset)="(https?:)?//|<link[^>]+href="(https?:)?//|@import|url\((["'"'"']?)(https?:)?//' "$public"/*.html "$public"/*.css | grep -v 'rel="canonical" href="https://residue\.whiteheap\.com/"'; then
  fail "a file above loads a resource from another origin"
else
  ok "every resource loads from the site itself"
fi

if [ "$failures" -eq 0 ]; then
  printf 'site check passed\n'
else
  printf 'site check failed: %s problem(s)\n' "$failures"
  exit 1
fi
