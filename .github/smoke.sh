#!/bin/sh
# End-to-end check of a residue binary on the machine it runs on. CI runs it
# on every platform residue supports. It builds a scratch repository, checks
# three verdicts (one of them needs the activity probe to see a process),
# then removes the candidate and checks that its branch stayed.
#
#   sh .github/smoke.sh PATH/TO/residue
#
# POSIX sh, because the BSDs do not ship bash.
set -eu

residue=$1
case $residue in /*) ;; *) residue="$PWD/$residue" ;; esac
S=$(mktemp -d "${TMPDIR:-/tmp}/residue-smoke.XXXXXX")
busy=
# `wait` returns the killed process's status; `|| true` keeps it from
# becoming the script's exit status under `set -e`.
trap 'if [ -n "$busy" ]; then kill "$busy" 2>/dev/null || true; wait "$busy" 2>/dev/null || true; fi; rm -rf "$S"' EXIT

git init -q -b main "$S/app"
cd "$S/app"
git config user.email smoke@example.com
git config user.name smoke
printf 'node_modules/\n.env\n' > .gitignore
echo hello > README.md
git add . && git commit -qm init

# candidate: committed work plus regenerable output
git worktree add -q .worktrees/clean -b clean
mkdir .worktrees/clean/node_modules
echo x > .worktrees/clean/node_modules/a.js
# keep: an ignored file that is not on the regenerable list
git worktree add -q .worktrees/env -b env
echo 'TOKEN=1' > .worktrees/env/.env
# keep: a process has its working directory inside
git worktree add -q .worktrees/busy -b busy
(cd .worktrees/busy && exec sleep 300) > /dev/null 2>&1 &
busy=$!
sleep 1

"$residue" --version
out=$("$residue" scan .)
printf '%s\n' "$out"
expect() {
  if ! printf '%s\n' "$out" | grep -E "$1" > /dev/null; then
    echo "smoke: expected a line matching /$1/" >&2
    exit 1
  fi
}
expect 'candidate .*worktrees/clean'
expect 'keep .*worktrees/env'
expect 'in use: sleep\['

"$residue" remove --yes .worktrees/clean
test ! -e .worktrees/clean
git rev-parse --verify -q clean > /dev/null
echo "smoke: ok"
