#!/usr/bin/env bash
# Contained check: residue where it cannot see every process, run as root.
# A sleep outside uses a clean linked worktree, w, while residue runs inside
# a PID namespace of its own (Linux, `unshare --pid --fork --mount-proc`, as
# a container has), a user namespace of its own (Linux, as toolbx and
# distrobox have) or a jail (FreeBSD, `jail -c path=/`). Outside, w is
# in_use by the sleep. Inside, its activity must be unknown
# (hidden_processes) with the reason in words, and `remove --yes` must keep
# it, untouched; with the sleep gone it must still be unknown, never
# not_observed. On Linux, also: a PID namespace under the host's /proc; WSL 2
# emulated, a distribution and a sandbox in it; and a /proc mounted with
# hidepid, for the user sudo ran this for and for root. A case skips, by
# name and with why, where it cannot set itself up: not root, no unshare,
# setpriv or jail, or a container that refuses a new namespace.
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
# No `git maintenance run --auto --detach` after a commit, which residue
# would see as a use of the worktree (#180, #195).
git config --global maintenance.auto false
# root reads the repository of the user sudo ran this for (activity-hidepid).
git config --global --add safe.directory '*'
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

# Run a command in a PID namespace of its own with its own /proc, in a user
# namespace of its own (as toolbx and distrobox run one), or in a jail whose
# root is the host's, and return the command's exit status.
# unshare passes it on; jail(8) exits 1 for any command that fails, so the
# command writes its own status to a file.
in_namespace() { unshare --pid --fork --mount-proc "$@"; }
in_user_namespace() { unshare --user --map-root-user "$@"; }
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

# What one explain document says of w: verdict, state, reason, detail.
said() { jq -c '[.worktree.verdict, .worktree.activity.state, .worktree.activity.reason, .worktree.activity.detail]' "$1" 2>&1 | head -c 600; }

# Why a Linux case below cannot run here, or nothing.
linux_root() {
    if [ "$(uname -s)" != Linux ]; then
        echo "it runs on Linux"
    elif [ "$(id -u)" != 0 ]; then
        echo "it runs as root, to make a namespace"
    elif ! command -v unshare >/dev/null; then
        echo "no unshare"
    fi
}

# Linux: a PID namespace without its own /proc (`unshare --pid --fork`):
# the host's /proc shows every process, so the sleep is in_use, yet with it
# gone residue still cannot tell which namespace that /proc belongs to, so
# w is unknown (hidden_processes), never not_observed.
host_proc_case() {
    name=activity-pid-namespace-host-proc
    fixture
    local pid=$holder
    unshare --pid --fork "$bin" explain --json "$wt" >"$work/$name.busy" 2>"$work/$name.err"
    stop_holder
    unshare --pid --fork "$bin" explain --json "$wt" >"$work/$name.idle" 2>>"$work/$name.err"
    if ! jq -e --argjson pid "$pid" '.worktree.activity.state == "in_use" and any(.worktree.activity.processes[]; .pid == $pid)' "$work/$name.busy" >/dev/null 2>&1; then
        fail "with the host's /proc and sleep[$pid] in w, explain says $(said "$work/$name.busy") $(head -c 300 "$work/$name.err")"
    elif ! jq -e '.worktree.verdict == "unknown" and (.worktree.activity | .state == "unknown" and .reason == "hidden_processes")' "$work/$name.idle" >/dev/null 2>&1; then
        fail "with the sleep gone, explain says $(said "$work/$name.idle")"
    else
        ok
        echo "     with the sleep: in_use; without: $(jq -r '.worktree.activity.detail' "$work/$name.idle")"
    fi
}

# Linux: WSL 2, emulated. A distribution is a PID namespace whose PID 1 is
# WSL's /init, on Microsoft's kernel, with WSL's variables and /run/WSL; it
# holds the whole Linux session, so residue judges it as any Linux: a sleep
# in the distribution is in_use, and with none there w is not_observed (the
# sleep outside stands for another distribution, which residue never sees).
# A sandbox started in the distribution with a /proc of its own keeps the
# variables and /run/WSL, but its /proc has no /init: unknown
# (hidden_processes).
wsl_case() {
    name=activity-wsl-emulated
    fixture
    local osr=$work/$name.osrelease script=$work/$name.sh out=$work/$name
    echo 6.6.87.2-microsoft-standard-WSL2 >"$osr"
    cat >"$script" <<'SCRIPT'
# PID 1 of a PID namespace, named /init, in a mount namespace of its own.
mount --bind "$OSR" /proc/sys/kernel/osrelease || exit 90
mount -t tmpfs tmpfs /run && mkdir /run/WSL || exit 91
export WSL_DISTRO_NAME=Ubuntu WSL_INTEROP=/run/WSL/1_interop
[ "$(tr '\0' ' ' </proc/1/cmdline)" = "/init $0 " ] || exit 92
(cd "$WT" && exec sleep 300) &
h=$!
sleep 0.5
"$BIN" explain --json "$WT" >"$OUT.busy"
kill "$h"
wait "$h" 2>/dev/null
"$BIN" explain --json "$WT" >"$OUT.idle"
unshare --pid --fork --mount-proc sh -c 'mount --bind "$OSR" /proc/sys/kernel/osrelease && exec "$BIN" explain --json "$WT"' >"$OUT.nested"
SCRIPT
    # shellcheck disable=SC2016 # $0 is the inner bash's
    OSR=$osr BIN=$bin WT=$wt OUT=$out unshare --pid --fork --mount --mount-proc \
        bash -c 'exec -a /init bash "$0"' "$script" 2>"$out.err"
    local status=$?
    stop_holder
    if [ "$status" != 0 ]; then
        fail "the emulated distribution exited $status: $(head -c 400 "$out.err")"
    elif ! jq -e '.worktree.activity.state == "in_use" and any(.worktree.activity.processes[]; .command == "sleep")' "$out.busy" >/dev/null 2>&1; then
        fail "in the distribution, with a sleep there in w, explain says $(said "$out.busy")"
    elif ! jq -e '.worktree.activity.state == "not_observed"' "$out.idle" >/dev/null 2>&1; then
        fail "in the distribution, with no sleep there, explain says $(said "$out.idle")"
    elif ! jq -e '.worktree.activity | .state == "unknown" and .reason == "hidden_processes" and (.detail | startswith("residue runs inside a container"))' "$out.nested" >/dev/null 2>&1; then
        fail "in a sandbox in the distribution, explain says $(said "$out.nested")"
    else
        ok
        echo "     distribution: in_use, then not_observed; sandbox in it: $(jq -r '.worktree.activity.reason' "$out.nested")"
    fi
}

# Linux: a /proc mounted with hidepid=invisible, a new instance in a mount
# namespace of its own (Linux 5.8 and later), so the host's /proc is never
# touched. For the user sudo ran this for, it hides processes residue may
# not read, so w is unknown (hidden_processes); root, with CAP_SYS_PTRACE,
# sees every process, so w is a candidate.
hidepid_case() {
    name=activity-hidepid
    local uid=${SUDO_UID:-} gid=${SUDO_GID:-} major minor
    IFS=. read -r major minor _ <<<"$(uname -r)"
    if [ -z "$uid" ] || [ "$uid" = 0 ] || [ -z "$gid" ]; then
        skip "it runs for the user sudo ran it for (SUDO_UID, SUDO_GID)"
        return
    elif ! command -v setpriv >/dev/null; then
        skip "no setpriv"
        return
    elif [ "$major" -lt 5 ] || { [ "$major" = 5 ] && [ "${minor%%[!0-9]*}" -lt 8 ]; }; then
        skip "before Linux 5.8 a /proc's hidepid is not its own"
        return
    elif ! why=$(unshare --mount --propagation private sh -c 'mount -t proc -o hidepid=invisible proc /proc' 2>&1); then
        skip "cannot mount a /proc with hidepid here: $why"
        return
    fi
    local dir=$work/$name
    setup install -d -o "$uid" -g "$gid" "$dir"
    setup chmod 711 "$work"
    as_user() { setpriv --reuid="$uid" --regid="$gid" --init-groups env HOME="$dir" "$@"; }
    setup as_user git init -q "$dir/repo"
    echo hi >"$dir/repo/a.txt" && chown "$uid:$gid" "$dir/repo/a.txt"
    setup as_user git -C "$dir/repo" add a.txt
    setup as_user git -C "$dir/repo" -c user.name=c -c user.email=c@example.invalid -c maintenance.auto=false commit -qm init
    setup as_user git -C "$dir/repo" worktree add -q "$dir/repo/w" -b w
    local hide='mount -t proc -o hidepid=invisible proc /proc && exec "$@"'
    unshare --mount --propagation private sh -c "$hide" sh setpriv --reuid="$uid" --regid="$gid" --init-groups env HOME="$dir" "$bin" explain --json "$dir/repo/w" >"$dir.user" 2>"$dir.err"
    unshare --mount --propagation private sh -c "$hide" sh "$bin" explain --json "$dir/repo/w" >"$dir.root" 2>>"$dir.err"
    if ! jq -e '.worktree.verdict == "unknown" and (.worktree.activity | .state == "unknown" and .reason == "hidden_processes" and (.detail | startswith("/proc is mounted with hidepid=invisible")))' "$dir.user" >/dev/null 2>&1; then
        fail "as uid $uid, under hidepid=invisible, explain says $(said "$dir.user") $(head -c 300 "$dir.err")"
    elif ! jq -e '.worktree.verdict == "candidate" and .worktree.activity.state == "not_observed"' "$dir.root" >/dev/null 2>&1; then
        fail "as root, under hidepid=invisible, explain says $(said "$dir.root") $(head -c 300 "$dir.err")"
    else
        ok
        echo "     uid $uid: $(jq -r '.worktree.activity.detail' "$dir.user"); root: candidate"
    fi
}

name=activity-pid-namespace-host-proc
if why=$(linux_root) && [ -n "$why" ]; then skip "$why"; else host_proc_case; fi

name=activity-user-namespace
if why=$(linux_root) && [ -n "$why" ]; then
    skip "$why"
elif ! why=$(unshare --user --map-root-user true 2>&1); then
    skip "unshare cannot make a user namespace here: $why"
else
    # root in a user namespace of its own may not read what the user it
    # maps to does not own, such as a checkout in a home folder: residue
    # runs from a copy in the scratch directory.
    setup cp "$bin" "$work/residue"
    bin_was=$bin
    bin=$work/residue
    contained "$name" "residue runs in a user namespace of its own, so it cannot see processes outside it" in_user_namespace
    bin=$bin_was
fi

name=activity-wsl-emulated
marker=
for m in /.dockerenv /run/.containerenv /run/systemd/container /run/host/container-manager; do
    [ -e "$m" ] && marker=$m
done
if why=$(linux_root) && [ -n "$why" ]; then
    skip "$why"
elif [ -n "$marker" ]; then
    skip "this machine has a container's marker, $marker, so no namespace here can stand for a WSL distribution"
elif ! why=$(unshare --pid --fork --mount --mount-proc true 2>&1); then
    skip "unshare cannot make a PID and mount namespace here: $why"
else
    wsl_case
fi

name=activity-hidepid
if why=$(linux_root) && [ -n "$why" ]; then skip "$why"; else hidepid_case; fi

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
