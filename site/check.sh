#!/bin/sh
# Static checks for the residue site. Run from anywhere: sh site/check.sh
# Reads files only; needs no network and no Cloudflare account.
#
# 1. site/public holds only the files the site serves; its one subdirectory
#    is .well-known.
# 2. The logo, light and dark, the dark chart, the social image and the PNG
#    icons are byte-identical to docs/assets, their owner. The fonts and their
#    license are the files of @fontsource/ibm-plex-mono 5.3.0, by SHA-256.
# 3. Every command on the page and in llms.txt appears verbatim in README.md,
#    and so does the minimum Git version.
# 4. Every local path and #fragment the pages link to exists, as a file or as
#    a redirect in _redirects.
# 5. Every README anchor the pages and llms.txt link to is a README heading.
# 6. One script only, /hero.js, same origin, and it loads and stores nothing.
#    No inline script or handler, no third-party resource, no form. The one
#    inline block allowed is the home page's JSON-LD, which is data that
#    browsers do not run.
# 7. The fonts load from the site itself, with font-display: swap, the 400
#    weight is preloaded, and the Content Security Policy is the exact one
#    below.
# 8. install.sh passes shellcheck (when it is installed), and the signing key
#    hashes it pins match packaging/signing-key.asc.
# 9. Search engines, link previews and agents: robots.txt has one group for
#    every crawler and the sitemap; sitemap.xml lists exactly the canonical
#    URLs; every page has a title that starts with the brand, and every
#    public page a description, a canonical, and Open Graph and Twitter tags;
#    the social image is 1200 by 630 and the icons their sizes; the JSON-LD
#    parses and has the fields search engines need (this needs jq); llms.txt
#    meets the llmstxt.org shape and Lighthouse's audit; _headers serves
#    every .txt file with charset=utf-8; the security.txt Expires is more
#    than 30 days and less than a year away; the IndexNow key file holds its
#    own name.
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

# 1. The publish directory allowlist. smoke.sh fetches each file as /NAME,
#    and each file in .well-known, the one subdirectory, as
#    /.well-known/NAME; a directory below those would not be fetched, so it
#    fails here.
expected='.well-known
.well-known/security.txt
404.html
_headers
_redirects
apple-touch-icon.png
b0581e2550b0bcc904c146120f116cc0.txt
hero.js
how-it-works-dark.svg
ibm-plex-mono-OFL.txt
ibm-plex-mono-latin-400-normal.woff2
ibm-plex-mono-latin-500-normal.woff2
icon-192.png
index.html
install.sh
llms.txt
logo-dark.svg
logo.svg
og.png
robots.txt
sitemap.xml
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

# 2. Copies of the artwork. The page is dark only; logo.svg stays as the
#    favicon, which sits on the browser's own light or dark tab bar, and so
#    does icon-192.png, its PNG. docs/assets/render.sh renders the PNGs.
for asset in logo.svg logo-dark.svg how-it-works-dark.svg og.png apple-touch-icon.png icon-192.png; do
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

# 3. Commands and facts come from the README: each <pre><code> on the page,
#    and each `code` span in llms.txt that starts with curl, brew or residue.
commands=$(sed -n 's|.*<pre[^>]*><code>\(.*\)</code></pre>.*|\1|p' "$public/index.html")
if [ -z "$commands" ]; then
  fail "no <pre><code> command found in index.html"
fi
llms_commands=$(grep -o '`[^`]*`' "$public/llms.txt" | tr -d '`' | grep -E '^(curl|brew|residue) ')
printf '%s\n%s\n' "$commands" "$llms_commands" | while IFS= read -r command; do
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
    elif grep -v '^#' "$public/_redirects" | cut -d ' ' -f 1 | grep -qxF -- "$path"; then
      ok "$name: $ref is a redirect in _redirects"
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
for anchor in $(grep -ohE 'https://(github\.com/whiteheap/residue|raw\.githubusercontent\.com/whiteheap/residue/main/README\.md)#[^")]*' "$public"/*.html "$public/llms.txt" | sed 's/.*#//' | sort -u); do
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
# The one other <script> tag: the home page's JSON-LD, a data block for
# search engines that browsers do not run. Rule 9 checks what it holds; any
# other type, or a src on it, counts as a script here.
data='<script type="application/ld+json">'
for page in "$public"/*.html; do
  name=$(basename "$page")
  tags=$(grep -oi '<script' "$page" | wc -l | tr -d ' ')
  exact=$(grep -oF "$script" "$page" | wc -l | tr -d ' ')
  blocks=$(grep -oF "$data" "$page" | wc -l | tr -d ' ')
  case $name in
    index.html) allowed=1 ;;
    *) allowed=0 ;;
  esac
  if [ "$tags" -eq $((exact + blocks)) ] && [ "$exact" -le 1 ] && [ "$blocks" -le "$allowed" ]; then
    ok "$name: no script other than /hero.js"
  else
    fail "$name: a script other than one $script, and on index.html one $data block"
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
csp="default-src 'none'; connect-src 'self'; script-src 'self'; style-src 'self'; img-src 'self'; font-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'"
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

# 9. Search engines, link previews and agents. A page with noindex (the 404
#    page) is not public: it needs only the title and the icons.
site=https://residue.whiteheap.com
noindex='<meta name="robots" content="noindex">'

# The content of a <meta> tag on one page, by its name or property.
meta() {
  sed -En "s#.*<meta (name|property)=\"$2\" content=\"([^\"]*)\">.*#\2#p" "$1"
}

# A PNG's size from its header, as WIDTHxHEIGHT; nothing when it is no PNG.
png_size() {
  if [ "$(od -An -tx1 -N8 "$1" | tr -d ' \n')" = 89504e470d0a1a0a ]; then
    od -An -tu1 -j16 -N8 "$1" | awk '{ for (i = 1; i <= NF; i++) b[n++] = $i }
      END { if (n == 8) printf "%dx%d\n", ((b[0] * 256 + b[1]) * 256 + b[2]) * 256 + b[3], ((b[4] * 256 + b[5]) * 256 + b[6]) * 256 + b[7] }'
  fi
}

# One group for every crawler, search engines and AI alike: a crawler with a
# group of its own stops reading the * group.
robots="$public/robots.txt"
if [ "$(grep -ci '^user-agent:' "$robots")" -eq 1 ] && grep -qix 'user-agent: \*' "$robots" &&
  ! grep -Eiq '^disallow:[[:space:]]*[^[:space:]]' "$robots"; then
  ok "robots.txt has one group, User-agent: *, that allows everything"
else
  fail "robots.txt must have one group, User-agent: *, that disallows nothing"
fi
if [ "$(grep -ci '^sitemap:' "$robots")" -eq 1 ] && grep -qxF "Sitemap: $site/sitemap.xml" "$robots"; then
  ok "robots.txt names the sitemap"
else
  fail "robots.txt must have one Sitemap line: Sitemap: $site/sitemap.xml"
fi
if grep -qi 'x-robots-tag' "$public/_headers"; then
  fail "_headers sets X-Robots-Tag; public pages must not carry noindex"
else
  ok "_headers sets no X-Robots-Tag"
fi
# Cloudflare serves a .txt file as text/plain with no charset, and llms.txt
# and security.txt need one. wrangler dev adds it anyway, so check the rule.
txt_type=$(awk '/^[^ #]/ { path = $0 } path == "/*.txt" && sub(/^  Content-Type: /, "")' "$public/_headers")
if [ "$txt_type" = 'text/plain; charset=utf-8' ]; then
  ok "_headers serves every .txt file as text/plain; charset=utf-8"
else
  fail "_headers needs a /*.txt rule with one line, Content-Type: text/plain; charset=utf-8"
fi

titles=
canonicals=
for page in "$public"/*.html; do
  name=$(basename "$page")
  title=$(sed -n 's|.*<title>\(.*\)</title>.*|\1|p' "$page")
  case $title in
    'residue · '?*) ok "$name: the title starts with the brand" ;;
    *) fail "$name: the title must start with 'residue · ', not '$title'" ;;
  esac
  titles="$titles$title
"
  missing=
  for link in '<link rel="icon" href="/logo.svg" type="image/svg+xml">' \
    '<link rel="icon" href="/icon-192.png" type="image/png" sizes="192x192">' \
    '<link rel="apple-touch-icon" href="/apple-touch-icon.png">'; do
    grep -qF "$link" "$page" || missing="$missing $link"
  done
  if [ -z "$missing" ]; then
    ok "$name links the SVG icon, the PNG icon and the Apple touch icon"
  else
    fail "$name lacks$missing"
  fi
  if grep -qF "$noindex" "$page"; then
    continue
  fi
  canonical=$(sed -n 's|.*<link rel="canonical" href="\([^"]*\)">.*|\1|p' "$page")
  case $canonical in
    "$site"/*) ok "$name has a canonical URL" ;;
    *) fail "$name: the canonical URL must be on $site, not '$canonical'" ;;
  esac
  canonicals="$canonicals$canonical
"
  description=$(meta "$page" description)
  if [ -n "$description" ]; then
    ok "$name has a meta description"
  else
    fail "$name has no meta description"
  fi
  if [ -n "$(meta "$page" og:image:alt)" ]; then
    ok "$name has og:image:alt"
  else
    fail "$name has no og:image:alt that says what the image shows"
  fi
  while read -r tag want; do
    got=$(meta "$page" "$tag")
    if [ "$got" = "$want" ]; then
      ok "$name has $tag"
    else
      fail "$name: $tag is '$got', expected '$want'"
    fi
  done <<TAGS
og:type website
og:site_name residue
og:url $canonical
og:title $title
og:description $description
og:image $site/og.png
og:image:width 1200
og:image:height 630
twitter:card summary_large_image
twitter:site @whiteheaphq
TAGS
done
if [ -z "$(printf '%s' "$titles" | sort | uniq -d)" ]; then
  ok "every page has its own title"
else
  fail "two pages share a title"
fi

locs=$(sed -n 's|.*<loc>\([^<]*\)</loc>.*|\1|p' "$public/sitemap.xml" | LC_ALL=C sort)
canonicals=$(printf '%s' "$canonicals" | LC_ALL=C sort)
if [ -n "$locs" ] && [ "$locs" = "$canonicals" ]; then
  ok "sitemap.xml lists exactly the public pages' canonical URLs"
else
  fail "sitemap.xml lists '$locs', and the public pages' canonical URLs are '$canonicals'"
fi
if command -v xmllint >/dev/null 2>&1; then
  if xmllint --noout "$public/sitemap.xml"; then
    ok "sitemap.xml is well-formed XML"
  else
    fail "sitemap.xml is not well-formed XML"
  fi
else
  printf 'skip sitemap.xml well-formedness: xmllint is not installed\n'
fi

while read -r file size; do
  got=$(png_size "$public/$file")
  if [ "$got" = "$size" ]; then
    ok "$file is a $size PNG"
  else
    fail "$file must be a $size PNG, not '$got'"
  fi
done <<'PNGS'
og.png 1200x630
apple-touch-icon.png 180x180
icon-192.png 192x192
PNGS

# The JSON-LD: the fields search engines need, matching the page. No
# softwareVersion, since nothing ties it to the release, and no ratings.
ld_rules='
def need(condition; problem): if condition then empty else problem end;
(.["@graph"] // []) as $graph
| [$graph[] | select(.["@type"] == "SoftwareApplication")] as $apps
| [$graph[] | select(.["@type"] == "WebSite")] as $sites
| ($apps[0] // {}) as $app
| ($sites[0] // {}) as $website
| need(.["@context"] == "https://schema.org"; "@context is not https://schema.org"),
  need(($apps | length) == 1; "not one SoftwareApplication"),
  need(($sites | length) == 1; "not one WebSite"),
  need($app.name == "residue"; "SoftwareApplication: name is not residue"),
  need($app.description == $description; "SoftwareApplication: description is not the meta description"),
  need($app.url == $url; "SoftwareApplication: url is not \($url)"),
  need($app.applicationCategory == "DeveloperApplication"; "SoftwareApplication: applicationCategory is not DeveloperApplication"),
  need(($app.operatingSystem | type) == "string"; "SoftwareApplication: no operatingSystem"),
  need($app.offers.price == "0"; "SoftwareApplication: offers.price is not 0"),
  need(($app.license // "") | startswith("https://"); "SoftwareApplication: no license URL"),
  need($app.softwareHelp.url == "https://github.com/whiteheap/residue#readme"; "SoftwareApplication: softwareHelp is not the README"),
  need($app.publisher["@id"] == $publisher; "SoftwareApplication: publisher is not \($publisher)"),
  need(($app | has("softwareVersion")) | not; "SoftwareApplication: softwareVersion, which nothing ties to the release"),
  need($website.url == $url; "WebSite: url is not \($url)"),
  need($website.publisher["@id"] == $publisher; "WebSite: publisher is not \($publisher)"),
  need([.. | objects | select(has("aggregateRating") or has("review"))] == []; "a rating or a review")
'
home="$public/index.html"
sed -n '\#^ *<script type="application/ld+json">$#,\#^ *</script>$#p' "$home" | sed '1d;$d' >"$scratch/ld.json"
if ! command -v jq >/dev/null 2>&1; then
  fail "jq is not installed; this check needs it to parse the JSON-LD"
elif [ ! -s "$scratch/ld.json" ]; then
  fail "index.html has no JSON-LD block, with its <script> and </script> tags on lines of their own"
elif problems=$(jq -r --arg url "$site/" --arg description "$(meta "$home" description)" \
  --arg publisher https://whiteheap.com/#organization "$ld_rules" "$scratch/ld.json"); then
  if [ -z "$problems" ]; then
    ok "index.html: the JSON-LD parses and has the fields search engines need"
  else
    printf '%s\n' "$problems" | sed 's/^/     /'
    fail "index.html: the JSON-LD has the problems above"
  fi
else
  fail "index.html: the JSON-LD does not parse"
fi

# llms.txt, for agents: llmstxt.org's shape (an H1, a > summary, then ##
# sections of links), which also meets Lighthouse's llms.txt audit (an H1, a
# markdown link and at least 50 characters).
llms="$public/llms.txt"
if [ "$(sed -n 1p "$llms" | cut -c1-2)" = '# ' ] && grep -q '^> ' "$llms" && grep -q '^## ' "$llms" &&
  grep -Eq '\[.+\]\(https://.+\)' "$llms" && [ "$(wc -c <"$llms")" -ge 50 ]; then
  ok "llms.txt has an H1, a > summary, sections of links and at least 50 characters"
else
  fail "llms.txt needs an H1 on its first line, a > summary, ## sections of [links](https://...), and at least 50 characters"
fi

# security.txt (RFC 9116): renew Expires before it is 30 days away, and keep
# it less than a year ahead.
security="$public/.well-known/security.txt"
for field in 'Contact: https://github.com/whiteheap/residue/security/advisories/new' \
  'Preferred-Languages: en' "Canonical: $site/.well-known/security.txt"; do
  if grep -qxF -- "$field" "$security"; then
    ok "security.txt has $field"
  else
    fail "security.txt lacks the line $field"
  fi
done
# The UTC time DAYS days from now, YYYY-MM-DDTHH:MM:SS: GNU date, else BSD.
days_from_now() {
  date -u -d "+$1 days" +%Y-%m-%dT%H:%M:%S 2>/dev/null || date -u -v+"$1"d +%Y-%m-%dT%H:%M:%S
}
expires=$(sed -n 's/^Expires: //p' "$security")
case $(grep -c '^Expires: ' "$security"):$expires in
  1:[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9]*Z)
    at=$(printf '%s' "$expires" | cut -c1-19)
    if LC_ALL=C expr "$at" \> "$(days_from_now 30)" >/dev/null &&
      LC_ALL=C expr "$(days_from_now 365)" \> "$at" >/dev/null; then
      ok "security.txt expires $expires, more than 30 days and less than a year away"
    else
      fail "security.txt expires $expires: set Expires more than 30 days and less than a year ahead"
    fi
    ;;
  *) fail "security.txt needs one Expires line in UTC, such as 2027-10-08T00:00:00.000Z" ;;
esac

# IndexNow: search engines fetch /KEY.txt to check that a submission came
# from this site, so the file holds its own name.
keys=$(cd "$public" && printf '%s\n' *.txt | grep -E '^[0-9a-f]{32}\.txt$')
case $keys in
  '' | *'
'*) fail "site/public needs exactly one IndexNow key file, KEY.txt with 32 hex digits, not '$keys'" ;;
  *)
    if printf '%s\n' "${keys%.txt}" | cmp -s - "$public/$keys"; then
      ok "$keys holds its own key, for IndexNow"
    else
      fail "$keys must hold one line, its own name without .txt"
    fi
    ;;
esac

if [ "$failures" -eq 0 ]; then
  printf 'site check passed\n'
else
  printf 'site check failed: %s problem(s)\n' "$failures"
  exit 1
fi
