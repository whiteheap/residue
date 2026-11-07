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
#   5. The removal child checks --only-if-upstream-gone itself, from fresh
#      local refs: an upstream restored before it runs keeps the worktree,
#      as do no upstream and a detached HEAD; a gone upstream is removed;
#      a dirty worktree stays refused; remove without it is unchanged.
# Every branch stays, and nothing goes to the trash: watch deletes.
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
[ ! -e .git/residue-trash ] || fail "watch moved fix to the trash instead of deleting it"

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
[ ! -e .git/residue-trash ] || fail "watch moved local to the trash instead of deleting it"

for b in fix wip local; do
  git rev-parse --verify -q "$b" > /dev/null || fail "branch $b is gone"
done

# 5. --only-if-upstream-gone in the removal child (#7). It runs `remove`
# exactly as watch does for a branch that landed.
git init -q --bare -b main "$S/child.git"
git clone -q "$S/child.git" "$S/child" 2> /dev/null
cd "$S/child"
git config user.email smoke@example.com
git config user.name smoke
echo hello > README.md
git add . && git commit -qm init && git push -q origin main
for name in back gone none dirty; do
  git worktree add -q "$S/child/.worktrees/$name" -b "$name"
  (cd "$S/child/.worktrees/$name" && echo "$name" >> README.md && git commit -qam "$name")
done
git worktree add -q --detach "$S/child/.worktrees/detached" HEAD
for name in back gone dirty; do
  (cd "$S/child/.worktrees/$name" && git push -q -u origin "$name")
  git push -q origin --delete "$name"
done
git fetch -q --prune
# back: its upstream came back between watch's scan and the child.
git update-ref refs/remotes/origin/back "$(git rev-parse back)"
echo edit >> .worktrees/dirty/README.md
find "$S/child/.worktrees" "$S/child/.git/worktrees" -exec touch -t 202601010000 {} +

child() { # PATH: sets $out and $st
  set +e
  out=$("$residue" remove --yes --json --no-trash --only-if-upstream-gone --idle 1m "$1")
  st=$?
  set -e
}
kept() { # NAME RESULT TEXT
  child ".worktrees/$1"
  [ "$st" = 3 ] || fail "$1: exit $st, want 3: $out"
  printf '%s' "$out" | grep -q "\"result\":\"$2\"" || fail "$1: want result $2: $out"
  printf '%s' "$out" | grep -q "$3" || fail "$1: want a reason with '$3': $out"
  [ -d ".worktrees/$1" ] || fail "$1: the worktree is gone"
  git worktree list --porcelain | grep -q "^worktree .*/\.worktrees/$1\$" || fail "$1: no longer registered"
  for leftover in .worktrees/.residue-removing-*; do
    [ ! -e "$leftover" ] || fail "$1: $leftover was left behind"
  done
  [ ! -e .git/residue-trash ] || [ -z "$(ls -A .git/residue-trash)" ] || fail "$1: a trash entry was left"
}
kept back skipped "its upstream origin/back exists in the local refs"
kept none skipped "branch none has no upstream"
kept detached skipped "detached HEAD, which has no upstream"
kept dirty refused "one or more checks did not pass"
child .worktrees/gone
[ "$st" = 0 ] || fail "gone: exit $st, want 0: $out"
[ ! -e .worktrees/gone ] || fail "gone: still exists"
# Without the requirement, remove is unchanged: back has its upstream and goes.
"$residue" remove --yes --no-trash --idle 1m .worktrees/back > /dev/null || fail "remove without --only-if-upstream-gone kept back"
[ ! -e .worktrees/back ] || fail "back: still exists after a plain remove"
set +e
"$residue" remove --all --only-if-upstream-gone "$S/child" > /dev/null 2>&1; st=$?
set -e
[ "$st" = 2 ] || fail "--all with --only-if-upstream-gone: exit $st, want 2"
for b in back gone none dirty; do
  git rev-parse --verify -q "$b" > /dev/null || fail "branch $b is gone"
done
echo "watch-smoke: ok"
