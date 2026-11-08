#!/bin/sh
# Cuts the site's two fonts to the characters in `unicodes` below and writes
# them to site/public. Run it when a page, hero.js, style.css or an SVG that
# docs/assets/render.sh draws gains a character the fonts lack: site/check.sh
# rule 11 names it. Add it to `unicodes`, run this, and write the lines it
# prints into site/check.sh. No check and no deploy runs this script; the
# fonts it writes are committed.
#
#   sh site/fonts.sh
#
# The source is IBM Plex Mono 400 and 500, latin, from @fontsource/ibm-plex-mono
# 5.3.0, pinned below by SHA-256. For each character it keeps, HarfBuzz's
# hb-subset keeps every table, hint and layout feature, and the font keeps
# every name record; Google's woff2_compress writes the WOFF2. Needs curl,
# tar and jq, hb-subset and hb-info (brew install harfbuzz), and
# woff2_compress and woff2_decompress (brew install woff2). The committed
# fonts were cut with HarfBuzz 12.3.0 and woff2 at 7b67e12 with Brotli 1.2.1;
# other versions may write other bytes, and so other hashes, for the same
# characters.
set -eu

# The characters to keep, in hex as hb-subset reads them: printable ASCII,
# the no-break space and the middle dot.
unicodes=20-7E,A0,B7

version=5.3.0
here=$(cd "$(dirname "$0")" && pwd)

for tool in curl tar jq hb-subset hb-info woff2_compress woff2_decompress; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    printf 'fonts.sh needs %s: brew install harfbuzz woff2\n' "$tool" >&2
    exit 1
  fi
done

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d ' ' -f 1
  else
    shasum -a 256 "$1" | cut -d ' ' -f 1
  fi
}

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
curl -fsSL -o "$work/package.tgz" "https://registry.npmjs.org/@fontsource/ibm-plex-mono/-/ibm-plex-mono-$version.tgz"
tar -xzf "$work/package.tgz" -C "$work" \
  package/files/ibm-plex-mono-latin-400-normal.woff2 package/files/ibm-plex-mono-latin-500-normal.woff2

# Each codepoint `unicodes` names, as hb-info writes it (U+0020), one a line.
jq -rn --arg u "$unicodes" '
  def hex: ascii_downcase | explode | reduce .[] as $c (0; . * 16 + (if $c >= 97 then $c - 87 else $c - 48 end));
  def tohex: if . < 16 then "0123456789ABCDEF"[.:. + 1] else (. / 16 | floor | tohex) + (. % 16 | tohex) end;
  $u | split(",")[] | split("-") | map(hex) | range(.[0]; (.[1] // .[0]) + 1) | tohex | "U+" + "0000"[length:] + .' |
  LC_ALL=C sort -u >"$work/wanted"

printf 'fonts_cover=%s\n' "$unicodes" >"$work/lines"
while read -r want weight; do
  name=ibm-plex-mono-latin-$weight-normal
  if [ "$(sha256 "$work/package/files/$name.woff2")" != "$want" ]; then
    printf 'fonts.sh: the %s weight of @fontsource/ibm-plex-mono %s is not the file its hash here names\n' "$weight" "$version" >&2
    exit 1
  fi
  woff2_decompress "$work/package/files/$name.woff2" >/dev/null
  hb-subset --unicodes="$unicodes" --name-IDs='*' --output-file="$work/$name.ttf" "$work/package/files/$name.ttf"
  # The font holds each character asked for: one the source lacks would be
  # missing while check.sh counts it in.
  hb-info --list-unicodes "$work/$name.ttf" | sed -n 's/^\(U+[0-9A-F]*\).*/\1/p' | LC_ALL=C sort -u >"$work/held"
  missing=$(LC_ALL=C comm -23 "$work/wanted" "$work/held" | tr '\n' ' ')
  if [ -n "$missing" ]; then
    printf 'fonts.sh: IBM Plex Mono %s has no glyph for %s\n' "$weight" "$missing" >&2
    exit 1
  fi
  woff2_compress "$work/$name.ttf" >/dev/null
  cp "$work/$name.woff2" "$here/public/$name.woff2"
  printf '%s %s.woff2\n' "$(sha256 "$here/public/$name.woff2")" "$name" >>"$work/lines"
done <<'SOURCE'
08949f728dc52d528e69b1667d15c89a5686a4ee9a296ff90983985f99c380f7 400
01d285447409c8a588692162439a038b8cbd7871309ee20267b0d2d91c6e8e22 500
SOURCE

printf 'Wrote the 400 and 500 weights to site/public. Write these lines into\n'
printf 'site/check.sh in place of the ones there, then run it:\n\n'
cat "$work/lines"
