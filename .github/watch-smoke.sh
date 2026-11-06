#!/bin/sh
# End-to-end check of `residue watch` on the machine it runs on. CI runs it
# on every platform that has a change-event backend (inotify or kqueue).
#
#   sh .github/watch-smoke.sh PATH/TO/residue
#
# A bare repository stands in for the remote. Three worktrees, all aged a
# week into the past: `fix` and `wip` were pushed, `local` never was.
#   1. --once --dry-run logs that it would remove `fix`, and removes nothing.
#   2. After `fix`'s upstream branch is deleted and pruned, --once removes
#      `fix` and keeps `wip` and `local`.
#   3. A running watch notices `wip`'s upstream being deleted (a fetch
#      --prune, no polling) and removes it within two minutes.
#   4. --min-free 99.9 (disk pressure) removes the idle `local`.
# Every branch stays.
set -eu

residue=$1
case $residue in /*) ;; *) residue="$PWD/$residue" ;; esac
S=$(mktemp -d "${TMPDIR:-/tmp}/residue-watch.XXXXXX")
watcher=
trap 'if [ -n "$watcher" ]; then kill "$watcher" 2>/dev/null || true; wait "$watcher" 2>/dev/null || true; fi; rm -rf "$S"' EXIT
# The lock of a real `residue service` is not touched.
XDG_STATE_HOME="$S/state"
export XDG_STATE_HOME
# Commits and reflog entries a week old.
GIT_AUTHOR_DATE="2026-01-01T00:00:00Z"
GIT_COMMITTER_DATE="2026-01-01T00:00:00Z"
export GIT_AUTHOR_DATE GIT_COMMITTER_DATE

fail() {
  echo "watch-smoke: $1" >&2
  [ -f "$S/log" ] && cat "$S/log" >&2
  exit 1
}

git init -q --bare -b main "$S/remote.git"
git clone -q "$S/remote.git" "$S/app" 2> /dev/null
cd "$S/app"
git config user.email smoke@example.com
git config user.name smoke
echo hello > README.md
git add . && git commit -qm init && git push -q origin main
for name in fix wip local; do
  git worktree add -q "$S/app/.worktrees/$name" -b "$name"
  (cd "$S/app/.worktrees/$name" && echo "$name" >> README.md && git commit -qam "$name")
done
(cd .worktrees/fix && git push -q -u origin fix)
(cd .worktrees/wip && git push -q -u origin wip)

age() {
  find "$S/app/.worktrees" "$S/app/.git/worktrees" -exec touch -t 202601010000 {} +
}
age

# 1. fix landed; a dry run names it and removes nothing.
git push -q origin --delete fix
git fetch -q --prune
age
"$residue" watch --once --dry-run --grace 1m "$S" 2> "$S/log" || fail "dry run failed"
grep -q "would remove .*worktrees/fix" "$S/log" || fail "dry run did not name fix"
[ -d .worktrees/fix ] || fail "dry run removed fix"

# 2. fix landed: removed. wip and local stay.
"$residue" watch --once --grace 1m "$S" 2> "$S/log" || fail "--once failed"
grep -q "removed .*worktrees/fix" "$S/log" || fail "fix was not removed"
[ ! -e .worktrees/fix ] || fail "fix still exists"
[ -d .worktrees/wip ] && [ -d .worktrees/local ] || fail "removed more than fix"

# 3. A running watch reacts to wip's upstream being pruned.
"$residue" watch --grace 1m "$S" 2> "$S/log" &
watcher=$!
sleep 5
git push -q origin --delete wip
git fetch -q --prune
age
i=0
while [ -d .worktrees/wip ]; do
  i=$((i + 1))
  [ "$i" -le 120 ] || fail "the running watch did not remove wip within 2 minutes"
  sleep 1
done
grep -q "removed .*worktrees/wip" "$S/log" || fail "wip vanished without a log line"
kill "$watcher" && wait "$watcher" 2> /dev/null || true
watcher=
[ -d .worktrees/local ] || fail "the running watch removed local"
echo "watch-smoke: running watch removed wip after ${i}s"

# 4. Disk pressure removes the idle, never-pushed local.
"$residue" watch --once --min-free 99.9 --idle 1m "$S" 2> "$S/log" || fail "pressure run failed"
grep -q "removed .*worktrees/local" "$S/log" || fail "pressure did not remove local"
[ ! -e .worktrees/local ] || fail "local still exists"

for b in fix wip local; do
  git rev-parse --verify -q "$b" > /dev/null || fail "branch $b is gone"
done
echo "watch-smoke: ok"
