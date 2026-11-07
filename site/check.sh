#!/bin/sh
# Static checks for the residue site. Run from anywhere: sh site/check.sh
# Reads files only; needs no network and no Cloudflare account.
#
# 1. site/public holds only the files the site serves, with no subdirectory.
# 2. The logo, light and dark, and the dark chart are byte-identical to
#    docs/assets, their owner. The fonts and their license are the files of
#    @fontsource/ibm-plex-mono 5.3.0, by SHA-256.
# 3. Every command on the page appears verbatim in README.md, and so does the
#    minimum Git version.
# 4. Every local path and #fragment the pages link to exists.
# 5. Every README anchor the pages link to is a README heading.
# 6. One script only, /hero.js, same origin, and it loads and stores nothing.
#    No inline script or handler, no third-party resource, no form.
# 7. The fonts load from the site itself, with font-display: swap, the 400
#    weight is preloaded, and the Content Security Policy is the exact one
#    below.
# 8. install.sh passes shellcheck (when it is installed), and the signing key
#    hashes it pins match packaging/signing-key.asc.
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

# 1. The publish directory allowlist. Flat: smoke.sh fetches each file as
#    /NAME, so a subdirectory is listed here and fails.
expected='404.html
_headers
hero.js
how-it-works-dark.svg
ibm-plex-mono-OFL.txt
ibm-plex-mono-latin-400-normal.woff2
ibm-plex-mono-latin-500-normal.woff2
index.html
install.sh
logo-dark.svg
logo.svg
style.css
whiteheap-mark-white.svg'
actual=$(cd "$public" && find . -mindepth 1 | sed 's|^\./||' | LC_ALL=C sort)
if [ "$actual" = "$expected" ]; then
  ok "site/public holds only the allowlisted files"
else
  fail "site/public differs from the allowlist in site/check.sh:"
  printf "%s\n" "$expected" > "$scratch/expected"
  printf "%s\n" "$actual" | diff -u "$scratch/expected" - || true
fi

# 2. Copies of the README artwork. The page is dark only; logo.svg stays as
#    the favicon, which sits on the browser's own light or dark tab bar.
for asset in logo.svg logo-dark.svg how-it-works-dark.svg; do
  if cmp -s "$root/docs/assets/$asset" "$public/$asset"; then
    ok "$asset matches docs/assets/$asset"
  else
    fail "$asset differs from docs/assets/$asset; copy it again"
  fi
done

# The fonts and their license, from @fontsource/ibm-plex-mono 5.3.0 (npm pack
# @fontsource/ibm-plex-mono@5.3.0: files/ and LICENSE). To update them, copy
# the files again and replace these hashes in the same pull request.
sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d ' ' -f 1
  else
    shasum -a 256 "$1" | cut -d ' ' -f 1
  fi
}
while read -r want file; do
  if [ "$(sha256 "$public/$file")" = "$want" ]; then
    ok "$file is the @fontsource/ibm-plex-mono 5.3.0 file"
  else
    fail "$file is not the @fontsource/ibm-plex-mono 5.3.0 file its hash in site/check.sh names"
  fi
done <<'FONTS'
08949f728dc52d528e69b1667d15c89a5686a4ee9a296ff90983985f99c380f7 ibm-plex-mono-latin-400-normal.woff2
01d285447409c8a588692162439a038b8cbd7871309ee20267b0d2d91c6e8e22 ibm-plex-mono-latin-500-normal.woff2
23b0a9d0c6d3f140a0b77e483c5cfa6bba574325ef5cb189ed9f2fec4884533f ibm-plex-mono-OFL.txt
FONTS

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
# The stylesheet's url()s: each one an absolute path to a file here.
for ref in $(grep -oE 'url\([^)]*\)' "$public/style.css" | sed 's/^url(//; s/)$//; s/^["'"'"']//; s/["'"'"']$//' | sort -u); do
  case $ref in
    /*)
      if [ -f "$public$ref" ]; then
        ok "style.css: $ref exists"
      else
        fail "style.css: $ref has no file in site/public"
      fi
      ;;
    *) fail "style.css: url($ref) is not a path on the site itself" ;;
  esac
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

# 6. One script, from the site itself, and nothing else runs. The Content
#    Security Policy (rule 7) enforces this in the browser; these lines keep
#    a change that would need a looser policy from reaching it.
script='<script src="/hero.js" defer></script>'
for page in "$public"/*.html; do
  name=$(basename "$page")
  tags=$(grep -oi '<script' "$page" | wc -l | tr -d ' ')
  exact=$(grep -oF "$script" "$page" | wc -l | tr -d ' ')
  if [ "$tags" -eq "$exact" ] && [ "$tags" -le 1 ]; then
    ok "$name: no script other than /hero.js"
  else
    fail "$name: a script other than one $script"
  fi
done
if grep -qF "$script" "$public/index.html"; then
  ok "index.html loads /hero.js"
else
  fail "index.html does not load the hero with $script"
fi
if grep -Eil '<script|<foreignObject' "$public"/*.svg; then
  fail "an image above has a script or foreignObject"
else
  ok "no script in an image"
fi
if grep -EiH '<[^>]*[[:space:]]on[a-z]+[[:space:]]*=|javascript:' "$public"/*.html "$public"/*.svg; then
  fail "a file above has an inline event handler or a javascript: URL"
else
  ok "no inline event handler or javascript: URL"
fi
if grep -Eil '<(form|iframe|frame|object|embed)' "$public"/*.html "$public"/*.svg; then
  fail "a page or image above has a form or embed"
else
  ok "no form or embed"
fi
# hero.js draws on its canvas and does nothing else: no request, no storage,
# no new element, no code built from a string.
if grep -En 'fetch\(|XMLHttpRequest|WebSocket|EventSource|sendBeacon|import|eval\(|Function\(|cookie|[lL]ocalStorage|sessionStorage|indexedDB|createElement|innerHTML|insertAdjacent|document\.write|://' "$public/hero.js"; then
  fail "hero.js, at the lines above, could load, store or run something"
else
  ok "hero.js loads, stores and runs nothing else"
fi
# rel="canonical" names the page's own address; it loads nothing.
if grep -Ei '(src|srcset)="(https?:)?//|<link[^>]+href="(https?:)?//|@import|url\((["'"'"']?)(https?:)?//' "$public"/*.html "$public"/*.css | grep -v 'rel="canonical" href="https://residue\.whiteheap\.com/"'; then
  fail "a file above loads a resource from another origin"
else
  ok "every resource loads from the site itself"
fi

# 7. Fonts and the Content Security Policy. The policy allows the site's own
#    script, styles, images and fonts, and nothing else. Change it here and in
#    public/_headers together.
csp="default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self'; font-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'"
if [ "$(sed -n 's/^  Content-Security-Policy: //p' "$public/_headers")" = "$csp" ]; then
  ok "_headers has the Content Security Policy, exactly"
else
  fail "_headers' Content-Security-Policy is not exactly: $csp"
fi
faces=$(grep -c '@font-face' "$public/style.css")
swaps=$(grep -c 'font-display: swap;' "$public/style.css")
if [ "$faces" -gt 0 ] && [ "$faces" -eq "$swaps" ]; then
  ok "each of the $faces @font-face rules has font-display: swap"
else
  fail "style.css has $faces @font-face rules and $swaps font-display: swap"
fi
preload='<link rel="preload" href="/ibm-plex-mono-latin-400-normal.woff2" as="font" type="font/woff2" crossorigin>'
for page in "$public"/*.html; do
  if grep -qF "$preload" "$page"; then
    ok "$(basename "$page") preloads the 400 weight"
  else
    fail "$(basename "$page") does not preload the 400 weight with $preload"
  fi
done

# 8. The install script. shellcheck is optional here; CI always runs it.
install_sh="$public/install.sh"
if command -v shellcheck >/dev/null 2>&1; then
  if shellcheck -s sh "$install_sh"; then
    ok "install.sh passes shellcheck"
  else
    fail "install.sh has shellcheck findings above"
  fi
else
  printf 'skip install.sh shellcheck: shellcheck is not installed\n'
fi
pinned() { sed -n "s/^$1=\([0-9a-fA-F]*\)$/\1/p" "$install_sh"; }
key="$root/packaging/signing-key.asc"
asc_sha=$(sha256 "$key")
if [ "$asc_sha" = "$(pinned KEY_ASC_SHA256)" ]; then
  ok "install.sh pins the SHA-256 of packaging/signing-key.asc"
else
  fail "install.sh KEY_ASC_SHA256 must be $asc_sha, the SHA-256 of packaging/signing-key.asc"
fi
if command -v gpg >/dev/null 2>&1; then
  gpg --dearmor < "$key" > "$scratch/keyring.gpg"
  gpg_sha=$(sha256 "$scratch/keyring.gpg")
  if [ "$gpg_sha" = "$(pinned KEY_GPG_SHA256)" ]; then
    ok "install.sh pins the SHA-256 of the apt keyring made from it"
  else
    fail "install.sh KEY_GPG_SHA256 must be $gpg_sha, the SHA-256 of gpg --dearmor < packaging/signing-key.asc"
  fi
  fpr=$(gpg --show-keys --with-colons "$key" | awk -F: '$1 == "fpr" { print $10; exit }')
  if [ "$fpr" = "$(sed -n 's/^KEY_FPR=//p' "$install_sh")" ]; then
    ok "install.sh names the fingerprint of packaging/signing-key.asc"
  else
    fail "install.sh KEY_FPR must be $fpr, the fingerprint of packaging/signing-key.asc"
  fi
else
  printf 'skip install.sh apt keyring and fingerprint: gpg is not installed\n'
fi

if [ "$failures" -eq 0 ]; then
  printf 'site check passed\n'
else
  printf 'site check failed: %s problem(s)\n' "$failures"
  exit 1
fi
