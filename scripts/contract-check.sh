#!/usr/bin/env bash
# Contract check: the real residue binary against real Git, with no prompt.
# Each scenario builds its own repository, runs `residue remove` (and for
# the removal records, `residue restore`), and checks what residue reports
# and what is left afterwards:
#
#   reported  the exit status; with --json, one residue.remove/v1 or
#             residue.restore/v1 document on stdout with the per-target
#             result, reason and failing check ids, and diagnostics on
#             stderr only
#   left      the worktree's file bytes, Git's worktree registration, the
#             branch and its commit, the unrelated worktree, no
#             .NAME.residue-removing-PID directory or trash entry, and the
#             removal records and their refs under refs/residue/removed/
#
#   bash scripts/contract-check.sh [BINARY]    (default: target/release/residue)
#
# Exit status: 0 every scenario passed, 1 a scenario failed, 2 the runner
# could not run (no binary, no jq, a terminal on stdin, or a fixture setup
# step failed). A setup failure is never reported as a product result.
#
# Needs bash, git and jq. Everything lives under one fresh directory with
# its own HOME and Git config, and only that directory is deleted. It never
# touches the caller's repositories, fetches nothing and starts no service.
# The prompt race is scripts/race-check.sh; the work-landed requirement is
# .github/watch-smoke.sh. This file does not repeat them.
set -uo pipefail

bin=${1:-target/release/residue}
case $bin in
/*) ;;
*) bin=$PWD/$bin ;;
esac
[ -x "$bin" ] || { echo "contract-check: no executable at $bin; run cargo build --release first" >&2; exit 2; }
command -v jq >/dev/null || { echo "contract-check: needs jq" >&2; exit 2; }
# NetBSD, OpenBSD and DragonFly: no worktree is a candidate (see the file).
# Every case that expects a removal or a prompt is skipped there by name;
# the kept-unknown cases check what happens instead, and each refusal must
# name the activity check as well as its own.
# shellcheck source=SCRIPTDIR/partial-probe.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/partial-probe.sh"

work=$(mktemp -d "${TMPDIR:-/tmp}/residue-contract.XXXXXX") || exit 2
case $work in
*/residue-contract.??????) ;;
*) echo "contract-check: unexpected scratch directory $work" >&2; exit 2 ;;
esac
trap 'rm -rf -- "$work"' EXIT

# A controlled environment for residue and Git.
export HOME=$work/home XDG_CONFIG_HOME=$work/home/.config XDG_STATE_HOME=$work/home/.local/state
export GIT_CONFIG_NOSYSTEM=1 NO_COLOR=1 LC_ALL=C
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_COMMON_DIR GIT_CONFIG GIT_CONFIG_PARAMETERS GIT_CONFIG_COUNT GIT_CONFIG_GLOBAL GIT_CONFIG_SYSTEM
mkdir -p "$HOME"
git config --global user.name contract-check
git config --global user.email contract-check@example.invalid
git config --global init.defaultBranch main

# No prompt can appear: stdin is not a terminal.
exec </dev/null
if [ -t 0 ]; then
    echo "contract-check: stdin is a terminal" >&2
    exit 2
fi

passes=0
failures=0
name=
out=
err=
status=

# A fixture step that fails stops the run with status 2.
setup() {
    "$@" >/dev/null 2>&1 || {
        echo "contract-check: setup failed in $name: $*" >&2
        exit 2
    }
}

fail() {
    echo "FAIL $name: $*"
    if [ -s "$out" ]; then
        echo "    stdout:"
        head -c 2000 "$out" | sed 's/^/    | /'
        echo
    fi
    if [ -s "$err" ]; then
        echo "    stderr:"
        head -c 2000 "$err" | sed 's/^/    | /'
    fi
    failures=$((failures + 1))
}

ok() {
    echo "ok   $name"
    passes=$((passes + 1))
}

# A repository with two linked worktrees: .worktrees/w on branch w (one
# commit of its own, a regenerable node_modules/), and .worktrees/u on
# branch u, which no scenario names and every scenario must leave alone.
make_repo() {
    repo=$work/$name
    setup git init -q "$repo"
    printf 'node_modules/\n.env\n' >"$repo/.gitignore"
    echo hello >"$repo/README.md"
    setup git -C "$repo" add .
    setup git -C "$repo" commit -qm init
    setup git -C "$repo" worktree add -q "$repo/.worktrees/w" -b w
    wt=$repo/.worktrees/w
    echo w >>"$wt/README.md"
    setup git -C "$wt" commit -qam w
    mkdir -p "$wt/node_modules" "$wt/sub"
    echo x >"$wt/node_modules/a.js"
    echo keep >"$wt/sub/file.txt"
    setup git -C "$wt" add sub/file.txt
    setup git -C "$wt" commit -qm sub
    setup git -C "$repo" worktree add -q "$repo/.worktrees/u" -b u
    w_sha=$(git -C "$repo" rev-parse w) || exit 2
    u_sum=$(sums "$repo/.worktrees/u") || exit 2
}

# File names and checksums under a directory, .git file excluded.
sums() {
    (cd "$1" && find . -type f ! -path ./.git -print | LC_ALL=C sort | while read -r f; do printf '%s ' "$f"; cksum <"$f"; done)
}

registered() { # PATH
    git -C "$repo" worktree list --porcelain | grep -qx "worktree $(cd "$1" 2>/dev/null && pwd -P)"
}

# Run residue with a 120-second deadline. Sets $status, $out, $err.
run() {
    out=$work/$name.out
    err=$work/$name.err
    "$bin" "$@" >"$out" 2>"$err" &
    local pid=$! i
    for ((i = 0; i < 1200; i++)); do
        kill -0 "$pid" 2>/dev/null || break
        sleep 0.1
    done
    if kill -0 "$pid" 2>/dev/null; then
        kill "$pid" 2>/dev/null
        wait "$pid" 2>/dev/null
        status=timeout
        return
    fi
    wait "$pid"
    status=$?
}

# stdout is exactly one residue.remove/v1 document with the fields every
# remove document has, of the right types, and N results.
json_ok() { # N
    jq -e -s --argjson n "$1" '
        length == 1 and (.[0] |
            .schema == "residue.remove/v1"
            and (.version | type) == "string"
            and (.generated_at | type) == "string"
            and (.results | type) == "array" and (.results | length) == $n
            and (.errors | type) == "array"
            and (.purged | type) == "array"
            and (.trash | type) == "object"
            and all(.results[]; (.path | type) == "string" and (.result | type) == "string"))
    ' "$out" >/dev/null
}

result() { # INDEX FIELD
    jq -r ".results[$1].$2 // empty" "$out"
}

failing_checks() { # INDEX: the ids of checks that did not pass, sorted, comma-separated
    jq -r "[.results[$1].worktree.checks[]? | select(.status != \"pass\") | .id] | sort | join(\",\")" "$out"
}

# The failing check ids to expect, sorted: CHECK-IDS, plus `activity` where
# the probe cannot show any worktree unused.
want_failing() { # CHECK-ID...
    { printf '%s\n' "$@"; [ -z "$partial_probe" ] || echo activity; } | LC_ALL=C sort | paste -s -d , -
}

# The activity answer for a worktree no process uses, on a partial probe:
# unknown, for the probe's reason, in the check and in `activity`.
partial_unknown() { # INDEX
    jq -e --argjson i "$1" --arg detail "$partial_probe_detail" '
        .results[$i].worktree as $w
        | $w.verdict == "unknown"
          and $w.activity.state == "unknown"
          and $w.activity.reason == "partial_probe"
          and ($w.activity.detail | endswith($detail))
          and ([$w.checks[] | select(.id == "activity") | .status] == ["unknown"])
    ' "$out" >/dev/null
}

# Nothing was removed or moved: w and u are registered with the same bytes,
# w's branch has its commit, and no residue leftovers exist.
untouched() { # BEFORE-SUMS-OF-W
    registered "$wt" || { fail "w is no longer registered"; return 1; }
    [ "$(sums "$wt")" = "$1" ] || { fail "w's files changed"; return 1; }
    common_ok || return 1
}

# u is untouched, branch w still has its commit, and nothing was left aside.
common_ok() {
    registered "$repo/.worktrees/u" || { fail "the unrelated worktree u is no longer registered"; return 1; }
    [ "$(sums "$repo/.worktrees/u")" = "$u_sum" ] || { fail "the unrelated worktree u changed"; return 1; }
    [ "$(git -C "$repo" rev-parse -q --verify w)" = "$w_sha" ] || { fail "branch w moved or is gone"; return 1; }
    local leftover
    for leftover in "$repo"/.worktrees/.*.residue-removing-*; do
        [ ! -e "$leftover" ] || { fail "$leftover was left behind"; return 1; }
    done
    if [ -n "$(ls -A "$repo/.git/residue-trash" 2>/dev/null)" ]; then
        fail "a trash entry was created"
        return 1
    fi
}

# --- scenarios -------------------------------------------------------------

# 1. Positive control: a clean worktree is removed, JSON and text. On a
# partial probe it is kept instead, as unknown for the probe's reason.
if [ -z "$partial_probe" ]; then
    name=control-json
    make_repo
    run remove --yes --json "$wt"
    if [ "$status" != 0 ]; then fail "exit $status, want 0"
    elif ! json_ok 1; then fail "stdout is not one valid remove document with 1 result"
    elif [ "$(result 0 result)" != removed ]; then fail "result $(result 0 result), want removed"
    elif [ "$(jq -r '.results[0].reason' "$out")" != null ]; then fail "reason is not null for a removal"
    elif [ -e "$wt" ]; then fail "the worktree is still on disk"
    elif registered "$wt"; then fail "Git still lists the removed worktree"
    elif [ -s "$err" ]; then fail "stderr is not empty"
    elif common_ok; then ok; fi

    name=control-text
    make_repo
    run remove --yes "$wt"
    if [ "$status" != 0 ]; then fail "exit $status, want 0"
    elif ! grep -q "^removed .*\.worktrees/w (branch w kept)" "$out"; then fail "no 'removed ... (branch w kept)' line"
    elif [ -e "$wt" ]; then fail "the worktree is still on disk"
    elif common_ok; then ok; fi
else
    skip_partial control-json
    skip_partial control-text

    name=kept-unknown-json
    make_repo
    before=$(sums "$wt")
    run remove --yes --json "$wt"
    if [ "$status" != 3 ]; then fail "exit $status, want 3"
    elif ! json_ok 1; then fail "stdout is not one valid remove document with 1 result"
    elif [ "$(result 0 result)" != refused ]; then fail "result $(result 0 result), want refused"
    elif [ "$(result 0 reason)" != "one or more checks did not pass" ]; then fail "reason: $(result 0 reason)"
    elif [ "$(failing_checks 0)" != activity ]; then fail "failing checks: $(failing_checks 0), want activity"
    elif ! partial_unknown 0; then fail "activity is not unknown (partial_probe): $(jq -c '.results[0].worktree.activity' "$out")"
    elif [ -s "$err" ]; then fail "stderr is not empty"
    elif untouched "$before"; then ok; fi

    name=kept-unknown-text
    make_repo
    before=$(sums "$wt")
    run remove --yes "$wt"
    if [ "$status" != 3 ]; then fail "exit $status, want 3"
    elif ! grep -q -E "worktrees/w +unknown: $partial_probe_short_re" "$out"; then fail "no '.worktrees/w  unknown: $partial_probe_short' line"
    elif untouched "$before"; then ok; fi
fi

# 2. Refusals, one cause each: exit 3, refused, the right failing check, and
# nothing changed. On a partial probe `activity` is unknown too.
refusal() { # NAME CHECK-ID CHANGE...
    name=$1
    local check
    check=$(want_failing "$2")
    shift 2
    make_repo
    (cd "$wt" && "$@") >/dev/null 2>&1 || { echo "contract-check: setup failed in $name: $*" >&2; exit 2; }
    local before
    before=$(sums "$wt")
    run remove --yes --json "$wt"
    if [ "$status" != 3 ]; then fail "exit $status, want 3"
    elif ! json_ok 1; then fail "stdout is not one valid remove document with 1 result"
    elif [ "$(result 0 result)" != refused ]; then fail "result $(result 0 result), want refused"
    elif [ "$(result 0 reason)" != "one or more checks did not pass" ]; then fail "reason: $(result 0 reason)"
    elif [ "$(failing_checks 0)" != "$check" ]; then fail "failing checks: $(failing_checks 0), want $check"
    elif untouched "$before"; then ok; fi
}
refusal refuse-staged tracked_changes sh -c 'echo staged >>README.md && git add README.md'
refusal refuse-modified tracked_changes sh -c 'echo edit >>README.md'
refusal refuse-untracked untracked sh -c 'echo notes >notes.md'
refusal refuse-ignored ignored sh -c 'echo TOKEN=1 >.env'
refusal refuse-locked locked git worktree lock --reason "resume later" .

# 3. Invalid targets: exit 3, invalid with the reason, nothing changed, and
# no fallback to another worktree.
invalid() { # NAME TARGET REASON-TEXT
    name=$1
    make_repo
    local target=$2 want=$3 before
    before=$(sums "$wt")
    case $target in
    MAIN) target=$repo ;;
    SUB) target=$wt/sub ;;
    MISSING) target=$repo/.worktrees/missing ;;
    esac
    run remove --yes --json "$target"
    if [ "$status" != 3 ]; then fail "exit $status, want 3"
    elif ! json_ok 1; then fail "stdout is not one valid remove document with 1 result"
    elif [ "$(result 0 result)" != invalid ]; then fail "result $(result 0 result), want invalid"
    elif ! result 0 reason | grep -q -e "$want"; then fail "reason without '$want': $(result 0 reason)"
    elif [ ! -d "$repo/.git" ] || [ ! -f "$repo/README.md" ]; then fail "the main checkout changed"
    elif untouched "$before"; then ok; fi
}
invalid invalid-main MAIN "is the main checkout of its repository; residue never removes it"
invalid invalid-subdirectory SUB "is inside the checkout .*; pass the worktree root itself"
invalid invalid-missing MISSING "No such file or directory"

# 4. No terminal and no --yes: skipped, nothing removed, in JSON and text.
# A partial probe leaves nothing to confirm, so there is no prompt to skip.
if [ -z "$partial_probe" ]; then
    name=no-terminal-json
    make_repo
    before=$(sums "$wt")
    run remove --json "$wt"
    if [ "$status" != 3 ]; then fail "exit $status, want 3"
    elif ! json_ok 1; then fail "stdout is not one valid remove document with 1 result"
    elif [ "$(result 0 result)" != skipped ]; then fail "result $(result 0 result), want skipped"
    elif ! result 0 reason | grep -q "stdin is not a terminal"; then fail "reason: $(result 0 reason)"
    elif untouched "$before"; then ok; fi

    name=no-terminal-text
    make_repo
    before=$(sums "$wt")
    run remove "$wt"
    if [ "$status" != 3 ]; then fail "exit $status, want 3"
    elif ! grep -q "^nothing removed: stdin is not a terminal" "$out"; then fail "no 'nothing removed: stdin is not a terminal' line"
    elif untouched "$before"; then ok; fi
else
    skip_partial no-terminal-json
    skip_partial no-terminal-text
fi

# 5. Several targets: one result per target, in order; only the clean one
# goes, and one refusal makes the run exit 3.
if [ -z "$partial_probe" ]; then
    name=mixed-targets
    make_repo
    echo notes >"$repo/.worktrees/u/notes.md"
    u_sum=$(sums "$repo/.worktrees/u")
    run remove --yes --json "$wt" "$repo/.worktrees/u"
    if [ "$status" != 3 ]; then fail "exit $status, want 3"
    elif ! json_ok 2; then fail "stdout is not one valid remove document with 2 results"
    elif [ "$(result 0 result),$(result 1 result)" != "removed,refused" ]; then fail "results $(result 0 result),$(result 1 result), want removed,refused"
    elif [ "$(failing_checks 1)" != untracked ]; then fail "u's failing checks: $(failing_checks 1), want untracked"
    elif [ -e "$wt" ]; then fail "w is still on disk"
    elif common_ok; then ok; fi
else
    skip_partial mixed-targets
fi

# 6. Diagnostics go to stderr, never into the JSON on stdout. On a partial
# probe the worktree is kept (refused, activity unknown) and the note and
# the document are checked the same way.
name=stderr-separate
make_repo
before=$(sums "$wt")
GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=residue.regenerable GIT_CONFIG_VALUE_0=.env run remove --yes --json "$wt"
if [ -z "$partial_probe" ]; then
    want_status=0 want_result=removed
else
    want_status=3 want_result=refused
fi
if [ "$status" != "$want_status" ]; then fail "exit $status, want $want_status"
elif ! grep -q "residue: note: ignored residue.regenerable set through the environment" "$err"; then fail "no note on stderr"
elif ! json_ok 1; then fail "stdout is not exactly one valid remove document"
elif [ "$(jq -r '.ignored_environment_rules | join(",")' "$out")" != .env ]; then fail "ignored_environment_rules: $(jq -c .ignored_environment_rules "$out")"
elif [ "$(result 0 result)" != "$want_result" ]; then fail "result $(result 0 result), want $want_result"
elif [ -z "$partial_probe" ]; then common_ok && ok
elif ! partial_unknown 0; then fail "activity is not unknown (partial_probe): $(jq -c '.results[0].worktree.activity' "$out")"
elif untouched "$before"; then ok; fi

# 7. Usage errors: exit 2, a message on stderr, nothing on stdout, nothing
# removed.
usage() { # NAME ARGS...
    name=$1
    shift
    make_repo
    local before
    before=$(sums "$wt")
    run "$@"
    if [ "$status" != 2 ]; then fail "exit $status, want 2"
    elif [ -s "$out" ]; then fail "stdout is not empty"
    elif ! grep -q "^residue: " "$err"; then fail "no 'residue: ' message on stderr"
    elif untouched "$before"; then ok; fi
}
usage usage-no-path remove --yes --json
usage usage-unknown-option remove --yes --bogus "$work/x"
usage usage-trash-and-no-trash remove --yes --trash --no-trash "$work/x"
usage usage-keep-removed-not-remove scan --keep-removed 1d "$work/x"
usage usage-removal-reason-unknown remove --yes --removal-reason sweep "$work/x"
usage usage-to-without-entry restore --to "$work/x"

# 8. Removal records: every removal writes one before it deletes, and
# `residue restore` recreates the worktree from it. A record that cannot be
# written stops the removal with nothing deleted. Every case needs a
# removal, so a partial probe skips them all.

# Removal records and their refs in $repo: "N records, M refs".
records() {
    local n m
    n=$(find "$repo/.git/residue-removed" -mindepth 1 -maxdepth 1 -type d 2>/dev/null | wc -l | tr -d ' ')
    m=$(git -C "$repo" for-each-ref refs/residue | wc -l | tr -d ' ')
    echo "$n records, $m refs"
}

# The files of w that Git tracks: what a restore from a record brings back.
tracked() {
    sums "$wt" | grep -v '^\./node_modules/'
}

# stdout is exactly one residue.restore/v1 document that restored an entry
# of KIND with CHECKOUT, and lists only that entry.
restore_ok() { # KIND CHECKOUT
    jq -e -s --arg kind "$1" --arg how "$2" '
        length == 1 and (.[0] |
            .schema == "residue.restore/v1"
            and (.version | type) == "string"
            and (.generated_at | type) == "string"
            and (.restored | type) == "string"
            and .restore.kind == $kind
            and .restore.checkout == $how
            and .restore.path == .restored
            and (.restore.not_kept | type) == "array"
            and (.restore.notes | type) == "array"
            and (.trash | type) == "array" and (.removed | type) == "array"
            and ((.trash | length) + (.removed | length)) == 1
            and .errors == [])
    ' "$out" >/dev/null
}

# Remove w with no prompt into a record; stops the run when it does not.
remove_w() {
    run remove --yes --json "$wt"
    if [ "$status" != 0 ] || [ "$(result 0 result)" != removed ] || [ "$(records)" != "1 records, 1 refs" ]; then
        echo "contract-check: setup failed in $name: remove exit $status, $(result 0 result), $(records)" >&2
        exit 2
    fi
}

# Restore w, after CHANGE... has moved its branch or its path: exit
# STATUS, and for 0 the restore document with CHECKOUT.
restore_case() { # NAME STATUS CHECKOUT CHANGE...
    name=$1
    local want=$2 how=$3
    shift 3
    make_repo
    remove_w
    (cd "$repo" && "$@") >/dev/null 2>&1 || { echo "contract-check: setup failed in $name: $*" >&2; exit 2; }
    run restore --json "$wt"
    if [ "$status" != "$want" ]; then fail "exit $status, want $want"; return 1; fi
    if [ "$want" = 0 ] && ! restore_ok removed "$how"; then fail "stdout is not one restore document with checkout $how"; return 1; fi
    if [ "$want" != 0 ] && [ -s "$out" ]; then fail "stdout is not empty"; return 1; fi
}

if [ -z "$partial_probe" ]; then
    name=record-json
    make_repo
    before=$(tracked)
    run remove --yes --json "$wt"
    record=$(result 0 removal_record)
    if [ "$status" != 0 ]; then fail "exit $status, want 0"
    elif [ "$(result 0 result)" != removed ]; then fail "result $(result 0 result), want removed"
    elif [ ! -f "$record/info" ]; then fail "removal_record '$record' has no info file"
    elif [ "$(jq -r '.removal_records.reason + " " + .removal_records.by' "$out")" != "remove remove" ]; then fail "removal_records: $(jq -c .removal_records "$out")"
    elif [ "$(records)" != "1 records, 1 refs" ]; then fail "$(records), want 1 records, 1 refs"
    elif [ "$(git -C "$repo" for-each-ref --format='%(objectname)' refs/residue/removed/)" != "$w_sha" ]; then fail "the ref does not pin w's HEAD"
    elif ! (cd "$repo" && "$bin" restore --json) >"$work/$name.list" 2>"$err"; then fail "restore --json (the list) failed"
    elif ! jq -e --arg sha "$w_sha" '
            .schema == "residue.restore/v1" and .restored == null and .restore == null
            and .trash == [] and (.removed | length) == 1
            and (.removed[0] | .kind == "removed" and .state == "removed" and .reason == "remove" and .by == "remove"
                 and .branch == "refs/heads/w" and .head == $sha
                 and (.original_path | endswith("/.worktrees/w"))
                 and (.ref | startswith("refs/residue/removed/"))
                 and ([.not_kept[].path] == ["node_modules/"]) and .error == null)
        ' "$work/$name.list" >/dev/null; then fail "the list: $(head -c 2000 "$work/$name.list")"
    else
        run restore --json "$wt"
        if [ "$status" != 0 ]; then fail "restore: exit $status, want 0"
        elif ! restore_ok removed checked_out_branch; then fail "stdout is not one restore document with checkout checked_out_branch"
        elif [ "$(jq -r '.restore.branch + " " + .restore.head' "$out")" != "refs/heads/w $w_sha" ]; then fail "restore branch and head: $(jq -c .restore "$out")"
        elif [ "$(jq -r '[.restore.not_kept[].path] | join(",")' "$out")" != node_modules/ ]; then fail "not_kept: $(jq -c .restore.not_kept "$out")"
        elif ! registered "$wt"; then fail "Git does not list the restored worktree"
        elif [ "$(git -C "$wt" symbolic-ref HEAD)" != refs/heads/w ]; then fail "the restored worktree is not on branch w"
        elif [ "$(tracked)" != "$before" ]; then fail "the restored files differ from w's"
        elif [ "$(records)" != "0 records, 0 refs" ]; then fail "$(records) after the restore, want 0 records, 0 refs"
        elif [ -s "$err" ]; then fail "stderr is not empty"
        elif common_ok; then ok; fi
    fi

    name=record-text
    make_repo
    run remove --yes "$wt"
    if [ "$status" != 0 ]; then fail "exit $status, want 0"
    elif ! grep -q "^removed .*\.worktrees/w (branch w kept); .*; restore: residue restore .*/residue-removed/w-" "$out"; then fail "no 'removed ...; restore: residue restore ...' line"
    elif ! (cd "$repo" && "$bin" restore) >"$work/$name.list" 2>"$err"; then fail "restore (the list) failed"
    elif ! grep -q "^  removed  w-.*\[w\]" "$work/$name.list"; then fail "the list has no 'removed  w-... [w]' row: $(cat "$work/$name.list")"
    else
        run restore "$wt"
        if [ "$status" != 0 ]; then fail "restore: exit $status, want 0"
        elif ! grep -q "^restored .*\.worktrees/w from the removal record w-" "$out"; then fail "no 'restored ... from the removal record' line"
        elif ! registered "$wt"; then fail "Git does not list the restored worktree"
        elif common_ok; then ok; fi
    fi

    # Fail closed: a file where the record or its ref goes stops the
    # removal: skipped (the result for a worktree moved back with nothing
    # deleted), exit 3, and the worktree is back in place with every byte.
    fail_closed() { # NAME IN-THE-WAY
        name=$1
        make_repo
        echo in-the-way >"$repo/.git/$2" || exit 2
        local before
        before=$(sums "$wt")
        run remove --yes --json "$wt"
        if [ "$status" != 3 ]; then fail "exit $status, want 3"
        elif ! json_ok 1; then fail "stdout is not one valid remove document with 1 result"
        elif [ "$(result 0 result)" != skipped ]; then fail "result $(result 0 result), want skipped"
        elif ! result 0 reason | grep -q "could not write its removal record, so residue deleted nothing"; then fail "reason: $(result 0 reason)"
        elif [ "$(jq -r '.results[0].removal_record' "$out")" != null ]; then fail "removal_record is not null"
        elif [ -n "$(git -C "$repo" for-each-ref refs/residue 2>/dev/null)" ]; then fail "a ref was left under refs/residue"
        elif [ "$(cat "$repo/.git/$2")" != in-the-way ]; then fail "the file in the way changed"
        elif untouched "$before"; then ok; fi
    }
    fail_closed record-fail-closed-dir residue-removed
    fail_closed record-fail-closed-ref refs/residue

    # Retention: a record older than --keep-removed is forgotten, with its
    # ref, by the next remove run in its repository; the new one stays.
    name=record-retention
    make_repo
    remove_w
    old=$(find "$repo/.git/residue-removed" -mindepth 1 -maxdepth 1 -type d)
    sed 's/^removed [0-9]* .*/removed 1767225600 2026-01-01T00:00:00Z/' "$old/info" >"$work/$name.info" && cat "$work/$name.info" >"$old/info" || exit 2
    setup git -C "$repo" worktree add -q "$repo/.worktrees/x" -b x
    run remove --yes --json --keep-removed 30d "$repo/.worktrees/x"
    if [ "$status" != 0 ]; then fail "exit $status, want 0; forgotten: $(jq -c "[.forgotten[] | {result, reason}]" "$out"); errors: $(jq -c .errors "$out")"
    elif [ "$(result 0 result)" != removed ]; then fail "result $(result 0 result), want removed"
    elif [ "$(jq -r '[.forgotten[] | .result + " " + .entry.entry] | join(",")' "$out")" != "forgotten $(basename "$old")" ]; then fail "forgotten: $(jq -c .forgotten "$out")"
    elif [ "$(jq -r '.removal_records.keep' "$out")" != 30d ]; then fail "removal_records: $(jq -c .removal_records "$out")"
    elif [ -e "$old" ]; then fail "the expired record is still on disk"
    elif [ "$(records)" != "1 records, 1 refs" ]; then fail "$(records), want 1 records, 1 refs (x's)"
    elif [ "$(git -C "$repo" for-each-ref --format='%(objectname)' refs/residue/removed/)" != "$(git -C "$repo" rev-parse x)" ]; then fail "the ref left is not x's"
    elif common_ok; then ok; fi

    # A commit only the worktree's own HEAD reflog holds: the removal pins
    # it under refs/residue/reflog/, `git gc --prune=now` keeps it, and the
    # restored worktree's reflog names it before the pin goes.
    name=record-reflog-pin
    make_repo
    setup git -C "$wt" checkout -q --detach
    setup git -C "$wt" commit -q --allow-empty -m lone
    lone=$(git -C "$wt" rev-parse HEAD) || exit 2
    setup git -C "$wt" checkout -q --detach "$w_sha"
    run remove --yes --json "$wt"
    entry=$(basename "$(result 0 removal_record)")
    if [ "$status" != 0 ]; then fail "exit $status, want 0"
    elif [ "$(result 0 result)" != removed ]; then fail "result $(result 0 result), want removed"
    elif [ "$(git -C "$repo" for-each-ref --format='%(refname) %(objectname)' refs/residue/reflog/)" != "refs/residue/reflog/$entry/1 $lone" ]; then fail "the reflog pin: $(git -C "$repo" for-each-ref refs/residue/reflog/)"
    elif ! git -C "$repo" gc -q --prune=now >/dev/null 2>&1; then fail "git gc failed"
    elif [ "$(git -C "$repo" cat-file -t "$lone" 2>/dev/null)" != commit ]; then fail "gc lost the commit only the reflog held"
    elif ! (cd "$repo" && "$bin" restore --json) >"$work/$name.list" 2>"$err"; then fail "restore --json (the list) failed"
    elif ! jq -e --arg lone "$lone" --arg ref "refs/residue/reflog/$entry/1" '
            .removed[0] | .head_reflog == true and .incomplete == false
            and .reflog_refs == [{"ref": $ref, "commit": $lone}]
        ' "$work/$name.list" >/dev/null; then fail "the list: $(jq -c '.removed[0] | {head_reflog, reflog_refs, incomplete}' "$work/$name.list")"
    else
        run restore --json "$wt"
        if [ "$status" != 0 ]; then fail "restore: exit $status, want 0"
        elif ! restore_ok removed detached; then fail "stdout is not one restore document with checkout detached"
        elif ! grep -qx "$lone" <<<"$(git -C "$wt" reflog --format=%H)"; then fail "the restored worktree's reflog does not name the lone commit"
        elif [ "$(records)" != "0 records, 0 refs" ]; then fail "$(records) after the restore, want 0 records, 0 refs"
        elif [ -s "$err" ]; then fail "stderr is not empty"
        elif common_ok; then ok; fi
    fi

    # A record residue cannot read or forget never fails a removal: the
    # PATH is removed, exit 0, and the problem is in `errors` and on
    # stderr. An entry with no info (a run stopped while writing it) and no
    # ref, an hour old, is cleared; a fresh one is left alone.
    name=record-unreadable
    make_repo
    remove_w
    rr=$repo/.git/residue-removed
    mkdir "$rr/bad-20260101T000000Z" "$rr/half-20260101T000000Z" "$rr/fresh-20260101T000000Z" || exit 2
    printf 'path /x\n' >"$rr/bad-20260101T000000Z/info" || exit 2
    echo partial >"$rr/half-20260101T000000Z/info.new" || exit 2
    touch -t 202601010000 "$rr/half-20260101T000000Z" || exit 2
    setup git -C "$repo" worktree add -q "$repo/.worktrees/x" -b x
    run remove --yes --json "$repo/.worktrees/x"
    if [ "$status" != 0 ]; then fail "exit $status, want 0"
    elif [ "$(result 0 result)" != removed ]; then fail "result $(result 0 result), want removed"
    elif ! jq -e '.errors | length == 1 and (.[0] | contains("bad-20260101T000000Z"))' "$out" >/dev/null; then fail "errors: $(jq -c .errors "$out")"
    elif ! grep -q '^residue: warning: removal record: .*bad-20260101T000000Z' "$err"; then fail "no warning on stderr"
    elif [ "$(jq -r '[.forgotten[] | .result + " " + .entry.entry] | join(",")' "$out")" != "forgotten half-20260101T000000Z" ]; then fail "forgotten: $(jq -c '[.forgotten[] | {result, reason}]' "$out")"
    elif [ -e "$rr/half-20260101T000000Z" ]; then fail "the hour-old entry with no info is still there"
    elif [ ! -d "$rr/fresh-20260101T000000Z" ] || [ ! -f "$rr/bad-20260101T000000Z/info" ]; then fail "a fresh or unreadable entry was cleared"
    elif [ "$(records)" != "4 records, 2 refs" ]; then fail "$(records), want 4 records, 2 refs"
    elif common_ok; then ok; fi

    # Branch and path cases: restore never moves an existing branch and
    # never overwrites a path.
    ahead() {
        local tip
        tip=$(git commit-tree -p "$w_sha" -m ahead "$w_sha^{tree}") && git update-ref refs/heads/w "$tip" "$w_sha"
    }
    if restore_case restore-branch-ahead 0 checked_out_branch ahead; then
        w_sha=$(git -C "$repo" rev-parse w)
        if [ "$(jq -r .restore.branch_tip "$out")" != "$w_sha" ]; then fail "branch_tip $(jq -r .restore.branch_tip "$out"), want $w_sha"
        elif [ "$(git -C "$wt" rev-parse HEAD)" != "$w_sha" ]; then fail "the restored worktree is not at w's new tip"
        elif common_ok; then ok; fi
    fi

    if restore_case restore-branch-rewritten 0 new_branch git branch -f w main; then
        recorded=$(jq -r .restore.head "$out")
        if [ "$(jq -r .restore.new_branch "$out")" != w-restored ]; then fail "new_branch $(jq -r .restore.new_branch "$out"), want w-restored"
        elif [ "$(git -C "$wt" symbolic-ref HEAD)" != refs/heads/w-restored ]; then fail "the restored worktree is not on w-restored"
        elif [ "$(git -C "$repo" rev-parse w-restored)" != "$recorded" ]; then fail "w-restored is not at the recorded HEAD"
        elif [ "$(git -C "$repo" rev-parse w)" != "$(git -C "$repo" rev-parse main)" ]; then fail "restore moved branch w"
        elif w_sha=$(git -C "$repo" rev-parse w) && common_ok; then ok; fi
    fi

    if restore_case restore-branch-gone 0 recreated_branch git branch -D w; then
        if [ "$(git -C "$repo" rev-parse -q --verify w)" != "$w_sha" ]; then fail "branch w is not back at its recorded HEAD"
        elif [ "$(git -C "$wt" symbolic-ref HEAD)" != refs/heads/w ]; then fail "the restored worktree is not on branch w"
        elif common_ok; then ok; fi
    fi

    if restore_case restore-branch-elsewhere 1 - git worktree add -q .worktrees/y w; then
        if ! grep -q "branch w is checked out .*worktrees/y" "$err"; then fail "the refusal does not name .worktrees/y"
        elif [ -e "$wt" ]; then fail "something was created at w's path"
        elif [ "$(records)" != "1 records, 1 refs" ]; then fail "$(records) after the refusal, want 1 records, 1 refs"
        elif common_ok; then ok; fi
    fi

    occupied() { mkdir -p .worktrees/w && echo mine >.worktrees/w/notes.md; }
    if restore_case restore-path-occupied 1 - occupied; then
        if ! grep -q "exists; move it away first, or restore somewhere else with --to PATH" "$err"; then fail "the refusal does not say how to go on"
        elif [ "$(cat "$wt/notes.md")" != mine ] || [ "$(ls -A "$wt")" != notes.md ]; then fail "the directory at w's path changed"
        elif [ "$(records)" != "1 records, 1 refs" ]; then fail "$(records) after the refusal, want 1 records, 1 refs"
        else
            run restore --json --to "$repo/.worktrees/w2" "$wt"
            if [ "$status" != 0 ]; then fail "restore --to: exit $status, want 0"
            elif ! restore_ok removed checked_out_branch; then fail "restore --to: not one restore document"
            elif ! registered "$repo/.worktrees/w2"; then fail "Git does not list .worktrees/w2"
            elif [ "$(cat "$wt/notes.md")" != mine ]; then fail "restore --to changed the directory at w's path"
            elif [ "$(records)" != "0 records, 0 refs" ]; then fail "$(records) after the restore, want 0 records, 0 refs"
            elif common_ok; then ok; fi
        fi
    fi

    detach() { git -C .worktrees/w checkout -q --detach; }
    name=restore-detached
    make_repo
    (cd "$repo" && detach) || exit 2
    remove_w
    run restore --json "$wt"
    if [ "$status" != 0 ]; then fail "exit $status, want 0"
    elif ! restore_ok removed detached; then fail "stdout is not one restore document with checkout detached"
    elif git -C "$wt" symbolic-ref -q HEAD >/dev/null; then fail "the restored worktree is on a branch"
    elif [ "$(git -C "$wt" rev-parse HEAD)" != "$w_sha" ]; then fail "the restored worktree is not at the recorded HEAD"
    elif common_ok; then ok; fi
else
    for c in record-json record-text record-fail-closed-dir record-fail-closed-ref record-retention \
        record-reflog-pin record-unreadable \
        restore-branch-ahead restore-branch-rewritten restore-branch-gone restore-branch-elsewhere \
        restore-path-occupied restore-detached; do
        skip_partial "$c"
    done
fi

echo "contract-check: $passes passed, $failures failed"
[ "$failures" -eq 0 ]
