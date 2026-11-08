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
#      `fix` and keeps `wip` and `local`; the log says what it freed, and
#      `residue stats` counts it under watch.
#   3. A running watch notices `wip`'s upstream being deleted (a fetch
#      --prune, no polling) and removes it within two minutes.
#   4. --min-free 99.9 (disk pressure) removes the idle `local`.
#   5. The removal child checks --only-if-upstream-gone itself, from fresh
#      local refs: an upstream restored before it runs keeps the worktree,
#      as do no upstream and a detached HEAD; a gone upstream is removed;
#      a failed check still wins (refused, not skipped); remove without it
#      is unchanged.
#   6. watch passes the reason, and the child reads it at its plan, at the
#      re-check before the move and after the move, right before deleting:
#      a git wrapper restores the upstream right before each read in turn,
#      and once during the re-measure that precedes the last read.
#      A worktree kept that way is not taken as disk pressure in the same
#      pass; the next pass judges pressure on its own terms. These runs
#      pass --no-fetch: they set remote-tracking refs by hand.
#   7. Without a ROOT, watch uses the code folders in HOME (a scratch one).
#      A branch deleted on the remote with no local fetch: --no-fetch
#      keeps its worktree; watch's own fetch prunes the ref, writes no
#      FETCH_HEAD, and the worktree is removed. A remote it cannot reach is
#      logged, its refs stay, and its worktree stays. A HOME with none of
#      the folders is a usage error that names them.
#   8. A removal's log line ends with `restore: residue restore RECORD`;
#      that restore brings the worktree back on its branch and HEAD. The
#      restored worktree is never removed again because its work landed,
#      with watch fetching, past --grace; disk pressure still removes it.
# Every branch stays, and nothing goes to the trash: watch deletes.
# On NetBSD, OpenBSD and DragonFly (scripts/partial-probe.sh) no worktree
# is a candidate: there it checks that every worktree is `unknown` for the
# probe's reason and that dry, landed and disk-pressure runs remove none,
# skips 1-6 and 8 by name, and runs 7 expecting no removal.
set -eu

residue=$1
case $residue in /*) ;; *) residue="$PWD/$residue" ;; esac
here=$(cd "$(dirname "$0")" && pwd)
# shellcheck source=SCRIPTDIR/../scripts/partial-probe.sh
. "$here/../scripts/partial-probe.sh"
S=$(mktemp -d "${TMPDIR:-/tmp}/residue-watch.XXXXXX")
watcher=
trap 'if [ -n "$watcher" ]; then kill "$watcher" 2>/dev/null || true; wait "$watcher" 2>/dev/null || true; fi; rm -rf "$S"' EXIT
# The lock of a real `residue service` is not touched, and the stats ledger
# is the scratch home folder's, never the caller's.
XDG_STATE_HOME="$S/state"
HOME="$S/home"
export XDG_STATE_HOME HOME
mkdir -p "$HOME"
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

# 7. No ROOT, and watch's own fetch. Its own repositories, in a home
# folder of its own. Runs last, and on NetBSD, OpenBSD and DragonFly too:
# the fetch does not depend on the activity probe.
fetch_case() {
  # Canonical, so the log shows ~/src (macOS's TMPDIR is under a symlink).
  H="$(cd "$S" && pwd -P)/home"
  mkdir -p "$H/src"
  for repo in app7 far; do
    git init -q --bare -b main "$S/$repo.git"
    git clone -q "$S/$repo.git" "$H/src/$repo" 2> /dev/null
    git -C "$H/src/$repo" config user.email smoke@example.com
    git -C "$H/src/$repo" config user.name smoke
    echo hello > "$H/src/$repo/README.md"
    git -C "$H/src/$repo" add README.md
    git -C "$H/src/$repo" commit -qm init
    git -C "$H/src/$repo" push -q origin main
  done
  for name in landed stays; do
    git -C "$H/src/app7" worktree add -q "$H/src/app7/.worktrees/$name" -b "$name"
    git -C "$H/src/app7/.worktrees/$name" push -q -u origin "$name"
  done
  git -C "$H/src/far" worktree add -q "$H/src/far/.worktrees/away" -b away
  git -C "$H/src/far/.worktrees/away" push -q -u origin away
  # Deleted on the remotes, with no fetch here: only watch's own fetch can
  # see it. far's remote then becomes unreachable.
  git -C "$S/app7.git" branch -q -D landed
  git -C "$S/far.git" branch -q -D away
  mv "$S/far.git" "$S/far-moved.git"
  find "$H/src/app7/.worktrees" "$H/src/app7/.git/worktrees" "$H/src/far/.worktrees" "$H/src/far/.git/worktrees" \
    -exec touch -t 202601010000 {} +

  # --no-fetch: the local refs still have origin/landed, so nothing landed.
  HOME="$H" "$residue" watch --once --no-fetch --grace 1m 2> "$S/log" || fail "7: the --no-fetch run failed"
  grep -q "residue watch: watching ~/src (.*the usual code folders; name ROOTs to change" "$S/log" \
    || fail "7: watch without a ROOT did not log ~/src as the usual code folders"
  grep -q "fetch: off (--no-fetch)" "$S/log" || fail "7: --no-fetch was not logged"
  ! grep -q -E "fetched |removed " "$S/log" || fail "7: --no-fetch fetched or removed"
  git -C "$H/src/app7" rev-parse -q --verify refs/remotes/origin/landed > /dev/null || fail "7: --no-fetch changed the refs"

  # Fetching: watch prunes origin/landed, then removes landed. far's fetch
  # fails: logged, its refs as they were, nothing removed for it.
  HOME="$H" "$residue" watch --once --grace 1m 2> "$S/log" || fail "7: the fetching run failed"
  grep -q "fetched ~/src/app7: origin/landed is gone from the remote" "$S/log" || fail "7: no line for the pruned origin/landed"
  grep -q "fetch of origin in ~/src/far failed; its refs stay as they were" "$S/log" \
    || fail "7: the failed fetch was not logged"
  ! git -C "$H/src/app7" rev-parse -q --verify refs/remotes/origin/landed > /dev/null || fail "7: origin/landed was not pruned"
  git -C "$H/src/app7" rev-parse -q --verify refs/remotes/origin/stays > /dev/null || fail "7: origin/stays is gone"
  git -C "$H/src/far" rev-parse -q --verify refs/remotes/origin/away > /dev/null || fail "7: the failed fetch changed far's refs"
  [ -d "$H/src/far/.worktrees/away" ] || fail "7: away is gone after a failed fetch"
  [ -d "$H/src/app7/.worktrees/stays" ] || fail "7: stays is gone"
  for dir in "$H/src/app7/.git" "$H/src/app7/.git/worktrees/landed" "$H/src/far/.git"; do
    [ ! -e "$dir/FETCH_HEAD" ] || fail "7: the fetch wrote $dir/FETCH_HEAD, which resets the idle clock"
  done
  if [ -n "$partial_probe" ]; then
    ! grep -q "removed " "$S/log" || fail "7: removed a worktree with a partial probe"
    [ -d "$H/src/app7/.worktrees/landed" ] || fail "7: landed is gone with a partial probe"
  else
    grep -q "removed ~/src/app7/.worktrees/landed (.*): origin/landed was deleted" "$S/log" \
      || fail "7: landed was not removed after watch's fetch"
    [ ! -e "$H/src/app7/.worktrees/landed" ] || fail "7: landed still exists"
  fi
  for b in landed stays; do
    git -C "$H/src/app7" rev-parse --verify -q "$b" > /dev/null || fail "7: branch $b is gone"
  done

  # No ROOT and none of the folders: a one-line usage error naming them.
  mkdir "$S/bare-home"
  set +e
  HOME="$S/bare-home" "$residue" watch --once 2> "$S/log"
  st=$?
  set -e
  [ "$st" = 2 ] || fail "7: no ROOT and no code folder: exit $st, want 2"
  grep -q "none of the usual code folders exists in your home folder (~/src, " "$S/log" || fail "7: the usage error does not name the folders"
  echo "watch-smoke: 7. watch without a ROOT fetched, pruned and kept what it should"
}

# 1. fix landed; a dry run names it and removes nothing.
git push -q origin --delete fix
git fetch -q --prune
age

# NetBSD, OpenBSD and DragonFly (scripts/partial-probe.sh): no worktree is
# a candidate, so watch must remove none, for a landed branch or for disk
# pressure, and every case that expects a removal is skipped by name.
if [ -n "$partial_probe" ]; then
  out=$("$residue" scan "$S")
  printf '%s\n' "$out"
  for name in fix wip local; do
    printf '%s\n' "$out" | grep -A 1 -E "^ +unknown .*worktrees/$name " | grep -q -E "$partial_probe_short_re" \
      || fail "$name is not unknown with '$partial_probe_short'"
  done
  "$residue" watch --once --dry-run --grace 1m "$S" 2> "$S/log" || fail "dry run failed"
  ! grep -q "would remove" "$S/log" || fail "dry run would remove a worktree"
  "$residue" watch --once --grace 1m "$S" 2> "$S/log" || fail "--once failed"
  ! grep -q "removed " "$S/log" || fail "--once removed a worktree"
  "$residue" watch --once --min-free 99.9 --idle 1m "$S" 2> "$S/log" || fail "pressure run failed"
  grep -q "and no candidate unchanged for 1m" "$S/log" || fail "the pressure run did not say no worktree is a candidate"
  ! grep -q "removed " "$S/log" || fail "the pressure run removed a worktree"
  for name in fix wip local; do
    [ -d ".worktrees/$name" ] || fail "$name is gone"
    git worktree list --porcelain | grep -q "^worktree .*/\.worktrees/$name\$" || fail "$name is no longer registered"
    git rev-parse --verify -q "$name" > /dev/null || fail "branch $name is gone"
  done
  for case in "1. a dry run names the landed fix" "2. --once removes the landed fix" \
    "3. a running watch removes wip" "4. disk pressure removes local" \
    "5. the removal child's --only-if-upstream-gone" "6. the child's three upstream reads" \
    "8. a restored worktree is not removed again because its work landed"; do
    skip_partial "$case"
  done
  fetch_case
  echo "watch-smoke: ok (every worktree kept as activity unknown)"
  exit 0
fi

"$residue" watch --once --dry-run --grace 1m "$S" 2> "$S/log" || fail "dry run failed"
grep -q "would remove .*worktrees/fix" "$S/log" || fail "dry run did not name fix"
[ -d .worktrees/fix ] || fail "dry run removed fix"

# 2. fix landed: removed. wip and local stay.
"$residue" watch --once --grace 1m "$S" 2> "$S/log" || fail "--once failed"
grep -q "removed .*worktrees/fix" "$S/log" || fail "fix was not removed"
[ ! -e .worktrees/fix ] || fail "fix still exists"
[ -d .worktrees/wip ] && [ -d .worktrees/local ] || fail "removed more than fix"
[ ! -e .git/residue-trash ] || fail "watch moved fix to the trash instead of deleting it"
grep -q "residue watch: freed about .* (about .* on this machine since " "$S/log" \
  || fail "the log does not say what removing fix freed"
"$residue" stats --json | grep -q '"watch":[{]"bytes":[0-9]*,"worktrees":1[}]' \
  || fail "residue stats does not count fix under watch"

# 3. A running watch reacts to wip's upstream being pruned.
"$residue" watch --grace 1m "$S" 2> "$S/log" &
watcher=$!
sleep 5
git push -q origin --delete wip
git fetch -q --prune
age
# Wait for the log line, not for the directory: remove first moves the
# worktree aside (so .worktrees/wip vanishes), then deletes it, then the
# watcher logs, so the directory goes before the line is written.
i=0
until grep -q "removed .*worktrees/wip" "$S/log"; do
  i=$((i + 1))
  [ "$i" -le 120 ] || fail "the running watch did not remove wip within 2 minutes"
  sleep 1
done
[ ! -e .worktrees/wip ] || fail "the log says wip was removed, but it is still there"
for leftover in .worktrees/.*.residue-removing-*; do
  [ ! -e "$leftover" ] || fail "$leftover was left behind"
done
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
git branch basel
for name in back gone none dirty treeref symref locked recent localup; do
  git worktree add -q "$S/child/.worktrees/$name" -b "$name"
  (cd "$S/child/.worktrees/$name" && echo "$name" >> README.md && git commit -qam "$name")
done
git worktree add -q --detach "$S/child/.worktrees/detached" HEAD
for name in back gone treeref symref locked recent; do
  (cd "$S/child/.worktrees/$name" && git push -q -u origin "$name")
  git push -q origin --delete "$name"
done
git fetch -q --prune
# back: its upstream came back between watch's scan and the child.
git update-ref refs/remotes/origin/back "$(git rev-parse back)"
# treeref and symref: Git says [gone] for these too, but the ref exists
# and does not resolve to a commit. That is not a deleted branch.
git update-ref refs/remotes/origin/treeref "$(git rev-parse 'HEAD^{tree}')"
# localup: its upstream is the local branch basel, then deleted. A deleted
# local branch is not work that landed on a remote.
git -C .worktrees/localup branch -q --set-upstream-to=basel
git branch -q -D basel
git symbolic-ref refs/remotes/origin/symref refs/remotes/origin/missing
echo edit >> .worktrees/dirty/README.md
git worktree lock --reason "resume later" .worktrees/locked
find "$S/child/.worktrees" "$S/child/.git/worktrees" -exec touch -t 202601010000 {} +
touch .worktrees/recent/README.md

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
  printf '%s' "$out" | grep -q -e "$3" || fail "$1: want a reason with '$3': $out"
  [ -d ".worktrees/$1" ] || fail "$1: the worktree is gone"
  git worktree list --porcelain | grep -q "^worktree .*/\.worktrees/$1\$" || fail "$1: no longer registered"
  for leftover in .worktrees/.*.residue-removing-*; do
    [ ! -e "$leftover" ] || fail "$1: $leftover was left behind"
  done
  [ ! -e .git/residue-trash ] || [ -z "$(ls -A .git/residue-trash)" ] || fail "$1: a trash entry was left"
}
# At the plan: the child's own reading, not the re-check's.
kept back skipped "--only-if-upstream-gone: its upstream origin/back exists in the local refs"
kept none skipped "--only-if-upstream-gone: branch none has no upstream"
kept detached skipped "--only-if-upstream-gone: it is on a detached HEAD, which has no upstream"
kept treeref skipped "residue could not read the upstream of branch treeref: .*does not resolve to a commit"
kept symref skipped "residue could not read the upstream of branch symref: .*is a symbolic ref"
kept localup skipped "--only-if-upstream-gone: its upstream basel is a local branch, not a remote-tracking ref"
# A failed check wins over the requirement: dirty has no upstream, and
# locked and recent (changed within --idle) have a gone one.
kept dirty refused "one or more checks did not pass"
kept locked refused "one or more checks did not pass"
kept recent refused "one or more checks did not pass"
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
for b in back gone none dirty treeref symref locked recent localup; do
  git rev-parse --verify -q "$b" > /dev/null || fail "branch $b is gone"
done

# 6. watch passes the reason, and the child's three reads each hold (#7).
# The wrapper counts the upstream lookups that `residue remove` (git's
# parent) makes, and restores origin/race right before lookup N: 1 is the
# plan, 2 the re-check before the move, 3 the read after the move. Nothing
# in residue knows about it, and nothing waits on a clock.
real_git=$(command -v git)
git init -q --bare -b main "$S/w6.git"
git clone -q "$S/w6.git" "$S/w6" 2> /dev/null
cd "$S/w6"
git config user.email smoke@example.com
git config user.name smoke
echo hello > README.md
git add . && git commit -qm init && git push -q origin main
git worktree add -q "$S/w6/.worktrees/race" -b race
(cd .worktrees/race && echo race >> README.md && git commit -qam race && git push -q -u origin race)
git push -q origin --delete race
git fetch -q --prune
race_sha=$(git rev-parse race)
mkdir "$S/w6bin"
landed_again() {
  git update-ref -d refs/remotes/origin/race 2> /dev/null || true
  find "$S/w6/.worktrees" "$S/w6/.git/worktrees" -exec touch -t 202601010000 {} +
}
wrap() { # N
  rm -f "$S/w6calls"
  cat > "$S/w6bin/git" << EOF
#!/bin/sh
case "\$*" in
*"%(upstream"*)
  if ps -ww -o args= -p "\$PPID" | grep -q " remove "; then
    n=\$(( \$(cat "$S/w6calls" 2> /dev/null || echo 0) + 1 ))
    echo "\$n" > "$S/w6calls"
    [ "\$n" != "$1" ] || "$real_git" -C "$S/w6" update-ref refs/remotes/origin/race "$race_sha"
  fi
  ;;
esac
exec "$real_git" "\$@"
EOF
  chmod +x "$S/w6bin/git"
}
# Case 6 sets origin/race by hand, as a fetch would, while the remote has
# no race branch: watch's own `git fetch --prune` would delete it again, so
# these runs pass --no-fetch. Case 7 checks the fetch.
wrapped_watch() {
  PATH="$S/w6bin:$PATH" "$residue" watch --once --no-fetch --grace 1m "$@" "$S/w6" 2> "$S/log" || fail "watch --once failed"
}
stage() { # N TEXT [WATCH OPTIONS]
  n=$1 text=$2
  shift 2
  landed_again
  wrap "$n"
  wrapped_watch "$@"
  [ "$(cat "$S/w6calls" 2> /dev/null)" = "$n" ] || fail "stage $n: the child made $(cat "$S/w6calls" 2> /dev/null || echo no) upstream lookups, want $n"
  grep -q "kept .*worktrees/race (skipped): $text" "$S/log" || fail "stage $n: no 'kept (skipped): $text' line"
  ! grep -q "removed .*worktrees/race" "$S/log" || fail "stage $n: race was removed"
  [ -d .worktrees/race ] || fail "stage $n: race is gone"
  git worktree list --porcelain | grep -q "^worktree .*/\.worktrees/race\$" || fail "stage $n: race is no longer registered"
  for leftover in .worktrees/.*.residue-removing-*; do
    [ ! -e "$leftover" ] || fail "stage $n: $leftover was left behind"
  done
}
stage 2 "changed since the plan: its upstream origin/race exists in the local refs"
stage 3 "changed after residue moved it aside: its upstream origin/race exists in the local refs; it is back in place"
# The read after the move must come after the re-measure, which can take
# seconds: restore the upstream while residue runs `git status` in the
# moved-aside directory, and the last read must still see it.
landed_again
cat > "$S/w6bin/git" << EOF
#!/bin/sh
case "\$*" in
*residue-removing-*status*) "$real_git" -C "$S/w6" update-ref refs/remotes/origin/race "$race_sha" ;;
esac
exec "$real_git" "\$@"
EOF
wrapped_watch
grep -q "kept .*worktrees/race (skipped): changed after residue moved it aside: its upstream origin/race exists in the local refs" "$S/log" || fail "stage 4: the upstream read after the move did not come after the re-measure"
! grep -q "removed .*worktrees/race" "$S/log" || fail "stage 4: race was removed"
[ -d .worktrees/race ] || fail "stage 4: race is gone"
# The plan, with disk pressure in the same pass: kept, not taken as pressure.
stage 1 "--only-if-upstream-gone: its upstream origin/race exists in the local refs" --min-free 99.9 --idle 1m
# The next pass, without the wrapper: the upstream exists, so race did not
# land, and disk pressure takes it on its own terms.
"$residue" watch --once --no-fetch --grace 1m --min-free 99.9 --idle 1m "$S/w6" 2> "$S/log" || fail "the pressure pass failed"
grep -q "removed .*worktrees/race (.*): [0-9.]*% free, below" "$S/log" || fail "pressure did not remove race on the next pass"
git rev-parse --verify -q race > /dev/null || fail "branch race is gone"

# 8. Each removal writes a removal record, and its log line names the
# command that restores it. A restored worktree is never removed again
# because its work landed, fetch or no fetch, past --grace; disk pressure
# still removes it.
git init -q --bare -b main "$S/r8.git"
git clone -q "$S/r8.git" "$S/r8" 2> /dev/null
cd "$S/r8"
git config user.email smoke@example.com
git config user.name smoke
echo hello > README.md
git add . && git commit -qm init && git push -q origin main
for name in back control; do
  git worktree add -q "$S/r8/.worktrees/$name" -b "$name"
  (cd ".worktrees/$name" && echo "$name" >> README.md && git commit -qam "$name" && git push -q -u origin "$name")
done
back_sha=$(git rev-parse back)
git push -q origin --delete back
git fetch -q --prune
age8() {
  find "$S/r8/.worktrees" "$S/r8/.git/worktrees" -exec touch -t 202601010000 {} +
}
age8
"$residue" watch --once --no-fetch --grace 1m "$S/r8" 2> "$S/log" || fail "8: the landed run failed"
grep -q "removed .*worktrees/back (.*): origin/back was deleted.*; restore: residue restore .*/residue-removed/back-" "$S/log" \
  || fail "8: the removal line has no 'restore: residue restore RECORD'"
record=$(sed -n 's/^.*; restore: residue restore //p' "$S/log" | head -n 1)
[ -f "$record/info" ] || fail "8: the record $record has no info file"
grep -qx 'by watch' "$record/info" || fail "8: the record does not say watch removed it"
[ "$(git for-each-ref --format='%(objectname)' refs/residue/removed/)" = "$back_sha" ] || fail "8: no ref pins back's HEAD"
"$residue" restore "$record" > "$S/restore.out" || fail "8: restore failed: $(cat "$S/restore.out")"
grep -q "^restored .*worktrees/back from the removal record back-" "$S/restore.out" || fail "8: no 'restored' line: $(cat "$S/restore.out")"
git worktree list --porcelain | grep -q "^worktree .*/\.worktrees/back\$" || fail "8: back is not registered after the restore"
[ "$(git -C .worktrees/back rev-parse HEAD)" = "$back_sha" ] || fail "8: back is not at its recorded HEAD"
[ -z "$(git for-each-ref refs/residue)" ] || fail "8: the restore left its ref"
# control lands too; both are aged past --grace, the restore marker
# included. watch fetches this time: control goes, back stays.
git push -q origin --delete control
age8
"$residue" watch --once --grace 1m "$S/r8" 2> "$S/log" || fail "8: the second landed run failed"
grep -q "removed .*worktrees/control (.*): origin/control was deleted" "$S/log" || fail "8: control was not removed"
! grep -q "removed .*worktrees/back" "$S/log" || fail "8: watch removed the restored back again because its work landed"
[ -d .worktrees/back ] || fail "8: back is gone"
# Disk pressure removes it, and writes a new record.
"$residue" watch --once --no-fetch --min-free 99.9 --idle 1m "$S/r8" 2> "$S/log" || fail "8: the pressure run failed"
grep -q "removed .*worktrees/back (.*): [0-9.]*% free, below.*; restore: residue restore .*/residue-removed/back-" "$S/log" \
  || fail "8: disk pressure did not remove the restored back"
for b in back control; do
  git rev-parse --verify -q "$b" > /dev/null || fail "8: branch $b is gone"
done
echo "watch-smoke: 8. removal records restored back; the landed rule kept it, disk pressure did not"

fetch_case
echo "watch-smoke: ok"
