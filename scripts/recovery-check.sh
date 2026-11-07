#!/usr/bin/env bash
# Recovery check: what `residue remove` leaves when it is stopped, refused
# or blocked after it moved a worktree aside, and when two removers race.
#
#   bash scripts/recovery-check.sh [BINARY]    (default: target/release/residue)
#
# Exit status: 0 every scenario passed, 1 a scenario failed, 2 the runner
# could not run (no binary or jq, or a fixture setup step failed).
#
# Design. residue runs Git by name, so a `git` script first on PATH sees
# every Git command residue runs, in order, and can stop or fail one of them.
# Nothing in residue knows about it. The removal is, after the checks:
#
#   worktree move ORIGINAL ASIDE        ASIDE is .NAME.residue-removing-PID
#   worktree lock --reason "residue: removing" ASIDE
#   -C ASIDE worktree list, status, ...  the check after the move
#   worktree unlock ASIDE
#   worktree remove ASIDE               the only step that deletes
#   (if the check after the move fails: unlock, worktree move ASIDE ORIGINAL)
#
# Barriers and faults (the wrapper's modes):
#   hold-unlock    stop at `worktree unlock ASIDE`, the last step before the
#                  deletion; the test kills residue there (scenario 1)
#   touch-aside    create a file in ASIDE before the check after the move
#                  runs `status` there, so that check fails (scenarios 2, 3)
#   fail-back      fail `worktree move ASIDE ORIGINAL`, the move back (3)
#   hold-move      stop each remover at its `worktree move ORIGINAL ASIDE`,
#                  so both have passed every check; the test releases one,
#                  waits for it to finish, then releases the other (4)
#
# The oracles are independent of residue's report: Git's worktree list and
# lock reasons, the branch's commit, the files' bytes (cksum), the unrelated
# worktree, and `residue scan --json`'s list of interrupted removals.
#
# Outside the model: a kill or power loss during `git worktree remove`
# itself. Deletion may then be partial; residue does not promise to roll
# back a deletion that has started, and nothing here claims it.
#
# Needs bash, git, jq, pgrep and pkill. Everything lives under one fresh
# directory with its own HOME and Git config, and only that directory is
# deleted. Every process it starts has a deadline and is reaped.
set -uo pipefail

bin=${1:-target/release/residue}
case $bin in
/*) ;;
*) bin=$PWD/$bin ;;
esac
[ -x "$bin" ] || { echo "recovery-check: no executable at $bin; run cargo build --release first" >&2; exit 2; }
for tool in jq pgrep pkill; do
    command -v "$tool" >/dev/null || { echo "recovery-check: needs $tool" >&2; exit 2; }
done
real_git=$(command -v git) || { echo "recovery-check: needs git" >&2; exit 2; }

work=$(mktemp -d "${TMPDIR:-/tmp}/residue-recovery.XXXXXX") || exit 2
case $work in
*/residue-recovery.??????) ;;
*) echo "recovery-check: unexpected scratch directory $work" >&2; exit 2 ;;
esac

# Stop a process and everything it started, then reap what is ours.
kill_tree() { # PID
    local child
    for child in $(pgrep -P "$1" 2>/dev/null); do
        kill_tree "$child"
    done
    kill -TERM "$1" 2>/dev/null
}
started=()
cleanup() {
    local p
    for p in "${started[@]}"; do
        kill_tree "$p"
    done
    [ -f "$work/held" ] && kill -TERM "$(cat "$work/held")" 2>/dev/null
    wait 2>/dev/null
    rm -rf -- "$work"
}
trap cleanup EXIT

export HOME=$work/home XDG_CONFIG_HOME=$work/home/.config XDG_STATE_HOME=$work/home/.local/state
export GIT_CONFIG_NOSYSTEM=1 NO_COLOR=1 LC_ALL=C
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_COMMON_DIR GIT_CONFIG GIT_CONFIG_PARAMETERS GIT_CONFIG_COUNT
mkdir -p "$HOME" "$work/bin"
git config --global user.name recovery-check
git config --global user.email recovery-check@example.invalid
git config --global init.defaultBranch main
exec </dev/null

# The wrapper. Its mode is the contents of $work/mode, so one script serves
# every scenario; every wait in it is bounded.
cat >"$work/bin/git" <<EOF
#!/bin/sh
real='$real_git'
work='$work'
mode=\$(cat "\$work/mode" 2>/dev/null)
dir= prev= second= last=
for a in "\$@"; do
    [ "\$prev" = -C ] && dir=\$a
    second=\$last last=\$a prev=\$a
done
hold() { # NAME: mark that this process got here, wait for release
    echo \$\$ >"\$work/at-\$1"
    i=0
    until [ -e "\$work/go-\$1" ]; do
        i=\$((i + 1))
        [ "\$i" -le 1200 ] || exit 1
        sleep 0.1
    done
    [ ! -e "\$work/abort-\$1" ] || exit 1
}
case " \$* " in
*" worktree unlock "*)
    case "\$last \$mode" in
    *residue-removing-*hold-unlock*) [ -e "\$work/at-unlock" ] || hold unlock ;;
    esac
    ;;
*" worktree move "*)
    case "\$second" in
    *residue-removing-*)
        case \$mode in
        *fail-back*)
            echo "fatal: recovery-check: the move back is failed on purpose" >&2
            exit 128
            ;;
        esac
        ;;
    *)
        case "\$last \$mode" in
        *residue-removing-*hold-move*) hold "move-\$PPID" ;;
        esac
        ;;
    esac
    ;;
*" status "*)
    case "\$dir \$mode" in
    *residue-removing-*touch-aside*)
        [ -e "\$work/touched" ] || { echo added >"\$dir/added.txt" && : >"\$work/touched"; }
        ;;
    esac
    ;;
esac
exec "\$real" "\$@"
EOF
chmod +x "$work/bin/git"

passes=0
failures=0
name=

setup() {
    "$@" >/dev/null 2>&1 || {
        echo "recovery-check: setup failed in $name: $*" >&2
        exit 2
    }
}

fail() {
    echo "FAIL $name: $*"
    local f
    for f in "$work/$name".out*; do
        [ -s "$f" ] || continue
        echo "    $(basename "$f"):"
        head -c 1500 "$f" | sed 's/^/    | /'
        echo
    done
    echo "    worktrees:"
    git -C "$repo" worktree list --porcelain 2>&1 | head -40 | sed 's/^/    | /'
    failures=$((failures + 1))
}

ok() {
    echo "ok   $name: $*"
    passes=$((passes + 1))
}

# One repository: .worktrees/w on branch w (its own commit, a regenerable
# node_modules/), and .worktrees/u on branch u, which nothing names.
make_repo() {
    repo=$work/$name
    rm -f "$work"/at-* "$work"/go-* "$work"/abort-* "$work/touched" "$work/held"
    setup git init -q "$repo"
    printf 'node_modules/\n' >"$repo/.gitignore"
    echo hello >"$repo/README.md"
    setup git -C "$repo" add .
    setup git -C "$repo" commit -qm init
    setup git -C "$repo" worktree add -q "$repo/.worktrees/w" -b w
    wt=$repo/.worktrees/w
    echo w >>"$wt/README.md"
    setup git -C "$wt" commit -qam w
    mkdir -p "$wt/node_modules"
    echo x >"$wt/node_modules/a.js"
    setup git -C "$repo" worktree add -q "$repo/.worktrees/u" -b u
    w_sha=$(git -C "$repo" rev-parse w) || exit 2
    w_sum=$(sums "$wt") || exit 2
    u_sum=$(sums "$repo/.worktrees/u") || exit 2
}

sums() { # DIR: file names and checksums, the .git file excluded
    (cd "$1" && find . -type f ! -path ./.git -print | LC_ALL=C sort | while read -r f; do printf '%s ' "$f"; cksum <"$f"; done)
}

# Git's record for the worktree at PATH: "registered", plus "locked: REASON".
record() { # PATH
    local want line found=
    want=$(cd "$1" 2>/dev/null && pwd -P) || { echo absent; return; }
    while IFS= read -r line; do
        case $line in
        "worktree $want") found=registered ;;
        worktree\ *) [ "$found" = registered ] && break ;;
        locked*) [ "$found" = registered ] && found="registered, $line" ;;
        esac
    done < <(git -C "$repo" worktree list --porcelain)
    echo "${found:-unregistered}"
}

asides() { # the .w.residue-removing-* directories that exist, canonical
    local d
    for d in "$repo"/.worktrees/.w.residue-removing-*; do
        [ -e "$d" ] && (cd "$d" && pwd -P)
    done
}

# Start residue with the wrapper on PATH, in the background. Sets $pid.
start() { # OUT-SUFFIX ARGS...
    local suffix=$1
    shift
    PATH="$work/bin:$PATH" "$bin" "$@" >"$work/$name.out$suffix" 2>"$work/$name.err$suffix" &
    pid=$!
    started+=("$pid")
}

# Wait for PID to exit within 120 s, then reap it. Sets $status.
finish() { # PID
    local i
    for ((i = 0; i < 1200; i++)); do
        kill -0 "$1" 2>/dev/null || break
        sleep 0.1
    done
    if kill -0 "$1" 2>/dev/null; then
        kill_tree "$1"
        wait "$1" 2>/dev/null
        status=timeout
        return
    fi
    wait "$1"
    status=$?
}

await() { # FILE: wait up to 60 s for a barrier to be reached
    local i
    for ((i = 0; i < 600; i++)); do
        [ -e "$1" ] && return 0
        sleep 0.1
    done
    return 1
}

common_ok() { # u untouched and branch w still has its commit
    [ "$(record "$repo/.worktrees/u")" = registered ] || { fail "the unrelated worktree u is not registered as before"; return 1; }
    [ "$(sums "$repo/.worktrees/u")" = "$u_sum" ] || { fail "the unrelated worktree u changed"; return 1; }
    [ "$(git -C "$repo" rev-parse -q --verify w)" = "$w_sha" ] || { fail "branch w moved or is gone"; return 1; }
}

interrupted() { # the interrupted removals scan reports, as JSON
    "$bin" scan --json "$repo" 2>/dev/null | jq -c '[.repositories[].interrupted[]]'
}

# --- 1. Stopped after the move, before the deletion ------------------------
name=stopped-after-move
make_repo
echo hold-unlock >"$work/mode"
start "" remove --yes --json "$wt"
if ! await "$work/at-unlock"; then
    finish "$pid"
    fail "residue never reached the unlock before the deletion (exit $status)"
else
    # The phase reached: moved aside and locked, the check after the move
    # done, the unlock before `git worktree remove` not yet run.
    cp "$work/at-unlock" "$work/held"
    aside=$(asides)
    phase="$(record "$aside")"
    kill -TERM "$pid"
    finish "$pid"
    touch "$work/abort-unlock" "$work/go-unlock"
    kill -TERM "$(cat "$work/held")" 2>/dev/null
    rm -f "$work/held"
    left=$(interrupted)
    if [ -z "$aside" ] || [ "$(asides | wc -l | tr -d ' ')" != 1 ]; then fail "want exactly one moved-aside directory, got: $(asides)"
    elif [ "$phase" != "registered, locked residue: removing" ]; then fail "at the barrier Git had: $phase"
    elif [ "$status" = 0 ]; then fail "residue exited 0 although it was stopped"
    elif [ -e "$wt" ]; then fail "something is at the original path again"
    elif [ "$(record "$aside")" != "registered, locked residue: removing" ]; then fail "afterwards Git has: $(record "$aside")"
    elif [ "$(sums "$aside")" != "$w_sum" ]; then fail "the moved worktree's files changed"
    elif [ "$(jq 'length' <<<"$left")" != 1 ]; then fail "scan lists $(jq length <<<"$left") interrupted removals, want 1: $left"
    elif [ "$(jq -r '.[0].original_path' <<<"$left")" != "$(cd "$repo/.worktrees" && pwd -P)/w" ]; then fail "scan names the original as $(jq -r '.[0].original_path' <<<"$left")"
    elif [ "$(jq -r '.[0].path' <<<"$left")" != "$aside" ] || [ "$(jq -r '.[0].present' <<<"$left")" != true ]; then fail "scan does not point at the moved worktree: $left"
    elif [ "$(jq -r '.[0].pid_running' <<<"$left")" != false ]; then fail "scan says the stopped run is still running"
    elif ! jq -r '.[0].recovery' <<<"$left" | grep -q "worktree unlock .*worktree move "; then fail "no unlock-and-move-back recovery line: $(jq -r '.[0].recovery' <<<"$left")"
    elif common_ok; then
        # residue does not resume it: naming the hidden directory is invalid.
        out=$work/$name.out-again
        "$bin" remove --yes --json "$aside" >"$out" 2>/dev/null
        again=$?
        if [ "$again" != 3 ] || [ "$(jq -r '.results[0].result' "$out")" != invalid ]; then fail "removing the moved-aside directory gave exit $again, $(jq -r '.results[0].result' "$out")"
        elif [ ! -d "$aside" ]; then fail "a second run removed the interrupted worktree"
        else ok "stopped at the unlock; scan lists it with: $(jq -r '.[0].recovery' <<<"$left" | sed "s#$work#\$work#g")"; fi
    fi
fi

name=stopped-after-move-control
make_repo
echo hold-unlock >"$work/mode"
start "" remove --yes --json "$wt"
if ! await "$work/at-unlock"; then
    finish "$pid"
    fail "residue never reached the barrier (exit $status)"
else
    touch "$work/go-unlock"
    finish "$pid"
    if [ "$status" != 0 ]; then fail "exit $status, want 0"
    elif [ "$(jq -r '.results[0].result' "$work/$name.out")" != removed ]; then fail "result $(jq -r '.results[0].result' "$work/$name.out"), want removed"
    elif [ -e "$wt" ] || [ -n "$(asides)" ]; then fail "something of w is left"
    elif common_ok; then ok "released at the same barrier, the removal completes"; fi
fi

# --- 2. The check after the move fails, the move back works ----------------
name=refused-after-move
make_repo
echo touch-aside >"$work/mode"
start "" remove --yes --json "$wt"
finish "$pid"
out=$work/$name.out
if [ "$status" != 3 ]; then fail "exit $status, want 3"
elif [ ! -e "$work/touched" ]; then fail "the fault was never injected: no status ran in the moved-aside directory"
elif [ "$(jq -r '.results[0].result' "$out")" != skipped ]; then fail "result $(jq -r '.results[0].result' "$out"), want skipped"
elif ! jq -r '.results[0].reason' "$out" | grep -q "it is back in place"; then fail "reason: $(jq -r '.results[0].reason' "$out")"
elif [ "$(record "$wt")" != registered ]; then fail "at its original path Git has: $(record "$wt")"
elif [ -n "$(asides)" ]; then fail "a moved-aside directory is left: $(asides)"
elif [ "$(cat "$wt/added.txt" 2>/dev/null)" != added ]; then fail "the file added after the move is not in the restored worktree"
elif [ "$(sums "$wt" | grep -v '^./added.txt ')" != "$w_sum" ]; then fail "the restored worktree's other files changed"
elif [ "$(interrupted)" != "[]" ]; then fail "scan lists an interrupted removal: $(interrupted)"
elif common_ok; then ok "$(jq -r '.results[0].reason' "$out")"; fi

name=refused-after-move-control
make_repo
echo none >"$work/mode"
start "" remove --yes --json "$wt"
finish "$pid"
if [ "$status" != 0 ]; then fail "exit $status, want 0"
elif [ -e "$wt" ] || [ -n "$(asides)" ]; then fail "something of w is left"
elif common_ok; then ok "with no fault, the same run removes it"; fi

# --- 3. The check after the move fails, and so does the move back ----------
name=move-back-fails
make_repo
echo "touch-aside fail-back" >"$work/mode"
start "" remove --yes --json "$wt"
finish "$pid"
out=$work/$name.out
aside=$(asides)
reason=$(jq -r '.results[0].reason' "$out")
if [ "$status" != 3 ]; then fail "exit $status, want 3"
elif [ "$(jq -r '.results[0].result' "$out")" != failed ]; then fail "result $(jq -r '.results[0].result' "$out"), want failed"
elif grep -q "back in place" <<<"$reason"; then fail "it claims the worktree is back in place: $reason"
elif ! grep -q "could not move it back to" <<<"$reason" || ! grep -q "Recover with: " <<<"$reason"; then fail "reason: $reason"
elif [ -z "$aside" ] || ! grep -qF "It is at $aside" <<<"$reason"; then fail "the reason does not name where it is ($aside): $reason"
elif [ -e "$wt" ]; then fail "something is at the original path"
elif [ "$(record "$aside")" != "registered, locked residue: removing" ]; then fail "at the moved-aside path Git has: $(record "$aside")"
elif [ "$(cat "$aside/added.txt" 2>/dev/null)" != added ] || [ "$(sums "$aside" | grep -v '^./added.txt ')" != "$w_sum" ]; then fail "the moved worktree's files changed"
elif [ "$(interrupted | jq 'length')" != 1 ]; then fail "scan does not list it as an interrupted removal: $(interrupted)"
elif common_ok; then ok "failed, locked at the hidden path, and scan lists it"; fi

# --- 4. Two removers of one worktree ---------------------------------------
# Both pass every check and stop at their move; the first is released and
# finishes, then the second. Deterministic order, not a stress test.
name=two-removers
make_repo
echo hold-move >"$work/mode"
start ".a" remove --yes --json "$wt"
pid_a=$pid
start ".b" remove --yes --json "$wt"
pid_b=$pid
if ! await "$work/at-move-$pid_a" || ! await "$work/at-move-$pid_b"; then
    finish "$pid_a"
    finish "$pid_b"
    fail "both removers did not reach their move"
else
    touch "$work/go-move-$pid_a"
    finish "$pid_a"
    status_a=$status
    touch "$work/go-move-$pid_b"
    finish "$pid_b"
    status_b=$status
    ra=$(jq -r '.results[0].result' "$work/$name.out.a")
    rb=$(jq -r '.results[0].result' "$work/$name.out.b")
    if [ "$status_a,$ra" != "0,removed" ]; then fail "the first remover: exit $status_a, $ra; want 0, removed"
    elif [ "$status_b" != 3 ] || [ "$rb" = removed ] || [ "$rb" = trashed ]; then fail "the second remover: exit $status_b, $rb; want exit 3 and no removal"
    elif [ -e "$wt" ] || [ -n "$(asides)" ]; then fail "something of w is left: $(asides)"
    elif [ "$(interrupted)" != "[]" ]; then fail "scan lists an interrupted removal: $(interrupted)"
    elif common_ok; then ok "first removed it; second: $rb, $(jq -r '.results[0].reason' "$work/$name.out.b" | cut -c1-120 | sed "s#$work#\$work#g")"; fi
fi

echo "recovery-check: $passes passed, $failures failed"
[ "$failures" -eq 0 ]
