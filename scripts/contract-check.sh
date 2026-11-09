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
# Each sleep a scenario starts to hold a worktree is listed in $holders, so
# an early exit stops it too. Only a holder still running as a job of this
# shell is signalled: once bash has reaped one, by `wait` or on its own, its
# PID may already belong to another process.
holders=
stop_holders() {
    local pid running
    running=" $(jobs -pr | tr '\n' ' ') "
    for pid in $holders; do
        case $running in *" $pid "*) kill "$pid" 2>/dev/null ;; esac
    done
}
trap 'stop_holders; rm -rf -- "$work"' EXIT

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
# A fixture step: on failure, stop with exit 2 and show what it printed,
# so a run that went wrong in CI can be read from its log.
setup() {
    local said st
    said=$("$@" 2>&1) && return 0
    st=$?
    echo "contract-check: setup failed in $name (exit $st): $*" >&2
    [ -n "$said" ] && head -n 40 <<<"$said" | sed 's/^/    | /' >&2
    exit 2
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
# With ref_format set, Git stores its refs that way (`--ref-format`). DIR,
# when given, is where the repository goes instead of $work/NAME.
make_repo() { # [DIR]
    repo=${1:-$work/$name}
    setup git init -q ${ref_format:+--ref-format="$ref_format"} "$repo"
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

# -z and -F: Git quotes a path with a backslash or a control character in
# the list without -z, and the path is matched as text, not a pattern.
registered() { # PATH
    git -C "$repo" worktree list --porcelain -z | tr '\0' '\n' | grep -qxF "worktree $(cd "$1" 2>/dev/null && pwd -P)"
}

# Run the rest of the arguments in DIR, then go back: `in_dir DIR run ...`
# keeps the variables `run` sets.
in_dir() { # DIR COMMAND...
    pushd "$1" >/dev/null || exit 2
    shift
    "$@"
    popd >/dev/null || exit 2
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
            and (.as_of | type) == "string"
            and (.results | type) == "array" and (.results | length) == $n
            and (.errors | type) == "array"
            and (.warnings | type) == "array"
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
usage usage-as-of-not-remove scan --as-of 2026-11-09T00:00:00Z "$work/x"
usage usage-as-of-invalid remove --yes --as-of yesterday "$work/x"
some_plan=0123456789abcdef0123456789abcdef
usage usage-expect-without-yes remove --json --expect "$some_plan" "$work/x"
usage usage-expect-not-remove scan --expect "$some_plan" "$work/x"
usage usage-expect-twice remove --yes --expect "$some_plan" --expect "$some_plan" "$work/x"
usage usage-expect-uppercase remove --yes --expect 0123456789ABCDEF0123456789ABCDEF "$work/x"
usage usage-expect-short remove --yes --expect 0123456789abcdef "$work/x"
usage usage-expect-no-value remove --yes "$work/x" --expect

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
    if [ "$status" != 0 ]; then fail "exit $status, want 0; forgotten: $(jq -c "[.forgotten[] | {result, reason}]" "$out"); errors: $(jq -c .errors "$out"); warnings: $(jq -c .warnings "$out")"
    elif [ "$(result 0 result)" != removed ]; then fail "result $(result 0 result), want removed"
    elif [ "$(jq -r '[.forgotten[] | .result + " " + .entry.entry] | join(",")' "$out")" != "forgotten $(basename "$old")" ]; then fail "forgotten: $(jq -c .forgotten "$out")"
    elif [ "$(jq -r '.removal_records.keep' "$out")" != 30d ]; then fail "removal_records: $(jq -c .removal_records "$out")"
    elif [ -e "$old" ]; then fail "the expired record is still on disk"
    elif [ "$(records)" != "1 records, 1 refs" ]; then fail "$(records), want 1 records, 1 refs (x's)"
    elif [ "$(git -C "$repo" for-each-ref --format='%(objectname)' refs/residue/removed/)" != "$(git -C "$repo" rev-parse x)" ]; then fail "the ref left is not x's"
    elif common_ok; then ok; fi

    # Make the removal record DIR say it was written DAYS days ago.
    age_record() { # DIR DAYS
        local then
        then=$(($(date +%s) - $2 * 86400))
        sed "s/^removed [0-9]* /removed $then /" "$1/info" >"$work/$name.info" && cat "$work/$name.info" >"$1/info" || exit 2
    }

    # Remove w with --keep-removed KEEP, for a record to age.
    remove_w_keeping() { # KEEP
        run remove --yes --json --keep-removed "$1" "$wt"
        if [ "$status" != 0 ] || [ "$(result 0 result)" != removed ] || [ "$(records)" != "1 records, 1 refs" ]; then
            echo "contract-check: setup failed in $name: remove exit $status, $(result 0 result), $(records)" >&2
            exit 2
        fi
    }

    # A record keeps the --keep-removed it was written with: written with
    # 12w and 31 days old, a plain remove (30d) of another worktree in its
    # repository leaves it, and restore --json shows each record's keep;
    # 85 days old, the next plain remove forgets it.
    name=record-keep-own
    make_repo
    remove_w_keeping 12w
    old=$(find "$repo/.git/residue-removed" -mindepth 1 -maxdepth 1 -type d)
    age_record "$old" 31
    setup git -C "$repo" worktree add -q "$repo/.worktrees/x" -b x
    run remove --yes --json "$repo/.worktrees/x"
    if [ "$status" != 0 ]; then fail "exit $status, want 0"
    elif [ "$(jq -c '[.forgotten[] | .result + " " + .entry.entry]' "$out")" != "[]" ]; then fail "a plain remove forgot a 31-day-old record written with --keep-removed 12w: $(jq -c '[.forgotten[] | .result + " " + .entry.entry]' "$out")"
    elif [ ! -e "$old" ]; then fail "the 12w record is gone after 31 days"
    elif [ "$(records)" != "2 records, 2 refs" ]; then fail "$(records), want 2 records, 2 refs (w's and x's)"
    elif ! (cd "$repo" && "$bin" restore --json) >"$work/$name.list" 2>"$err"; then fail "restore --json (the list) failed"
    elif ! jq -e --arg old "$(basename "$old")" '
            ([.removed[] | select(.entry == $old)][0] | .keep == "12w" and .keep_seconds == 7257600)
            and ([.removed[] | select(.entry != $old)][0] | .keep == "30d" and .keep_seconds == 2592000)
        ' "$work/$name.list" >/dev/null; then fail "restore --json keep: $(jq -c '[.removed[] | {entry, keep, keep_seconds}]' "$work/$name.list")"
    else
        age_record "$old" 85
        setup git -C "$repo" worktree add -q "$repo/.worktrees/y" -b y
        run remove --yes --json "$repo/.worktrees/y"
        if [ "$status" != 0 ]; then fail "past 12w: exit $status, want 0"
        elif [ "$(jq -r '[.forgotten[] | .result + " " + .entry.entry] | join(",")' "$out")" != "forgotten $(basename "$old")" ]; then fail "past 12w, forgotten: $(jq -c .forgotten "$out")"
        elif [ -e "$old" ]; then fail "the 12w record is still on disk after 85 days"
        elif [ "$(records)" != "2 records, 2 refs" ]; then fail "$(records) past 12w, want 2 records, 2 refs (x's and y's)"
        elif common_ok; then ok; fi
    fi

    # `watch --keep-removed` reaches its removal child: a record written
    # with --keep-removed 1d and two days old, which the child's default
    # 30d would keep, is forgotten by a `watch` removal in its repository
    # with --keep-removed 1d, and the record that removal writes keeps 1d.
    # The removal is disk pressure (--min-free 99.9999) of x, which last
    # changed on 2026-01-01; u changed just now, so --idle 1m keeps it.
    name=watch-keep-removed
    make_repo
    remove_w_keeping 1d
    old=$(find "$repo/.git/residue-removed" -mindepth 1 -maxdepth 1 -type d)
    age_record "$old" 2
    GIT_COMMITTER_DATE=2026-01-01T00:00:00Z GIT_AUTHOR_DATE=2026-01-01T00:00:00Z \
        setup git -C "$repo" worktree add -q "$repo/.worktrees/x" -b x
    setup find "$repo/.worktrees/x" "$repo/.git/worktrees/x" -exec touch -t 202601010000 {} +
    run watch --once --no-fetch --min-free 99.9999 --idle 1m --keep-removed 1d "$repo"
    if [ "$status" != 0 ]; then fail "exit $status, want 0"
    elif ! grep -qF "residue watch: removed $(cd "$repo" && pwd -P)/.worktrees/x " "$err"; then fail "the log does not say it removed x"
    elif ! grep -q "residue watch: forgot the removal record $(basename "$old") " "$err"; then fail "the log does not say it forgot the two-day-old record"
    elif [ -e "$old" ]; then fail "the two-day-old record is still on disk: the child kept the default --keep-removed"
    elif registered "$repo/.worktrees/x"; then fail "x is still registered"
    elif [ "$(records)" != "1 records, 1 refs" ]; then fail "$(records), want 1 records, 1 refs (x's)"
    elif ! grep -qx "by watch" "$repo"/.git/residue-removed/*/info; then fail "the record left does not say by watch"
    elif ! grep -qx "keep 86400 1d" "$repo"/.git/residue-removed/*/info; then fail "the record left does not keep 1d: $(grep '^keep' "$repo"/.git/residue-removed/*/info)"
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

    # A commit both the worktree's HEAD reflog and its branch's reflog name:
    # the one before an amend. The branch's reflog goes with `git branch
    # -D`, the usual step once a branch landed, so the removal pins that
    # commit too, and the restored worktree's reflog names it again.
    name=record-reflog-branch
    make_repo
    echo b >>"$wt/README.md"
    setup git -C "$wt" commit -qam b
    before_amend=$(git -C "$wt" rev-parse HEAD) || exit 2
    setup git -C "$wt" commit -q --amend -m b2
    w_sha=$(git -C "$repo" rev-parse w) || exit 2
    run remove --yes --json "$wt"
    entry=$(basename "$(result 0 removal_record)")
    if [ "$status" != 0 ]; then fail "exit $status, want 0"
    elif [ "$(result 0 result)" != removed ]; then fail "result $(result 0 result), want removed"
    elif [ "$(git -C "$repo" for-each-ref --format='%(refname) %(objectname)' refs/residue/reflog/)" != "refs/residue/reflog/$entry/1 $before_amend" ]; then fail "the reflog pin: $(git -C "$repo" for-each-ref refs/residue/reflog/)"
    elif ! git -C "$repo" branch -q -D w >/dev/null 2>&1; then fail "git branch -D w failed"
    elif ! git -C "$repo" gc -q --prune=now >/dev/null 2>&1; then fail "git gc failed"
    elif [ "$(git -C "$repo" cat-file -t "$before_amend" 2>/dev/null)" != commit ]; then fail "gc lost the commit before the amend once branch w was deleted"
    else
        run restore --json "$wt"
        if [ "$status" != 0 ]; then fail "restore: exit $status, want 0"
        elif ! restore_ok removed recreated_branch; then fail "stdout is not one restore document with checkout recreated_branch"
        elif ! grep -qx "$before_amend" <<<"$(git -C "$wt" reflog --format=%H)"; then fail "the restored worktree's reflog does not name the commit before the amend"
        elif [ "$(records)" != "0 records, 0 refs" ]; then fail "$(records) after the restore, want 0 records, 0 refs"
        elif [ -s "$err" ]; then fail "stderr is not empty"
        elif common_ok; then ok; fi
    fi

    # Reftable repositories (Git 2.45 and later) have no logs/HEAD file to
    # copy: Git reads the worktree's reflog for residue. Restore cannot
    # write that reflog back, so after it the record and its two refs stay,
    # and the restore says so in its notes, on stderr and in the JSON.
    reftable_restore_ok() { # CHECKOUT
        if [ "$status" != 0 ]; then fail "restore: exit $status, want 0"
        elif ! restore_ok removed "$1"; then fail "stdout is not one restore document with checkout $1"
        elif [ "$(records)" != "1 records, 2 refs" ]; then fail "$(records) after the restore, want 1 records, 2 refs"
        elif ! jq -e '.restore.notes | any(startswith("its HEAD reflog was not put back: in a reftable repository"))' "$out" >/dev/null; then fail "the notes do not say the HEAD reflog was not put back: $(jq -c .restore.notes "$out")"
        elif ! grep -q '^residue: note: its HEAD reflog was not put back' "$err"; then fail "stderr does not say the HEAD reflog was not put back"
        elif common_ok; then ok; fi
    }
    # The removal pinned COMMIT, alone, and `git gc --prune=now` keeps it.
    reftable_pin_ok() { # COMMIT
        entry=$(basename "$(result 0 removal_record)")
        if [ "$status" != 0 ]; then fail "exit $status, want 0"
        elif [ "$(result 0 result)" != removed ]; then fail "result $(result 0 result), want removed"
        elif [ -e "$repo/.git/residue-removed/$entry/HEAD.reflog" ]; then fail "a HEAD.reflog was saved from a reftable repository"
        elif [ "$(git -C "$repo" for-each-ref --format='%(refname) %(objectname)' refs/residue/reflog/)" != "refs/residue/reflog/$entry/1 $1" ]; then fail "the reflog pin, want refs/residue/reflog/$entry/1 $1: $(git -C "$repo" for-each-ref refs/residue/reflog/)"
        elif ! git -C "$repo" gc -q --prune=now >/dev/null 2>&1; then fail "git gc failed"
        elif [ "$(git -C "$repo" cat-file -t "$1" 2>/dev/null)" != commit ]; then fail "gc lost the commit only the reflog held"
        else return 0; fi
        return 1
    }
    # A fixture whose Git does not leave the gap the case needs stops the
    # run: COMMIT must be no entry's new value in w's HEAD reflog.
    reftable_gap() { # COMMIT
        if git -C "$wt" rev-list --walk-reflogs HEAD | grep -qx "$1"; then
            echo "contract-check: setup failed in $name: an entry of the HEAD reflog still names $1 as its new value" >&2
            exit 2
        fi
    }
    if ! git init -q --ref-format=reftable "$work/reftable.probe" >/dev/null 2>&1; then
        for c in record-reflog-reftable record-reflog-reftable-expired record-reflog-reftable-deleted record-reflog-reftable-stray; do
            echo "skip $c: this Git cannot make a reftable repository (Git 2.45 and later)"
        done
        echo "skip record-reflog-migrated: this Git has no git refs migrate (Git 2.46 and later)"
    else
        # The lone commit of record-reflog-pin.
        name=record-reflog-reftable
        ref_format=reftable make_repo
        setup git -C "$wt" checkout -q --detach
        setup git -C "$wt" commit -q --allow-empty -m lone
        lone=$(git -C "$wt" rev-parse HEAD) || exit 2
        setup git -C "$wt" checkout -q --detach "$w_sha"
        run remove --yes --json "$wt"
        if reftable_pin_ok "$lone"; then
            run restore --json "$wt"
            reftable_restore_ok detached
        fi

        # The gap `git gc` leaves when it expires reflog entries. The entry
        # that made the lone commit is 60 days old, older than either
        # expiry period, and the one that left it 10 days, newer than
        # either; so on Git's documented defaults (90 days, 30 for a
        # commit no ref reaches) and on the swapped ones of Git 2.50 to
        # 2.56 alike, only the old value of the newer entry names the
        # commit. The removal pins it from the commits the worktree's
        # reflogs name that the main worktree's do not.
        name=record-reflog-reftable-expired
        ref_format=reftable make_repo
        now=$(git -C "$repo" log -1 --format=%ct) || exit 2
        setup git -C "$wt" checkout -q --detach
        setup env GIT_COMMITTER_DATE="$((now - 60 * 86400)) +0000" git -C "$wt" commit -q --allow-empty -m lone
        lone=$(git -C "$wt" rev-parse HEAD) || exit 2
        setup env GIT_COMMITTER_DATE="$((now - 10 * 86400)) +0000" git -C "$wt" checkout -q --detach "$w_sha"
        setup git -C "$repo" gc -q
        reftable_gap "$lone"
        run remove --yes --json "$wt"
        if reftable_pin_ok "$lone"; then
            run restore --json "$wt"
            reftable_restore_ok detached
        fi

        # `git reflog delete` of every entry before an amend leaves the
        # commit before it as the oldest entry's old value, which Git
        # reads as HEAD@{N}: the state `git gc` leaves on Git 2.50 to 2.56,
        # which expire every entry older than 30 days. Branch w's reflog
        # names that commit too, so it is not the worktree's alone, but
        # `git branch -D w` deletes that reflog, and the pin keeps it.
        name=record-reflog-reftable-deleted
        ref_format=reftable make_repo
        echo b >>"$wt/README.md"
        setup git -C "$wt" commit -qam b
        before_amend=$(git -C "$wt" rev-parse HEAD) || exit 2
        setup git -C "$wt" commit -q --amend -m b2
        w_sha=$(git -C "$repo" rev-parse w) || exit 2
        n=$(git -C "$wt" rev-list --walk-reflogs HEAD | wc -l) || exit 2
        for ((i = n - 1; i >= 1; i--)); do
            setup git -C "$wt" reflog delete "HEAD@{$i}"
        done
        reftable_gap "$before_amend"
        run remove --yes --json "$wt"
        if reftable_pin_ok "$before_amend"; then
            if ! git -C "$repo" branch -q -D w >/dev/null 2>&1; then fail "git branch -D w failed"
            elif ! git -C "$repo" gc -q --prune=now >/dev/null 2>&1; then fail "git gc failed"
            elif [ "$(git -C "$repo" cat-file -t "$before_amend" 2>/dev/null)" != commit ]; then fail "gc lost the commit before the amend once branch w was deleted"
            else
                run restore --json "$wt"
                reftable_restore_ok recreated_branch
            fi
        fi

        # A logs/HEAD file in a reftable repository's worktree, which Git
        # does not read: the restore of residue 0.0.7 wrote one there after
        # a `git refs migrate`. The file names only w's own commit; Git's
        # reflog names the lone commit of record-reflog-pin. The removal
        # neither copies the file nor reads it, so it pins the lone commit.
        name=record-reflog-reftable-stray
        ref_format=reftable make_repo
        setup git -C "$wt" checkout -q --detach
        setup git -C "$wt" commit -q --allow-empty -m lone
        lone=$(git -C "$wt" rev-parse HEAD) || exit 2
        setup git -C "$wt" checkout -q --detach "$w_sha"
        w_git=$(git -C "$wt" rev-parse --absolute-git-dir) || exit 2
        setup mkdir -p "$w_git/logs"
        printf '%s %s C O Mitter <committer@example.com> 1700000000 +0000\tcheckout: a file Git does not read\n' \
            0000000000000000000000000000000000000000 "$w_sha" >"$w_git/logs/HEAD" || exit 2
        run remove --yes --json "$wt"
        if reftable_pin_ok "$lone"; then
            run restore --json "$wt"
            reftable_restore_ok detached
        fi

        # `git refs migrate` (Git 2.46 and later) between the removal and
        # the restore. Removed while the repository kept its refs in
        # files, w leaves a copy of its logs/HEAD and a pin on the lone
        # commit of record-reflog-pin. Once the refs are in reftable, Git
        # reads no logs/HEAD file, so the restore writes none, says the
        # reflog was not put back and keeps the record and both refs, and
        # `git gc --prune=now` keeps the lone commit. The other way,
        # removed in reftable and restored once the refs are in files
        # again, the recreated worktree's reflog does not name the lone
        # commit either, so that record stays too. `git refs migrate`
        # refuses a repository with linked worktrees, so u goes out for
        # the migration and comes back.
        name=record-reflog-migrated
        migrate() { # FORMAT
            setup git -C "$repo" worktree remove "$repo/.worktrees/u"
            setup git -C "$repo" refs migrate --ref-format="$1"
            setup git -C "$repo" worktree add -q "$repo/.worktrees/u" u
        }
        # Remove w after the lone commit; stops the run when the removal
        # does not pin it.
        remove_lone() {
            setup git -C "$wt" checkout -q --detach
            setup git -C "$wt" commit -q --allow-empty -m lone
            lone=$(git -C "$wt" rev-parse HEAD) || exit 2
            setup git -C "$wt" checkout -q --detach "$w_sha"
            run remove --yes --json "$wt"
            entry=$(basename "$(result 0 removal_record)")
            if [ "$status" != 0 ] || [ "$(result 0 result)" != removed ] \
                || [ "$(git -C "$repo" for-each-ref --format='%(objectname)' "refs/residue/reflog/$entry/")" != "$lone" ]; then
                echo "contract-check: setup failed in $name: remove exit $status, $(result 0 result), $(records), no pin on $lone" >&2
                exit 2
            fi
        }
        probe=$work/migrate.probe
        if ! { git init -q "$probe" && git -C "$probe" commit -q --allow-empty -m probe \
            && git -C "$probe" refs migrate --ref-format=reftable; } >/dev/null 2>&1; then
            echo "skip $name: this Git has no git refs migrate (Git 2.46 and later)"
        else
            make_repo
            remove_lone
            migrate reftable
            run restore --json "$wt"
            if [ "$status" != 0 ]; then fail "restore after the migration to reftable: exit $status, want 0"
            elif ! restore_ok removed detached; then fail "stdout is not one restore document with checkout detached"
            elif ! git -C "$repo" gc -q --prune=now >/dev/null 2>&1; then fail "git gc failed"
            elif [ "$(git -C "$repo" cat-file -t "$lone" 2>/dev/null)" != commit ]; then fail "gc lost the commit only the reflog held, restored after the migration to reftable ($(records) left)"
            elif [ "$(records)" != "1 records, 2 refs" ]; then fail "$(records) after the restore in reftable, want 1 records, 2 refs"
            elif ! jq -e '.restore.notes | any(startswith("its HEAD reflog was not put back: in a reftable repository"))' "$out" >/dev/null; then fail "the notes do not say the HEAD reflog was not put back: $(jq -c .restore.notes "$out")"
            elif ! grep -q '^residue: note: its HEAD reflog was not put back' "$err"; then fail "stderr does not say the HEAD reflog was not put back"
            elif [ -e "$repo/.git/worktrees/w/logs/HEAD" ]; then fail "the restore wrote a logs/HEAD file, which Git does not read in a reftable repository"
            elif common_ok; then
                ref_format=reftable make_repo "$work/$name-files"
                remove_lone
                migrate files
                run restore --json "$wt"
                if [ "$status" != 0 ]; then fail "restore after the migration back to files: exit $status, want 0"
                elif ! restore_ok removed detached; then fail "stdout is not one restore document with checkout detached, back in files"
                elif ! git -C "$repo" gc -q --prune=now >/dev/null 2>&1; then fail "git gc failed, back in files"
                elif [ "$(git -C "$repo" cat-file -t "$lone" 2>/dev/null)" != commit ]; then fail "gc lost the commit only the reflog held, restored after the migration back to files ($(records) left)"
                elif [ "$(records)" != "1 records, 2 refs" ]; then fail "$(records) after the restore in files, want 1 records, 2 refs"
                elif ! jq -e '.restore.notes | any(startswith("its reflog does not name 1 of the 1 commits only the old reflog held"))' "$out" >/dev/null; then fail "the notes do not say the reflog lacks the lone commit: $(jq -c .restore.notes "$out")"
                elif common_ok; then ok; fi
            fi
        fi
    fi

    # A record residue cannot read or forget never fails a removal: the
    # PATH is removed, exit 0, and the problem is a warning, in `warnings`
    # and on stderr, never in `errors`, which keeps meaning exit 3. An
    # entry with no info (a run stopped while writing it) and no ref, an
    # hour old, is cleared; a fresh one is left alone.
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
    elif [ "$(jq -c .errors "$out")" != "[]" ]; then fail "errors: $(jq -c .errors "$out"), want []: a record problem is a warning"
    elif ! jq -e '.warnings | length == 1 and (.[0] | contains("bad-20260101T000000Z"))' "$out" >/dev/null; then fail "warnings: $(jq -c .warnings "$out")"
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

    # Branch w checked out in another worktree: restore leaves it and that
    # worktree alone, and brings w back on w-restored at the recorded HEAD.
    if restore_case restore-branch-elsewhere 0 new_branch git worktree add -q .worktrees/y w; then
        recorded=$(jq -r .restore.head "$out")
        if [ "$(jq -r .restore.new_branch "$out")" != w-restored ]; then fail "new_branch $(jq -r .restore.new_branch "$out"), want w-restored"
        elif [ "$(git -C "$wt" symbolic-ref HEAD)" != refs/heads/w-restored ]; then fail "the restored worktree is not on w-restored"
        elif [ "$(git -C "$repo" rev-parse w-restored)" != "$recorded" ]; then fail "w-restored is not at the recorded HEAD"
        elif [ "$(git -C "$repo/.worktrees/y" symbolic-ref HEAD)" != refs/heads/w ]; then fail "the worktree that has branch w is no longer on it"
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

    # A name from the home folder is never a search of all of it (on
    # macOS, a permission prompt for Documents, Desktop and Downloads):
    # only of the usual code folders in it, the ones `watch` uses. One
    # record of w is in ~/Documents/app, one in ~/src/app. From the home
    # folder, the list and `restore w` see only the one in ~/src; once it
    # is restored, `restore w` finds nothing and names the folders it
    # searched. Inside ~/Documents, `restore w` still finds that one.
    name=restore-name-from-home
    home=$(cd "$HOME" && pwd -P)
    make_repo "$HOME/Documents/app"
    remove_w
    docs=$repo docs_sha=$w_sha
    make_repo "$HOME/src/app"
    remove_w
    (cd "$HOME" && "$bin" restore --json) >"$work/$name.list" 2>"$work/$name.list-err"
    in_dir "$HOME" run restore --json w
    if ! jq -e --arg src "$home/src/app/" '(.removed | length) == 1 and (.removed[0].original_path | startswith($src))' "$work/$name.list" >/dev/null; then fail "the list from the home folder: $(head -c 2000 "$work/$name.list")"
    elif ! grep -qF "residue: note: listed only the usual code folders in your home folder (src); for another repository, run residue restore inside it" "$work/$name.list-err"; then fail "the list does not say where it looked: $(cat "$work/$name.list-err")"
    elif [ "$status" != 0 ]; then fail "restore w from the home folder: exit $status, want 0"
    elif ! restore_ok removed checked_out_branch; then fail "stdout is not one restore document with checkout checked_out_branch"
    elif [ "$(jq -r .restored "$out")" != "$home/src/app/.worktrees/w" ]; then fail "restored $(jq -r .restored "$out"), want $home/src/app/.worktrees/w"
    elif ! common_ok; then :
    else
        in_dir "$HOME" run restore --json w
        repo=$docs w_sha=$docs_sha
        if [ "$status" != 1 ]; then fail "restore w from the home folder, with only ~/Documents holding one: exit $status, want 1"
        elif [ -s "$out" ]; then fail "stdout is not empty"
        elif ! grep -qF "residue: nothing named w in the usual code folders in your home folder (src); run residue restore inside its repository, or pass the entry's path" "$err"; then fail "the message does not name where it looked"
        elif [ "$(records)" != "1 records, 1 refs" ]; then fail "$docs has $(records), want 1 records, 1 refs"
        else
            in_dir "$docs" run restore --json w
            if [ "$status" != 0 ]; then fail "restore w inside ~/Documents/app: exit $status, want 0"
            elif [ "$(jq -r .restored "$out")" != "$home/Documents/app/.worktrees/w" ]; then fail "restored $(jq -r .restored "$out"), want $home/Documents/app/.worktrees/w"
            elif common_ok; then ok; fi
        fi
    fi
    rm -rf -- "$work/home/Documents" "$work/home/src"
else
    for c in record-json record-text record-fail-closed-dir record-fail-closed-ref record-retention record-keep-own watch-keep-removed \
        record-reflog-pin record-reflog-branch record-reflog-reftable record-reflog-reftable-expired \
        record-reflog-reftable-deleted record-reflog-reftable-stray record-reflog-migrated record-unreadable \
        restore-branch-ahead restore-branch-rewritten restore-branch-gone restore-branch-elsewhere \
        restore-path-occupied restore-detached restore-name-from-home; do
        skip_partial "$c"
    done
fi

# 8b. Trash entries that hold only their record, on every platform: a sweep
# that removes nothing still runs the purge. One an hour old or more is
# cleared: an empty record by its trashed time, one with no info or a
# damaged one by its directory's time. A younger one may be another run's,
# about to move a worktree in, so it stays, and one with no info yet is
# no error. An entry with no info that holds anything else, which an older
# release could leave, stays: residue cannot tell what it holds, so it is
# kept, with a warning naming what it holds, and no run fails for it.
name=trash-record-only
make_repo
tr=$repo/.git/residue-trash
mkdir -p "$tr/old-20260101T000000Z" "$tr/fresh-20260101T000000Z" "$tr/half-20260101T000000Z" \
    "$tr/fresh-half-20260101T000000Z" "$tr/damaged-20260101T000000Z" || exit 2
printf 'path %s\ntrashed 1767225600 2026-01-01T00:00:00Z\n' "$repo/.worktrees/old" >"$tr/old-20260101T000000Z/info" || exit 2
printf 'path %s\ntrashed %s now\n' "$repo/.worktrees/fresh" "$(date +%s)" >"$tr/fresh-20260101T000000Z/info" || exit 2
echo partial >"$tr/half-20260101T000000Z/info.new" || exit 2
echo partial >"$tr/fresh-half-20260101T000000Z/info.new" || exit 2
printf 'path /x\n' >"$tr/damaged-20260101T000000Z/info" || exit 2
touch -t 202601010000 "$tr/old-20260101T000000Z" "$tr/half-20260101T000000Z" "$tr/damaged-20260101T000000Z" || exit 2
before=$(sums "$wt")
run remove --all --yes --json --idle 30d "$repo"
left=$(find "$tr" -mindepth 1 -maxdepth 1 -exec basename {} \; 2>/dev/null | LC_ALL=C sort | paste -s -d , -)
if [ "$status" != 0 ]; then fail "exit $status, want 0"
elif [ "$(jq -c .errors "$out")" != "[]" ]; then fail "errors: $(jq -c .errors "$out"), want []"
elif [ "$(jq -r '[.purged[] | .result + " " + .entry.entry] | sort | join(",")' "$out")" != "purged damaged-20260101T000000Z,purged half-20260101T000000Z,purged old-20260101T000000Z" ]; then fail "purged: $(jq -c '[.purged[] | {result, reason, entry: .entry.entry}]' "$out")"
elif [ "$left" != "fresh-20260101T000000Z,fresh-half-20260101T000000Z" ]; then fail "left in the trash: $left"
elif ! registered "$wt" || [ "$(sums "$wt")" != "$before" ]; then fail "w changed"
elif ! registered "$repo/.worktrees/u" || [ "$(sums "$repo/.worktrees/u")" != "$u_sum" ]; then fail "the unrelated worktree u changed"
else
    mkdir -p "$tr/held-20260101T000000Z/w" || exit 2
    echo keep >"$tr/held-20260101T000000Z/w/file.txt" || exit 2
    touch -t 202601010000 "$tr/held-20260101T000000Z" || exit 2
    run remove --all --yes --json --idle 30d "$repo"
    if [ "$status" != 0 ]; then fail "with an entry holding more than its record: exit $status, want 0"
    elif [ "$(jq -c .errors "$out")" != "[]" ]; then fail "errors: $(jq -c .errors "$out"), want []"
    elif [ "$(jq -r '[.purged[] | .result + " " + .entry.entry] | join(",")' "$out")" != "kept held-20260101T000000Z" ]; then fail "purged: $(jq -c '[.purged[] | {result, reason, entry: .entry.entry}]' "$out")"
    elif ! jq -e '.purged[0].reason | contains("it has no info") and contains("w")' "$out" >/dev/null; then fail "reason: $(jq -c '.purged[0].reason' "$out")"
    elif ! jq -e '.warnings | length == 1 and (.[0] | contains("held-20260101T000000Z") and contains("it has no info"))' "$out" >/dev/null; then fail "warnings: $(jq -c .warnings "$out")"
    elif ! grep -q 'residue: warning: kept the trash entry .*held-20260101T000000Z' "$err"; then fail "no warning on stderr"
    elif [ "$(cat "$tr/held-20260101T000000Z/w/file.txt" 2>/dev/null)" != keep ]; then fail "the entry holding more than its record changed"
    else ok; fi
fi

# 8c. A trash entry that holds a file residue did not put there, beside
# the worktree and its record. The purge deletes the worktree and keeps
# the entry's record with that file, with a warning. Later runs keep the
# entry, with a warning, and fail for it no more than a run that removes
# another worktree does, until the entry holds only its record again,
# which the next run clears.
if [ -z "$partial_probe" ]; then
    name=trash-stray-beside-record
    make_repo
    run remove --yes --json --trash "$wt"
    entry=$(result 0 trash_entry)
    [ "$status" = 0 ] && [ -d "$entry" ] || { echo "contract-check: setup failed in $name: remove --trash exit $status" >&2; exit 2; }
    sed 's/^trashed [0-9]* .*/trashed 1767225600 2026-01-01T00:00:00Z/' "$entry/info" >"$work/$name.info" && cat "$work/$name.info" >"$entry/info" || exit 2
    echo stray >"$entry/stray.txt" || exit 2
    setup git -C "$repo" worktree add -q "$repo/.worktrees/x" -b x
    setup git -C "$repo" worktree add -q "$repo/.worktrees/y" -b y
    run remove --yes --json --no-trash "$repo/.worktrees/x"
    left=$(ls -A "$entry" 2>/dev/null | LC_ALL=C sort | paste -s -d , -)
    if [ "$status" != 0 ]; then fail "the purge: exit $status, want 0"
    elif [ "$(result 0 result)" != removed ]; then fail "x: $(result 0 result), want removed"
    elif [ "$(jq -r '[.purged[] | .result] | join(",")' "$out")" != purged ]; then fail "purged: $(jq -c '[.purged[] | {result, reason}]' "$out")"
    elif [ "$(jq -r '.purged[0].removal_record | type' "$out")" != string ]; then fail "the purge wrote no removal record"
    elif [ -e "$entry/w" ]; then fail "the purged worktree is still in the entry"
    elif [ "$left" != "info,stray.txt" ]; then fail "left in the entry: $left, want info,stray.txt"
    elif ! grep -qxF "path $(cd "$repo" && pwd -P)/.worktrees/w" "$entry/info"; then fail "its info changed: $(cat "$entry/info")"
    elif ! jq -e '.warnings | length == 1 and (.[0] | contains("stray.txt"))' "$out" >/dev/null; then fail "warnings: $(jq -c .warnings "$out")"
    else
        run remove --yes --json --no-trash "$repo/.worktrees/y"
        if [ "$status" != 0 ]; then fail "the next run: exit $status, want 0"
        elif [ "$(result 0 result)" != removed ]; then fail "y: $(result 0 result), want removed"
        elif [ "$(jq -c .errors "$out")" != "[]" ]; then fail "the next run's errors: $(jq -c .errors "$out")"
        elif [ "$(jq -r '[.purged[] | .result] | join(",")' "$out")" != kept ]; then fail "the next run purged: $(jq -c '[.purged[] | {result, reason}]' "$out")"
        elif ! jq -e '.purged[0].reason | contains("stray.txt")' "$out" >/dev/null; then fail "the next run's reason: $(jq -c '.purged[0].reason' "$out")"
        elif ! jq -e '.warnings | length == 1 and (.[0] | contains("stray.txt"))' "$out" >/dev/null; then fail "the next run's warnings: $(jq -c .warnings "$out")"
        elif [ "$(cat "$entry/stray.txt" 2>/dev/null)" != stray ]; then fail "stray.txt changed"
        else
            rm -f "$entry/stray.txt"
            run remove --all --yes --json --idle 30d "$repo"
            if [ "$status" != 0 ]; then fail "once only the record is left: exit $status, want 0"
            elif [ "$(jq -r '[.purged[] | .result] | join(",")' "$out")" != purged ]; then fail "once only the record is left, purged: $(jq -c '[.purged[] | {result, reason}]' "$out")"
            elif [ -e "$entry" ]; then fail "the entry is still there: $(ls -A "$entry")"
            elif common_ok; then ok; fi
        fi
    fi
else
    skip_partial trash-stray-beside-record
fi

# 8d. A trash entry keeps the --keep-trash it was trashed with, as a
# removal record keeps its --keep-removed: no run purges it sooner, and
# restore --json shows it. Trashed with 30d and set ten days back, it
# outlives a sweep with the default 7d and one with --keep-trash 1d; set
# 31 days back, the default sweep purges it.
if [ -z "$partial_probe" ]; then
    name=trash-own-keep
    make_repo
    run remove --yes --json --trash --keep-trash 30d "$wt"
    entry=$(result 0 trash_entry)
    [ "$status" = 0 ] && [ -d "$entry" ] || { echo "contract-check: setup failed in $name: remove --trash exit $status" >&2; exit 2; }
    trashed_ago() { # SECONDS
        sed "s/^trashed [0-9]* .*/trashed $(($(date +%s) - $1)) earlier/" "$entry/info" >"$work/$name.info" && cat "$work/$name.info" >"$entry/info" || exit 2
    }
    trashed_ago 864000
    if ! grep -qx 'keep 2592000 30d' "$entry/info"; then fail "its info has no keep line: $(cat "$entry/info")"
    elif ! (cd "$repo" && "$bin" restore --json) >"$work/$name.list" 2>"$err"; then fail "restore --json (the list) failed"
    elif ! jq -e '[.trash[] | {keep, keep_seconds}] == [{"keep": "30d", "keep_seconds": 2592000}]' "$work/$name.list" >/dev/null; then fail "restore --json keep: $(jq -c '[.trash[] | {entry, keep, keep_seconds}]' "$work/$name.list")"
    else
        run remove --all --yes --json --idle 30d "$repo"
        if [ "$status" != 0 ]; then fail "the 7d sweep: exit $status, want 0"
        elif [ "$(jq -c .purged "$out")" != "[]" ]; then fail "the 7d sweep purged: $(jq -c '[.purged[] | {result, reason}]' "$out")"
        else
            run remove --all --yes --json --idle 30d --keep-trash 1d "$repo"
            if [ "$status" != 0 ]; then fail "the 1d sweep: exit $status, want 0"
            elif [ "$(jq -c .purged "$out")" != "[]" ]; then fail "the 1d sweep purged: $(jq -c '[.purged[] | {result, reason}]' "$out")"
            elif [ ! -d "$entry/w" ]; then fail "the worktree is gone from the trash"
            else
                trashed_ago 2678400
                run remove --all --yes --json --idle 30d "$repo"
                if [ "$status" != 0 ]; then fail "at 31 days: exit $status, want 0"
                elif [ "$(jq -r '[.purged[] | .result] | join(",")' "$out")" != purged ]; then fail "at 31 days, purged: $(jq -c '[.purged[] | {result, reason}]' "$out")"
                elif common_ok; then ok; fi
            fi
        fi
    fi
else
    skip_partial trash-own-keep
fi

# 8e. Two expired entries no purge can act on: one that holds its worktree
# while Git lists that worktree at its old path (a move into the trash that
# stopped after its rename, or a repository moved since), and one whose
# keep line this release cannot read. Both stay, with a warning each, and
# the sweep exits 0. The second is still restorable.
if [ -z "$partial_probe" ]; then
    name=trash-kept-unpurgeable
    make_repo
    setup git -C "$repo" worktree add -q "$repo/.worktrees/v" -b v
    run remove --yes --json --trash "$wt" "$repo/.worktrees/v"
    lost=$(result 0 trash_entry) odd=$(result 1 trash_entry)
    [ "$status" = 0 ] && [ -d "$lost" ] && [ -d "$odd" ] || { echo "contract-check: setup failed in $name: remove --trash exit $status" >&2; exit 2; }
    for e in "$lost" "$odd"; do
        sed 's/^trashed [0-9]* .*/trashed 1767225600 2026-01-01T00:00:00Z/' "$e/info" >"$work/$name.info" && cat "$work/$name.info" >"$e/info" || exit 2
    done
    sed 's/^keep .*/keep later/' "$odd/info" >"$work/$name.info" && cat "$work/$name.info" >"$odd/info" || exit 2
    # Git's record of w points at its old path again; its folder stays in the entry.
    printf '%s\n' "$wt/.git" >"$repo/.git/worktrees/w/gitdir" || exit 2
    before=$(sums "$lost/w")
    want=$(printf 'kept %s\nkept %s\n' "$(basename "$lost")" "$(basename "$odd")" | LC_ALL=C sort | paste -s -d , -)
    run remove --all --yes --json --idle 30d "$repo"
    if [ "$status" != 0 ]; then fail "exit $status, want 0"
    elif [ "$(jq -c .errors "$out")" != "[]" ]; then fail "errors: $(jq -c .errors "$out")"
    elif [ "$(jq -r '[.purged[] | .result + " " + .entry.entry] | sort | join(",")' "$out")" != "$want" ]; then fail "purged: $(jq -c '[.purged[] | {result, reason, entry: .entry.entry}]' "$out")"
    elif ! jq -e --arg e "$(basename "$lost")" '[.purged[] | select(.entry.entry == $e) | .reason][0] | contains("Git does not list its worktree here") and contains("worktree repair")' "$out" >/dev/null; then fail "the reason for the entry Git lists elsewhere: $(jq -c '.purged' "$out")"
    elif ! jq -e --arg e "$(basename "$odd")" '[.purged[] | select(.entry.entry == $e)][0] | (.reason | contains("keep line (later)")) and .entry.error == null and .entry.keep == null' "$out" >/dev/null; then fail "the entry with the odd keep line: $(jq -c '.purged' "$out")"
    elif [ "$(jq '.warnings | length' "$out")" != 2 ]; then fail "warnings: $(jq -c .warnings "$out"), want 2"
    elif [ "$(sums "$lost/w")" != "$before" ] || [ ! -d "$odd/v" ]; then fail "an entry changed"
    else
        in_dir "$repo" run restore --json "$odd"
        if [ "$status" != 0 ]; then fail "restore of the entry with the odd keep line: exit $status: $(cat "$err")"
        elif ! registered "$repo/.worktrees/v"; then fail "v is not back"
        else ok; fi
    fi
else
    skip_partial trash-kept-unpurgeable
fi

# 8f. An expired entry beside a damaged removal record named for its
# worktree, which may be an earlier purge's: residue cannot tell, so the
# purge keeps the entry, with a warning naming the record, and never
# forgets that record. Two sweeps, each exit 0.
if [ -z "$partial_probe" ]; then
    name=trash-damaged-purge-record
    make_repo
    run remove --yes --json --trash "$wt"
    entry=$(result 0 trash_entry)
    [ "$status" = 0 ] && [ -d "$entry" ] || { echo "contract-check: setup failed in $name: remove --trash exit $status" >&2; exit 2; }
    sed 's/^trashed [0-9]* .*/trashed 1767225600 2026-01-01T00:00:00Z/' "$entry/info" >"$work/$name.info" && cat "$work/$name.info" >"$entry/info" || exit 2
    damaged=$repo/.git/residue-removed/w-20260601T000000Z
    mkdir -p "$damaged" && printf 'not a record\n' >"$damaged/info" || exit 2
    wrong=
    for round in 1 2; do
        run remove --all --yes --json --idle 30d "$repo"
        if [ "$status" != 0 ]; then wrong+=" sweep $round: exit $status, want 0;"
        elif [ "$(jq -r '[.purged[] | .result] | join(",")' "$out")" != kept ]; then wrong+=" sweep $round purged: $(jq -c '[.purged[] | {result, reason}]' "$out");"
        elif ! jq -e '.purged[0].reason | contains("w-20260601T000000Z") and contains("cannot read")' "$out" >/dev/null; then wrong+=" sweep $round reason: $(jq -c '.purged[0].reason' "$out");"
        elif ! jq -e '[.warnings[] | select(contains("kept the trash entry") and contains("w-20260601T000000Z"))] | length == 1' "$out" >/dev/null; then wrong+=" sweep $round warnings: $(jq -c .warnings "$out");"
        elif [ "$(jq -c .errors "$out")" != "[]" ]; then wrong+=" sweep $round errors: $(jq -c .errors "$out");"
        fi
    done
    if [ -n "$wrong" ]; then fail "$wrong"
    elif [ "$(cat "$damaged/info" 2>/dev/null)" != "not a record" ]; then fail "the damaged record changed"
    elif ! registered "$entry/w"; then fail "the trashed worktree is gone"
    else ok; fi
else
    skip_partial trash-damaged-purge-record
fi

# 9. Stats: each removal, move into the trash, purge and restore adds a
# line to the stats ledger, `remove --json` reports what it freed in
# `stats`, and `residue stats --json` (residue.stats/v1) adds them up. A
# ledger residue cannot read or write is a warning that never changes a
# removal; a damaged line is skipped and counted; a run seeds the ledger
# from the removal records it lacks, once each. Each case starts from an
# empty ledger.
ledger=$("$bin" stats --json 2>/dev/null | jq -r '.ledger // empty' 2>/dev/null)
case $ledger in
"$HOME"/*) ;;
*) ledger= ;;
esac
reset_ledger() {
    [ -z "$ledger" ] || rm -rf -- "$ledger"
}
# The ledger's lines as compact JSON, one per line.
ledger_lines() {
    jq -c . "$ledger" 2>/dev/null
}
# stdout is exactly one residue.stats/v1 document with every field.
stats_ok() {
    jq -e -s '
        length == 1 and (.[0] |
            .schema == "residue.stats/v1"
            and (.version | type) == "string"
            and (.generated_at | type) == "string"
            and (.ledger | type) == "string"
            and ((.since | type) == "string" or .since == null)
            and ([.freed.all_time, .freed.last_30_days, .freed.last_7_days][]
                | (.bytes | type) == "number" and (.worktrees | type) == "number"
                  and (.by | keys) == ["other", "remove", "sweep", "watch"])
            and (.estimate_note | type) == "string"
            and (.estimate_note | startswith("Sizes are estimates"))
            and (.weeks | length) == 12
            and all(.weeks[]; (.by | keys) == ["other", "remove", "sweep", "watch"])
            and all(.repositories[]; (.by | keys) == ["other", "remove", "sweep", "watch"])
            and (.repositories | type) == "array"
            and (.trashed.worktrees | type) == "number"
            and (.trash_now.entries | type) == "number"
            and (.restored.worktrees | type) == "number"
            and (.last_freed_at | keys) == ["all", "other", "remove", "sweep", "watch"]
            and (.skipped_lines | type) == "number"
            and (.errors | type) == "array")
    ' "$out" >/dev/null
}
# Run `residue stats --json` into $work/$name.stats, with nothing on
# stderr; `jq -e JQ-ARGS...` on it must hold.
stats_has() { # [--arg NAME VALUE]... FILTER
    "$bin" stats --json >"$work/$name.stats" 2>"$work/$name.stats.err" \
        && [ ! -s "$work/$name.stats.err" ] \
        && jq -e "$@" "$work/$name.stats" >/dev/null
}
stats_show() {
    jq -c '{freed: .freed.all_time, trashed, trash_now, restored, skipped_lines, errors}' "$work/$name.stats" 2>/dev/null
}

# On every platform: an empty ledger, then damaged lines.
name=stats-empty
reset_ledger
run stats --json
if [ -z "$ledger" ]; then fail "residue stats --json does not name a ledger under HOME"
elif [ "$status" != 0 ]; then fail "exit $status, want 0"
elif ! stats_ok; then fail "stdout is not one valid residue.stats/v1 document"
elif ! jq -e '.since == null and .freed.all_time.worktrees == 0 and .skipped_lines == 0 and .repositories == []' "$out" >/dev/null; then fail "an empty ledger reports something: $(jq -c . "$out")"
elif [ -s "$err" ]; then fail "stderr is not empty"
else
    run stats
    if [ "$status" != 0 ]; then fail "text: exit $status, want 0"
    elif ! grep -q "^residue has not counted any freed space yet\. It counts from version 0\.0\.8 on\.$" "$out"; then fail "text: no 'has not counted any freed space' line"
    elif [ -e "$ledger" ]; then fail "reading the stats created the ledger"
    else ok; fi
fi

name=stats-damaged-line
reset_ledger
if [ -n "$ledger" ]; then
    mkdir -p "$(dirname "$ledger")" || exit 2
    {
        echo '{"v":1,"at":1767225600,"event":"freed","by":"watch","reason":"watch_landed","bytes":1000,"repo":"/r/.git","entry":"a-20260101T000000Z"}'
        echo 'not json'
        echo '{"v":1,"at":1767225600,"event":"freed","by":"watch","reason":"watch_landed","bytes":1000,"repo":"/r/.git","entry":"a-20260101T000000Z","seeded":true}'
        printf '{"v":1,"at":1767225601,"event":"fre'
    } >"$ledger" || exit 2
fi
run stats --json
if [ "$status" != 0 ]; then fail "exit $status, want 0"
elif ! stats_ok; then fail "stdout is not one valid residue.stats/v1 document"
elif ! jq -e '.skipped_lines == 2 and .repositories[0].by.watch == {"bytes": 1000, "worktrees": 1} and .freed.all_time == {"bytes": 1000, "worktrees": 1, "by": {"remove": {"bytes": 0, "worktrees": 0}, "sweep": {"bytes": 0, "worktrees": 0}, "watch": {"bytes": 1000, "worktrees": 1}, "other": {"bytes": 0, "worktrees": 0}}} and .since == "2026-01-01T00:00:00Z"' "$out" >/dev/null; then fail "want 1 worktree once and 2 skipped lines: $(jq -c '{freed: .freed.all_time, skipped_lines, since}' "$out")"
elif [ -s "$err" ]; then fail "stderr is not empty"
else
    run stats
    if ! grep -q "skipped 2 lines it could not read" "$out"; then fail "text: the skipped lines are not counted"
    else ok; fi
fi

if [ -z "$partial_probe" ]; then
    name=stats-remove-json
    make_repo
    reset_ledger
    run remove --yes --json "$wt"
    entry=$(basename "$(result 0 removal_record)")
    if [ "$status" != 0 ]; then fail "exit $status, want 0"
    elif ! json_ok 1; then fail "stdout is not one valid remove document with 1 result"
    elif ! jq -e --arg ledger "$ledger" '
            .stats as $s | .results[0].worktree.size.reclaimable_bytes as $r
            | $s.freed_worktrees == 1 and $s.freed_bytes == $r
              and $s.trashed_worktrees == 0 and $s.trashed_bytes == 0
              and $s.total_freed_worktrees == 1 and $s.total_freed_bytes == $r
              and ($s.since | type) == "string" and $s.ledger == $ledger and $s.error == null
        ' "$out" >/dev/null; then fail "stats: $(jq -c .stats "$out")"
    elif [ -s "$err" ]; then fail "stderr is not empty"
    elif ! ledger_lines | jq -e -s --arg entry "$entry" --arg repo "$(cd "$repo/.git" && pwd -P)" '
            length == 1 and (.[0] | .v == 1 and .event == "freed" and .by == "remove"
              and .reason == "remove" and .entry == $entry and .repo == $repo
              and .worktree == "w" and (.seeded | not))
        ' >/dev/null; then fail "the ledger: $(cat "$ledger" 2>/dev/null)"
    elif ! stats_has --arg path "$(cd "$repo" && pwd -P)" '.freed.all_time.worktrees == 1 and .freed.all_time.by.remove.worktrees == 1 and .freed.last_7_days.worktrees == 1 and .weeks[11].worktrees == 1 and .weeks[11].by.remove.worktrees == 1 and .weeks[11].by.watch.worktrees == 0 and .repositories[0].by.remove.worktrees == 1 and .repositories[0].by.watch.worktrees == 0 and .repositories[0].path == $path and .last_freed_at.remove != null'; then fail "residue stats: $(stats_show) $(jq -c .repositories "$work/$name.stats" 2>/dev/null)"
    elif common_ok; then ok; fi

    name=stats-trash-and-purge
    make_repo
    reset_ledger
    run remove --yes --json --trash "$wt"
    entry=$(result 0 trash_entry)
    if [ "$status" != 0 ] || [ "$(result 0 result)" != trashed ]; then fail "remove --trash: exit $status, $(result 0 result)"
    elif ! jq -e '.stats | .trashed_worktrees == 1 and .trashed_bytes > 0 and .freed_worktrees == 0 and .total_freed_worktrees == 0' "$out" >/dev/null; then fail "remove --trash stats: $(jq -c .stats "$out")"
    elif ! stats_has '.trashed.worktrees == 1 and .trash_now.entries == 1 and .trash_now.repositories == 1 and .freed.all_time.worktrees == 0'; then fail "residue stats after the move: $(stats_show)"
    else
        sed 's/^trashed [0-9]* .*/trashed 1767225600 2026-01-01T00:00:00Z/' "$entry/info" >"$work/$name.info" && cat "$work/$name.info" >"$entry/info" || exit 2
        setup git -C "$repo" worktree add -q "$repo/.worktrees/x" -b x
        run remove --yes --json --no-trash --keep-trash 1d "$repo/.worktrees/x"
        if [ "$status" != 0 ]; then fail "remove x and purge: exit $status, want 0"
        elif [ "$(jq -r '[.purged[].result] | join(",")' "$out")" != purged ]; then fail "purged: $(jq -c '[.purged[] | {result, reason}]' "$out")"
        elif ! jq -e '.stats | .freed_worktrees == 2 and .trashed_worktrees == 0 and .total_freed_worktrees == 2' "$out" >/dev/null; then fail "purge stats: $(jq -c .stats "$out")"
        elif ! ledger_lines | jq -e -s '[.[] | select(.event == "freed") | .reason] | sort == ["remove", "trash_purge"]' >/dev/null; then fail "the ledger: $(cat "$ledger" 2>/dev/null)"
        elif ! stats_has '.freed.all_time.worktrees == 2 and .trashed.worktrees == 1 and .trash_now.entries == 0'; then fail "residue stats after the purge: $(stats_show)"
        elif [ -s "$err" ]; then fail "stderr is not empty"
        elif common_ok; then ok; fi
    fi

    name=stats-restore
    make_repo
    reset_ledger
    remove_w
    run restore --json "$wt"
    if [ "$status" != 0 ]; then fail "restore: exit $status, want 0"
    elif ! restore_ok removed checked_out_branch; then fail "stdout is not one restore document"
    elif [ -s "$err" ]; then fail "stderr is not empty"
    elif ! ledger_lines | jq -e -s '[.[].event] == ["freed", "restored"] and .[1].from == "removed" and .[1].entry == .[0].entry' >/dev/null; then fail "the ledger: $(cat "$ledger" 2>/dev/null)"
    elif ! stats_has '.restored == {"worktrees": 1, "from_trash": 0, "from_removal_records": 1} and .freed.all_time.worktrees == 1'; then fail "residue stats: $(stats_show)"
    elif common_ok; then ok; fi

    # A removal record the ledger lacks (a ledger that was lost, or one a
    # run could not write) is seeded by the next scan, once.
    name=stats-seed
    make_repo
    reset_ledger
    remove_w
    reset_ledger
    run scan "$repo"
    first=$status
    scan_tail=$(tail -n 1 "$out")
    run scan "$repo"
    if [ "$first" != 0 ] || [ "$status" != 0 ]; then fail "scan: exit $first and $status, want 0"
    elif [ -s "$err" ]; then fail "stderr is not empty"
    elif ! grep -q "^freed about .* from 1 worktree since [0-9-]*; .* in the last 7 days · residue stats\$" <<<"$scan_tail"; then fail "scan does not end with the totals: $scan_tail"
    elif ! ledger_lines | jq -e -s 'length == 1 and (.[0] | .event == "freed" and .seeded == true and .by == "remove" and .reason == "remove" and .bytes > 0)' >/dev/null; then fail "the ledger: $(cat "$ledger" 2>/dev/null)"
    elif ! stats_has '.freed.all_time.worktrees == 1'; then fail "residue stats: $(stats_show)"
    else
        run scan --json "$repo"
        if [ "$status" != 0 ] || ! jq -e 'has("stats") | not' "$out" >/dev/null; then fail "scan --json changed"
        elif common_ok; then ok; fi
    fi

    # A last line cut short, as a crash leaves it: the next removal's line
    # starts on a line of its own, so only the cut one is skipped.
    name=stats-torn-line
    make_repo
    reset_ledger
    if [ -n "$ledger" ]; then
        mkdir -p "$(dirname "$ledger")" || exit 2
        {
            echo '{"v":1,"at":1767225600,"event":"freed","by":"watch","reason":"watch_landed","bytes":1000,"repo":"/r/.git","entry":"a-20260101T000000Z"}'
            printf '{"v":1,"at":1767225601,"event":"fre'
        } >"$ledger" || exit 2
    fi
    run remove --yes --json "$wt"
    if [ -z "$ledger" ]; then fail "residue stats --json does not name a ledger under HOME"
    elif [ "$status" != 0 ]; then fail "exit $status, want 0"
    elif [ "$(result 0 result)" != removed ]; then fail "result $(result 0 result), want removed"
    elif ! stats_has '.skipped_lines == 1 and .freed.all_time.worktrees == 2 and .freed.all_time.by.remove.worktrees == 1'; then fail "the removal's line was glued onto the cut one: $(stats_show)"
    elif common_ok; then ok; fi

    # A FIFO at the ledger path: never waited on. The removal goes ahead
    # with the warning, and `residue stats` fails at once.
    name=stats-ledger-fifo
    make_repo
    reset_ledger
    if [ -n "$ledger" ]; then
        mkdir -p "$(dirname "$ledger")" && mkfifo "$ledger" || exit 2
    fi
    run remove --yes --json "$wt"
    if [ -z "$ledger" ]; then fail "residue stats --json does not name a ledger under HOME"
    elif [ "$status" = timeout ]; then fail "remove waited on the FIFO for 120 s"
    elif [ "$status" != 0 ]; then fail "exit $status, want 0"
    elif [ "$(result 0 result)" != removed ]; then fail "result $(result 0 result), want removed"
    elif [ -e "$wt" ] || registered "$wt"; then fail "the worktree is still there"
    elif ! jq -e '.errors == [] and any(.warnings[]; test("^the stats ledger is not up to date .*not a regular file")) and .stats.freed_worktrees == 1' "$out" >/dev/null; then fail "errors, warnings and stats: $(jq -c '{errors, warnings, stats}' "$out")"
    elif ! common_ok; then :
    else
        run stats --json
        if [ "$status" = timeout ]; then fail "stats waited on the FIFO for 120 s"
        elif [ "$status" != 1 ]; then fail "stats: exit $status, want 1"
        elif ! grep -q "not a regular file" "$err"; then fail "stats: stderr does not say why: $(cat "$err")"
        else ok; fi
    fi
    [ -z "$ledger" ] || rm -f -- "$ledger"

    # A ledger residue cannot write never changes a removal: the PATH is
    # removed, exit 0, nothing in `errors`, a warning on stderr, in
    # `warnings` and in stats.error.
    name=stats-unwritable-ledger
    make_repo
    reset_ledger
    [ -z "$ledger" ] || mkdir -p "$ledger" || exit 2
    run remove --yes --json "$wt"
    if [ -z "$ledger" ]; then fail "residue stats --json does not name a ledger under HOME"
    elif [ "$status" != 0 ]; then fail "exit $status, want 0"
    elif [ "$(result 0 result)" != removed ]; then fail "result $(result 0 result), want removed"
    elif [ -e "$wt" ] || registered "$wt"; then fail "the worktree is still there"
    elif ! jq -e '.errors == [] and any(.warnings[]; test("^the stats ledger is not up to date")) and (.stats | .freed_worktrees == 1 and .total_freed_bytes == null and (.error | test("stats ledger")))' "$out" >/dev/null; then fail "errors, warnings and stats: $(jq -c '{errors, warnings, stats}' "$out")"
    elif ! grep -q "^residue: warning: the stats ledger is not up to date" "$err"; then fail "no warning on stderr"
    elif common_ok; then ok; fi
    rmdir "$ledger" 2>/dev/null

    # Text: the run ends with what it freed and the total; a run that
    # removes nothing says nothing about the ledger.
    name=stats-text
    make_repo
    reset_ledger
    setup git -C "$repo" worktree add -q "$repo/.worktrees/x" -b x
    run remove "$repo/.worktrees/x"
    preview=$out
    mv "$preview" "$work/$name.preview" || exit 2
    run remove --yes "$wt"
    if [ "$status" != 0 ]; then fail "exit $status, want 0"
    elif grep -q "freed" "$work/$name.preview"; then fail "a run that removed nothing printed a freed line"
    elif ! tail -n 1 "$out" | grep -q -E '^freed about [0-9.]+ [KMG]?B \(about [0-9.]+ [KMG]?B on this machine since [0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]\)$'; then fail "the last line is not the freed line: $(tail -n 1 "$out")"
    elif [ "$(ledger_lines | wc -l | tr -d ' ')" != 1 ]; then fail "the ledger has $(ledger_lines | wc -l | tr -d ' ') lines, want 1"
    else
        run stats
        if [ "$status" != 0 ]; then fail "stats: exit $status"
        elif ! grep -q "^freed on this machine since " "$out" || ! grep -q "residue remove (by hand) .* 1 worktree" "$out"; then fail "stats text"
        elif ! grep -q "^Sizes are estimates" "$out"; then fail "stats text does not say the sizes are estimates"
        elif common_ok; then ok; fi
    fi
else
    for c in stats-remove-json stats-trash-and-purge stats-restore stats-seed stats-torn-line stats-ledger-fifo stats-unwritable-ledger stats-text; do
        skip_partial "$c"
    done
fi

# 10. Linux: a process whose only link to a worktree is a mapped file
# (/proc/PID/maps) keeps it in use. A copy of the system's libm in the
# worktree's ignored node_modules/ is preloaded into a sleep that runs in
# another folder, so it holds no working directory, executable or open
# file there, only the mapping. Once it exits, the worktree is not in use.
# The case also prints what reading every process's maps costs here.
if [ "$(uname -s)" = Linux ]; then
    name=activity-mapped-only
    make_repo
    libc=$(grep -m 1 -o '/[^ ]*/libc\.so\.6$' /proc/self/maps)
    [ -r "${libc%/*}/libm.so.6" ] || { echo "contract-check: setup failed in $name: no libm.so.6 beside ${libc:-libc}" >&2; exit 2; }
    setup cp "${libc%/*}/libm.so.6" "$wt/node_modules/libmapped.so"
    before=$(sums "$wt")
    wt_real=$(cd "$wt" && pwd -P) || exit 2
    mapped=$wt_real/node_modules/libmapped.so
    (cd "$work" && LD_PRELOAD=$mapped exec sleep 300) >/dev/null 2>&1 &
    holder=$!
    holders="$holders $holder"
    for ((i = 0; i < 100; i++)); do
        grep -qF "$mapped" "/proc/$holder/maps" 2>/dev/null && break
        sleep 0.1
    done
    grep -qF "$mapped" "/proc/$holder/maps" 2>/dev/null || { echo "contract-check: setup failed in $name: sleep never mapped $mapped" >&2; kill "$holder" 2>/dev/null; exit 2; }
    links=$({ readlink "/proc/$holder/cwd"; readlink "/proc/$holder/exe"; for f in /proc/"$holder"/fd/*; do readlink "$f"; done; } 2>/dev/null | grep -cF "$wt_real/")
    [ "$(readlink "/proc/$holder/cwd")" != "$wt_real" ] && [ "$links" = 0 ] || { echo "contract-check: setup failed in $name: sleep holds more than a mapping in the worktree" >&2; kill "$holder" 2>/dev/null; exit 2; }
    TIMEFORMAT=%R
    { time run explain --json "$wt"; } 2>"$work/$name.time"
    explain_s=$(cat "$work/$name.time")
    busy=$work/$name.busy
    mv "$out" "$busy" || exit 2
    maps_procs=0
    for f in /proc/[0-9]*/maps; do
        { read -r _ <"$f"; } 2>/dev/null && maps_procs=$((maps_procs + 1))
    done
    { time cat /proc/[0-9]*/maps >"$work/$name.maps" 2>/dev/null; } 2>"$work/$name.time"
    maps_s=$(cat "$work/$name.time")
    read -r maps_lines maps_bytes < <(wc -l -c <"$work/$name.maps")
    kill "$holder" 2>/dev/null
    wait "$holder" 2>/dev/null
    run explain --json "$wt"
    if [ "$status" != 0 ]; then fail "explain exit $status, want 0"
    elif ! jq -e --argjson pid "$holder" '.worktree.activity.state == "in_use" and .worktree.verdict == "keep" and any(.worktree.activity.processes[]; .pid == $pid and .command == "sleep")' "$busy" >/dev/null; then fail "with sleep mapping a file in it, activity is $(jq -c '.worktree.activity' "$busy"), want in_use by sleep[$holder]"
    elif ! jq -e '[.worktree.checks[] | select(.status != "pass") | .id] == ["activity"]' "$busy" >/dev/null; then fail "failing checks: $(jq -c '[.worktree.checks[] | select(.status != "pass") | .id]' "$busy"), want activity"
    elif ! jq -e '.worktree.activity.state == "not_observed" and .worktree.verdict == "candidate"' "$out" >/dev/null; then fail "after sleep exited, activity is $(jq -c '.worktree.activity' "$out") and the verdict $(jq -r '.worktree.verdict' "$out"), want not_observed and candidate"
    elif untouched "$before"; then
        ok
        echo "     maps: $maps_procs processes readable, $maps_lines lines, $maps_bytes bytes; one cat of them all took ${maps_s} s, the explain that read them ${explain_s} s"
    fi
fi

# 11. Ignored folders that `git status --ignored=matching` lists
# differently from `--ignored=traditional`: residue judges the list
# traditional mode prints, so each result is the one it always was (#83).
# The patterns go in .git/info/exclude, which no commit carries; `sub/`
# holds a tracked file, so what is below it takes matching mode's list.
make_layout() { # PATTERNS SETUP
    make_repo
    printf '%b' "$1" >>"$repo/.git/info/exclude"
    (cd "$wt" && sh -c "$2") >/dev/null 2>&1 || { echo "contract-check: setup failed in $name: $2" >&2; exit 2; }
}

ignored_paths() { # INDEX: the reported ignored entries, sorted, space-separated
    jq -r "[.results[$1].worktree.ignored[]?.path] | sort | join(\" \")" "$out"
}

layout_refused() { # NAME CHECK-IDS(comma-separated) ENTRIES PATTERNS SETUP [REMOVE-ARGS...]
    name=$1
    local check entries before ids
    IFS=, read -r -a ids <<<"$2"
    check=$(want_failing "${ids[@]}")
    entries=$3
    make_layout "$4" "$5"
    shift 5
    before=$(sums "$wt")
    run remove --yes --json "$@" "$wt"
    if [ "$status" != 3 ]; then fail "exit $status, want 3"
    elif ! json_ok 1; then fail "stdout is not one valid remove document with 1 result"
    elif [ "$(result 0 result)" != refused ]; then fail "result $(result 0 result), want refused"
    elif [ "$(failing_checks 0)" != "$check" ]; then fail "failing checks: $(failing_checks 0), want $check"
    elif [ "$(ignored_paths 0)" != "$entries" ]; then fail "ignored entries: $(ignored_paths 0), want $entries"
    elif untouched "$before"; then ok; fi
}

layout_removed() { # NAME PATTERNS SETUP [REMOVE-ARGS...]
    name=$1
    if [ -n "$partial_probe" ]; then
        skip_partial "$name"
        return
    fi
    make_layout "$2" "$3"
    shift 3
    run remove --yes --json "$@" "$wt"
    if [ "$status" != 0 ]; then fail "exit $status, want 0 (failing checks: $(failing_checks 0))"
    elif ! json_ok 1; then fail "stdout is not one valid remove document with 1 result"
    elif [ "$(result 0 result)" != removed ]; then fail "result $(result 0 result), want removed"
    elif [ -e "$wt" ] || registered "$wt"; then fail "the worktree is still there"
    elif common_ok; then ok; fi
}

# Empty ignored folders: matching mode lists them, traditional mode hides
# them, so none is an entry that blocks.
layout_removed ignored-empty-folders 'secrets/\n' 'mkdir -p secrets sub/secrets/a'
# A folder holding only a nested repository is listed, and the repository
# blocks.
layout_refused ignored-nested-repo-only ignored,nested_repo "node_modules/ sub/secrets/" \
    'secrets/\n' 'mkdir -p sub/secrets && git init -q sub/secrets/dep'
# A `.git` entry in any case hides what is in it from Git when
# core.ignorecase is set, as it is on macOS: `private/` is then no ignored
# entry, and nested_repo blocks (#102). With it unset, Git lists `private/`
# as well. A plain folder named `.git` blocks the same way in either.
layout_refused ignored-dotgit-upper-ignorecase nested_repo "node_modules/" \
    'private/\n' 'git config core.ignorecase true && mkdir -p private/.GIT && echo notes >private/.GIT/notes.txt'
layout_refused ignored-dotgit-upper-case-sensitive ignored,nested_repo "node_modules/ private/" \
    'private/\n' 'git config core.ignorecase false && mkdir -p private/.GIT && echo notes >private/.GIT/notes.txt'
layout_refused ignored-dotgit-plain-folder nested_repo "node_modules/" \
    'private/\n' 'mkdir -p private/.git && echo notes >private/.git/notes.txt'
# The same at the worktree's root, beside its `.git` link: neither list
# names `.GIT/`, so only nested_repo keeps its files. A case-insensitive
# file system cannot hold `.GIT` there, so the case runs only on a
# case-sensitive one (Linux; a case-sensitive APFS volume as TMPDIR).
setup touch "$work/case-probe"
if [ -e "$work/CASE-PROBE" ]; then
    echo "skip ignored-dotgit-upper-root: this file system is case-insensitive, so .GIT would be the worktree's .git link"
else
    layout_refused ignored-dotgit-upper-root nested_repo "node_modules/" \
        '' 'git config core.ignorecase true && mkdir .GIT && echo notes >.GIT/notes.txt'
fi
# An untracked folder of ignored entries is one entry, `orphan/`, which the
# rules for what is inside it do not match. Matching mode would list
# `orphan/a.log` and `orphan/node_modules/`, which they do.
layout_refused ignored-only-ignored-files ignored "node_modules/ orphan/" \
    '*.log\n' 'mkdir orphan && echo a >orphan/a.log' --regenerable '*.log'
layout_refused ignored-orphan-node-modules ignored "node_modules/ orphan/" \
    '' 'mkdir -p orphan/node_modules/p && echo x >orphan/node_modules/p/i.js'
layout_refused ignored-deep-glob ignored "deep/ node_modules/" \
    'deep/**/gen/\n' 'mkdir -p deep/a/gen && echo g >deep/a/gen/out.js' --regenerable gen
# The other way: `coverage/` matches its rule, and the entries matching
# mode lists inside it would not.
layout_removed ignored-coverage-contents 'coverage/*\n' \
    'mkdir -p coverage/html && echo x >coverage/lcov.info && echo y >coverage/html/i.html' --regenerable coverage

# 12. An ignored folder whose name the disk stores decomposed (NFD), `café/`
# as `cafe` and U+0301. On macOS Git prints it precomposed (NFC), elsewhere
# as stored; either way the walk finds it, so its entry has a size (#103).
# The folder named precomposed is the control.
nfd=$(printf 'cafe\314\201')
nfc=$(printf 'caf\303\251')
ignored_sized() { # NAME FOLDER
    name=$1
    local before
    make_layout 'caf*/\n' "mkdir -p '$2/sub' && echo notes >'$2/sub/notes.txt'"
    before=$(sums "$wt")
    run remove --yes --json "$wt"
    if [ "$status" != 3 ]; then fail "exit $status, want 3"
    elif ! json_ok 1; then fail "stdout is not one valid remove document with 1 result"
    elif [ "$(failing_checks 0)" != "$(want_failing ignored)" ]; then fail "failing checks: $(failing_checks 0), want $(want_failing ignored)"
    elif ! jq -e '[.results[0].worktree.ignored[] | select(.path != "node_modules/")] | length == 1 and .[0].files == 1 and .[0].bytes == 6' "$out" >/dev/null; then
        fail "ignored entries: $(jq -c '.results[0].worktree.ignored' "$out"), want the folder with 1 file and 6 bytes"
    elif untouched "$before"; then ok; fi
}
ignored_sized ignored-nfd-folder-size "$nfd"
ignored_sized ignored-nfc-folder-size "$nfc"
# A path rule inside it, written precomposed as a person types it, matches
# on macOS, where Git spells the path that way too. Elsewhere Git keeps the
# disk's spelling, so only a rule in that spelling matches there.
if [ "$(uname -s)" = Darwin ]; then
    layout_removed ignored-nfd-folder-path-rule 'caf*/\n' \
        "mkdir -p '$nfd/sub' && echo notes >'$nfd/sub/notes.txt'" --regenerable "$nfc/sub/*.txt"
else
    layout_removed ignored-nfd-folder-path-rule 'caf*/\n' \
        "mkdir -p '$nfd/sub' && echo notes >'$nfd/sub/notes.txt'" --regenerable "$nfd/sub/*.txt"
fi
layout_removed ignored-nfc-folder-path-rule 'caf*/\n' \
    "mkdir -p '$nfc/sub' && echo notes >'$nfc/sub/notes.txt'" --regenerable "$nfc/sub/*.txt"

# 12b. An ignored `build/` below two tracked folders: `café`, stored
# decomposed, and an emoji, which Apple's converter refuses. Git spells each
# name on its own, `café` precomposed and the emoji as stored, and so does
# the walk: the entry has its size, and the `.env` in it keeps the
# worktree, in either order of the two folders. Without the `.env` it is
# removed. Only macOS precomposes, so the cases run there alone, on a file
# system that keeps a name as written (APFS) or stores it decomposed (HFS+).
emoji=$(printf '\360\237\232\200')
mixed_names() { # NAME PARENT-PATH(of café, may be empty) CHILD-PATH(below café, may be empty) env|none
    name=$1
    local up=$2 down=$3 kind=$4 tracked disk entry before
    tracked=${up:+$up/}$nfc${down:+/$down}
    disk=${up:+$up/}$nfd${down:+/$down}
    entry=$tracked/build/
    if [ "$(uname -s)" != Darwin ]; then
        echo "skip $name: only Git on macOS precomposes names"
        return
    fi
    if [ "$kind" = none ] && [ -n "$partial_probe" ]; then
        skip_partial "$name"
        return
    fi
    make_repo
    printf 'build/\n' >>"$repo/.git/info/exclude"
    setup mkdir -p "$wt/$tracked"
    echo k >"$wt/$tracked/keep.txt"
    setup git -C "$wt" add -A
    setup git -C "$wt" commit -qm names
    w_sha=$(git -C "$repo" rev-parse w) || exit 2
    # A rename through another name stores `café` decomposed.
    setup mv "$wt/${up:+$up/}$nfc" "$wt/${up:+$up/}rename.tmp"
    setup mv "$wt/${up:+$up/}rename.tmp" "$wt/${up:+$up/}$nfd"
    if [ "$(git -C "$wt" config core.precomposeunicode)" != true ] ||
        ! (cd "$wt" && find . -name 'cafe*' | grep -q .); then
        echo "skip $name: Git does not precompose here, or the file system keeps no decomposed name"
        return
    fi
    setup mkdir "$wt/$disk/build"
    printf 'o' >"$wt/$disk/build/out.o"
    [ "$kind" = none ] || printf 'SECRET=1\n' >"$wt/$disk/build/.env"
    before=$(sums "$wt")
    run remove --yes --json "$wt"
    if [ "$kind" = none ]; then
        if [ "$status" != 0 ]; then fail "exit $status, want 0 (failing checks: $(failing_checks 0))"
        elif ! json_ok 1; then fail "stdout is not one valid remove document with 1 result"
        elif [ "$(result 0 result)" != removed ]; then fail "result $(result 0 result), want removed"
        elif [ -e "$wt" ] || registered "$wt"; then fail "the worktree is still there"
        elif common_ok; then ok; fi
        return
    fi
    if [ "$status" != 3 ]; then fail "exit $status, want 3"
    elif ! json_ok 1; then fail "stdout is not one valid remove document with 1 result"
    elif [ "$(result 0 result)" != refused ]; then fail "result $(result 0 result), want refused"
    elif [ "$(failing_checks 0)" != "$(want_failing ignored)" ]; then fail "failing checks: $(failing_checks 0), want $(want_failing ignored)"
    elif ! jq -e --arg p "$entry" '[.results[0].worktree.ignored[] | select(.path == $p)] == [{"path": $p, "regenerable": false, "files": 2, "bytes": 10}]' "$out" >/dev/null; then
        fail "ignored entries: $(jq -c '.results[0].worktree.ignored' "$out"), want $entry with 2 files and 10 bytes, not regenerable"
    elif ! jq -e '.results[0].worktree.checks[] | select(.id == "ignored") | .detail | contains("is an environment file no rule names")' "$out" >/dev/null; then
        fail "the ignored check does not name the environment file: $(jq -c '.results[0].worktree.checks[] | select(.id == "ignored")' "$out")"
    elif untouched "$before"; then ok; fi
}
mixed_names ignored-mixed-names-env '' "$emoji" env
mixed_names ignored-mixed-names-env-swapped "$emoji" '' env
mixed_names ignored-mixed-names-clean '' "$emoji" none

# 13. Untracked folders that hold only ignored entries: residue lists them
# again with one `git status --ignored=traditional` limited to them, not
# the whole worktree (#120). The oracle is Git's own full traditional list,
# which residue's ignored entries must equal; a Git trace counts the
# `git status` calls and the limited ones among them.
git_traditional() { # the ignored paths of a full traditional run in $wt, sorted, |-separated
    git -C "$wt" -c core.quotePath=false status --porcelain=v1 -z --untracked-files=normal \
        --ignored=traditional --ignore-submodules=none | tr '\0' '\n' | sed -n 's/^!! //p' |
        LC_ALL=C sort | paste -s -d '|' -
}

layout_limited() { # NAME STATUS-CALLS LIMITED-CALLS PATTERNS SETUP
    name=$1
    local calls=$2 limited want got before starts
    make_layout "$4" "$5"
    want=$(git_traditional)
    before=$(sums "$wt")
    export GIT_TRACE2_EVENT=$work/$name.trace
    run explain --json "$wt"
    unset GIT_TRACE2_EVENT
    got=$(jq -r '.worktree.ignored[].path' "$out" 2>/dev/null | LC_ALL=C sort | paste -s -d '|' -)
    starts=$(grep '"event":"start"' "$work/$name.trace" | grep '"status"')
    limited=$(printf '%s\n' "$starts" | grep -c 'literal-pathspecs')
    if [ "$status" != 0 ]; then fail "explain exit $status, want 0"
    elif ! jq -e '.schema == "residue.explain/v1"' "$out" >/dev/null 2>&1; then fail "stdout is not one explain document"
    elif [ "$got" != "$want" ]; then fail "ignored entries: $got, want Git's traditional list: $want"
    elif [ "$(printf '%s\n' "$starts" | grep -c .)" != "$calls" ] || [ "$limited" != "$3" ]; then
        fail "git status ran $(printf '%s\n' "$starts" | grep -c .) times, $limited of them limited; want $calls and $3"
    elif untouched "$before"; then ok; fi
}

# Several such folders, at the top and below a tracked one, take one
# limited run.
layout_limited limited-several 2 1 '*.log\nbuild/\n' \
    'mkdir -p logs tmp/cache sub/gen/deep && echo a >logs/a.log && echo c >tmp/cache/c.log && echo g >sub/gen/deep/g.log'
# Each name means only itself: read as a glob, `sub/[slug]` would be
# `sub/s`, and `:x` would be pathspec magic.
layout_limited limited-literal-names 2 1 '*.log\n' \
    'mkdir -p -- "sub/[slug]" sub/s "a*b" "q?" :x ":(exclude)y" -d && for d in "sub/[slug]" sub/s "a*b" "q?" :x ":(exclude)y" -d; do echo l >"$d/x.log"; done'
# An empty ignored folder outside the limited folder is still dropped;
# inside one, traditional mode's answer stands.
layout_limited limited-empty-outside 2 1 'secrets/\n*.log\nbuild/\n' \
    'mkdir -p secrets sub/build logs/build && echo a >logs/a.log'
# A nested repository in a limited folder, and one in an ignored folder
# outside it that holds nothing else: that one is looked up with a second
# limited run, never the whole worktree.
layout_limited limited-nested-repos 3 2 '*.log\nbuild/\n' \
    'mkdir -p u sub/build && git init -q u/dep && echo a >u/a.log && git init -q sub/build/dep'
# Past 64 folders, the whole worktree runs in traditional mode.
layout_limited limited-past-cap 2 0 '*.log\n' \
    'i=0; while [ $i -lt 65 ]; do mkdir u$i && echo l >u$i/x.log; i=$((i + 1)); done'

# 14. --as-of TIME: a run confirming a plan made at TIME judges every age
# at TIME. TIME is 2026-01-01T00:01:00Z, so an entry trashed or a record
# written at 2026-01-01T00:00:30Z is 30 seconds old at TIME, under
# --keep-trash 1m and --keep-removed 1m, and long expired now. Each case
# needs a move into the trash and a removal, so a partial probe skips them.

# Trash t (with --keep-trash 1m) and remove r (with --keep-removed 1m) in
# $repo, each keeping 1m of its own, and say both happened at AT, Unix
# seconds and RFC 3339.
as_of_fixture() { # AT AT-RFC3339
    setup git -C "$repo" worktree add -q "$repo/.worktrees/t" -b t
    setup git -C "$repo" worktree add -q "$repo/.worktrees/r" -b r
    run remove --yes --json --trash --keep-trash 1m "$repo/.worktrees/t"
    entry=$(result 0 trash_entry)
    [ "$status" = 0 ] && [ -d "$entry" ] || { echo "contract-check: setup failed in $name: remove --trash t exit $status" >&2; exit 2; }
    run remove --yes --json --keep-removed 1m "$repo/.worktrees/r"
    rec=$(result 0 removal_record)
    [ "$status" = 0 ] && [ -d "$rec" ] || { echo "contract-check: setup failed in $name: remove r exit $status" >&2; exit 2; }
    sed "s/^trashed [0-9]* .*/trashed $1 $2/" "$entry/info" >"$work/$name.info" && cat "$work/$name.info" >"$entry/info" || exit 2
    sed "s/^removed [0-9]* .*/removed $1 $2/" "$rec/info" >"$work/$name.info" && cat "$work/$name.info" >"$rec/info" || exit 2
}

# The sweep that confirms the plan: it removes nothing (w and u changed
# within --idle 30d), and purges and forgets what expired by TIME.
as_of_sweep() { # ARGS...
    run remove --all --yes --json --idle 30d --keep-trash 1m --keep-removed 1m "$@" "$repo"
}

taken() { # what the run purged and forgot
    jq -r '[(.purged[] | .result + " " + .entry.entry), (.forgotten[] | .result + " " + .entry.entry)] | join(",")' "$out"
}

if [ -z "$partial_probe" ]; then
    # An entry and a record 30 seconds old at TIME stay; a run without
    # --as-of takes both.
    name=as-of-ages
    make_repo
    as_of_fixture 1767225630 2026-01-01T00:00:30Z
    touch -t 202512310000 "$entry" "$rec" "$repo/.git/worktrees" || exit 2
    as_of_sweep --as-of 2026-01-01T00:01:00Z
    if [ "$status" != 0 ]; then fail "exit $status, want 0"
    elif ! json_ok 2; then fail "stdout is not one valid remove document with 2 results"
    elif [ "$(jq -r .as_of "$out")" != 2026-01-01T00:01:00Z ]; then fail "as_of $(jq -r .as_of "$out"), want 2026-01-01T00:01:00Z"
    elif [ -n "$(taken)" ]; then fail "took what was 30 seconds old at TIME: $(taken)"
    elif [ ! -d "$entry" ] || [ ! -d "$rec" ]; then fail "the trash entry or the record is gone"
    else
        as_of_sweep
        if [ "$status" != 0 ]; then fail "without --as-of: exit $status, want 0"
        elif [ "$(taken)" != "purged $(basename "$entry"),forgotten $(basename "$rec")" ]; then fail "without --as-of, took: $(taken)"
        elif [ "$(jq -r '.as_of == .generated_at' "$out")" != true ]; then fail "without --as-of, as_of $(jq -r .as_of "$out") is not generated_at"
        elif common_ok; then ok; fi
    fi

    # Expired at TIME, but changed after it: the entry's directory, where
    # a stalled run wrote info or moved the worktree in, and Git's list of
    # worktrees, where a removal still running at TIME finished. A plan at
    # TIME could not have listed either, so both stay; once both changed
    # before TIME, the same run takes both.
    name=as-of-changed-after
    make_repo
    as_of_fixture 1767225600 2026-01-01T00:00:00Z
    touch -t 202512310000 "$rec" || exit 2
    touch "$entry" "$repo/.git/worktrees" || exit 2
    as_of_sweep --as-of 2026-01-01T02:01:00+02:00
    if [ "$status" != 0 ]; then fail "exit $status, want 0"
    elif [ "$(jq -r .as_of "$out")" != 2026-01-01T00:01:00Z ]; then fail "as_of $(jq -r .as_of "$out"), want 2026-01-01T00:01:00Z"
    elif [ -n "$(taken)" ]; then fail "took what changed after TIME: $(taken)"
    elif [ ! -d "$entry" ] || [ ! -d "$rec" ]; then fail "the trash entry or the record is gone"
    else
        touch -t 202512310000 "$entry" "$repo/.git/worktrees" || exit 2
        as_of_sweep --as-of 2026-01-01T00:01:00Z
        if [ "$status" != 0 ]; then fail "once unchanged since TIME: exit $status, want 0"
        elif [ "$(taken)" != "purged $(basename "$entry"),forgotten $(basename "$rec")" ]; then fail "once unchanged since TIME, took: $(taken)"
        elif [ -e "$entry" ] || [ -e "$rec" ]; then fail "the trash entry or the record is still on disk"
        elif common_ok; then ok; fi
    fi
else
    skip_partial as-of-ages
    skip_partial as-of-changed-after
fi

# A TIME after the run's start counts as the start: the flag never makes a
# run purge sooner.
name=as-of-later-than-start
make_repo
before=$(sums "$wt")
run remove --all --yes --json --idle 30d --as-of 2100-01-01T00:00:00Z "$repo"
if [ "$status" != 0 ]; then fail "exit $status, want 0"
elif [ "$(jq -r '.as_of == .generated_at' "$out")" != true ]; then fail "as_of $(jq -r .as_of "$out"), generated_at $(jq -r .generated_at "$out")"
elif untouched "$before"; then ok; fi

# 15. A bare repository with a linked worktree per branch, the layout
# `git clone --bare` and then `git worktree add` makes. The worktree on the
# branch the bare repository's HEAD names is its main checkout in all but
# name: scan, explain and remove keep it (`head` blocks, saying why). A
# worktree on another branch there is a candidate as anywhere else.
name=bare-head-branch
base=$work/$name
setup git init -q "$base/src"
echo hello >"$base/src/README.md"
setup git -C "$base/src" add .
setup git -C "$base/src" commit -qm init
setup git clone -q --bare "$base/src" "$base/repo.git"
setup git -C "$base/repo.git" worktree add -q "$base/main" main
setup git -C "$base/repo.git" worktree add -q "$base/feat" -b feat
head_detail="on main, the branch the bare repository's HEAD names; kept like a main checkout"
bare_head() { # the main worktree's head check in a scan or explain document: STATUS DETAIL
    jq -r --arg p "$(cd "$base/main" && pwd -P)" '
        [.. | objects | select(.path? == $p and has("checks")) | .checks[] | select(.id == "head") | .status + " " + .detail] | first // empty
    ' "$out"
}
# One refusal makes the run exit 3; on a partial probe feat is kept too.
if [ -z "$partial_probe" ]; then want_feat=removed; else want_feat=refused; fi
run scan --json "$base"
if [ "$status" != 0 ]; then fail "scan exit $status, want 0"
elif [ "$(bare_head)" != "block $head_detail" ]; then fail "scan's head check for main: $(bare_head)"
else
    run explain --json "$base/main"
    if [ "$status" != 0 ]; then fail "explain exit $status, want 0"
    elif [ "$(bare_head)" != "block $head_detail" ]; then fail "explain's head check: $(bare_head)"
    else
        run remove --yes --json "$base/main" "$base/feat"
        if [ "$status" != 3 ]; then fail "remove exit $status, want 3"
        elif ! json_ok 2; then fail "stdout is not one valid remove document with 2 results"
        elif [ "$(result 0 result),$(result 1 result)" != "refused,$want_feat" ]; then fail "results $(result 0 result),$(result 1 result), want refused,$want_feat"
        elif [ "$(failing_checks 0)" != "$(want_failing head)" ]; then fail "main's failing checks: $(failing_checks 0), want $(want_failing head)"
        elif [ ! -f "$base/main/README.md" ]; then fail "main's files are gone"
        elif [ -z "$partial_probe" ] && [ -e "$base/feat" ]; then fail "feat is still on disk"
        else ok; fi
    fi
fi

# 15b. A trash entry on the branch a bare repository's HEAD names is kept
# like that branch's worktree: the purge's plan judges it as its re-check
# does, so the entry stays, kept by `head`, and the run does not fail.
# Once HEAD names another branch, the next sweep purges it.
if [ -z "$partial_probe" ]; then
    name=bare-head-trash
    base=$work/$name
    setup git init -q "$base/src"
    echo hello >"$base/src/README.md"
    setup git -C "$base/src" add .
    setup git -C "$base/src" commit -qm init
    setup git clone -q --bare "$base/src" "$base/repo.git"
    setup git -C "$base/repo.git" worktree add -q "$base/main" main
    setup git -C "$base/repo.git" worktree add -q "$base/feat" -b feat
    run remove --yes --json --trash "$base/feat"
    entry=$(result 0 trash_entry)
    [ "$status" = 0 ] && [ -d "$entry" ] || { echo "contract-check: setup failed in $name: remove --trash exit $status" >&2; exit 2; }
    sed 's/^trashed [0-9]* .*/trashed 1767225600 2026-01-01T00:00:00Z/' "$entry/info" >"$work/$name.info" && cat "$work/$name.info" >"$entry/info" || exit 2
    setup git -C "$base/repo.git" symbolic-ref HEAD refs/heads/feat
    run remove --all --yes --json --idle 30d "$base/repo.git"
    if [ "$status" != 0 ]; then fail "exit $status, want 0"
    elif [ "$(jq -r '[.purged[] | .result] | join(",")' "$out")" != kept ]; then fail "purged: $(jq -c '[.purged[] | {result, reason}]' "$out")"
    elif [ "$(jq -r '[.purged[0].worktree.checks[] | select(.status != "pass") | .id] | join(",")' "$out")" != head ]; then fail "the entry's failing checks: $(jq -c '[.purged[0].worktree.checks[] | select(.status != "pass") | {id, detail}]' "$out")"
    elif [ ! -f "$entry/feat/README.md" ]; then fail "the trashed worktree's files are gone"
    else
        setup git -C "$base/repo.git" symbolic-ref HEAD refs/heads/main
        run remove --all --yes --json --idle 30d "$base/repo.git"
        if [ "$status" != 0 ]; then fail "with HEAD on main again: exit $status, want 0"
        elif [ "$(jq -r '[.purged[] | .result] | join(",")' "$out")" != purged ]; then fail "with HEAD on main again, purged: $(jq -c '[.purged[] | {result, reason}]' "$out")"
        elif [ -e "$entry" ]; then fail "the entry is still there"
        elif [ ! -f "$base/main/README.md" ]; then fail "main's files are gone"
        else ok; fi
    fi
else
    skip_partial bare-head-trash
fi

# 16. A process inside a worktree keeps it in use whatever bytes its path
# holds, though a probe may print such a name escaped (lsof on macOS): é,
# CJK and a backslash in the worktree's name (given by `git worktree
# move`), an emoji in a folder above it, and a process whose only link is
# an open file in a worktree below a folder named with ï. The last two are
# not moved, so they test the bytes alone. Then the open file once more,
# with the worktree moved after the file was opened: illumos may then have
# no path for the file, and finds it by its inode, or else answers unknown
# (`unnamed_file`), never idle. Each holder is a sleep. `explain` and `remove --yes` must keep the
# worktree, untouched; once the sleep exits it is idle again, which shows
# the use was the sleep's. On a partial probe, which reads no open files,
# the open-file cases are kept as unknown (partial_probe) instead.
held() { # NAME WORKTREE-NAME|w cwd|open|open-then-move [REPO-DIR]
    name=$1
    make_repo "${4:-}"
    local dest=$repo/.worktrees/$2 before holder i busy explain_status
    local partial=false unnamed=false
    case $3 in open*) [ -z "$partial_probe" ] || partial=true ;; esac
    if [ "$3" = open-then-move ] && [ "$(uname -s)" = SunOS ]; then unnamed=true; fi
    if [ "$2" != w ] && [ "$3" != open-then-move ]; then
        setup git -C "$repo" worktree move "$wt" "$dest"
        wt=$dest
    fi
    before=$(sums "$wt")
    if [ "$3" = cwd ]; then
        (cd "$wt/sub" && exec sleep 300) >/dev/null 2>&1 &
    else
        (cd "$work" && exec sleep 300 3<"$wt/sub/file.txt") >/dev/null 2>&1 &
    fi
    holder=$!
    holders="$holders $holder"
    for ((i = 0; i < 100; i++)); do
        case $(ps -o comm= -p "$holder" 2>/dev/null) in *sleep) break ;; esac
        sleep 0.1
    done
    if [ "$3" = open-then-move ]; then
        setup git -C "$repo" worktree move "$wt" "$dest"
        wt=$dest
    fi
    # The sleep's use, or an answer that cannot show the worktree unused.
    local kept='def kept:
        if $partial then .state == "unknown" and .reason == "partial_probe"
        else (.state == "in_use" and any(.processes[]; .pid == $pid and .command == "sleep"))
            or ($unnamed and .state == "unknown" and .reason == "unnamed_file") end;'
    run explain --json "$wt"
    busy=$work/$name.busy
    explain_status=$status
    mv "$out" "$busy" || exit 2
    run remove --yes --json --no-trash "$wt"
    kill "$holder" 2>/dev/null
    wait "$holder" 2>/dev/null
    if [ "$explain_status" != 0 ]; then fail "explain exit $explain_status, want 0"
    elif ! jq -e --argjson pid "$holder" --argjson partial "$partial" --argjson unnamed "$unnamed" "$kept"'
        .worktree | .verdict == (if .activity.state == "in_use" then "keep" else "unknown" end)
            and (.activity | kept)' "$busy" >/dev/null; then
        fail "with sleep[$holder] using it, explain says $(jq -c '[.worktree.verdict, .worktree.activity]' "$busy")"
    elif [ "$status" != 3 ]; then fail "remove --yes exit $status, want 3"
    elif ! json_ok 1; then fail "stdout is not one valid remove document with 1 result"
    elif [ "$(result 0 result)" != refused ]; then fail "result $(result 0 result), want refused"
    elif [ "$(failing_checks 0)" != activity ]; then fail "failing checks: $(failing_checks 0), want activity"
    elif ! jq -e --argjson pid "$holder" --argjson partial "$partial" --argjson unnamed "$unnamed" "$kept"'
        .results[0].worktree.activity | kept' "$out" >/dev/null; then
        fail "with sleep[$holder] using it, remove's activity is $(jq -c '.results[0].worktree.activity' "$out")"
    elif untouched "$before"; then
        run explain --json "$wt"
        if [ "$status" != 0 ]; then fail "after sleep exited, explain exit $status, want 0"
        elif [ -z "$partial_probe" ] && ! jq -e '.worktree.activity.state == "not_observed" and .worktree.verdict == "candidate"' "$out" >/dev/null; then
            fail "after sleep exited, explain says $(jq -c '[.worktree.verdict, .worktree.activity]' "$out"), want candidate and not_observed"
        elif [ -n "$partial_probe" ] && ! jq -e '.worktree.activity.state == "unknown" and .worktree.activity.reason == "partial_probe"' "$out" >/dev/null; then
            fail "after sleep exited, explain says $(jq -c '.worktree.activity' "$out"), want unknown (partial_probe)"
        else
            ok
            if [ "$3" = open-then-move ]; then
                echo "     while the sleep held it: $(jq -c '.worktree.activity | {state, reason}' "$busy")"
            fi
        fi
    fi
}
held activity-cwd-accented "$(printf 'caf\303\251')" cwd
held activity-cwd-cjk "$(printf '\346\274\242\345\255\227')" cwd
held activity-cwd-backslash 'back\slash' cwd
held activity-cwd-emoji-above w cwd "$work/$(printf '\360\237\232\200')-repo"
held activity-open-file-only w open "$work/$(printf 'na\303\257ve')-repo"
held activity-open-file-moved moved open-then-move

# 17. A probe names a path whole only below a length (PATH_LIMIT in
# src/activity.rs): lsof on macOS prints a longer one cut short, Linux's
# /proc names none at 4,096 bytes, and the BSDs' and illumos's kernels none
# at 1,024. A worktree that holds a path that long is unknown (long_path),
# with or without a process there, and never a candidate; a process the
# probe does name is in_use. The long path is ignored folders below
# node_modules/, made one at a time, since a system call takes no path that
# long; the deepest is longer than both the limit and 1,024 bytes, so lsof
# cuts it. A sleep holds it, then one in a shallow folder, then none. Once
# the long folders are gone the worktree is a candidate again. FreeBSD's
# kernel gives the deep sleep's folder by its filesystem and inode, which
# the walk finds, so there it is in_use instead.
long_path_case() {
    name=activity-long-path
    make_repo
    local limit deep_at part parts=() i before holder shallow len
    case $(uname -s) in
    Darwin) limit=1004 ;;
    Linux) limit=4096 ;;
    *) limit=1024 ;;
    esac
    deep_at=$((limit > 1024 ? limit + 16 : 1024 + 16))
    len=$(($(cd "$wt" && pwd -P | wc -c) - 1 + 13)) # "/node_modules"
    while [ "$len" -lt "$deep_at" ]; do
        i=$((deep_at - len - 1))
        [ "$i" -gt 200 ] && i=200
        [ "$i" -lt 1 ] && i=1
        part=$(printf '%*s' "$i" '' | tr ' ' d)
        parts+=("$part")
        len=$((len + 1 + i))
    done
    (cd "$wt/node_modules" && for part in "${parts[@]}"; do mkdir "$part" && cd "$part" || exit 1; done && : >deep.js) || { echo "contract-check: setup failed in $name: the long folders" >&2; exit 2; }
    before=$(sums "$wt")
    (cd "$wt/node_modules" && for part in "${parts[@]}"; do cd "$part" || exit 1; done && exec sleep 300) >/dev/null 2>&1 &
    holder=$!
    holders="$holders $holder"
    for ((i = 0; i < 100; i++)); do
        case $(ps -o comm= -p "$holder" 2>/dev/null) in *sleep) break ;; esac
        sleep 0.1
    done
    run explain --json "$wt"
    mv "$out" "$work/$name.deep" || exit 2
    local deep_status=$status
    run remove --yes --json --no-trash "$wt"
    kill "$holder" 2>/dev/null
    wait "$holder" 2>/dev/null
    local kept='(.state == "unknown" and .reason == "long_path") or (.state == "in_use" and any(.processes[]; .pid == $pid))'
    if [ "$deep_status" != 0 ]; then fail "explain exit $deep_status, want 0"; return
    elif ! jq -e --argjson pid "$holder" ".worktree.verdict != \"candidate\" and (.worktree.activity | $kept)" "$work/$name.deep" >/dev/null; then
        fail "with sleep[$holder] at a path $deep_at bytes long, explain says $(jq -c '[.worktree.verdict, .worktree.activity]' "$work/$name.deep"), want unknown (long_path)"; return
    elif [ "$status" != 3 ]; then fail "remove --yes exit $status, want 3"; return
    elif ! json_ok 1 || [ "$(result 0 result)" != refused ]; then fail "result $(result 0 result), want refused"; return
    elif ! grep -qx activity <<<"$(failing_checks 0 | tr , '\n')"; then fail "failing checks: $(failing_checks 0), want activity among them"; return
    elif ! untouched "$before"; then return
    fi
    (cd "$wt/node_modules/${parts[0]}" && exec sleep 300) >/dev/null 2>&1 &
    shallow=$!
    holders="$holders $shallow"
    for ((i = 0; i < 100; i++)); do
        case $(ps -o comm= -p "$shallow" 2>/dev/null) in *sleep) break ;; esac
        sleep 0.1
    done
    run explain --json "$wt"
    kill "$shallow" 2>/dev/null
    wait "$shallow" 2>/dev/null
    if ! jq -e --argjson pid "$shallow" '.worktree.activity | .state == "in_use" and any(.processes[]; .pid == $pid)' "$out" >/dev/null; then
        fail "with sleep[$shallow] in a shallow folder, explain says $(jq -c '.worktree.activity' "$out"), want in_use"; return
    fi
    run explain --json "$wt"
    if ! jq -e '.worktree.verdict != "candidate" and (.worktree.activity | .state == "unknown" and .reason == "long_path")' "$out" >/dev/null; then
        fail "with no process there, explain says $(jq -c '[.worktree.verdict, .worktree.activity]' "$out"), want unknown (long_path)"; return
    fi
    rm -rf -- "${wt:?}/node_modules/${parts[0]}"
    run explain --json "$wt"
    if ! jq -e '.worktree.verdict == "candidate" and .worktree.activity.state == "not_observed"' "$out" >/dev/null; then
        fail "once the long folders are gone, explain says $(jq -c '[.worktree.verdict, .worktree.activity]' "$out"), want candidate and not_observed"; return
    fi
    ok
}
if [ -z "$partial_probe" ]; then long_path_case; else skip_partial activity-long-path; fi

# 17b. Moved aside to be deleted, a worktree's paths grow by as much as its
# own: its hidden name, .NAME.residue-removing-PID, or its trash entry is
# longer than its path. A worktree whose longest path is 10 bytes short of
# the limit here reaches it there, so the plan keeps it as unknown
# (long_path) and remove refuses it before it moves anything, rather than
# moving it aside and back on every run.
near_limit_case() {
    name=activity-near-path-limit
    make_repo
    local limit target part parts=() i before len
    case $(uname -s) in
    Darwin) limit=1004 ;;
    Linux) limit=4096 ;;
    *) limit=1024 ;;
    esac
    # The folders end 8 bytes short of the target, for "/deep.js".
    target=$((limit - 10 - 8))
    len=$(($(cd "$wt" && pwd -P | wc -c) - 1 + 13)) # "/node_modules"
    while [ "$len" -lt "$target" ]; do
        i=$((target - len - 1))
        [ "$i" -gt 200 ] && i=200
        [ "$i" -lt 1 ] && i=1
        part=$(printf '%*s' "$i" '' | tr ' ' d)
        parts+=("$part")
        len=$((len + 1 + i))
    done
    [ "$len" = "$target" ] || { echo "contract-check: setup failed in $name: the folders are $len bytes, want $target" >&2; exit 2; }
    (cd "$wt/node_modules" && for part in "${parts[@]}"; do mkdir "$part" && cd "$part" || exit 1; done && : >deep.js) || { echo "contract-check: setup failed in $name: the long folders" >&2; exit 2; }
    before=$(sums "$wt")
    run explain --json "$wt"
    if [ "$status" != 0 ]; then fail "explain exit $status, want 0"; return
    elif ! jq -e '.worktree.verdict != "candidate" and (.worktree.activity | .state == "unknown" and .reason == "long_path" and (.detail | contains("where residue deletes it")))' "$out" >/dev/null; then
        fail "with a path $((limit - 10)) bytes long, explain says $(jq -c '[.worktree.verdict, .worktree.activity]' "$out"), want unknown (long_path) where residue deletes it"; return
    fi
    run remove --yes --json --no-trash "$wt"
    if [ "$status" != 3 ]; then fail "remove --yes exit $status, want 3"; return
    elif ! json_ok 1 || [ "$(result 0 result)" != refused ]; then fail "result $(result 0 result): $(result 0 reason), want refused at the plan"; return
    elif ! grep -qx activity <<<"$(failing_checks 0 | tr , '\n')"; then fail "failing checks: $(failing_checks 0), want activity among them"; return
    elif ! untouched "$before"; then return
    fi
    ok
}
if [ -z "$partial_probe" ]; then near_limit_case; else skip_partial activity-near-path-limit; fi

# 18. FreeBSD, as root, with an ordinary user named smoke (CI makes one):
# with security.bsd.unprivileged_proc_debug at 0 the kernel shows that user
# the files of none of their processes but residue's own, so a sleep of
# theirs in a worktree cannot be seen, and its activity must be unknown
# (probe_failed), never not_observed. With the setting as it was, the same
# sleep is in_use. The setting is put back right after. That second run
# also has processes of smoke's that the kernel shows to root alone,
# whatever the setting: an ssh-agent, which turns tracing off (P2_NOTRACE),
# and, where this machine's sshd takes a key, an SSH session to localhost,
# whose process for the login changed its credentials without an exec
# (P_SUGID). They are not inspected, as on Linux, so the sleep is still
# in_use; the log prints their flags.
proc_debug_case() {
    name=activity-proc-debug-off
    local was dir abs_bin on session='' login=''
    was=$(sysctl -n security.bsd.unprivileged_proc_debug) || { fail "cannot read security.bsd.unprivileged_proc_debug"; return; }
    abs_bin=$(cd "$(dirname "$bin")" && pwd)/$(basename "$bin")
    dir=$work/$name
    mkdir -p "$dir/home"
    make_repo "$dir/repo"
    # A sleep in w's sub/, and residue's explain of w, both as smoke.
    cat >"$dir/explain.sh" <<SCRIPT
cd '$wt/sub' || exit 2
sleep 300 >/dev/null 2>&1 &
held=\$!
agent=
if [ "\$1" = agent ]; then
    eval "\$(ssh-agent -s)" >/dev/null || exit 2
    agent=\$SSH_AGENT_PID
    echo "ssh-agent[\$agent] flags \$(ps -o flags= -o flags2= -p "\$agent")" >'$dir/agent.flags'
fi
sleep 1
HOME='$dir/home' NO_COLOR=1 LC_ALL=C '$abs_bin' explain --json '$wt'
s=\$?
kill "\$held" 2>/dev/null
[ -z "\$agent" ] || kill "\$agent" 2>/dev/null
exit "\$s"
SCRIPT
    chmod 711 "$work"
    chown -R smoke "$dir"
    out=$work/$name.out err=$work/$name.err
    sysctl security.bsd.unprivileged_proc_debug=0 >/dev/null || { chmod 700 "$work"; fail "cannot set security.bsd.unprivileged_proc_debug"; return; }
    su smoke -c "sh $dir/explain.sh" >"$out" 2>"$err"
    sysctl security.bsd.unprivileged_proc_debug="$was" >/dev/null || fail "cannot put security.bsd.unprivileged_proc_debug back to $was"
    # An SSH session as smoke, from a key made here, where sshd takes it.
    if ssh-keygen -q -t ed25519 -N '' -f "$work/$name.key" &&
        install -d -o smoke -m 700 ~smoke/.ssh &&
        install -o smoke -m 600 "$work/$name.key.pub" ~smoke/.ssh/authorized_keys; then
        ssh -n -i "$work/$name.key" -o BatchMode=yes -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
            smoke@127.0.0.1 'sleep 120' >"$work/$name.ssh" 2>&1 &
        session=$!
        holders="$holders $session"
        for ((i = 0; i < 100; i++)); do
            login=$(ps -U smoke -o pid= -o comm= | while read -r pid comm; do [ "${comm#sshd}" = "$comm" ] || { echo "$pid" && break; }; done)
            [ -n "$login" ] && break
            kill -0 "$session" 2>/dev/null || break
            sleep 0.1
        done
    fi
    if [ -n "$login" ]; then
        echo "     $(ps -o comm= -p "$login")[$login] flags $(ps -o flags= -o flags2= -p "$login")"
    else
        echo "     no SSH session as smoke: $(head -c 300 "$work/$name.ssh" 2>/dev/null)"
    fi
    on=$(su smoke -c "sh $dir/explain.sh agent" 2>&1)
    [ -z "$session" ] || kill "$session" 2>/dev/null
    echo "     $(cat "$dir/agent.flags" 2>/dev/null)"
    chmod 700 "$work"
    if ! jq -e '.worktree.verdict != "candidate" and (.worktree.activity | .state == "unknown" and .reason == "probe_failed" and (.detail | contains("security.bsd.unprivileged_proc_debug is 0")))' "$out" >/dev/null; then
        fail "with the setting at 0, explain says $(jq -c '[.worktree.verdict, .worktree.activity]' "$out" 2>&1)"
    elif ! jq -e '.worktree.activity | .state == "in_use" and any(.processes[]; .command == "sleep")' <<<"$on" >/dev/null; then
        fail "with the setting at $was, explain says: $(head -c 600 <<<"$on")"
    else ok; fi
}
if [ "$(uname -s)" = FreeBSD ] && [ "$(id -u)" = 0 ] && id smoke >/dev/null 2>&1; then
    proc_debug_case
else
    echo "skip activity-proc-debug-off: it runs on FreeBSD as root, with a user named smoke"
fi

# 19. --expect PLAN: a run that confirms a plan acts only while its own
# plan, made before it moves anything, is that one. Each case shows a plan
# with `remove --trash --json` and no --yes, changes something or nothing,
# then runs the same command with --yes --as-of GENERATED_AT --expect PLAN.
# The fixture is as_of_fixture's: an expired trash entry and an expired
# removal record that every plan here lists, so a run that does nothing is
# seen to purge and forget nothing too.

# The plan: sets $plan and $gen from a run without --yes.
expect_preview() { # PATH...
    run remove --trash --json --keep-trash 1m --keep-removed 1m "$@"
    plan=$(jq -r .plan "$out") gen=$(jq -r .generated_at "$out")
    if [ "$status" != 3 ] || [ "${#plan}" != 32 ] || [ -n "$(printf %s "$plan" | tr -d 0-9a-f)" ]; then
        echo "contract-check: setup failed in $name: the plan run exited $status with plan $plan" >&2
        exit 2
    fi
}

# The run that confirms it.
expect_confirm() { # PATH...
    run remove --trash --json --keep-trash 1m --keep-removed 1m --yes --as-of "$gen" --expect "$plan" "$@"
}

# An expired trash entry ($entry, holding t) and an expired removal record
# ($rec) beside w and u, both unchanged since well before any plan, as is
# Git's list of worktrees: a change to it in the plan's second would leave
# the record for a later run (--as-of). Each WORKTREE named is added first.
expect_fixture() { # [WORKTREE...]
    make_repo
    as_of_fixture 1767225600 2026-01-01T00:00:00Z
    local add
    for add in "$@"; do
        setup git -C "$repo" worktree add -q "$repo/.worktrees/$add" -b "$add"
    done
    touch -t 202512310000 "$entry" "$rec" "$repo/.git/worktrees" || exit 2
}

# The run did nothing because its plan is not $plan: exit 4, and its
# document gives its own plan, and says so for each thing it would act on.
expect_differs() { # N-RESULTS
    if [ "$status" != 4 ]; then
        fail "exit $status, want 4"
        return 1
    fi
    json_ok "$1" || {
        fail "stdout is not one valid remove document with $1 results"
        return 1
    }
    jq -e --arg plan "$plan" '
        .expected_plan == $plan and .plan != $plan and (.plan | test("^[0-9a-f]{32}$"))
        and all(.results[], .purged[], .forgotten[]; .result as $r | ["removed", "trashed", "purged", "forgotten"] | index([$r]) == null)
        and any(.results[], .purged[], .forgotten[]; .result == "skipped" and (.reason | startswith("the plan is not the one --expect names")))
    ' "$out" >/dev/null || {
        fail "the document does not say the plan differs: $(jq -c '{plan, expected_plan, results: [(.results[], .purged[], .forgotten[]) | [.result, .reason]]}' "$out")"
        return 1
    }
}

# Nothing was removed, moved aside, trashed, purged or forgotten: w has the
# same bytes and Git lists it at its path, the trash holds only $entry,
# which still holds t, the removal record and its ref are there, and u is
# untouched.
expect_nothing_done() { # BEFORE-SUMS-OF-W
    registered "$wt" || {
        fail "w is no longer registered"
        return 1
    }
    [ "$(sums "$wt")" = "$1" ] || {
        fail "w's files changed"
        return 1
    }
    registered "$repo/.worktrees/u" && [ "$(sums "$repo/.worktrees/u")" = "$u_sum" ] || {
        fail "the unrelated worktree u changed"
        return 1
    }
    [ "$(ls -A "$repo/.git/residue-trash")" = "$(basename "$entry")" ] && registered "$entry/t" || {
        fail "the trash holds $(ls -A "$repo/.git/residue-trash" | tr '\n' ' '), not only $(basename "$entry") with t in it"
        return 1
    }
    [ -f "$rec/info" ] && git -C "$repo" rev-parse -q --verify "refs/residue/removed/$(basename "$rec")" >/dev/null || {
        fail "the removal record or its ref is gone"
        return 1
    }
    local leftover
    for leftover in "$repo"/.worktrees/.*.residue-removing-*; do
        [ ! -e "$leftover" ] || {
            fail "$leftover was left behind"
            return 1
        }
    done
}

if [ -z "$partial_probe" ]; then
    # The plan, confirmed with its PATHs in the other order: w and v go to
    # the trash, the entry is purged and the record forgotten. The text
    # plan ends with the same `plan:` line.
    name=expect-matches
    expect_fixture v
    run remove --trash --keep-trash 1m --keep-removed 1m "$wt" "$repo/.worktrees/v"
    text_plan=$(sed -n 's/^plan: //p' "$out")
    expect_preview "$wt" "$repo/.worktrees/v"
    common=$(cd "$repo/.git" && pwd -P)
    expect_confirm "$repo/.worktrees/v" "$wt"
    if [ "$status" != 0 ]; then fail "exit $status, want 0"
    elif ! json_ok 2; then fail "stdout is not one valid remove document with 2 results"
    elif [ "$text_plan" != "$plan" ]; then fail "the text plan says plan: $text_plan, the JSON $plan"
    elif ! jq -e --arg plan "$plan" --arg common "$common" '
        .plan == $plan and .expected_plan == $plan
        and ([.results[] | .result + " " + .repository] | unique) == ["trashed " + $common]
        and [.purged[].result] == ["purged"] and [.forgotten[].result] == ["forgotten"]
    ' "$out" >/dev/null; then fail "results: $(jq -c '{plan, expected_plan, r: [.results[] | [.result, .repository]], p: [.purged[].result], f: [.forgotten[].result]}' "$out")"
    elif [ -e "$wt" ] || [ -e "$repo/.worktrees/v" ]; then fail "w or v is still at its path"
    elif [ -e "$entry" ] || [ -e "$rec" ]; then fail "the trash entry or the removal record is still on disk"
    elif ! registered "$repo/.worktrees/u" || [ "$(sums "$repo/.worktrees/u")" != "$u_sum" ]; then fail "the unrelated worktree u changed"
    else ok; fi

    # w switched to another branch after the plan: exit 4, nothing done.
    # The plan that run reported, made at its as_of, confirmed in turn,
    # runs.
    name=expect-branch-switched
    expect_fixture
    expect_preview "$wt"
    setup git -C "$wt" checkout -q -b other
    before=$(sums "$wt")
    expect_confirm "$wt"
    if expect_differs 1 && expect_nothing_done "$before"; then
        plan=$(jq -r .plan "$out") gen=$(jq -r .as_of "$out")
        expect_confirm "$wt"
        if [ "$status" != 0 ]; then fail "the plan it reported, confirmed: exit $status, want 0"
        elif [ "$(jq -r '[.results[].result, .purged[].result, .forgotten[].result] | join(",")' "$out")" != trashed,purged,forgotten ]; then
            fail "the plan it reported, confirmed: $(jq -c '[.results[].result, .purged[].result, .forgotten[].result]' "$out")"
        elif [ -e "$wt" ]; then fail "the plan it reported, confirmed: w is still at its path"
        else ok; fi
    fi

    # w's HEAD moved on the same branch after the plan: exit 4, nothing
    # done.
    name=expect-head-moved
    expect_fixture
    expect_preview "$wt"
    setup git -C "$wt" commit -q --allow-empty -m more
    before=$(sums "$wt")
    expect_confirm "$wt"
    expect_differs 1 && expect_nothing_done "$before" && ok

    # w removed by hand and another repository's worktree added at its
    # path, on a branch of the same name: exit 4, and the new worktree,
    # the trash entry and the removal record stay as they are.
    name=expect-recreated
    expect_fixture
    expect_preview "$wt"
    setup git -C "$repo" worktree remove "$wt"
    other=$work/$name-other
    setup git init -q "$other"
    echo other >"$other/README.md"
    setup git -C "$other" add .
    setup git -C "$other" commit -qm other
    setup git -C "$other" worktree add -q "$wt" -b w
    before=$(sums "$wt")
    expect_confirm "$wt"
    if ! expect_differs 1; then :
    elif ! git -C "$other" worktree list --porcelain | grep -qxF "worktree $(cd "$wt" && pwd -P)"; then fail "the other repository no longer lists its worktree"
    elif [ "$(sums "$wt")" != "$before" ]; then fail "the new worktree's files changed"
    elif [ ! -f "$rec/info" ] || ! registered "$entry/t"; then fail "the trash entry or the removal record changed"
    else ok; fi

    # An expired trash entry that appears after the plan, unchanged since
    # before the plan's time, is one this run would purge: exit 4, nothing
    # done. Once its directory changed after that time, --as-of leaves it
    # for a later run, the plan is the one confirmed, and w goes.
    name=expect-trash-appeared
    make_repo
    expect_preview "$wt"
    setup git -C "$repo" worktree add -q "$repo/.worktrees/t" -b t
    run remove --yes --json --trash --keep-trash 1m "$repo/.worktrees/t"
    entry=$(result 0 trash_entry)
    [ "$status" = 0 ] && [ -d "$entry" ] || { echo "contract-check: setup failed in $name: remove --trash t exit $status" >&2; exit 2; }
    sed "s/^trashed [0-9]* .*/trashed 1767225600 2026-01-01T00:00:00Z/" "$entry/info" >"$work/$name.info" && cat "$work/$name.info" >"$entry/info" || exit 2
    touch -t 202512310000 "$entry" || exit 2
    before=$(sums "$wt")
    expect_confirm "$wt"
    if ! expect_differs 1; then :
    elif [ "$(jq -r '[.purged[] | .result + " " + .entry.entry] | join(",")' "$out")" != "skipped $(basename "$entry")" ]; then fail "purged: $(jq -c '[.purged[] | [.result, .entry.entry]]' "$out")"
    elif ! registered "$wt" || [ "$(sums "$wt")" != "$before" ]; then fail "w changed"
    elif ! registered "$entry/t"; then fail "the trash entry's worktree is gone"
    else
        touch "$entry" || exit 2
        expect_confirm "$wt"
        if [ "$status" != 0 ]; then fail "once the entry changed after the plan: exit $status, want 0"
        elif [ "$(jq -r '[.results[].result] | join(",")' "$out"),$(jq -r '.purged | length' "$out")" != trashed,0 ]; then
            fail "once the entry changed after the plan: $(jq -c '{r: [.results[].result], purged}' "$out")"
        elif ! registered "$entry/t"; then fail "once the entry changed after the plan, it was purged"
        else ok; fi
    fi
else
    skip_partial expect-matches
    skip_partial expect-branch-switched
    skip_partial expect-head-moved
    skip_partial expect-recreated
    skip_partial expect-trash-appeared
fi

# A worktree the plan keeps counts only as kept: its branch changed after
# the plan, and the plan is the same, so the run goes on and refuses it
# (exit 3, not 4). On a partial probe it is kept as unknown, the same way.
name=expect-kept-unchanged
make_repo
echo notes >"$wt/notes.md"
run remove --json "$wt"
plan=$(jq -r .plan "$out")
setup git -C "$wt" checkout -q -b other
before=$(sums "$wt")
run remove --yes --json --expect "$plan" "$wt"
if [ "$status" != 3 ]; then fail "exit $status, want 3"
elif ! json_ok 1; then fail "stdout is not one valid remove document with 1 result"
elif [ "$(result 0 result)" != refused ]; then fail "result $(result 0 result), want refused"
elif [ "$(jq -r --arg plan "$plan" '.plan == $plan and .expected_plan == $plan' "$out")" != true ]; then fail "plan $(jq -r .plan "$out"), want $plan"
elif untouched "$before"; then ok; fi

# 19. macOS: residue in a sandbox that lets it list every process but
# inspect only its own (Seatbelt, `sandbox-exec`), the shape a coding
# agent's command sandbox can have. lsof there lists only the sandbox's
# processes and still exits 0. A sleep outside the sandbox works in w:
# outside, w is in_use by it; inside, its activity must be unknown
# (probe_failed), naming what lsof did not list, and `remove --yes` must
# keep w, never take it as not_observed.
sandboxed_case() {
    name=activity-sandboxed
    make_repo
    local before holder i busy explain_status outside real=$bin listed
    local profile=$work/$name.sb
    printf '%s\n' '(version 1)' '(allow default)' '(deny process-info*)' \
        '(allow process-info-listpids)' '(allow process-info* (target same-sandbox))' >"$profile"
    if ! sandbox-exec -f "$profile" /usr/bin/true 2>/dev/null; then
        echo "skip $name: sandbox-exec cannot apply a profile here (already in a sandbox?)"
        return
    fi
    before=$(sums "$wt")
    (cd "$wt/sub" && exec sleep 300) >/dev/null 2>&1 &
    holder=$!
    holders="$holders $holder"
    for ((i = 0; i < 100; i++)); do
        case $(ps -o comm= -p "$holder" 2>/dev/null) in *sleep) break ;; esac
        sleep 0.1
    done
    # What lsof lists from inside the sandbox, as residue runs it.
    listed=$(cd / && sandbox-exec -f "$profile" /usr/sbin/lsof -n -P -F p 2>/dev/null | grep -c '^p')
    run explain --json "$wt"
    outside=$work/$name.outside
    mv "$out" "$outside" || exit 2
    bin=/usr/bin/sandbox-exec
    run -f "$profile" "$real" explain --json "$wt"
    explain_status=$status
    busy=$work/$name.busy
    mv "$out" "$busy" || exit 2
    run -f "$profile" "$real" remove --yes --json --no-trash "$wt"
    bin=$real
    kill "$holder" 2>/dev/null
    wait "$holder" 2>/dev/null
    local hidden='.state == "unknown" and .reason == "probe_failed" and (.detail | startswith("lsof did not list"))'
    if ! jq -e --argjson pid "$holder" '.worktree.activity.state == "in_use" and any(.worktree.activity.processes[]; .pid == $pid)' "$outside" >/dev/null; then
        fail "outside the sandbox, with sleep[$holder] in it, explain says $(jq -c '.worktree.activity' "$outside")"
    elif [ "$explain_status" != 0 ]; then fail "in the sandbox, explain exit $explain_status, want 0"
    elif ! jq -e ".worktree.verdict == \"unknown\" and (.worktree.activity | $hidden) and (.activity.available == false)" "$busy" >/dev/null; then
        fail "in the sandbox (pids lsof lists there: $listed), with sleep[$holder] in w, explain says $(jq -c '[.worktree.verdict, .worktree.activity]' "$busy")"
    elif [ "$status" != 3 ]; then fail "in the sandbox, remove --yes exit $status, want 3"
    elif ! json_ok 1; then fail "stdout is not one valid remove document with 1 result"
    elif [ "$(result 0 result)" != refused ]; then fail "result $(result 0 result), want refused"
    elif [ "$(failing_checks 0)" != activity ]; then fail "failing checks: $(failing_checks 0), want activity"
    elif ! jq -e ".results[0].worktree.activity | $hidden" "$out" >/dev/null; then
        fail "in the sandbox, remove's activity is $(jq -c '.results[0].worktree.activity' "$out")"
    elif untouched "$before"; then
        ok
        echo "     pids lsof lists in the sandbox: $listed; $(jq -r '.worktree.activity.detail' "$busy")"
    fi
}
if [ "$(uname -s)" = Darwin ] && [ -x /usr/bin/sandbox-exec ]; then
    sandboxed_case
else
    echo "skip activity-sandboxed: it runs on macOS, with sandbox-exec"
fi

echo "contract-check: $passes passed, $failures failed"
[ "$failures" -eq 0 ]
