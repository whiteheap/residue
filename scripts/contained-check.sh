#!/usr/bin/env bash
# Contained check: residue where it cannot see every process, run as root.
# A sleep outside uses a clean linked worktree, w, while residue runs inside
# a PID namespace of its own (Linux, `unshare --pid --fork --mount-proc`, as
# a container has) or a jail (FreeBSD, `jail -c path=/`). Outside, w is
# in_use by the sleep. Inside, its activity must be unknown
# (hidden_processes) with the reason in words, and `remove --yes` must keep
# it, untouched; with the sleep gone it must still be unknown, never
# not_observed. A case skips, by name and with why, where it cannot make the
# namespace or the jail: not root, no unshare or jail, or a container that
# refuses a new namespace.
#
#   bash scripts/contained-check.sh [BINARY]    (default: target/release/residue)
#
# Exit status: 0 every case passed or skipped, 1 a case failed, 2 the runner
# could not run (no binary, no jq, or a fixture setup step failed). Needs
# bash, git and jq. Everything lives under one fresh directory with its own
# HOME and Git config, and only that directory is deleted.
set -uo pipefail

bin=${1:-target/release/residue}
case $bin in
/*) ;;
*) bin=$PWD/$bin ;;
esac
[ -x "$bin" ] || { echo "contained-check: no executable at $bin; run cargo build --release first" >&2; exit 2; }
command -v jq >/dev/null || { echo "contained-check: needs jq" >&2; exit 2; }

work=$(mktemp -d "${TMPDIR:-/tmp}/residue-contained.XXXXXX") || exit 2
case $work in
*/residue-contained.??????) ;;
*) echo "contained-check: unexpected scratch directory $work" >&2; exit 2 ;;
esac
# The sleep holding w, while it runs as a job of this shell.
holder=
stop_holder() {
    if [ -n "$holder" ] && kill -0 "$holder" 2>/dev/null; then
        kill "$holder" 2>/dev/null
        wait "$holder" 2>/dev/null
    fi
    holder=
}
trap 'stop_holder; rm -rf -- "$work"' EXIT

export HOME=$work/home XDG_CONFIG_HOME=$work/home/.config XDG_STATE_HOME=$work/home/.local/state XDG_DATA_HOME=$work/home/.local/share
export GIT_CONFIG_NOSYSTEM=1 NO_COLOR=1 LC_ALL=C
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_COMMON_DIR GIT_CONFIG GIT_CONFIG_PARAMETERS GIT_CONFIG_COUNT GIT_CONFIG_GLOBAL GIT_CONFIG_SYSTEM
mkdir -p "$HOME"
git config --global user.name contained-check
git config --global user.email contained-check@example.invalid
exec </dev/null

passes=0
failures=0
skips=0
name=

# A fixture step: on failure, stop with exit 2 and show what it printed,
# as contract-check does, so a run that went wrong in CI can be read from
# its log.
setup() {
    local said st
    said=$("$@" 2>&1) && return 0
    st=$?
    echo "contained-check: setup failed in $name (exit $st): $*" >&2
    [ -n "$said" ] && head -n 40 <<<"$said" | sed 's/^/    | /' >&2
    exit 2
}
fail() {
    echo "FAIL $name: $*"
    failures=$((failures + 1))
}
ok() {
    echo "ok   $name"
    passes=$((passes + 1))
}
skip() {
    echo "skip $name: $*"
    skips=$((skips + 1))
}

# A repository with one commit and a clean linked worktree, $wt, and a sleep
# outside whose working directory is in it, $holder.
fixture() {
    repo=$work/$name
    setup git init -q "$repo"
    echo hi >"$repo/a.txt"
    setup git -C "$repo" add a.txt
    setup git -C "$repo" commit -qm init
    setup git -C "$repo" worktree add -q "$repo/w" -b w
    wt=$repo/w
    (cd "$wt" && exec sleep 300) >/dev/null 2>&1 &
    holder=$!
    local i
    for ((i = 0; i < 100; i++)); do
        case $(ps -o comm= -p "$holder" 2>/dev/null) in *sleep) return ;; esac
        sleep 0.1
    done
    echo "contained-check: setup failed in $name: the sleep never started" >&2
    exit 2
}

# Run a command in a PID namespace of its own with its own /proc, or in a
# jail whose root is the host's, and return the command's exit status.
# unshare passes it on; jail(8) exits 1 for any command that fails, so the
# command writes its own status to a file.
in_namespace() { unshare --pid --fork --mount-proc "$@"; }
in_jail() {
    local status=$work/jail.status
    rm -f "$status"
    # shellcheck disable=SC2016 # expanded by the sh inside the jail
    jail -c path=/ name=residue-contained-check command=/bin/sh -c '"$@"; echo $? >"$0"' "$status" "$@"
    [ -s "$status" ] || return 125
    return "$(cat "$status")"
}

# NAME DETAIL WRAPPER: residue run through WRAPPER keeps w as unknown
# (hidden_processes) with DETAIL, while the sleep outside is in_use.
contained() {
    name=$1
    local detail=$2 explain_status remove_status
    shift 2
    fixture
    "$bin" explain --json "$wt" >"$work/$name.outside" 2>&1
    "$@" "$bin" explain --json "$wt" >"$work/$name.inside" 2>"$work/$name.err"
    explain_status=$?
    "$@" "$bin" remove --yes --json --no-trash "$wt" >"$work/$name.remove" 2>>"$work/$name.err"
    remove_status=$?
    local pid=$holder
    stop_holder
    "$@" "$bin" explain --json "$wt" >"$work/$name.idle" 2>>"$work/$name.err"
    # shellcheck disable=SC2016 # $detail is jq's, from --arg
    local kept='.state == "unknown" and .reason == "hidden_processes" and .detail == $detail'
    if ! jq -e --argjson pid "$pid" '.worktree.activity.state == "in_use" and any(.worktree.activity.processes[]; .pid == $pid)' "$work/$name.outside" >/dev/null 2>&1; then
        fail "outside, with sleep[$pid] in w, explain says $(head -c 600 "$work/$name.outside")"
    elif [ "$explain_status" != 0 ]; then
        fail "inside, explain exit $explain_status, want 0: $(head -c 600 "$work/$name.err")"
    elif ! jq -e --arg detail "$detail" ".worktree.verdict == \"unknown\" and (.worktree.activity | $kept)" "$work/$name.inside" >/dev/null 2>&1; then
        fail "inside, with sleep[$pid] in w, explain says $(jq -c '[.worktree.verdict, .worktree.activity]' "$work/$name.inside" 2>&1 | head -c 600)"
    elif [ "$remove_status" != 3 ]; then
        fail "inside, remove --yes exit $remove_status, want 3: $(head -c 600 "$work/$name.remove")"
    elif ! jq -e --arg detail "$detail" ".results[0].result == \"refused\" and ([.results[0].worktree.checks[] | select(.status != \"pass\") | .id] == [\"activity\"]) and (.results[0].worktree.activity | $kept)" "$work/$name.remove" >/dev/null 2>&1; then
        fail "inside, remove says $(jq -c '.results[0] | [.result, .worktree.activity]' "$work/$name.remove" 2>&1 | head -c 600)"
    elif ! git -C "$repo" worktree list --porcelain | grep -qxF "worktree $(cd "$wt" && pwd -P)" || [ "$(cat "$wt/a.txt" 2>/dev/null)" != hi ]; then
        fail "w is gone or changed after the remove"
    elif ! jq -e --arg detail "$detail" ".worktree.activity | $kept" "$work/$name.idle" >/dev/null 2>&1; then
        fail "inside, with the sleep gone, explain says $(jq -c '.worktree.activity' "$work/$name.idle" 2>&1 | head -c 600)"
    else
        ok
        echo "     $(jq -r '.activity | "\(.backend) inspected \(.inspected_processes) of \(.running_processes)"' "$work/$name.inside"); $detail"
    fi
}

# Linux: a PID namespace with its own /proc, as a container has.
name=activity-pid-namespace
if [ "$(uname -s)" != Linux ]; then
    skip "it runs on Linux"
elif [ "$(id -u)" != 0 ]; then
    skip "it runs as root, to make a PID namespace"
elif ! command -v unshare >/dev/null; then
    skip "no unshare"
elif ! why=$(unshare --pid --fork --mount-proc true 2>&1); then
    skip "unshare cannot make a PID namespace here: $why"
else
    contained "$name" "residue runs inside a container (a PID namespace of its own), so it cannot see processes outside it" in_namespace
fi

# FreeBSD: a jail whose root is the host's, so it sees the same files.
name=activity-jail
if [ "$(uname -s)" != FreeBSD ]; then
    skip "it runs on FreeBSD"
elif [ "$(id -u)" != 0 ]; then
    skip "it runs as root, to make a jail"
elif ! command -v jail >/dev/null; then
    skip "no jail"
elif ! why=$(jail -c path=/ name=residue-contained-probe command=/usr/bin/true 2>&1); then
    skip "jail cannot make a jail here: $why"
else
    contained "$name" "residue runs inside a jail, so it cannot see processes outside it" in_jail
fi

echo "contained-check: $passes passed, $failures failed, $skips skipped"
[ "$failures" -eq 0 ]
