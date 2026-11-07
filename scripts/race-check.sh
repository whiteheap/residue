#!/usr/bin/env bash
# Race check: change a worktree while `residue remove` waits at its
# confirmation prompt, answer yes, and check that residue removes nothing.
#
#   bash scripts/race-check.sh [BINARY]    (default: target/release/residue)
#
# Each case builds a scratch repository with one linked worktree that
# residue judges a candidate, starts the real binary under a pseudo-terminal
# (`script`), waits for the prompt, makes one change, then types yes. A
# changed worktree must be skipped: exit status 3, a reason that says what
# changed since the user confirmed, and the worktree still in place and
# registered. The control cases change nothing and must be removed (or
# moved to the trash). Runs on macOS, Linux and FreeBSD; needs bash, git and
# script.
set -euo pipefail

bin=${1:-target/release/residue}
case $bin in
/*) ;;
*) bin=$PWD/$bin ;;
esac
[ -x "$bin" ] || { echo "race-check: no executable at $bin; run cargo build --release first" >&2; exit 2; }

work=$(mktemp -d "${TMPDIR:-/tmp}/residue-race.XXXXXX")
trap 'rm -rf "$work"' EXIT
export NO_COLOR=1 GIT_CONFIG_NOSYSTEM=1 HOME=$work/home
mkdir -p "$HOME"
git config --global user.name race-check
git config --global user.email race-check@example.invalid
git config --global init.defaultBranch main

failures=0
passes=0

# residue runs under `sh -c`, which prints its exit status after it on a
# line of its own, `race-check-status N`. The status `script` returns is not
# reliable: util-linux `script` 2.39.3 (Ubuntu 24.04) returned 0 for a run
# that printed `skipped` and so exited 3, once in CI.
run_status='"$@"; echo "race-check-status $?"'

# The keyboard: types yes once $work/type appears, so `script` never sees
# the end of its input before the prompt. Then:
# - util-linux `script` (2.39.3, Ubuntu 24.04) spins, and stops copying
#   output, when its command exits while its input is still open, so on
#   Linux the input ends right after yes. residue reads one line and never
#   sees the end of file, and Linux does not echo it.
# - BSD and macOS `script` pass the end of input on as an end-of-file
#   character, which the terminal echoes as ^D into residue's output, so
#   there the input stays open until the status line is in the output.
# It also stops at $work/done and when the work directory is gone, so an
# interrupted run leaves no process behind.
# (macOS `script` rejects a FIFO as its input, so this is a pipe.)
keyboard() { # out-file
    until [ -e "$work/type" ]; do
        [ -d "$work" ] || return 0
        sleep 0.1
    done
    printf 'yes\n'
    [ "$(uname -s)" = Linux ] && return 0
    until grep -q '^race-check-status ' "$1" 2>/dev/null || [ -e "$work/done" ]; do
        [ -d "$work" ] || return 0
        sleep 0.1
    done
}

# Run residue under a pseudo-terminal fed by the keyboard. Sets $pid to
# the `script` process alone: the keyboard runs in a process substitution,
# outside the job, so `wait $pid` does not wait for it.
start() { # out-file, residue arguments...
    local out=$1
    shift
    rm -f "$work/type" "$work/done"
    case $(uname -s) in
    Linux)
        local line
        line=$(printf '%q ' sh -c "$run_status" sh "$bin" "$@")
        script -q -f -c "$line" /dev/null < <(keyboard "$out") >"$out" 2>&1 &
        ;;
    *) script -q /dev/null sh -c "$run_status" sh "$bin" "$@" < <(keyboard "$out") >"$out" 2>&1 & ;;
    esac
    pid=$!
}

# Wait until the prompt is on screen; fail after 60 seconds.
await_prompt() { # out-file
    local i
    for ((i = 0; i < 600; i++)); do
        grep -q 'Type yes to continue' "$1" && return 0
        kill -0 "$pid" 2>/dev/null || return 1
        sleep 0.1
    done
    return 1
}

finish() { # sets $status: residue's exit status, or "none" without a status line
    touch "$work/type"
    wait "$pid" || true
    touch "$work/done"
    status=$(tr -d '\r' <"$out" | sed -n 's/^race-check-status \([0-9][0-9]*\)$/\1/p' | tail -n 1)
    status=${status:-none}
}

fail() {
    echo "FAIL $name: $*"
    sed 's/^/    | /' "$out"
    failures=$((failures + 1))
}

# A repository with one linked worktree, .worktrees/w on branch w, that
# holds only committed files and one regenerable directory.
make_repo() {
    repo=$work/$name
    git init -q "$repo"
    printf 'node_modules/\n' >"$repo/.gitignore"
    echo hello >"$repo/README.md"
    git -C "$repo" add . && git -C "$repo" commit -qm init
    git -C "$repo" worktree add -q "$repo/.worktrees/w" -b w
    wt=$repo/.worktrees/w
    mkdir "$wt/node_modules"
    echo x >"$wt/node_modules/a.js"
}

# The same, with a bare remote that had branch w and deleted it, then a
# fetch --prune: w's upstream is gone, as after a merged pull request.
make_landed_repo() {
    make_repo
    git init -q --bare "$work/$name.git"
    git -C "$repo" remote add origin "$work/$name.git"
    git -C "$repo" push -q origin HEAD
    git -C "$wt" push -q -u origin w
    git -C "$repo" push -q origin --delete w
    git -C "$repo" fetch -q --prune origin
}

registered() {
    git -C "$repo" worktree list --porcelain | grep -qx "worktree $(cd "$wt" && pwd -P)"
}

# One case: NAME, the change to make while the prompt waits, the text the
# output must contain, then extra residue flags. $setup builds the
# repository (make_repo unless set).
run_case() {
    name=$1 change=$2 expect=$3
    shift 3
    ${setup:-make_repo}
    out=$work/$name.out
    start "$out" remove "$@" "$wt"
    if ! await_prompt "$out"; then
        finish
        fail "the prompt never appeared (exit $status)"
        return
    fi
    (cd "$wt" && eval "$change")
    finish
    if ls -a "$repo/.worktrees" | grep -q 'residue-removing'; then
        fail "a .residue-removing- directory was left behind"
        return
    fi
    case $change in
    :)
        [ "$status" = 0 ] || { fail "exit $status, want 0"; return; }
        grep -q "$expect" "$out" || { fail "no line matching: $expect"; return; }
        [ ! -e "$wt" ] || { fail "the worktree is still at $wt"; return; }
        git -C "$repo" rev-parse -q --verify refs/heads/w >/dev/null || { fail "branch w is gone"; return; }
        ;;
    *)
        [ "$status" = 3 ] || { fail "exit $status, want 3"; return; }
        grep -q "$expect" "$out" || { fail "no line matching: $expect"; return; }
        [ -d "$wt" ] || { fail "the worktree is gone"; return; }
        registered || { fail "Git no longer lists the worktree at $wt"; return; }
        if [ -n "$(ls -A "$repo/.git/residue-trash" 2>/dev/null)" ]; then
            fail "a trash entry was left behind"
            return
        fi
        ;;
    esac
    echo "ok   $name: $(grep -E '^(removed|moved|skipped|failed) ' "$out" | tr -d '\r' | head -1)"
    passes=$((passes + 1))
}

"$bin" --version
run_case control ':' '^removed .*\.worktrees/w'
run_case untracked 'echo notes >notes.md' 'skipped .*changed since you confirmed: 1 untracked'
run_case modified 'echo edit >>README.md' 'skipped .*changed since you confirmed: 1 change'
run_case ignored 'echo y >node_modules/b.js' 'skipped .*node_modules/ changed since you confirmed (1 -> 2 files)'
run_case commit 'git commit -q --allow-empty -m later' 'skipped .*HEAD changed since you confirmed'
run_case control-trash ':' '^moved .*\.worktrees/w to the trash' --trash
run_case ignored-trash 'echo yy >node_modules/a.js' 'skipped .*node_modules/ changed since you confirmed' --trash
# --only-if-upstream-gone, what `residue watch` passes for a branch that
# landed: an upstream that comes back while the prompt waits keeps it.
setup=make_landed_repo run_case landed-control ':' '^removed .*\.worktrees/w' --only-if-upstream-gone
setup=make_landed_repo run_case landed-upstream-back 'git update-ref refs/remotes/origin/w HEAD' 'skipped .*changed since you confirmed: its upstream origin/w exists in the local refs' --only-if-upstream-gone
setup=make_landed_repo run_case landed-upstream-back-trash 'git update-ref refs/remotes/origin/w HEAD' 'skipped .*its upstream origin/w exists in the local refs' --only-if-upstream-gone --trash

echo "race-check: $passes passed, $failures failed"
[ "$failures" -eq 0 ]
