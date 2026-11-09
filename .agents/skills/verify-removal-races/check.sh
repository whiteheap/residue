#!/usr/bin/env bash
# Model-check residue's removal pipeline and trash lifecycle with TLC.
#
#   bash .agents/skills/verify-removal-races/check.sh
#
# Checks model/Removal.tla in the fourteen setups below (a removal or a
# purge; no fault, a kill, a Git timeout, or residue killed alone; with
# the races between them left out; and a removal or a purge by a release
# that takes no hold, stopped with its Git or killed alone while its Git
# goes on), then runs each mutant in
# model/mutants, a model with one guard of the code removed, which must
# fail its rule. The result must match model/expected: every rule holds
# except the known breaks listed there, each with its shortest path.
#
# Exit status: 0 the result matches model/expected, 1 it does not, 2 the
# tools could not be set up or TLC did not run.
#
# The first run downloads TLC and a Java runtime, both pinned by version
# and SHA-256, into ${RESIDUE_TLA_TOOLS:-~/.cache/residue-tla}. Set
# TLA2TOOLS to a tla2tools.jar and JAVA to a java binary (Java 11 or later)
# to use your own; the run says when they differ from the pinned ones.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
model=$here/model
tools=${RESIDUE_TLA_TOOLS:-${XDG_CACHE_HOME:-$HOME/.cache}/residue-tla}

# TLC 2.19 (tla2tools v1.7.4, 2024-08-08).
tlc_url=https://github.com/tlaplus/tlaplus/releases/download/v1.7.4/tla2tools.jar
tlc_sha=936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88
# Eclipse Temurin JRE 25.0.5+8.
jre_base=https://github.com/adoptium/temurin25-binaries/releases/download/jdk-25.0.5%2B8
jre_dir=jdk-25.0.5+8-jre

sha256() {
  if command -v sha256sum >/dev/null; then sha256sum "$1" | cut -d' ' -f1
  else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

fetch() { # URL SHA256 FILE
  local tmp=$3.part
  if [ -f "$3" ] && [ "$(sha256 "$3")" = "$2" ]; then return 0; fi
  echo "downloading $1" >&2
  curl -fsSL --retry 3 -o "$tmp" "$1" || { echo "check.sh: cannot download $1" >&2; exit 2; }
  if [ "$(sha256 "$tmp")" != "$2" ]; then
    echo "check.sh: $1 has SHA-256 $(sha256 "$tmp"), not the pinned $2" >&2
    rm -f "$tmp"; exit 2
  fi
  mv "$tmp" "$3"
}

setup_tools() {
  mkdir -p "$tools"
  if [ -n "${TLA2TOOLS:-}" ]; then
    jar=$TLA2TOOLS
    [ "$(sha256 "$jar")" = "$tlc_sha" ] || echo "note: $jar is not the pinned tla2tools.jar v1.7.4" >&2
  else
    jar=$tools/tla2tools.jar
    fetch "$tlc_url" "$tlc_sha" "$jar"
  fi
  if [ -n "${JAVA:-}" ]; then
    java=$JAVA
    "$java" -version 2>&1 | grep -q 'Temurin-25\.0\.5+8' ||
      echo "note: $java is not the pinned Temurin 25.0.5+8" >&2
    return
  fi
  local os arch sha bin
  case "$(uname -s)-$(uname -m)" in
    Darwin-arm64) os=mac; arch=aarch64
      sha=72ebd76b502c849e88dea39afae51baf5f82f2b5b1b6389c28ea707b20451bce
      bin=Contents/Home/bin/java ;;
    Darwin-x86_64) os=mac; arch=x64
      sha=806902c796c670969b7b27df73b8f0fe0924a61bd65083aa53064115e49cab2d
      bin=Contents/Home/bin/java ;;
    Linux-x86_64) os=linux; arch=x64
      sha=6fac98f0c8202efc3135a9e9e42aa61963ec42d1c5cbb40caf231e7b14a24ef6
      bin=bin/java ;;
    Linux-aarch64) os=linux; arch=aarch64
      sha=963931723891302ebb2687178532c3d793d71e2577cd058061c7efb396140a86
      bin=bin/java ;;
    *) echo "check.sh: no pinned Java for $(uname -s) $(uname -m); set JAVA to a java binary" >&2; exit 2 ;;
  esac
  java=$tools/$arch-$os/$jre_dir/$bin
  if [ ! -x "$java" ]; then
    local tarball=$tools/jre-$arch-$os.tar.gz
    fetch "$jre_base/OpenJDK25U-jre_${arch}_${os}_hotspot_25.0.5_8.tar.gz" "$sha" "$tarball"
    mkdir -p "$tools/$arch-$os"
    tar -xzf "$tarball" -C "$tools/$arch-$os"
    # Only the unpacked runtime is used from here on.
    rm -f "$tarball"
  fi
}

work=$(mktemp -d "${TMPDIR:-/tmp}/residue-tla.XXXXXX")
trap 'rm -rf "$work"' EXIT
# TLC looks up this machine's name once per run. On macOS, a name that is
# not in /etc/hosts makes Java wait 5 s for that lookup (5.7 s against
# 0.9 s for one run of this model, measured 2026-11-09 on an M-series Mac).
# A hosts file read only by this JVM answers it at once.
printf '127.0.0.1 localhost %s\n' "$(hostname)" > "$work/hosts"

rules="TypeOK NotDeletedOnceBack OnlyCheckedStateDeleted ReflogCommitsKept RestoreTellsTheTruth OnlyOwnLocksLifted RecordDroppedOnlyWhenWhole"

# One TLC run. Prints "holds N" (N distinct states, all of them checked),
# "breaks RULE: ACTION ACTION ...", or "error: ...", and on standard error
# the path of TLC's output.
tlc() { # START FAULTS OFF AFTER OLD LATE BROKEN RULES...
  local start=$1 faults=$2 off=$3 after=$4 old=$5 late=$6 broken=$7; shift 7
  local dir; dir=$(mktemp -d "$work/run.XXXXXX")
  cp "$model/Removal.tla" "$dir/"
  {
    echo "SPECIFICATION Spec"
    echo "CONSTANTS"
    echo "  Start = \"$start\""
    echo "  Faults = $faults"
    echo "  Off = $off"
    echo "  After = $after"
    echo "  Old = $old"
    echo "  Late = $late"
    echo "  Broken = $broken"
    echo "INVARIANTS"
    for r in "$@"; do echo "  $r"; done
  } > "$dir/run.cfg"
  local code=0
  (cd "$dir" && "$java" -XX:+UseParallelGC -Djdk.net.hosts.file="$work/hosts" \
    -cp "$jar" tlc2.TLC -workers 1 -fp 0 \
    -config run.cfg -metadir "$dir/states" Removal.tla > tlc.out 2>&1) || code=$?
  local out=$dir/tlc.out
  if [ "$code" = 0 ] && grep -q "No error has been found" "$out"; then
    echo "holds $(sed -n 's/.* \([0-9]*\) distinct states found, 0 states left on queue.*/\1/p' "$out" | tail -1)"
  elif [ "$code" = 12 ] && grep -q "^Error: Invariant .* is violated" "$out"; then
    local rule path
    rule=$(sed -n 's/^Error: Invariant \(.*\) is violated.*/\1/p' "$out" | head -1)
    path=$(grep -v '^State 1: ' "$out" | sed -n 's/^State [0-9]*: <\([A-Za-z]*\).*/\1/p' | tr '\n' ' ' | sed 's/ $//')
    echo "breaks $rule: $path"
  else
    echo "error: TLC exited $code; $(grep -m1 -E 'Error|Exception|deadlock' "$out" || tail -1 "$out")"
  fi
  echo "$out" >&2
}

setup_tools
"$java" -version > /dev/null 2>&1 || { echo "check.sh: $java does not run" >&2; exit 2; }

found=$work/found
: > "$found"
failed=0
start_time=$(date +%s)

# Each setup: check every rule; when one breaks, record its shortest path
# and check the rest again without it. TLC stops at a rule's first break,
# so a rule that breaks is not checked further in that setup. Two kinds of
# setup leave out the races, so a rule a race breaks still gets checked
# in the rest: "unraced" switches off the restore and the Git
# timeout, and "after" lets the restores list only once the remove run or
# purge has stopped. "faults" is at most one kill of residue with its Git
# child (Ctrl-C) or one Git timeout; "orphan" is at most one kill of
# residue alone, which leaves its Git child running. "old" runs the remove
# run or purge as a release that takes no hold (OLD), and the trash
# restore lists only once that purge has written its record or stopped
# (LATE); "old-orphan" may kill that run alone, so its Git goes on with no
# hold to show it.
#
# Each entry: NAME KIND START FAULTS OFF AFTER OLD LATE.
for setup in "Remove no-faults orig {} {} {} {} {}" \
             "Remove faults orig {\"kill\",\"timeout\"} {} {} {} {}" \
             "Remove orphan orig {\"orphan\"} {} {} {} {}" \
             "Remove unraced orig {\"kill\",\"orphan\"} {\"rr\"} {} {} {}" \
             "Remove after orig {\"kill\",\"timeout\"} {} {\"rr\"} {} {}" \
             "Trash no-faults trash {} {} {} {} {}" \
             "Trash faults trash {\"kill\",\"timeout\"} {} {} {} {}" \
             "Trash orphan trash {\"orphan\"} {} {} {} {}" \
             "Trash unraced trash {\"kill\",\"orphan\"} {\"tr\"} {} {} {}" \
             "Trash after trash {\"kill\",\"timeout\"} {} {\"tr\",\"rr\"} {} {}" \
             "Remove old orig {\"kill\",\"timeout\"} {} {} {\"rm\"} {}" \
             "Trash old trash {\"kill\",\"timeout\"} {} {} {\"pg\"} {\"tr\"}" \
             "Remove old-orphan orig {\"orphan\"} {} {} {\"rm\"} {}" \
             "Trash old-orphan trash {\"orphan\"} {} {} {\"pg\"} {\"tr\"}"; do
  read -r name kind start faults off after old late <<< "$setup"
  label="$name $kind"
  left=$rules
  while :; do
    # shellcheck disable=SC2086
    result=$(tlc "$start" "$faults" "$off" "$after" "$old" "$late" "{}" $left 2>"$work/last")
    case $result in
      holds*)
        if [ "$left" = "$rules" ]; then
          echo "$label: every rule holds (${result#holds } states)"
        else
          echo "$label: the other rules hold (${result#holds } states)"
        fi
        break ;;
      breaks*)
        rule=${result#breaks }; rule=${rule%%:*}
        echo "$label: breaks $rule"
        echo "$label ${result#breaks }" >> "$found"
        left=$(echo " $left " | sed "s/ $rule / /; s/^ //; s/ $//")
        [ -n "$left" ] || break ;;
      *)
        echo "$label: $result (output: $(cat "$work/last"))" >&2
        exit 2 ;;
    esac
  done
done

# Each mutant must break the rule it names in a setup where the code, with
# no guard removed, holds that rule.
while read -r mutant start faults off rule old late; do
  case $mutant in ''|\#*) continue ;; esac
  old=${old:-\{\}} late=${late:-\{\}}
  control=$(tlc "$start" "$faults" "$off" "{}" "$old" "$late" "{}" "$rule" 2>"$work/last")
  if [ "${control%% *}" != holds ]; then
    echo "mutant $mutant: no control, $rule does not hold with Start $start, Faults $faults, Off $off, Old $old, Late $late: $control"
    failed=1
    continue
  fi
  result=$(tlc "$start" "$faults" "$off" "{}" "$old" "$late" "{\"$mutant\"}" "$rule" 2>"$work/last")
  case $result in
    "breaks $rule"*) echo "mutant $mutant: caught by $rule" ;;
    holds*) echo "mutant $mutant: NOT caught, $rule still holds"; failed=1 ;;
    *) echo "mutant $mutant: $result (output: $(cat "$work/last"))" >&2; exit 2 ;;
  esac
done < "$model/mutants"

# The found paths against the known ones.
{ grep -v -e '^#' -e '^$' "$model/expected" || true; } | sort > "$work/expected"
sort "$found" > "$work/found.sorted"
if ! diff -u "$work/expected" "$work/found.sorted" > "$work/diff"; then
  echo
  echo "The result differs from model/expected (- expected, + found):"
  sed -n '3,$p' "$work/diff" | grep '^[-+]' || true
  echo "A + line is a new break or a changed path: read it with /statechart and replay it against the code."
  echo "A - line alone is a known break that no longer happens: a fix, or a model that lost the path."
  failed=1
fi

echo
echo "$(grep -m1 -o 'TLC2 Version [^ ]*' "$(cat "$work/last")"), $("$java" -version 2>&1 | head -1); $(( $(date +%s) - start_time ))s"
if [ "$failed" = 0 ]; then
  known=$(wc -l < "$work/expected" | tr -d ' ')
  if [ "$known" = 0 ]; then
    echo "matches model/expected: every rule holds in every setup, and every mutant is caught"
  else
    echo "matches model/expected: every rule holds except the $known known breaks listed there, and every mutant is caught"
  fi
fi
exit "$failed"
