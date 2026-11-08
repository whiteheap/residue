#!/bin/sh
# Renders the README artwork: each SVG in docs/assets to the PNG of the same
# name beside it, at twice its drawn size, on a transparent background.
# GitHub shows the PNGs, because it strips the styles and fonts an SVG would
# need. Text is set in IBM Plex Mono from the site's own copy of the font
# (site/public, pinned by site/check.sh), so a render looks the same on any
# machine that has Chrome.
#
#   sh docs/assets/render.sh                  every SVG here
#   sh docs/assets/render.sh hero.svg ...     only these
#
# Needs Google Chrome or Chromium. Set CHROME to its binary when it is not in
# the usual place. Reads the SVGs and the fonts, and writes only the PNGs.
#
# Each SVG keeps its <svg ...> start tag on one line, with a viewBox of whole
# numbers: that is the drawn size, and the PNG is twice it.
#
# This script does not copy anything to the site. After a change to logo.svg,
# logo-dark.svg or how-it-works-dark.svg, copy them to site/public as
# site/README.md says.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
fonts=$here/../../site/public

chrome=${CHROME:-}
if [ -z "$chrome" ]; then
  for candidate in \
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" \
    "/Applications/Chromium.app/Contents/MacOS/Chromium"; do
    if [ -x "$candidate" ]; then
      chrome=$candidate
      break
    fi
  done
fi
if [ -z "$chrome" ]; then
  for candidate in google-chrome google-chrome-stable chromium chromium-browser; do
    if command -v "$candidate" >/dev/null 2>&1; then
      chrome=$(command -v "$candidate")
      break
    fi
  done
fi
if [ -z "$chrome" ]; then
  echo "render.sh: no Chrome or Chromium found; set CHROME to its binary" >&2
  exit 1
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

face() {
  printf '@font-face{font-family:"IBM Plex Mono";font-style:normal;font-weight:%s;src:url(data:font/woff2;base64,%s) format("woff2")}\n' \
    "$1" "$(base64 < "$fonts/ibm-plex-mono-latin-$1-normal.woff2" | tr -d '\n')"
}
faces=$(face 400; face 500)

if [ "$#" -eq 0 ]; then
  set -- "$here"/*.svg
fi

for svg in "$@"; do
  case $svg in
    */*) ;;
    *) svg=$here/$svg ;;
  esac
  png=${svg%.svg}.png
  size=$(sed -n 's/.*<svg [^>]*viewBox="[0-9.-]* [0-9.-]* \([0-9]*\) \([0-9]*\)".*/\1 \2/p' "$svg" | head -n 1)
  if [ -z "$size" ]; then
    echo "render.sh: $svg: no <svg ... viewBox=\"X Y WIDTH HEIGHT\"> on one line, in whole numbers" >&2
    exit 1
  fi
  width=${size% *}
  height=${size#* }
  {
    printf '<!doctype html><meta charset="utf-8"><style>\n%s\n' "$faces"
    printf 'html,body{margin:0;background:transparent}body>svg{display:block;width:%spx;height:%spx}</style>\n' "$width" "$height"
    sed '/^<?xml /d' "$svg"
  } > "$tmp/page.html"
  "$chrome" --headless --disable-gpu --hide-scrollbars --no-first-run \
    --no-default-browser-check --user-data-dir="$tmp/profile" \
    --force-color-profile=srgb --force-device-scale-factor=2 \
    --default-background-color=00000000 --window-size="$width,$height" \
    --screenshot="$png" "file://$tmp/page.html" > "$tmp/chrome.log" 2>&1 || {
    cat "$tmp/chrome.log" >&2
    echo "render.sh: Chrome failed on $svg" >&2
    exit 1
  }
  printf '%s -> %s, %sx%s px\n' "${svg##*/}" "${png##*/}" "$((width * 2))" "$((height * 2))"
done
