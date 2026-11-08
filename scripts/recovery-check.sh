#!/usr/bin/env bash
# Recovery check: what `residue remove` leaves when it is stopped, refused
# or blocked after it moved a worktree aside, and when a second remover of
# the same worktree runs after the first.
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
#   worktree move ORIGINAL ASIDE        ASIDE is .NAME.residue-removing-PID,
#                                       or a trash entry with --trash
#   worktree lock --reason "residue: removing" ASIDE
#   -C ASIDE worktree list, status, ...  the check after the move
#   update-ref refs/residue/removed/E   the removal record (and its info
#                                       file), which `residue restore` takes
#   worktree unlock ASIDE
#   worktree remove ASIDE               the only step that deletes
#   (if the check after the move fails: unlock, worktree move ASIDE ORIGINAL)
#
# Barriers and faults (the wrapper's modes):
#   hold-remove    stop at `worktree remove ASIDE`, before it runs: only a
#                  removal that passed the check after the move gets there
#                  (a refused one moves back instead); the test kills
#                  residue there (scenario 1)
#   touch-aside    create a file in ASIDE before the check after the move
#                  runs `status` there, so that check fails (scenarios 2, 3)
#   fail-back      fail `worktree move ASIDE ORIGINAL`, the move back (3)
#   occupy-original with touch-aside, also create a directory at the
#                  original path, so residue cannot move it back there (3b)
#   hold-move      stop each remover at its `worktree move ORIGINAL ASIDE`,
#                  so both have passed every check; the test releases one,
#                  waits for it to finish, then releases the other (4, and
#                  4b with --trash, where each made a trash entry first)
#   part-remove-reflog, part-remove-head, part-remove-dotgit
#                  fail `worktree remove ASIDE` after deleting the
#                  worktree's HEAD reflog, or its HEAD, from its Git
#                  directory, or the `.git` file from its checkout: what
#                  Git's own remove leaves when it stops at a file it
#                  cannot delete, or is killed during the delete (5, 5b, 5c)
#   hold-after     with a part-remove mode, hold the Git command after the
#                  failed remove, so residue still runs (5d)
#
# A held command waits at most 50 s, and never runs once its residue has
# exited or the test aborted it. The two removers of scenario 4 start one
# after the other, the second once the first is held, so neither one's
# checks can see the other's Git inside the worktree.
#
# The oracles are independent of residue's report: Git's worktree list and
# lock reasons, the branch's commit, the files' bytes (cksum), the unrelated
# worktree, and `residue scan --json`'s list of interrupted removals.
#
# Outside the model: a kill or power loss during `git worktree remove`
# itself. Deletion may then be partial; residue does not promise to roll
# back a deletion that has started, and nothing here claims it. What it
# does promise, and 5 checks: a remove that fails part way keeps the
# removal record, so the commits the worktree held survive `git gc`, and
# `residue restore` brings it back.
#
# Needs bash, git, jq and pgrep. Everything lives under one fresh
# directory with its own HOME and Git config, and only that directory is
# deleted. Every process it starts has a deadline and is reaped.
set -uo pipefail

bin=${1:-target/release/residue}
case $bin in
/*) ;;
*) bin=$PWD/$bin ;;
esac
[ -x "$bin" ] || { echo "recovery-check: no executable at $bin; run cargo build --release first" >&2; exit 2; }
for tool in jq pgrep; do
    command -v "$tool" >/dev/null || { echo "recovery-check: needs $tool" >&2; exit 2; }
done
real_git=$(command -v git) || { echo "recovery-check: needs git" >&2; exit 2; }
# shellcheck source=SCRIPTDIR/partial-probe.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/partial-probe.sh"

work=$(mktemp -d "${TMPDIR:-/tmp}/residue-recovery.XXXXXX") || exit 2
case $work in
*/residue-recovery.??????) ;;
*) echo "recovery-check: unexpected scratch directory $work" >&2; exit 2 ;;
esac

# PID and every process under it, the parent first.
tree() { # PID
    local child
    echo "$1"
    for child in $(pgrep -P "$1" 2>/dev/null); do
        tree "$child"
    done
}
# Stop PID and everything it started. The whole list is taken first, and
# the parent is signalled first, so none of them reacts to a child's death
# or starts a new one; whatever is left gets KILL a second later.
kill_tree() { # PID
    local pids
    pids=$(tree "$1")
    # shellcheck disable=SC2086
    kill -TERM $pids 2>/dev/null
    sleep 1
    # shellcheck disable=SC2086
    kill -KILL $pids 2>/dev/null
    return 0
}
# The residue runs this script started and has not reaped yet.
started=()
forget() { # PID
    local p keep=()
    for p in "${started[@]+"${started[@]}"}"; do
        [ "$p" = "$1" ] || keep+=("$p")
    done
    started=("${keep[@]+"${keep[@]}"}")
}
cleanup() {
    local p
    for p in "${started[@]+"${started[@]}"}"; do
        kill_tree "$p"
    done
    [ -f "$work/held" ] && kill -TERM "$(cat "$work/held")" 2>/dev/null
    wait 2>/dev/null
    rm -rf -- "$work"
}
trap cleanup EXIT

export HOME=$work/home XDG_CONFIG_HOME=$work/home/.config XDG_STATE_HOME=$work/home/.local/state
export GIT_CONFIG_NOSYSTEM=1 NO_COLOR=1 LC_ALL=C
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_COMMON_DIR GIT_CONFIG GIT_CONFIG_PARAMETERS GIT_CONFIG_COUNT GIT_CONFIG_GLOBAL GIT_CONFIG_SYSTEM
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
# Wait for release, at most 50 s: under residue's 60 s limit on one Git
# query. A held command never runs once its residue is gone or the test
# aborted it, so a stopped run cannot go on to delete.
hold() { # NAME
    echo \$\$ >"\$work/at-\$1"
    i=0
    until [ -e "\$work/go-\$1" ]; do
        i=\$((i + 1))
        [ "\$i" -le 500 ] || exit 1
        sleep 0.1
    done
    [ ! -e "\$work/abort-\$1" ] || exit 1
    kill -0 "\$PPID" 2>/dev/null || exit 1
}
# hold-after: once the part-way remove failed, hold the next Git command,
# so that residue still runs while its removal record names the worktree.
case \$mode in
*hold-after*) [ ! -e "\$work/part-done" ] || [ -e "\$work/at-after" ] || hold after ;;
esac
case " \$* " in
*" worktree remove "*)
    case "\$last \$mode" in
    *residue-removing-*hold-remove*) [ -e "\$work/at-remove" ] || hold remove ;;
    *residue-removing-*part-remove-*)
        # Git's own remove stops at the first file it cannot delete, after
        # the ones the filesystem listed before it: here, the worktree's
        # HEAD reflog or its HEAD, or the checkout's .git file, while Git
        # still lists the worktree.
        gd=\$("\$real" -C "\$last" rev-parse --absolute-git-dir) || exit 1
        case \$mode in
        *part-remove-reflog*) rm -f "\$gd/logs/HEAD" ;;
        *part-remove-head*) rm -f "\$gd/HEAD" ;;
        *part-remove-dotgit*) rm -f "\$last/.git" ;;
        esac
        : >"\$work/part-done"
        echo "error: recovery-check: the remove stops part way on purpose" >&2
        exit 255
        ;;
    esac
    ;;
*" worktree move "*)
    # ASIDE is .NAME.residue-removing-PID, or a trash entry with --trash.
    case "\$second" in
    *residue-removing-* | */residue-trash/*)
        case \$mode in
        *fail-back*)
            echo "fatal: recovery-check: the move back is failed on purpose" >&2
            exit 128
            ;;
        esac
        ;;
    *)
        case "\$last \$mode" in
        *residue-removing-*hold-move* | */residue-trash/*hold-move*) hold "move-\$PPID" ;;
        esac
        ;;
    esac
    ;;
*" status "*)
    case "\$dir \$mode" in
    *residue-removing-*touch-aside*)
        if [ ! -e "\$work/touched" ]; then
            echo added >"\$dir/added.txt"
            case \$mode in
            *occupy-original*)
                # Something new at the original path: .NAME.residue-removing-PID -> NAME
                base=\$(basename "\$dir" | sed 's/^\\.\\(.*\\)\\.residue-removing-[0-9]*\$/\\1/')
                mkdir -p "\$(dirname "\$dir")/\$base" && echo occupied >"\$(dirname "\$dir")/\$base/occupied.txt"
                ;;
            esac
            : >"\$work/touched"
        fi
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
    rm -f "$work"/at-* "$work"/go-* "$work"/abort-* "$work/touched" "$work/held" "$work/part-done"
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
        forget "$1"
        status=timeout
        return
    fi
    wait "$1"
    status=$?
    forget "$1"
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

# Removal records: residue writes one, and its ref, after the check after
# the move and right before `git worktree remove`; one that did not delete
# drops both. "N records, M refs".
records() {
    local n m
    n=$(find "$repo/.git/residue-removed" -mindepth 1 -maxdepth 1 -type d 2>/dev/null | wc -l | tr -d ' ')
    m=$(git -C "$repo" for-each-ref refs/residue | wc -l | tr -d ' ')
    echo "$n records, $m refs"
}

# --- 0. NetBSD, OpenBSD and DragonFly: nothing reaches the move ------------
# Every scenario below starts from a clean worktree that residue removes.
# Where the probe cannot show a worktree unused (scripts/partial-probe.sh),
# that worktree is unknown and no run gets past its checks, so each one is
# skipped by name. Instead: the same run, with every Git move held, never
# reaches a move, keeps the worktree as it was, and leaves nothing behind.
if [ -n "$partial_probe" ]; then
    name=kept-unknown
    make_repo
    echo hold-move >"$work/mode"
    start "" remove --yes --json "$wt"
    finish "$pid"
    out=$work/$name.out
    reached=$(find "$work" -maxdepth 1 -name 'at-move-*' | head -1)
    if [ "$status" != 3 ]; then fail "exit $status, want 3"
    elif [ -n "$reached" ]; then fail "residue reached git worktree move"
    elif [ "$(jq -r '.results[0].result' "$out")" != refused ]; then fail "result $(jq -r '.results[0].result' "$out"), want refused"
    elif ! jq -e --arg detail "$partial_probe_detail" '.results[0].worktree.activity | .state == "unknown" and .reason == "partial_probe" and (.detail | endswith($detail))' "$out" >/dev/null; then fail "activity is not unknown (partial_probe): $(jq -c '.results[0].worktree.activity' "$out")"
    elif [ "$(record "$wt")" != registered ]; then fail "at its path Git has: $(record "$wt")"
    elif [ "$(sums "$wt")" != "$w_sum" ]; then fail "the worktree's files changed"
    elif [ -n "$(asides)" ]; then fail "a moved-aside directory exists: $(asides)"
    elif [ -n "$(ls -A "$repo/.git/residue-trash" 2>/dev/null)" ]; then fail "a trash entry was created"
    elif [ "$(interrupted)" != "[]" ]; then fail "scan lists an interrupted removal: $(interrupted)"
    elif [ "$(records)" != "0 records, 0 refs" ]; then fail "removal records: $(records), want 0 records, 0 refs"
    elif common_ok; then ok "refused before any move: $(jq -r '.results[0].worktree.activity.detail' "$out")"; fi
    for scenario in stopped-after-move stopped-after-move-restore stopped-after-move-control refused-after-move \
        refused-after-move-control move-back-fails original-taken two-removers two-removers-trash trash-move-in-flight \
        remove-fails-part-way-reflog remove-fails-part-way-head remove-fails-part-way-dotgit remove-fails-part-way-running; do
        skip_partial "$scenario"
    done
    echo "recovery-check: $passes passed, $failures failed"
    [ "$failures" -eq 0 ]
    exit
fi

# --- 1. Stopped after the move, before the deletion ------------------------
name=stopped-after-move
make_repo
echo hold-remove >"$work/mode"
start "" remove --yes --json "$wt"
if ! await "$work/at-remove"; then
    finish "$pid"
    fail "residue never reached git worktree remove (exit $status)"
else
    # The phase reached: moved aside, locked, checked after the move,
    # unlocked; `git worktree remove` called but not yet run.
    cp "$work/at-remove" "$work/held"
    aside=$(asides)
    phase="$(record "$aside")"
    main=$(cd "$repo" && pwd -P)
    kill -TERM "$pid"
    finish "$pid"
    touch "$work/abort-remove" "$work/go-remove"
    kill -TERM "$(cat "$work/held")" 2>/dev/null
    rm -f "$work/held"
    left=$(interrupted)
    if [ -z "$aside" ] || [ "$(asides | wc -l | tr -d ' ')" != 1 ]; then fail "want exactly one moved-aside directory, got: $(asides)"
    elif [ "$phase" != registered ]; then fail "at the barrier Git had: $phase, want registered and unlocked"
    elif [ "$status" = 0 ]; then fail "residue exited 0 although it was stopped"
    elif [ -e "$wt" ]; then fail "something is at the original path again"
    elif [ "$(record "$aside")" != registered ]; then fail "afterwards Git has: $(record "$aside")"
    elif [ "$(sums "$aside")" != "$w_sum" ]; then fail "the moved worktree's files changed"
    elif [ "$(jq 'length' <<<"$left")" != 1 ]; then fail "scan lists $(jq length <<<"$left") interrupted removals, want 1: $left"
    elif [ "$(jq -r '.[0].original_path' <<<"$left")" != "$(cd "$repo/.worktrees" && pwd -P)/w" ]; then fail "scan names the original as $(jq -r '.[0].original_path' <<<"$left")"
    elif [ "$(jq -r '.[0].path' <<<"$left")" != "$aside" ] || [ "$(jq -r '.[0].present' <<<"$left")" != true ]; then fail "scan does not point at the moved worktree: $left"
    elif [ "$(jq -r '.[0].pid_running' <<<"$left")" != false ]; then fail "scan says the stopped run is still running"
    elif [ "$(jq -r '.[0].recovery' <<<"$left")" != "git -C $main worktree move $aside $main/.worktrees/w" ]; then fail "scan's recovery line: $(jq -r '.[0].recovery' <<<"$left")"
    elif common_ok; then
        # residue does not resume it: naming the hidden directory is invalid.
        out=$work/$name.out-again
        "$bin" remove --yes --json "$aside" >"$out" 2>/dev/null
        again=$?
        if [ "$again" != 3 ] || [ "$(jq -r '.results[0].result' "$out")" != invalid ]; then fail "removing the moved-aside directory gave exit $again, $(jq -r '.results[0].result' "$out")"
        elif [ ! -d "$aside" ]; then fail "a second run removed the interrupted worktree"
        else ok "stopped at git worktree remove; scan lists it, with the line that moves it back"; fi
    fi
fi

# 1, continued: the run wrote its removal record before it stopped, so
# `residue restore` lists the interrupted removal and moves it back.
name=stopped-after-move-restore
if [ -d "${aside:-}" ] && [ "$(record "$aside")" = registered ]; then
    listed=$(cd "$repo" && "$bin" restore --json 2>/dev/null | jq -c '[.removed[] | {state, aside_path}]')
    out=$work/$name.out
    (cd "$repo" && "$bin" restore --json w >"$out" 2>"$work/$name.err")
    restored=$?
    if [ "$(records)" = "0 records, 0 refs" ] && [ "$restored" != 0 ]; then fail "the stopped run left no removal record"
    elif [ "$listed" != "[{\"state\":\"interrupted\",\"aside_path\":\"$aside\"}]" ]; then fail "restore lists: $listed, want one interrupted removal at $aside"
    elif [ "$restored" != 0 ]; then fail "restore exited $restored: $(cat "$work/$name.err")"
    elif [ "$(jq -r '.restore.checkout' "$out")" != moved_back ]; then fail "checkout $(jq -r '.restore.checkout' "$out"), want moved_back"
    elif [ "$(record "$wt")" != registered ]; then fail "at its path Git has: $(record "$wt")"
    elif [ -n "$(asides)" ]; then fail "a moved-aside directory is left: $(asides)"
    elif [ "$(sums "$wt")" != "$w_sum" ]; then fail "the restored worktree's files changed"
    elif [ "$(records)" != "0 records, 0 refs" ]; then fail "after restore: $(records)"
    elif [ "$(interrupted)" != "[]" ]; then fail "scan still lists an interrupted removal: $(interrupted)"
    elif common_ok; then ok "restore lists it as interrupted and moves it back, every file as it was"; fi
else
    fail "scenario 1 left no interrupted removal to restore"
fi

name=stopped-after-move-control
make_repo
echo hold-remove >"$work/mode"
start "" remove --yes --json "$wt"
if ! await "$work/at-remove"; then
    finish "$pid"
    fail "residue never reached the barrier (exit $status)"
else
    touch "$work/go-remove"
    finish "$pid"
    if [ "$status" != 0 ]; then fail "exit $status, want 0"
    elif [ "$(jq -r '.results[0].result' "$work/$name.out")" != removed ]; then fail "result $(jq -r '.results[0].result' "$work/$name.out"), want removed"
    elif [ -e "$wt" ] || [ -n "$(asides)" ]; then fail "something of w is left"
    elif [ "$(records)" != "1 records, 1 refs" ]; then fail "removal records: $(records), want 1 records, 1 refs"
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
elif [ "$(records)" != "0 records, 0 refs" ]; then fail "removal records: $(records), want 0 records, 0 refs"
elif common_ok; then ok "$(jq -r '.results[0].reason' "$out")"; fi

name=refused-after-move-control
make_repo
echo none >"$work/mode"
start "" remove --yes --json "$wt"
finish "$pid"
if [ "$status" != 0 ]; then fail "exit $status, want 0"
elif [ -e "$wt" ] || [ -n "$(asides)" ]; then fail "something of w is left"
elif [ "$(records)" != "1 records, 1 refs" ]; then fail "removal records: $(records), want 1 records, 1 refs"
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
elif ! grep -qF "Recover with: git -C $(cd "$repo" && pwd -P) worktree unlock $aside && git -C $(cd "$repo" && pwd -P) worktree move $aside $(cd "$repo" && pwd -P)/.worktrees/w" <<<"$reason"; then fail "the recovery line does not unlock, then move back: $reason"
elif [ "$(interrupted | jq -r '.[0].recovery')" != "git -C $(cd "$repo" && pwd -P) worktree unlock $aside && git -C $(cd "$repo" && pwd -P) worktree move $aside $(cd "$repo" && pwd -P)/.worktrees/w" ]; then fail "scan's recovery line: $(interrupted | jq -r '.[0].recovery')"
elif [ -e "$wt" ]; then fail "something is at the original path"
elif [ "$(record "$aside")" != "registered, locked residue: removing" ]; then fail "at the moved-aside path Git has: $(record "$aside")"
elif [ "$(cat "$aside/added.txt" 2>/dev/null)" != added ] || [ "$(sums "$aside" | grep -v '^./added.txt ')" != "$w_sum" ]; then fail "the moved worktree's files changed"
elif [ "$(interrupted | jq 'length')" != 1 ]; then fail "scan does not list it as an interrupted removal: $(interrupted)"
elif [ "$(records)" != "0 records, 0 refs" ]; then fail "removal records: $(records), want 0 records, 0 refs"
elif common_ok; then ok "failed, locked at the hidden path, and scan lists it"; fi

# --- 3b. Something new at the original path: residue cannot move it back ---
name=original-taken
make_repo
echo "touch-aside occupy-original" >"$work/mode"
start "" remove --yes --json "$wt"
finish "$pid"
out=$work/$name.out
aside=$(asides)
reason=$(jq -r '.results[0].reason' "$out")
if [ "$status" != 3 ]; then fail "exit $status, want 3"
elif [ "$(jq -r '.results[0].result' "$out")" != failed ]; then fail "result $(jq -r '.results[0].result' "$out"), want failed"
elif ! grep -q "something exists at that path again" <<<"$reason" || ! grep -q "Recover with: " <<<"$reason"; then fail "reason: $reason"
elif [ "$(cat "$wt/occupied.txt" 2>/dev/null)" != occupied ]; then fail "what was put at the original path changed"
elif [ -z "$aside" ] || [ "$(record "$aside")" != "registered, locked residue: removing" ]; then fail "at the moved-aside path Git has: $(record "$aside")"
elif [ "$(cat "$aside/added.txt" 2>/dev/null)" != added ] || [ "$(sums "$aside" | grep -v '^./added.txt ')" != "$w_sum" ]; then fail "the moved worktree's files changed"
elif [ "$(interrupted | jq 'length')" != 1 ]; then fail "scan does not list it as an interrupted removal: $(interrupted)"
elif [ "$(records)" != "0 records, 0 refs" ]; then fail "removal records: $(records), want 0 records, 0 refs"
elif common_ok; then ok "failed, still locked 'residue: removing' at the hidden path"; fi

# --- 4. Two removers of one worktree ---------------------------------------
# The first passes every check and stops at its move; then the second
# starts, passes every check and stops at its move. The first is released
# and finishes, then the second. Deterministic order, not a stress test.
name=two-removers
make_repo
echo hold-move >"$work/mode"
start ".a" remove --yes --json "$wt"
pid_a=$pid
await "$work/at-move-$pid_a"
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
    elif [ "$status_b,$rb" != "3,failed" ]; then fail "the second remover: exit $status_b, $rb; want 3, failed"
    elif ! jq -r '.results[0].reason' "$work/$name.out.b" | grep -q "could not move it aside: git worktree move refused"; then fail "the second remover did not fail at Git's move: $(jq -r '.results[0].reason' "$work/$name.out.b")"
    elif [ -e "$wt" ] || [ -n "$(asides)" ]; then fail "something of w is left: $(asides)"
    elif [ "$(interrupted)" != "[]" ]; then fail "scan lists an interrupted removal: $(interrupted)"
    elif [ "$(records)" != "1 records, 1 refs" ]; then fail "removal records: $(records), want 1 records, 1 refs"
    elif common_ok; then ok "first removed it; second: $rb, $(jq -r '.results[0].reason' "$work/$name.out.b" | cut -c1-120 | sed "s#$work#\$work#g")"; fi
fi

# --- 4b. Two removers with --trash: each made a trash entry before its move;
# the one whose move failed must drop it.
name=two-removers-trash
make_repo
echo hold-move >"$work/mode"
start ".a" remove --yes --json --trash "$wt"
pid_a=$pid
await "$work/at-move-$pid_a"
start ".b" remove --yes --json --trash "$wt"
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
    entries=$(find "$repo/.git/residue-trash" -mindepth 1 -maxdepth 1 2>/dev/null | wc -l | tr -d ' ')
    if [ "$status_a,$ra" != "0,trashed" ]; then fail "the first remover: exit $status_a, $ra; want 0, trashed"
    elif [ "$status_b,$rb" != "3,failed" ]; then fail "the second remover: exit $status_b, $rb; want 3, failed"
    elif ! jq -r '.results[0].reason' "$work/$name.out.b" | grep -q "could not move it aside: git worktree move refused"; then fail "the second remover did not fail at Git's move: $(jq -r '.results[0].reason' "$work/$name.out.b")"
    elif [ "$entries" != 1 ]; then fail "$entries trash entries, want exactly 1 (the second remover's must be dropped)"
    elif [ -e "$wt" ] || [ -n "$(asides)" ]; then fail "something of w is left outside the trash"
    elif [ "$(records)" != "0 records, 0 refs" ]; then fail "removal records: $(records), want 0 records, 0 refs"
    elif common_ok; then ok "one trash entry; second: $rb"; fi
fi

# --- 4c. A move into the trash in flight while another run purges -----------
# The first remover made its trash entry, wrote its info and is held at
# its `git worktree move`, so the entry holds only its record. A second
# remove run in the same repository lists the trash then: it must leave
# that entry alone (an entry that holds only its record is cleared an hour
# after it was made), and once released the first must trash w whole, with
# its info, so `residue restore` brings it back. Outside the wrapper: a
# move landing between the purge's two syscalls, `info` removed and the
# directory removed; the hour keeps a purge from getting that far.
name=trash-move-in-flight
make_repo
echo hold-move >"$work/mode"
start ".a" remove --yes --json --trash "$wt"
pid_a=$pid
if ! await "$work/at-move-$pid_a"; then
    finish "$pid_a"
    fail "the first remover never reached its move (exit $status)"
else
    : >"$work/mode"
    entry=$(find "$repo/.git/residue-trash" -mindepth 1 -maxdepth 1 -type d 2>/dev/null)
    setup git -C "$repo" worktree add -q "$repo/.worktrees/x" -b x
    start ".b" remove --yes --json "$repo/.worktrees/x"
    finish "$pid"
    status_b=$status
    purged_b=$(jq -r '[.purged[] | .result + " " + .entry.entry] | join(",")' "$work/$name.out.b")
    info_kept=no
    [ -f "$entry/info" ] && info_kept=yes
    touch "$work/go-move-$pid_a"
    finish "$pid_a"
    status_a=$status
    ra=$(jq -r '.results[0].result' "$work/$name.out.a")
    moved=$entry/w
    if [ -z "$entry" ] || [ "$(printf '%s\n' "$entry" | wc -l | tr -d ' ')" != 1 ]; then fail "want one trash entry while the first remover is held, got: ${entry:-none}"
    elif [ "$status_b" != 0 ]; then fail "the second run: exit $status_b, want 0"
    elif [ -n "$purged_b" ]; then fail "the second run purged the entry being made: $purged_b"
    elif [ "$info_kept" != yes ]; then fail "the second run removed the entry's info"
    elif [ "$status_a,$ra" != "0,trashed" ]; then fail "the first remover: exit $status_a, $ra; want 0, trashed"
    elif [ ! -f "$entry/info" ]; then fail "the entry has no info after the move"
    elif [ "$(record "$moved")" != "registered, locked residue: in trash" ]; then fail "in the trash Git has: $(record "$moved")"
    elif [ "$(sums "$moved")" != "$w_sum" ]; then fail "the trashed worktree's files changed"
    else
        (cd "$repo" && "$bin" restore --json w >"$work/$name.out-restore" 2>"$work/$name.err-restore")
        restored=$?
        if [ "$restored" != 0 ]; then fail "restore exited $restored: $(cat "$work/$name.err-restore")"
        elif [ "$(record "$wt")" != registered ] || [ "$(sums "$wt")" != "$w_sum" ]; then fail "w is not back whole: $(record "$wt")"
        elif [ -n "$(ls -A "$repo/.git/residue-trash" 2>/dev/null)" ]; then fail "the trash entry is left after the restore"
        elif common_ok; then ok "the second run left the entry alone; w trashed whole and restored"; fi
    fi
fi

# --- 5. git worktree remove fails part way ----------------------------------
# Git deletes the checkout, then its own directory for the worktree, each
# in the order the filesystem lists the files, and stops at the first it
# cannot delete; a kill stops it anywhere. Here it lost its HEAD reflog
# (5), its HEAD (5b) or the checkout's .git file (5c), still lists the
# worktree, and kept its other files. w holds a commit only its HEAD
# reflog names. The removal record must stay, that commit must survive `git
# gc`, and `residue restore` must bring w back with the commit in its
# reflog: moved back (5), or recreated once Git's leftovers of a worktree
# it can no longer use are cleared, as restore says (5b, 5c). For those
# two, scan must print only the restore of the record: Git refuses the
# move back, and `recovery`, which a script may run, deletes nothing.
part_remove() { # MODE: make the repository and run the failing removal
    make_repo
    setup git -C "$wt" checkout -q --detach
    setup git -C "$wt" commit -q --allow-empty -m lone
    lone=$(git -C "$wt" rev-parse HEAD) || exit 2
    setup git -C "$wt" checkout -q w
    echo "$1" >"$work/mode"
    start "" remove --yes --json "$wt"
    finish "$pid"
    out=$work/$name.out
    aside=$(asides)
    reason=$(jq -r '.results[0].reason' "$out")
    git -C "$repo" -c gc.reflogExpire=now -c gc.reflogExpireUnreachable=now gc -q --prune=now >/dev/null 2>&1
}
# The failed removal left w at the moved-aside path, and its record keeps
# it. LACK is what the reason must say is gone.
part_remove_ok() { # LACK
    if [ "$status" != 3 ]; then fail "exit $status, want 3"
    elif [ "$(jq -r '.results[0].result' "$out")" != failed ]; then fail "result $(jq -r '.results[0].result' "$out"), want failed"
    elif ! grep -qF "but $1" <<<"$reason" || ! grep -q "; its removal record keeps its HEAD and reflog: residue restore " <<<"$reason"; then fail "reason: $reason"
    elif grep -q "Recover with: " <<<"$reason"; then fail "it gives a move back as the recovery: $reason"
    elif [ -z "$aside" ] || [ "$(record "$aside")" != registered ]; then fail "want it at the moved-aside path, which Git lists: ${aside:-none}"
    elif [ "$(sums "$aside")" != "$w_sum" ]; then fail "the moved worktree's files changed"
    elif [ "$(records)" != "1 records, 2 refs" ]; then fail "removal records: $(records), want 1 records, 2 refs"
    elif ! git -C "$repo" cat-file -e "$lone^{commit}" 2>/dev/null; then fail "git gc lost the commit only its HEAD reflog held"
    else return 0; fi
    return 1
}
# w is back at its path, its reflog names the lone commit, and the record
# and its refs are gone. CHECKOUT is how restore brought it back.
restored_ok() { # CHECKOUT
    if [ "$restored" != 0 ]; then fail "restore exited $restored: $(cat "$work/$name.err-restore")"
    elif [ "$(jq -r '.restore.checkout' "$work/$name.out-restore")" != "$1" ]; then fail "checkout $(jq -r '.restore.checkout' "$work/$name.out-restore"), want $1"
    elif [ "$(record "$wt")" != registered ]; then fail "at its path Git has: $(record "$wt")"
    elif [ -n "$(asides)" ]; then fail "a moved-aside directory is left: $(asides)"
    elif ! grep -qx "$lone" <<<"$(git -C "$wt" reflog --format=%H)"; then fail "the restored worktree's reflog does not name the commit only its HEAD reflog held"
    elif [ "$(records)" != "0 records, 0 refs" ]; then fail "after restore: $(records), want 0 records, 0 refs"
    elif common_ok; then return 0; fi
    return 1
}
restore_w() { # sets $restored
    (cd "$repo" && "$bin" restore --json w >"$work/$name.out-restore" 2>"$work/$name.err-restore")
    restored=$?
}

name=remove-fails-part-way-reflog
part_remove part-remove-reflog
if part_remove_ok "part of its HEAD reflog is gone"; then
    restore_w
    if ! restored_ok moved_back; then :
    elif [ "$(sums "$wt")" != "$w_sum" ]; then fail "the restored worktree's files changed"
    else ok "the record stays, gc keeps the commit, and restore moves it back with its reflog"; fi
fi

# What scan says of w, which Git can no longer use for the reason WHY: the
# restore of its record, and no move back, in the text and in `recovery`.
scan_unusable_ok() { # WHY
    local main entry text left
    main=$(cd "$repo" && pwd -P)
    entry=$(cd "$repo/.git/residue-removed" && ls)
    text=$(cd "$repo" && "$bin" scan 2>&1)
    left=$(interrupted)
    if ! grep -qF "Git can no longer use it ($1); restore says what to clear, then recreates it: residue restore $main/.git/residue-removed/$entry" <<<"$text"; then fail "scan does not give the restore of its record: $text"
    elif grep -qE "put it back|worktree move|or: residue restore" <<<"$text"; then fail "scan still gives a move back Git refuses: $text"
    elif [ "$(jq -r '.[0].recovery' <<<"$left")" != "residue restore $main/.git/residue-removed/$entry" ]; then fail "scan's recovery: $(jq -r '.[0].recovery' <<<"$left")"
    elif ! jq -e --arg why "$1" --arg rec "$main/.git/residue-removed/$entry" '.[0].unusable == $why and .[0].removal_record == $rec' <<<"$left" >/dev/null; then fail "scan's interrupted removal: $left"
    else return 0; fi
    return 1
}
# 5b, 5c: restore refuses, says what to clear, and once it is cleared
# recreates w.
part_remove_unusable() { # MODE WHY
    part_remove "$1"
    part_remove_ok "${3:-Git can no longer use it}" || return
    scan_unusable_ok "$2" || return
    restore_w
    main=$(cd "$repo" && pwd -P)
    if [ "$restored" != 1 ]; then fail "restore of a worktree Git cannot use exited $restored, want 1"
    elif ! grep -qF "Git can no longer use it" "$work/$name.err-restore" || ! grep -qF "delete $aside, run \`git -C $main worktree prune\`, then run this restore again" "$work/$name.err-restore"; then fail "restore does not say what to clear: $(cat "$work/$name.err-restore")"
    elif [ "$(records)" != "1 records, 2 refs" ]; then fail "after the refused restore: $(records), want 1 records, 2 refs"
    else
        rm -rf -- "$aside"
        setup git -C "$repo" worktree prune
        restore_w
        if ! restored_ok checked_out_branch; then :
        elif [ "$(sums "$wt")" != "$(grep -v '^\./node_modules/' <<<"$w_sum")" ]; then fail "the recreated worktree's tracked files differ"
        else ok "the record stays, gc keeps the commit, scan and restore say what to clear, then restore recreates it"; fi
    fi
}

name=remove-fails-part-way-head
part_remove_unusable part-remove-head "Git reads no HEAD in its Git directory"

name=remove-fails-part-way-dotgit
part_remove_unusable part-remove-dotgit "its .git file is gone" "its files are gone"

# 5d: restore while the run that left w, which Git can no longer use, still
# runs. No command can move it back: restore says to wait for that process
# and run it again, or, if the PID is another process (PIDs are reused),
# what to clear; it never prints an empty command.
name=remove-fails-part-way-running
make_repo
echo "part-remove-head hold-after" >"$work/mode"
start "" remove --yes --json "$wt"
if ! await "$work/at-after"; then
    fail "the removal never got past its failed git worktree remove"
    : >"$work/abort-after"
    : >"$work/go-after"
    finish "$pid"
else
    aside=$(asides)
    restore_w
    : >"$work/go-after"
    finish "$pid"
    if [ "$restored" != 1 ]; then fail "restore while the removal runs exited $restored, want 1"
    elif ! grep -qF "process $pid still runs, so residue may still be removing it, and Git can no longer use it (Git reads no HEAD in its Git directory); once that process has exited, run this restore again: it says what to clear; if it is not residue (\`ps -p $pid\` shows what it is): delete $aside, run \`git -C $(cd "$repo" && pwd -P) worktree prune\`, then run this restore again" "$work/$name.err-restore"; then fail "restore: $(cat "$work/$name.err-restore")"
    elif grep -qE "put it back with: *$" "$work/$name.err-restore"; then fail "restore prints an empty command: $(cat "$work/$name.err-restore")"
    elif [ "$status" != 3 ]; then fail "the removal exited $status, want 3"
    elif [ -z "$aside" ] || [ "$(record "$aside")" != registered ]; then fail "want it at the moved-aside path, which Git lists: ${aside:-none}"
    elif [ "$(records)" = "0 records, 0 refs" ]; then fail "the removal record is gone"
    else ok "restore says to wait for process $pid, with no empty command"; fi
fi

echo "recovery-check: $passes passed, $failures failed"
[ "$failures" -eq 0 ]
