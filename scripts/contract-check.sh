#!/usr/bin/env bash
# Contract check: the real residue binary against real Git, with no prompt.
# Each scenario builds its own repository, runs `residue remove`, and
# checks what residue reports and what is left afterwards:
#
#   reported  the exit status; with --json, one residue.remove/v1 document
#             on stdout with the per-target result, reason and failing
#             check ids, and diagnostics on stderr only
#   left      the worktree's file bytes, Git's worktree registration, the
#             branch and its commit, the unrelated worktree, and no
#             .NAME.residue-removing-PID directory or trash entry
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

work=$(mktemp -d "${TMPDIR:-/tmp}/residue-contract.XXXXXX") || exit 2
case $work in
*/residue-contract.??????) ;;
*) echo "contract-check: unexpected scratch directory $work" >&2; exit 2 ;;
esac
trap 'rm -rf -- "$work"' EXIT

# A controlled environment for residue and Git.
export HOME=$work/home XDG_CONFIG_HOME=$work/home/.config XDG_STATE_HOME=$work/home/.local/state
export GIT_CONFIG_NOSYSTEM=1 NO_COLOR=1 LC_ALL=C
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_COMMON_DIR GIT_CONFIG GIT_CONFIG_PARAMETERS GIT_CONFIG_COUNT
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

failing_checks() { # INDEX: the ids of checks that did not pass, comma-separated
    jq -r "[.results[$1].worktree.checks[]? | select(.status != \"pass\") | .id] | join(\",\")" "$out"
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

# 1. Positive control: a clean worktree is removed, JSON and text.
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

# 2. Refusals, one cause each: exit 3, refused, the right failing check, and
# nothing changed.
refusal() { # NAME CHECK-ID CHANGE...
    name=$1
    local check=$2
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

# 5. Several targets: one result per target, in order; only the clean one
# goes, and one refusal makes the run exit 3.
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

# 6. Diagnostics go to stderr, never into the JSON on stdout.
name=stderr-separate
make_repo
GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=residue.regenerable GIT_CONFIG_VALUE_0=.env run remove --yes --json "$wt"
if [ "$status" != 0 ]; then fail "exit $status, want 0"
elif ! grep -q "residue: note: ignored residue.regenerable set through the environment" "$err"; then fail "no note on stderr"
elif ! json_ok 1; then fail "stdout is not exactly one valid remove document"
elif [ "$(jq -r '.ignored_environment_rules | join(",")' "$out")" != .env ]; then fail "ignored_environment_rules: $(jq -c .ignored_environment_rules "$out")"
elif [ "$(result 0 result)" != removed ]; then fail "result $(result 0 result), want removed"
elif common_ok; then ok; fi

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

echo "contract-check: $passes passed, $failures failed"
[ "$failures" -eq 0 ]
