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
#                  residue there (scenario 1), or residue alone while this
#                  command, as Git, keeps the hold it inherited (1b to 1e,
#                  1g)
#   touch-aside    create a file in ASIDE before the check after the move
#                  runs `status` there, so that check fails (scenarios 2, 3)
#   plant-nested, plant-mount, plant-unreadable
#                  at the same point, put in ASIDE what Git's status does
#                  not list and Git's remove would delete or stop at: a
#                  repository in a tracked folder, a volume mounted at an
#                  ignored path, or a folder nobody can read (2b, 2c, 2d)
#   fail-back      fail `worktree move ASIDE ORIGINAL`, the move back (3)
#   occupy-original with touch-aside, also create a directory at the
#                  original path, so residue cannot move it back there (3b)
#   hold-move      stop each remover at its `worktree move ORIGINAL ASIDE`,
#                  so both have passed every check; the test releases one,
#                  waits for it to finish, then releases the other (4, and
#                  4b with --trash, where each made a trash entry first)
#   part-remove-reflog, part-remove-head, part-remove-dotgit,
#   part-remove-files
#                  fail `worktree remove ASIDE` after deleting the
#                  worktree's HEAD reflog, or its HEAD, from its Git
#                  directory, the `.git` file from its checkout, or one of
#                  its ignored files: what Git's own remove leaves when it
#                  stops at a file it cannot delete, or is killed during
#                  the delete (5, 5b, 5c, 5e)
#   hold-after     with a part-remove mode, hold the Git command after the
#                  failed remove, so residue still runs (5d)
#   hold-add      stop `residue restore` at its `git worktree add`, so a
#                  sweep can forget the same record meanwhile (6)
#   hold-update-ref stop a sweep at the `git update-ref` that deletes a
#                  record's refs, so a restore of it can finish first (6b)
#   hold-second-list stop a run at its second `git worktree list`: in
#                  `residue restore`, the one after it listed the records
#                  and before it reads the one it restores (6c)
#   hold-restore-unlock
#                  stop a restore of a trash entry at its `worktree unlock`
#                  of the entry's worktree: after it read the removal
#                  records, right before it moves the worktree back (4m,
#                  4q)
#   hold-purge     stop a purge at `worktree remove` of its trash entry,
#                  after it wrote its removal record (4d, 4l)
#   stop-purge     at the same point, delete an ignored file and the
#                  worktree's logs/, then stop there, so the test can kill
#                  the purge part way through the delete (4i, 4j)
#   cut-purge      at the same point, delete the checkout and the HEAD in
#                  the worktree's Git directory, then stop there, so the
#                  test can kill the purge during Git's delete of its own
#                  directory (4k)
#   hold-in-w      stop at the first Git command run in .worktrees/w: a
#                  restore's first one in the worktree it recreated, before
#                  it writes its marker; the test kills it there (4e)
#   part-purge, part-purge-all
#                  fail a purge's `worktree remove` of its trash entry
#                  after deleting the checkout and the `gitdir` file in the
#                  worktree's Git directory, or all of that directory: what
#                  Git's own remove leaves when it stops part way (4f)
#   fail-limited   fail every `git status` limited to some folders (a
#                  `--literal-pathspecs status`), and log each status
#                  call's kind to $work/statuses (7)
#   drop-after-move
#                  run a trash restore's `worktree move` back, then delete
#                  the worktree's Git directory, as a purge's Git does
#                  after its delete of the files failed (4r)
#   die-after-rename
#                  stop a trash restore's `worktree move` back after it
#                  renamed the directory, before Git records the new
#                  path (4s)
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
# Needs bash, git, jq and pgrep, and cc for the no-flock cases (9), which
# are skipped without it. Everything lives under one fresh
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
# The volume scenario 2c mounts: a disk image, which any user may attach on
# macOS; elsewhere a tmpfs, which only root may mount.
mount_how=
case $(uname -s) in
Darwin) mount_how=hdiutil ;;
Linux | FreeBSD) [ "$(id -u)" = 0 ] && mount_how=tmpfs ;;
SunOS) [ "$(id -u)" = 0 ] && mount_how=swap ;;
esac
# Unmount that volume, at PATH (where the moves left it) or where it was
# mounted, and wait until it is gone: at most 10 s.
unmount_planted() { # [PATH]
    local at i gone
    [ -e "$work/mounted-at" ] || return 0
    at=$(cat "$work/mounted-at")
    for ((i = 0; i < 10; i++)); do
        umount "${1:-$at}" 2>/dev/null || umount "$at" 2>/dev/null
        [ -e "$work/mounted-dev" ] && hdiutil detach -quiet "$(cat "$work/mounted-dev")" 2>/dev/null
        gone=yes
        mount | grep -qF "/$(basename "$work")/" && gone=
        [ "$mount_how" = hdiutil ] && hdiutil info | grep -qF "/$(basename "$work")/" && gone=
        if [ -n "$gone" ]; then
            rm -f "$work/mounted-at" "$work/mounted-dev"
            return 0
        fi
        sleep 1
    done
    return 1
}
cleanup() {
    local p
    for p in "${started[@]+"${started[@]}"}"; do
        kill_tree "$p"
    done
    [ -f "$work/held" ] && kill -TERM "$(cat "$work/held")" 2>/dev/null
    wait 2>/dev/null
    unmount_planted "${wt:-}/node_modules/mnt" || echo "recovery-check: could not unmount the scratch volume in $work" >&2
    chmod -R u+rwx -- "$work" 2>/dev/null # scenario 2d's unreadable folder
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
# hold-in-w: hold the first Git command run in .worktrees/w, such as the
# first one a restore runs in the worktree it just recreated.
case "\$dir \$mode" in
*/.worktrees/w" "*hold-in-w*) [ -e "\$work/at-in-w" ] || hold in-w ;;
esac
case " \$* " in
*" worktree remove "*)
    case "\$last \$mode" in
    *residue-removing-*hold-remove*) [ -e "\$work/at-remove" ] || hold remove ;;
    */residue-trash/*hold-purge*) [ -e "\$work/at-purge" ] || hold purge ;;
    */residue-trash/*stop-purge*)
        # Git's own remove, stopped part way: an ignored file and the
        # worktree's logs/ deleted, the rest still there. The test kills
        # the sweep and this command together, as Ctrl-C does.
        gd=\$("\$real" -C "\$last" rev-parse --absolute-git-dir) || exit 1
        rm -f "\$last/node_modules/a.js"
        rm -rf "\$gd/logs"
        hold purge
        exit 1
        ;;
    */residue-trash/*cut-purge*)
        # Git's own remove, killed in the delete of its own directory: the
        # checkout gone, then the HEAD in that directory, so Git lists the
        # worktree with no files and cannot read it. The test kills the
        # sweep and this command together.
        gd=\$("\$real" -C "\$last" rev-parse --absolute-git-dir) || exit 1
        rm -rf "\$last"
        rm -f "\$gd/HEAD"
        hold purge
        exit 1
        ;;
    */residue-trash/*part-purge*)
        gd=\$("\$real" -C "\$last" rev-parse --absolute-git-dir) || exit 1
        rm -rf "\$last"
        case \$mode in
        *part-purge-all*) rm -rf "\$gd" ;;
        *) rm -f "\$gd/gitdir" ;;
        esac
        echo "error: recovery-check: the purge's remove stops part way on purpose" >&2
        exit 255
        ;;
    *residue-removing-*part-remove-*)
        # Git's own remove stops at the first file it cannot delete, after
        # the ones the filesystem listed before it: here, the worktree's
        # HEAD reflog or its HEAD, the checkout's .git file, or one of its
        # ignored files, while Git still lists the worktree.
        gd=\$("\$real" -C "\$last" rev-parse --absolute-git-dir) || exit 1
        case \$mode in
        *part-remove-reflog*) rm -f "\$gd/logs/HEAD" ;;
        *part-remove-head*) rm -f "\$gd/HEAD" ;;
        *part-remove-dotgit*) rm -f "\$last/.git" ;;
        *part-remove-files*) rm -f "\$last/node_modules/a.js" ;;
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
        *drop-after-move*)
            # The Git of a purge whose delete of the files failed goes on
            # to delete the worktree's Git directory, right after this
            # move back.
            gd=\$("\$real" -C "\$second" rev-parse --absolute-git-dir) || exit 1
            "\$real" "\$@" || exit
            rm -rf "\$gd"
            exit 0
            ;;
        *die-after-rename*)
            # Git renames the directory, then stops before it records the
            # new path (update_worktree_location).
            mv "\$second" "\$last" || exit 1
            echo "fatal: recovery-check: the move stops after its rename on purpose" >&2
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
    case "\$* \$mode" in
    *" --literal-pathspecs status "*fail-limited*)
        echo limited >>"\$work/statuses"
        echo "fatal: recovery-check: the limited status is failed on purpose" >&2
        exit 128
        ;;
    *--ignored=traditional*fail-limited*) echo full >>"\$work/statuses" ;;
    *--ignored=matching*fail-limited*) echo matching >>"\$work/statuses" ;;
    esac
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
    *residue-removing-*plant-*)
        if [ ! -e "\$work/touched" ]; then
            case \$mode in
            *plant-nested*)
                # A repository in src/, which the worktree tracks, holding
                # only the file the worktree tracks there. Status skips
                # .git, so it lists nothing.
                "\$real" init -q "\$dir/src" &&
                    "\$real" -C "\$dir/src" add a.txt &&
                    "\$real" -C "\$dir/src" commit -qm inner &&
                    "\$real" -C "\$dir/src" rev-parse HEAD >"\$work/inner" || exit 1
                ;;
            *plant-mount*)
                # Over node_modules/mnt, an empty folder with an old time,
                # so the ignored folder's files, bytes and times stay.
                at=\$dir/node_modules/mnt
                echo "\$at" >"\$work/mounted-at"
                case '$mount_how' in
                hdiutil)
                    out=\$(hdiutil attach -nobrowse -noverify -noautoopen -mountpoint "\$at" "\$work/vol.dmg") || exit 1
                    echo "\${out%%[[:space:]]*}" >"\$work/mounted-dev"
                    ;;
                tmpfs) mount -t tmpfs tmpfs "\$at" && echo kept >"\$at/kept.txt" || exit 1 ;;
                swap) mount -F tmpfs swap "\$at" && echo kept >"\$at/kept.txt" || exit 1 ;;
                *) exit 1 ;;
                esac
                ;;
            *plant-unreadable*)
                # Status warns that it cannot open x/, and lists nothing.
                mkdir "\$dir/x" && echo kept >"\$dir/x/kept.txt" && chmod 000 "\$dir/x" || exit 1
                ;;
            esac
            : >"\$work/touched"
        fi
        ;;
    esac
    ;;
*" worktree unlock "*)
    case "\$last \$mode" in
    */residue-trash/*hold-restore-unlock*) [ -e "\$work/at-restore-unlock" ] || hold restore-unlock ;;
    esac
    ;;
*" worktree add "*)
    case \$mode in *hold-add*) [ -e "\$work/at-add" ] || hold add ;; esac
    ;;
*" update-ref "*)
    case \$mode in *hold-update-ref*) [ -e "\$work/at-update-ref" ] || hold update-ref ;; esac
    ;;
*" worktree list "*)
    case \$mode in
    *hold-second-list*)
        n=\$(cat "\$work/lists-\$PPID" 2>/dev/null || echo 0)
        n=\$((n + 1))
        echo "\$n" >"\$work/lists-\$PPID"
        [ "\$n" != 2 ] || hold list
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
# With ref_format set, Git stores its refs that way (`--ref-format`).
make_repo() {
    repo=$work/$name
    rm -f "$work"/at-* "$work"/go-* "$work"/abort-* "$work/touched" "$work/held" "$work/part-done" "$work/inner" "$work/statuses"
    setup git init -q ${ref_format:+--ref-format="$ref_format"} "$repo"
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

# The guard residue puts before a printed move back to PATH: Git's move
# into an existing directory would put the worktree inside it.
guard() { # PATH
    printf 'test ! -e %s && test ! -L %s && ' "$1" "$1"
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

# Abort the Git command held at the barrier NAME without running it, and
# wait at most 10 s for it to exit: until then it keeps any hold on a
# removal record it inherited. It exits between two of its sleeps, so no
# child of it outlives it with that hold, as one would after a kill.
end_held() { # NAME
    local held i
    held=$(cat "$work/at-$1" 2>/dev/null) || return 1
    touch "$work/abort-$1" "$work/go-$1"
    for ((i = 0; i < 100; i++)); do
        kill -0 "$held" 2>/dev/null || return 0
        # A zombie has closed its files; only its new parent's wait is left.
        case $(ps -o stat= -p "$held" 2>/dev/null) in Z*) return 0 ;; esac
        sleep 0.1
    done
    kill -KILL "$held" 2>/dev/null
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

# Run each line in backquotes that starts `git -C` in the text OUT, but
# those that name NEWPATH, in order, as someone who follows it would. Sets
# $followed, those lines, and $follow_failed, those that failed, one a line.
follow() { # OUT
    local cmd
    followed= follow_failed=
    while IFS= read -r cmd; do
        cmd=${cmd#\`}
        cmd=${cmd%\`}
        case $cmd in *NEWPATH*) continue ;; "git -C "*) ;; *) continue ;; esac
        followed+="$cmd"$'\n'
        sh -c "$cmd" >>"$work/$name.out-follow" 2>&1 || follow_failed+="$cmd"$'\n'
    done < <(grep -o '`[^`]*`' "$1")
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
    for scenario in stopped-after-move stopped-after-move-restore stopped-after-move-control stopped-while-git-runs stopped-unheld-writer unprobed-record refused-after-move \
        refused-after-move-control move-back-fails original-taken two-removers two-removers-trash trash-move-in-flight \
        purge-restore-race restore-stopped-before-mark purge-other-worktree purge-other-worktree-reftable \
        back-locked-missing back-unknown purge-stopped-restore purge-unheld-restore purge-cut-restore \
        killed-unheld-git unheld-finished-record unheld-reused-pid purge-killed-unheld-git restore-locks-unlocked \
        restore-lifts-its-lock old-damaged-record restore-opens-no-fifo restore-unlock-raced \
        remove-fails-part-way-reflog remove-fails-part-way-head remove-fails-part-way-dotgit remove-fails-part-way-running \
        remove-fails-part-way-files \
        nested-repo-after-move mount-after-move unreadable-after-move private-ref-after-move bisect-after-move \
        restore-to-other-fs stale-no-index leftover-dotgit-kept restore-forget-race forget-restore-race \
        forget-before-restore-reads restore-forget-race-reftable stale-names-repair gone-aside-no-record \
        stale-keeps-reflog stale-keeps-reflog-reftable stale-keeps-same-name stale-locked-no-remove move-back-guard \
        restore-raced-git-dir restore-move-stops-after-rename noflock-recreate noflock-kept; do
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
    end_held remove
    ended=$?
    rm -f "$work/held"
    left=$(interrupted)
    if [ "$ended" != 0 ]; then fail "the held git worktree remove did not exit"
    elif [ -z "$aside" ] || [ "$(asides | wc -l | tr -d ' ')" != 1 ]; then fail "want exactly one moved-aside directory, got: $(asides)"
    elif [ "$phase" != registered ]; then fail "at the barrier Git had: $phase, want registered and unlocked"
    elif [ "$status" = 0 ]; then fail "residue exited 0 although it was stopped"
    elif [ -e "$wt" ]; then fail "something is at the original path again"
    elif [ "$(record "$aside")" != registered ]; then fail "afterwards Git has: $(record "$aside")"
    elif [ "$(sums "$aside")" != "$w_sum" ]; then fail "the moved worktree's files changed"
    elif [ "$(jq 'length' <<<"$left")" != 1 ]; then fail "scan lists $(jq length <<<"$left") interrupted removals, want 1: $left"
    elif [ "$(jq -r '.[0].original_path' <<<"$left")" != "$(cd "$repo/.worktrees" && pwd -P)/w" ]; then fail "scan names the original as $(jq -r '.[0].original_path' <<<"$left")"
    elif [ "$(jq -r '.[0].path' <<<"$left")" != "$aside" ] || [ "$(jq -r '.[0].present' <<<"$left")" != true ]; then fail "scan does not point at the moved worktree: $left"
    elif [ "$(jq -r '.[0].pid_running' <<<"$left")" != false ]; then fail "scan says the stopped run is still running"
    elif [ "$(jq -r '.[0].removal_running' <<<"$left")" != false ]; then fail "scan says its removal still runs: $left"
    elif [ "$(jq -r '.[0].recovery' <<<"$left")" != "$(guard "$main/.worktrees/w")git -C $main worktree move $aside $main/.worktrees/w" ]; then fail "scan's recovery line: $(jq -r '.[0].recovery' <<<"$left")"
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

# --- 1b. residue stopped alone while its `git worktree remove` runs -------
# residue is killed alone at `git worktree remove`, as `kill PID`, a Force
# Quit or watch's deadline do, and that Git (the wrapper, held) runs on
# with the hold on the removal record it inherited. Until it exits, scan
# must say the removal may still run and give no recovery line, and
# restore must refuse and leave w where it is: moved back, w would be
# deleted where it lands. Once it exits, here without deleting, scan gives
# the move back and restore brings w back.
name=stopped-while-git-runs
make_repo
echo hold-remove >"$work/mode"
start "" remove --yes --json "$wt"
if ! await "$work/at-remove"; then
    finish "$pid"
    fail "residue never reached git worktree remove (exit $status)"
else
    cp "$work/at-remove" "$work/held"
    aside=$(asides)
    main=$(cd "$repo" && pwd -P)
    kill -KILL "$pid"
    finish "$pid"
    during=$(interrupted)
    (cd "$repo" && "$bin" restore --json w >"$work/$name.out-restore" 2>"$work/$name.err-restore")
    restored=$?
    still=$(record "$aside")
    still_sum=$(sums "$aside")
    end_held remove
    ended=$?
    rm -f "$work/held"
    left=$(interrupted)
    (cd "$repo" && "$bin" restore --json w >"$work/$name.out-again" 2>"$work/$name.err-again")
    again=$?
    if [ -z "$aside" ]; then fail "no moved-aside directory at the barrier"
    elif ! jq -e '.[0].pid_running == false and .[0].removal_running == true and .[0].recovery == ""' <<<"$during" >/dev/null; then fail "while its Git runs, scan says: $during"
    elif [ "$restored" != 1 ]; then fail "restore while its Git runs exited $restored, want 1"
    elif ! grep -qF "has exited, but another process still holds it (\`lsof " "$work/$name.err-restore" || ! grep -qF "may still be deleting the worktree" "$work/$name.err-restore"; then fail "restore: $(cat "$work/$name.err-restore")"
    elif [ "$still" != registered ] || [ "$still_sum" != "$w_sum" ]; then fail "the refused restore changed w, at the moved-aside path: $still"
    elif [ "$ended" != 0 ]; then fail "the held git worktree remove did not exit"
    elif [ "$(jq -r '.[0].removal_running' <<<"$left")" != false ]; then fail "once its Git exited, scan says: $left"
    elif [ "$(jq -r '.[0].recovery' <<<"$left")" != "$(guard "$main/.worktrees/w")git -C $main worktree move $aside $main/.worktrees/w" ]; then fail "once its Git exited, scan's recovery line: $(jq -r '.[0].recovery' <<<"$left")"
    elif [ "$again" != 0 ]; then fail "restore once its Git exited: exit $again: $(cat "$work/$name.err-again")"
    elif [ "$(jq -r '.restore.checkout' "$work/$name.out-again")" != moved_back ]; then fail "checkout $(jq -r '.restore.checkout' "$work/$name.out-again"), want moved_back"
    elif [ "$(record "$wt")" != registered ] || [ "$(sums "$wt")" != "$w_sum" ]; then fail "w is not back whole: $(record "$wt")"
    elif [ "$(records)" != "0 records, 0 refs" ]; then fail "after restore: $(records), want 0 records, 0 refs"
    elif common_ok; then ok "while its Git holds the record, scan gives no recovery and restore refuses; once it exits, restore moves w back"; fi
fi

# Make the removal record in DIR one that a release taking no hold writes
# (0.0.10 and earlier): no `hold` line, and the PID of a process this
# script starts, which runs until the test ends it. Sets $runner. That
# process stands in for the run that wrote the record, so `remove` is one
# of its arguments, as it is of residue's; with `other` it is a plain
# `sleep`, another program that has the record's PID now.
unheld() { # DIR [other]
    if [ "${2:-}" = other ]; then
        sleep 300 &
    else
        sh -c 'while :; do sleep 1; done' remove &
    fi
    runner=$!
    started+=("$runner")
    grep -v '^hold ' "$1/info" | sed "s/^pid .*/pid $runner/" >"$work/$name.unheld" && cat "$work/$name.unheld" >"$1/info" || exit 2
}

# --- 1c. Its removal record from a release that takes no hold -------------
# As 1b, but once the held Git has exited, the removal record is made one
# that a release taking no hold writes, while the PID it names still runs:
# its hold is free, and only that PID says the removal may still run. Scan
# must say so and give no recovery line, and restore must refuse and leave
# w where it is; as that PID may be reused, both must give the move back
# for when `ps -p` shows it is not residue. Once that process ends, scan
# gives the move back and restore brings w back.
name=stopped-unheld-writer
make_repo
echo hold-remove >"$work/mode"
start "" remove --yes --json "$wt"
if ! await "$work/at-remove"; then
    finish "$pid"
    fail "residue never reached git worktree remove (exit $status)"
else
    cp "$work/at-remove" "$work/held"
    aside=$(asides)
    main=$(cd "$repo" && pwd -P)
    move="$(guard "$main/.worktrees/w")git -C $main worktree move $aside $main/.worktrees/w"
    kill -KILL "$pid"
    finish "$pid"
    end_held remove
    ended=$?
    rm -f "$work/held"
    unheld "$(find "$repo/.git/residue-removed" -mindepth 1 -maxdepth 1 -type d)"
    during=$(interrupted)
    "$bin" scan "$repo" >"$work/$name.out-scan" 2>&1
    (cd "$repo" && "$bin" restore --json w >"$work/$name.out-restore" 2>"$work/$name.err-restore")
    restored=$?
    still=$(record "$aside")
    still_sum=$(sums "$aside")
    kill "$runner"
    wait "$runner" 2>/dev/null
    forget "$runner"
    left=$(interrupted)
    (cd "$repo" && "$bin" restore --json w >"$work/$name.out-again" 2>"$work/$name.err-again")
    again=$?
    if [ -z "$aside" ]; then fail "no moved-aside directory at the barrier"
    elif [ "$ended" != 0 ]; then fail "the held git worktree remove did not exit"
    elif ! jq -e '.[0].removal_running == true and .[0].recovery == ""' <<<"$during" >/dev/null; then fail "while the record's writer runs, scan says: $during"
    elif ! grep -qF "is from a release that takes no hold and still runs" "$work/$name.out-scan"; then fail "scan: $(cat "$work/$name.out-scan")"
    elif [ "$restored" != 1 ]; then fail "restore while the record's writer runs exited $restored, want 1"
    elif ! grep -qF "takes no hold, and its process $runner still runs, so it may still be deleting the worktree" "$work/$name.err-restore"; then fail "restore: $(cat "$work/$name.err-restore")"
    elif ! grep -qF "if \`ps -p $runner\` shows it is not residue, this moves it back: $move" "$work/$name.err-restore"; then fail "restore does not give the move back for when process $runner is not residue: $(cat "$work/$name.err-restore")"
    elif ! grep -qF "if \`ps -p $runner\` shows it is not residue, this moves it back: $move" "$work/$name.out-scan"; then fail "scan does not give the move back for when process $runner is not residue: $(cat "$work/$name.out-scan")"
    elif [ "$still" != registered ] || [ "$still_sum" != "$w_sum" ]; then fail "the refused restore changed w, at the moved-aside path: $still"
    elif [ "$(jq -r '.[0].removal_running' <<<"$left")" != false ]; then fail "once the writer ended, scan says: $left"
    elif [ "$(jq -r '.[0].recovery' <<<"$left")" != "$(guard "$main/.worktrees/w")git -C $main worktree move $aside $main/.worktrees/w" ]; then fail "once the writer ended, scan's recovery line: $(jq -r '.[0].recovery' <<<"$left")"
    elif [ "$again" != 0 ]; then fail "restore once the writer ended: exit $again: $(cat "$work/$name.err-again")"
    elif [ "$(jq -r '.restore.checkout' "$work/$name.out-again")" != moved_back ]; then fail "checkout $(jq -r '.restore.checkout' "$work/$name.out-again"), want moved_back"
    elif [ "$(record "$wt")" != registered ] || [ "$(sums "$wt")" != "$w_sum" ]; then fail "w is not back whole: $(record "$wt")"
    elif [ "$(records)" != "0 records, 0 refs" ]; then fail "after restore: $(records), want 0 records, 0 refs"
    elif common_ok; then ok "while a record with no hold names a run that still runs, scan gives no recovery and restore refuses; once it ends, restore moves w back"; fi
fi

# --- 1d. A removal record residue cannot probe ------------------------------
# As 1b, while the held Git keeps the hold on the removal record. With the
# record's `info` read-only (mode 0444), scan must still find it held, by
# an open for reading: flock(2) on one works on every local filesystem
# this runs on, illumos's included. With `info` unreadable (mode 0000),
# scan cannot tell whether the removal still runs: it must give no
# recovery line and say why, and still so once that Git has exited.
# Read-only again, scan gives the move back.
name=unprobed-record
if [ "$(id -u)" = 0 ]; then
    echo "skip $name: root may open any file, so no mode makes one unreadable"
else
    make_repo
    echo hold-remove >"$work/mode"
    start "" remove --yes --json "$wt"
    if ! await "$work/at-remove"; then
        finish "$pid"
        fail "residue never reached git worktree remove (exit $status)"
    else
        cp "$work/at-remove" "$work/held"
        aside=$(asides)
        main=$(cd "$repo" && pwd -P)
        move="$(guard "$main/.worktrees/w")git -C $main worktree move $aside $main/.worktrees/w"
        kill -KILL "$pid"
        finish "$pid"
        info=$(find "$repo/.git/residue-removed" -mindepth 2 -maxdepth 2 -name info)
        chmod 0444 "$info"
        ro_held=$(interrupted)
        chmod 0000 "$info"
        none_held=$(interrupted)
        "$bin" scan "$repo" >"$work/$name.out-scan" 2>&1
        end_held remove
        ended=$?
        rm -f "$work/held"
        none_after=$(interrupted)
        chmod 0444 "$info"
        ro_after=$(interrupted)
        chmod 0644 "$info"
        (cd "$repo" && "$bin" restore --json w >"$work/$name.out-again" 2>"$work/$name.err-again")
        again=$?
        undecided='.[0].removal_running == null and .[0].recovery == ""'
        ro_busy='.[0].removal_running == true and .[0].recovery == ""'
        ro_free='.[0].removal_running == false and .[0].recovery == $move'
        if [ -z "$aside" ] || [ -z "$info" ]; then fail "no moved-aside directory or removal record at the barrier"
        elif [ "$ended" != 0 ]; then fail "the held git worktree remove did not exit"
        elif ! jq -e "$ro_busy" <<<"$ro_held" >/dev/null; then fail "with info read-only while its Git runs, scan says: $ro_held"
        elif ! jq -e "$undecided" <<<"$none_held" >/dev/null; then fail "with info unreadable while its Git runs, scan says: $none_held"
        elif ! grep -qF "residue cannot tell whether its removal still runs" "$work/$name.out-scan"; then fail "scan: $(cat "$work/$name.out-scan")"
        elif ! jq -e "$undecided" <<<"$none_after" >/dev/null; then fail "with info unreadable once its Git exited, scan says: $none_after"
        elif ! jq -e --arg move "$move" "$ro_free" <<<"$ro_after" >/dev/null; then fail "with info read-only once its Git exited, scan says: $ro_after"
        elif [ "$again" != 0 ]; then fail "restore once its Git exited: exit $again: $(cat "$work/$name.err-again")"
        elif [ "$(jq -r '.restore.checkout' "$work/$name.out-again")" != moved_back ]; then fail "checkout $(jq -r '.restore.checkout' "$work/$name.out-again"), want moved_back"
        elif [ "$(record "$wt")" != registered ] || [ "$(sums "$wt")" != "$w_sum" ]; then fail "w is not back whole: $(record "$wt")"
        elif [ "$(records)" != "0 records, 0 refs" ]; then fail "after restore: $(records), want 0 records, 0 refs"
        elif common_ok; then ok "a read-only record is still probed; one residue cannot read gives no recovery line, with the reason"; fi
    fi
fi

# Rewrite the removal record in DIR as a new file, as a release that takes
# no hold writes it: no `hold` line. A hold a Git inherited on the old file
# then shows nothing.
unheld_file() { # DIR
    grep -v '^hold ' "$1/info" >"$work/$name.unheld" && mv "$work/$name.unheld" "$1/info" || exit 2
}

# --- 1e. A release that takes no hold, killed alone while its Git runs -----
# As 1b: residue is killed alone at `git worktree remove`, and that Git
# (the wrapper, held) runs on. The record is then rewritten, as a new
# file, the way a release taking no hold writes it, so neither its hold
# nor its PID, which is gone, shows the removal; only that Git, a `git
# worktree remove` of w's hidden path, does. Scan must say so and give no
# recovery line, and restore must refuse, name that Git and leave w where
# it is. The record's PID then runs again, as a reused one would: while
# that Git runs, neither may give the move back by hand. Once that Git
# exits, restore moves w back.
name=killed-unheld-git
make_repo
echo hold-remove >"$work/mode"
start "" remove --yes --json "$wt"
if ! await "$work/at-remove"; then
    finish "$pid"
    fail "residue never reached git worktree remove (exit $status)"
else
    cp "$work/at-remove" "$work/held"
    git_pid=$(cat "$work/at-remove")
    aside=$(asides)
    main=$(cd "$repo" && pwd -P)
    kill -KILL "$pid"
    finish "$pid"
    unheld_file "$(find "$repo/.git/residue-removed" -mindepth 1 -maxdepth 1 -type d)"
    during=$(interrupted)
    "$bin" scan "$repo" >"$work/$name.out-scan" 2>&1
    (cd "$repo" && "$bin" restore --json w >"$work/$name.out-restore" 2>"$work/$name.err-restore")
    restored=$?
    still=$(record "$aside")
    still_sum=$(sums "$aside")
    # The record's PID then runs again, as a reused one would.
    unheld "$(find "$repo/.git/residue-removed" -mindepth 1 -maxdepth 1 -type d)"
    "$bin" scan "$repo" >"$work/$name.out-scan2" 2>&1
    (cd "$repo" && "$bin" restore --json w >"$work/$name.out-restore2" 2>"$work/$name.err-restore2")
    restored2=$?
    kill "$runner"
    wait "$runner" 2>/dev/null
    forget "$runner"
    end_held remove
    ended=$?
    rm -f "$work/held"
    left=$(interrupted)
    (cd "$repo" && "$bin" restore --json w >"$work/$name.out-again" 2>"$work/$name.err-again")
    again=$?
    if [ -z "$aside" ]; then fail "no moved-aside directory at the barrier"
    elif ! jq -e '.[0].pid_running == false and .[0].removal_running == true and .[0].recovery == ""' <<<"$during" >/dev/null; then fail "while its Git runs, scan says: $during"
    elif ! grep -qF "process $git_pid, a \`git worktree remove\` of it, still runs" "$work/$name.out-scan"; then fail "scan: $(cat "$work/$name.out-scan")"
    elif [ "$restored" != 1 ]; then fail "restore while its Git runs exited $restored, want 1"
    elif ! grep -qF "takes no hold, and process $git_pid, a \`git worktree remove\` of the worktree, still runs, so it may still be deleting it" "$work/$name.err-restore"; then fail "restore: $(cat "$work/$name.err-restore")"
    elif [ "$restored2" != 1 ]; then fail "restore while its Git and the record's PID run exited $restored2, want 1"
    elif grep -qF "shows it is not residue" "$work/$name.err-restore2" "$work/$name.out-scan2"; then fail "while its Git runs, a move back by hand is given: $(cat "$work/$name.err-restore2" "$work/$name.out-scan2")"
    elif [ "$still" != registered ] || [ "$still_sum" != "$w_sum" ]; then fail "the refused restore changed w, at the moved-aside path: $still"
    elif [ "$ended" != 0 ]; then fail "the held git worktree remove did not exit"
    elif [ "$(jq -r '.[0].removal_running' <<<"$left")" != false ]; then fail "once its Git exited, scan says: $left"
    elif [ "$(jq -r '.[0].recovery' <<<"$left")" != "$(guard "$main/.worktrees/w")git -C $main worktree move $aside $main/.worktrees/w" ]; then fail "once its Git exited, scan's recovery line: $(jq -r '.[0].recovery' <<<"$left")"
    elif [ "$again" != 0 ]; then fail "restore once its Git exited: exit $again: $(cat "$work/$name.err-again")"
    elif [ "$(jq -r '.restore.checkout' "$work/$name.out-again")" != moved_back ]; then fail "checkout $(jq -r '.restore.checkout' "$work/$name.out-again"), want moved_back"
    elif [ "$(record "$wt")" != registered ] || [ "$(sums "$wt")" != "$w_sum" ]; then fail "w is not back whole: $(record "$wt")"
    elif [ "$(records)" != "0 records, 0 refs" ]; then fail "after restore: $(records), want 0 records, 0 refs"
    elif common_ok; then ok "while a Git outlives a run that takes no hold, scan gives no recovery and restore names that Git and refuses; once it exits, restore moves w back"; fi
fi

# --- 1f. A finished removal's record from a release that takes no hold ----
# w is removed whole, and its record is then made one a release taking no
# hold writes, naming a process that still runs, as a reused PID does. Git
# no longer lists w, so nothing of that removal is left to delete: restore
# must recreate w while that process runs.
name=unheld-finished-record
make_repo
: >"$work/mode"
setup "$bin" remove --yes --json "$wt"
unheld "$(find "$repo/.git/residue-removed" -mindepth 1 -maxdepth 1 -type d)"
(cd "$repo" && "$bin" restore --json w >"$work/$name.out-restore" 2>"$work/$name.err-restore")
restored=$?
alive=no
kill -0 "$runner" 2>/dev/null && alive=yes
kill "$runner"
wait "$runner" 2>/dev/null
forget "$runner"
if [ "$alive" != yes ]; then fail "the process the record names ended before the restore"
elif [ "$restored" != 0 ]; then fail "restore while that process runs exited $restored: $(cat "$work/$name.err-restore")"
elif [ "$(jq -r '.restore.checkout' "$work/$name.out-restore")" != checked_out_branch ]; then fail "checkout $(jq -r '.restore.checkout' "$work/$name.out-restore"), want checked_out_branch"
elif [ "$(record "$wt")" != registered ] || [ "$(git -C "$wt" rev-parse HEAD)" != "$w_sha" ]; then fail "w is not back at its HEAD: $(record "$wt")"
elif [ "$(sums "$wt")" != "$(grep -v '^\./node_modules/' <<<"$w_sum")" ]; then fail "the recreated worktree's tracked files differ"
elif [ "$(records)" != "0 records, 0 refs" ]; then fail "after restore: $(records), want 0 records, 0 refs"
elif common_ok; then ok "a finished removal's record that names a running process is restored: Git no longer lists its worktree"; fi

# --- 1g. 1c, with a record an hour older than the process it names ---------
# As 1c, but the record says it was written an hour before the process it
# names started. While that process is one whose arguments name `remove`,
# as a run's do, it may still be the run that wrote the record, its start
# time moved by a step of the clock: restore must refuse. Once the record
# names a plain `sleep` instead, another program that has its PID now,
# restore must move w back while it runs, wherever residue reads a
# process's arguments (every platform CI runs); elsewhere it must refuse.
name=unheld-reused-pid
make_repo
echo hold-remove >"$work/mode"
start "" remove --yes --json "$wt"
if ! await "$work/at-remove"; then
    finish "$pid"
    fail "residue never reached git worktree remove (exit $status)"
else
    cp "$work/at-remove" "$work/held"
    aside=$(asides)
    kill -KILL "$pid"
    finish "$pid"
    end_held remove
    ended=$?
    rm -f "$work/held"
    record_dir=$(find "$repo/.git/residue-removed" -mindepth 1 -maxdepth 1 -type d)
    unheld "$record_dir"
    sed "s/^removed [0-9]* /removed $(($(date +%s) - 3600)) /" "$record_dir/info" >"$work/$name.info" && cat "$work/$name.info" >"$record_dir/info" || exit 2
    writer=$runner
    (cd "$repo" && "$bin" restore --json w >"$work/$name.out-writer" 2>"$work/$name.err-writer")
    refused=$?
    still=$(record "$aside")
    still_sum=$(sums "$aside")
    kill "$runner"
    wait "$runner" 2>/dev/null
    forget "$runner"
    # A restore that did not refuse took the record, and the check below
    # fails on that; there is no record left to rewrite.
    restored= alive=no
    if [ -e "$record_dir/info" ]; then
        unheld "$record_dir" other
        (cd "$repo" && "$bin" restore --json w >"$work/$name.out-restore" 2>"$work/$name.err-restore")
        restored=$?
        kill -0 "$runner" 2>/dev/null && alive=yes
        kill "$runner"
        wait "$runner" 2>/dev/null
        forget "$runner"
    fi
    case $(uname -s) in
    Darwin | Linux | FreeBSD | DragonFly | NetBSD | OpenBSD | SunOS) want=0 ;;
    *) want=1 ;;
    esac
    if [ -z "$aside" ]; then fail "no moved-aside directory at the barrier"
    elif [ "$ended" != 0 ]; then fail "the held git worktree remove did not exit"
    elif [ "$refused" != 1 ]; then fail "restore while a process whose arguments name remove has the record's PID exited $refused, want 1: $(cat "$work/$name.err-writer")"
    elif ! grep -qF "takes no hold, and its process $writer still runs, so it may still be deleting the worktree" "$work/$name.err-writer"; then fail "restore: $(cat "$work/$name.err-writer")"
    elif [ "$still" != registered ] || [ "$still_sum" != "$w_sum" ]; then fail "the refused restore changed w, at the moved-aside path: $still"
    elif [ "$alive" != yes ]; then fail "the process the record names ended before the restore"
    elif [ "$restored" != "$want" ]; then fail "restore while a plain sleep has the record's PID exited $restored, want $want: $(cat "$work/$name.err-restore")"
    elif [ "$want" = 1 ]; then
        if ! grep -qF "takes no hold, and its process $runner still runs, so it may still be deleting the worktree" "$work/$name.err-restore"; then fail "restore: $(cat "$work/$name.err-restore")"
        elif [ "$(record "$aside")" != registered ] || [ "$(sums "$aside")" != "$w_sum" ]; then fail "the refused restore changed w, at the moved-aside path: $(record "$aside")"
        elif common_ok; then ok "without its arguments, restore refuses while the record's PID runs"; fi
    elif [ "$(jq -r '.restore.checkout' "$work/$name.out-restore")" != moved_back ]; then fail "checkout $(jq -r '.restore.checkout' "$work/$name.out-restore"), want moved_back"
    elif [ "$(record "$wt")" != registered ] || [ "$(sums "$wt")" != "$w_sum" ]; then fail "w is not back whole: $(record "$wt")"
    elif [ "$(records)" != "0 records, 0 refs" ]; then fail "after restore: $(records), want 0 records, 0 refs"
    elif common_ok; then ok "an hour-old record holds the restore back while a process whose arguments name remove has its PID, and not while a plain sleep has it"; fi
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

# --- 2b-2d. The check after the move judges what the walk finds ------------
# Each mode plants in the moved worktree what Git's status does not list
# and Git's remove, which runs without --force, deletes or stops at: 2b a
# repository in a tracked folder, whose commits would be lost; 2c a volume
# mounted at an ignored path, which Git deletes into; 2d a folder nobody
# can read, where Git stops part way. The plan held none of them, since a
# worktree whose walk finds one is no candidate. The check after the move
# must refuse with REASON and put w back.
outcome() { # what residue says it did with w: "RESULT: REASON"
    jq -r '.results[0] | "\(.result): \(.reason)"' "$work/$name.out"
}
refused_after_plant() { # REASON
    local out=$work/$name.out
    if [ "$status" != 3 ]; then fail "exit $status, want 3"
    elif [ ! -e "$work/touched" ]; then fail "nothing was planted: no status ran in the moved-aside directory"
    elif [ "$(jq -r '.results[0].result' "$out")" != skipped ]; then fail "result $(outcome), want skipped"
    elif [ "$(jq -r '.results[0].reason' "$out")" != "$1" ]; then fail "reason: $(jq -r '.results[0].reason' "$out"); want: $1"
    elif [ "$(record "$wt")" != registered ]; then fail "at its original path Git has: $(record "$wt")"
    elif [ -n "$(asides)" ]; then fail "a moved-aside directory is left: $(asides)"
    elif [ "$(interrupted)" != "[]" ]; then fail "scan lists an interrupted removal: $(interrupted)"
    elif [ "$(records)" != "0 records, 0 refs" ]; then fail "removal records: $(records), want 0 records, 0 refs"
    else return 0; fi
    return 1
}

name=nested-repo-after-move
make_repo
mkdir "$wt/src" && echo a >"$wt/src/a.txt" || exit 2
setup git -C "$wt" add src
setup git -C "$wt" commit -qm src
w_sha=$(git -C "$repo" rev-parse w) || exit 2
w_sum=$(sums "$wt") || exit 2
echo plant-nested >"$work/mode"
start "" remove --yes --json "$wt"
finish "$pid"
if [ -e "$work/inner" ] && [ "$(git -C "$wt/src" rev-parse HEAD 2>/dev/null)" != "$(cat "$work/inner")" ]; then fail "the repository made in src/ after the move is gone, and its commit with it; residue: $(outcome)"
elif ! refused_after_plant "contains a .git entry at src/.git after residue moved it aside; it is back in place"; then :
elif [ "$(sums "$wt" | grep -v '^\./src/\.git/')" != "$w_sum" ]; then fail "the restored worktree's other files changed"
elif common_ok; then ok "$(jq -r '.results[0].reason' "$work/$name.out")"; fi

name=mount-after-move
if [ -z "$mount_how" ]; then
    echo "skip $name: only root may mount a volume on $(uname -s), so a mount point is not witnessed here"
else
    make_repo
    mkdir "$wt/node_modules/mnt" && touch -t 202001010000 "$wt/node_modules/mnt" || exit 2
    w_sum=$(sums "$wt") || exit 2
    if [ "$mount_how" = hdiutil ]; then
        # No Spotlight index and no event log, which macOS would otherwise
        # keep open on the volume.
        mkdir -p "$work/vol/.fseventsd" && echo kept >"$work/vol/kept.txt" || exit 2
        : >"$work/vol/.metadata_never_index" && : >"$work/vol/.fseventsd/no_log" || exit 2
        setup hdiutil create -quiet -fs HFS+ -volname residue-recovery -layout NONE -format UDRW -srcfolder "$work/vol" "$work/vol.dmg"
    fi
    echo plant-mount >"$work/mode"
    start "" remove --yes --json "$wt"
    finish "$pid"
    kept=$(cat "$wt/node_modules/mnt/kept.txt" 2>/dev/null)
    unmount_planted "$wt/node_modules/mnt"
    unmounted=$?
    if [ -e "$work/touched" ] && [ "$kept" != kept ]; then fail "the file on the volume mounted after the move is gone; residue: $(outcome)"
    elif ! refused_after_plant "contains a mount point at node_modules/mnt after residue moved it aside; it is back in place"; then :
    elif [ "$unmounted" != 0 ]; then fail "the volume did not unmount within 10 s"
    elif [ "$(sums "$wt")" != "$w_sum" ]; then fail "the restored worktree's other files changed"
    elif common_ok; then ok "$(jq -r '.results[0].reason' "$work/$name.out")"; fi
fi

name=unreadable-after-move
if [ "$(id -u)" = 0 ]; then
    echo "skip $name: root may list any folder, so chmod 000 makes none unreadable"
else
    make_repo
    echo plant-unreadable >"$work/mode"
    start "" remove --yes --json "$wt"
    finish "$pid"
    for d in "$wt/x" "$repo"/.worktrees/.w.residue-removing-*/x; do
        [ -d "$d" ] && chmod 755 "$d"
    done
    if ! refused_after_plant "could not read 1 entry after residue moved it aside: x: Permission denied (os error 13); it is back in place"; then :
    elif [ "$(cat "$wt/x/kept.txt" 2>/dev/null)" != kept ]; then fail "the file in the unreadable folder changed"
    elif [ "$(sums "$wt" | grep -v '^\./x/kept\.txt ')" != "$w_sum" ]; then fail "the restored worktree's other files changed"
    elif common_ok; then ok "$(jq -r '.results[0].reason' "$work/$name.out")"; fi
fi

# --- 2e. A private ref or a bisect started during the move ------------------
# The removal passed every check and waits at its move; meanwhile a Git
# outside it gives w a private ref to a commit nothing else reaches, or
# starts a bisect there. `git worktree remove` would delete both with w,
# so the check after the move must refuse and put w back with them.
for kind in private-ref bisect; do
    name=$kind-after-move
    make_repo
    echo hold-move >"$work/mode"
    start "" remove --yes --json "$wt"
    if ! await "$work/at-move-$pid"; then
        finish "$pid"
        fail "the removal did not reach its move"
        continue
    fi
    case $kind in
    private-ref)
        kept=$(git -C "$wt" commit-tree -p HEAD -m private-only "$(git -C "$wt" rev-parse 'HEAD^{tree}')") || exit 2
        setup git -C "$wt" update-ref refs/worktree/keep "$kept"
        want="it has worktree-private refs after residue moved it aside, which its deletion would delete: refs/worktree/keep; it is back in place"
        ;;
    bisect)
        setup git -C "$wt" bisect start
        setup git -C "$wt" bisect bad
        want="bisect in progress after residue moved it aside; it is back in place"
        ;;
    esac
    touch "$work/go-move-$pid"
    finish "$pid"
    out=$work/$name.out
    if [ "$status" != 3 ]; then fail "exit $status, want 3: $(outcome)"
    elif [ "$(jq -r '.results[0].result' "$out")" != skipped ]; then fail "result $(outcome), want skipped"
    elif [ "$(jq -r '.results[0].reason' "$out")" != "$want" ]; then fail "reason: $(jq -r '.results[0].reason' "$out"); want: $want"
    elif [ "$(record "$wt")" != registered ]; then fail "at its original path Git has: $(record "$wt")"
    elif [ -n "$(asides)" ]; then fail "a moved-aside directory is left: $(asides)"
    elif [ "$(records)" != "0 records, 0 refs" ]; then fail "removal records: $(records), want 0 records, 0 refs"
    elif [ "$kind" = private-ref ] && [ "$(git -C "$wt" rev-parse -q --verify refs/worktree/keep)" != "$kept" ]; then fail "w's refs/worktree/keep no longer names the commit only it kept"
    elif [ "$kind" = bisect ] && { [ ! -e "$repo/.git/worktrees/w/BISECT_LOG" ] || [ "$(git -C "$wt" rev-parse -q --verify refs/bisect/bad)" != "$w_sha" ]; }; then fail "w's bisect is gone"
    elif common_ok; then ok "$(jq -r '.results[0].reason' "$out")"; fi
done

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
elif ! grep -qF "Recover with: $(guard "$(cd "$repo" && pwd -P)/.worktrees/w")git -C $(cd "$repo" && pwd -P) worktree unlock $aside && git -C $(cd "$repo" && pwd -P) worktree move $aside $(cd "$repo" && pwd -P)/.worktrees/w" <<<"$reason"; then fail "the recovery line does not unlock, then move back: $reason"
elif [ "$(interrupted | jq -r '.[0].recovery')" != "$(guard "$(cd "$repo" && pwd -P)/.worktrees/w")git -C $(cd "$repo" && pwd -P) worktree unlock $aside && git -C $(cd "$repo" && pwd -P) worktree move $aside $(cd "$repo" && pwd -P)/.worktrees/w" ]; then fail "scan's recovery line: $(interrupted | jq -r '.[0].recovery')"
elif [ -e "$wt" ]; then fail "something is at the original path"
elif [ "$(record "$aside")" != "registered, locked residue: removing" ]; then fail "at the moved-aside path Git has: $(record "$aside")"
elif [ "$(cat "$aside/added.txt" 2>/dev/null)" != added ] || [ "$(sums "$aside" | grep -v '^./added.txt ')" != "$w_sum" ]; then fail "the moved worktree's files changed"
elif [ "$(interrupted | jq 'length')" != 1 ]; then fail "scan does not list it as an interrupted removal: $(interrupted)"
elif [ "$(records)" != "1 records, 1 refs" ]; then fail "removal records: $(records), want 1 records, 1 refs"
elif ! grep -qF "and its removal record stays: residue restore " <<<"$reason" || [ "${reason##*Recover with: }" != "$(interrupted | jq -r '.[0].recovery')" ]; then fail "the reason does not name the record's restore before the recovery line, which ends it: $reason"
elif common_ok; then
    # Moved aside whole, it keeps its record, whose restore moves it back.
    (cd "$repo" && "$bin" restore --json w >"$work/$name.out-restore" 2>"$work/$name.err-restore")
    restored=$?
    if [ "$restored" != 0 ]; then fail "restore exited $restored: $(cat "$work/$name.err-restore")"
    elif [ "$(jq -r '.restore.checkout' "$work/$name.out-restore")" != moved_back ]; then fail "checkout $(jq -r '.restore.checkout' "$work/$name.out-restore"), want moved_back"
    elif [ "$(record "$wt")" != registered ] || [ -n "$(asides)" ]; then fail "w is not back at its path: $(record "$wt")"
    elif [ "$(records)" != "0 records, 0 refs" ]; then fail "after restore: $(records), want 0 records, 0 refs"
    elif common_ok; then ok "failed, locked at the hidden path with its record, scan lists it, and restore moves it back"; fi
fi

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
elif [ "$(records)" != "1 records, 1 refs" ]; then fail "removal records: $(records), want 1 records, 1 refs"
elif common_ok; then ok "failed, still locked 'residue: removing' at the hidden path, with its record"; fi

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
# its `git worktree move`, so the entry holds only its record. A restore
# of the entry then must refuse and name what holds it: the remover holds
# the entry from before its info is in place until it has checked w in
# the trash, so on a filesystem without flock(2) nothing moves in at all,
# which this cannot show on a scratch directory. A second
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
    # The remover holds the entry from before its info is in place, so a
    # restore of the entry refuses while the move in is under way.
    (cd "$repo" && "$bin" restore --json "$entry" >"$work/$name.out-early" 2>"$work/$name.err-early")
    early=$?
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
    elif [ "$early" != 1 ]; then fail "restore of the entry during the move in exited $early, want 1: $(cat "$work/$name.err-early")"
    elif ! grep -qF "or moving a worktree into it (\`lsof " "$work/$name.err-early"; then fail "restore of the entry during the move in: $(cat "$work/$name.err-early")"
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

# --- 4d. A trash restore while a purge deletes the entry -------------------
# A sweep purges w's week-old trash entry and is held at its `git worktree
# remove`, after it wrote its removal record and unlocked the entry's
# worktree. The purge holds the entry, and so does the Git it started.
# `residue restore` of the entry meanwhile must refuse and leave w where it
# is: moved back, w would be deleted where it lands. Released, the purge
# must delete w and empty the trash, and the removal record it left must
# bring w back. Outside the wrapper: the same restore while Git's own
# remove is deleting w, which the hold keeps out the same way.
name=purge-restore-race
make_repo
setup "$bin" remove --yes --json --trash "$wt"
entry=$(find "$repo/.git/residue-trash" -mindepth 1 -maxdepth 1 -type d 2>/dev/null)
tinfo=$entry/info
sed 's/^trashed [0-9]* .*/trashed 1767225600 2026-01-01T00:00:00Z/' "$tinfo" >"$work/$name.info" && cat "$work/$name.info" >"$tinfo" || exit 2
echo hold-purge >"$work/mode"
start ".sweep" remove --all --yes --json --idle 30d "$repo"
sweep_pid=$pid
if ! await "$work/at-purge"; then
    finish "$sweep_pid"
    fail "the sweep never reached the purge's git worktree remove (exit $status)"
else
    held_records=$(records)
    (cd "$repo" && "$bin" restore --json "$entry" >"$work/$name.out-restore" 2>"$work/$name.err-restore")
    restored=$?
    during=$(record "$entry/w")
    during_sum=$(sums "$entry/w")
    at_path=no
    [ -e "$wt" ] && at_path=yes
    touch "$work/go-purge"
    finish "$sweep_pid"
    sweep_status=$status
    out=$work/$name.out.sweep
    result=$(jq -r '.purged[0].result' "$out")
    after_records=$(records)
    left=$(ls -A "$repo/.git/residue-trash" 2>/dev/null)
    record_dir=$(find "$repo/.git/residue-removed" -mindepth 1 -maxdepth 1 -type d 2>/dev/null)
    (cd "$repo" && "$bin" restore --json "$record_dir" >"$work/$name.out-record" 2>"$work/$name.err-record")
    again=$?
    if [ "$held_records" != "1 records, 1 refs" ]; then fail "while the purge is held: $held_records, want 1 records, 1 refs"
    elif [ "$restored" != 1 ]; then fail "restore while the purge runs exited $restored, want 1"
    elif ! grep -qF "is purging or restoring this trash entry now, or moving a worktree into it (\`lsof " "$work/$name.err-restore" || ! grep -qF "/info\` names it); run this restore again once it ends" "$work/$name.err-restore"; then fail "restore: $(cat "$work/$name.err-restore")"
    elif [ "$during" != registered ] || [ "$during_sum" != "$w_sum" ]; then fail "the refused restore changed w, in the trash: $during"
    elif [ "$at_path" != no ]; then fail "something is at w's path while the purge runs"
    elif [ "$sweep_status" != 0 ]; then fail "the sweep exited $sweep_status, want 0"
    elif [ "$result" != purged ]; then fail "the purge's result is $result, want purged: $(jq -r '.purged[0].reason' "$out")"
    elif [ "$after_records" != "1 records, 1 refs" ]; then fail "after the purge: $after_records, want 1 records, 1 refs"
    elif [ -n "$left" ]; then fail "the trash entry is left after the purge: $left"
    elif [ "$again" != 0 ]; then fail "restore of the purge's record exited $again: $(cat "$work/$name.err-record")"
    elif [ "$(jq -r '.restore.checkout' "$work/$name.out-record")" != checked_out_branch ]; then fail "checkout $(jq -r '.restore.checkout' "$work/$name.out-record"), want checked_out_branch"
    elif [ "$(record "$wt")" != registered ] || [ "$(git -C "$wt" rev-parse HEAD)" != "$w_sha" ]; then fail "w is not back at its HEAD: $(record "$wt")"
    elif [ "$(sums "$wt")" != "$(grep -v '^\./node_modules/' <<<"$w_sum")" ]; then fail "the recreated worktree's tracked files differ"
    elif [ "$(records)" != "0 records, 0 refs" ]; then fail "after restore: $(records), want 0 records, 0 refs"
    elif common_ok; then ok "restore refuses while the purge holds the entry; the purge completes and its record brings w back"; fi
fi

# --- 4e. A restore stopped after `git worktree add`, before its marker -------
# w has a commit only its HEAD reflog holds. restore recreates w from its
# removal record and is killed at its first Git command in the new
# worktree: Git lists w on its branch at its recorded HEAD, the record and
# its refs stay, and w has neither its restored marker nor its old reflog.
# The record must read as back (scan's `removed[].state`), and stats must
# seed no `freed` line for it. restore of it again must finish the job:
# mark w, put the recorded reflog in front of its own, and drop the record
# once w's reflog names that commit. `.github/watch-smoke.sh` 9 checks that
# watch's landed rule keeps such a worktree.
name=restore-stopped-before-mark
make_repo
echo lost >>"$wt/README.md"
setup git -C "$wt" commit -qam lost
lost=$(git -C "$wt" rev-parse HEAD) || exit 2
setup git -C "$wt" reset -q --hard HEAD~1
: >"$work/mode"
setup "$bin" remove --yes --json "$wt"
echo hold-in-w >"$work/mode"
# exec, so that $! is the restore itself: bash 3.2 and 5.3 both fork the
# last command of `(cd ... && cmd)`, and a kill of the subshell alone left
# the restore running on, still holding its record.
(cd "$repo" && PATH="$work/bin:$PATH" exec "$bin" restore --json w >"$work/$name.out" 2>"$work/$name.err") &
pid=$!
started+=("$pid")
if ! await "$work/at-in-w"; then
    finish "$pid"
    fail "restore never ran a Git command in the recreated w (exit $status)"
else
    cp "$work/at-in-w" "$work/held"
    kill -KILL "$pid"
    finish "$pid"
    end_held in-w
    ended=$?
    rm -f "$work/held"
    : >"$work/mode"
    # A ledger without the removal's own line, so what seeding adds shows.
    rm -f "$XDG_STATE_HOME/residue/ledger.jsonl" "$HOME/Library/Application Support/residue/ledger.jsonl"
    "$bin" scan --json "$repo" >"$work/$name.scan" 2>/dev/null
    state=$(jq -r '[.repositories[].removed[].state] | join(",")' "$work/$name.scan")
    marker=$(jq -r --arg p "$(cd "$wt" && pwd -P)" '.repositories[].worktrees[] | select(.path == $p) | .restored' "$work/$name.scan")
    held_records=$(records)
    reflog_before=$(git -C "$wt" log -g --format=%H HEAD | grep -c "$lost")
    (cd "$repo" && "$bin" restore >"$work/$name.out-list" 2>&1)
    "$bin" stats --json >"$work/$name.stats" 2>/dev/null
    freed=$(jq -r '.freed.all_time.worktrees // 0' "$work/$name.stats")
    (cd "$repo" && "$bin" restore --json w >"$work/$name.out-again" 2>"$work/$name.err-again")
    again=$?
    checkout=$(jq -r '.restore.checkout' "$work/$name.out-again" 2>/dev/null)
    "$bin" scan --json "$repo" >"$work/$name.scan-after" 2>/dev/null
    marker_after=$(jq -r --arg p "$(cd "$wt" && pwd -P)" '.repositories[].worktrees[] | select(.path == $p) | .restored.from // empty' "$work/$name.scan-after")
    if [ "$ended" != 0 ]; then fail "the held Git command in w did not exit"
    elif [ -s "$work/$name.out" ]; then fail "the killed restore ran on and finished: $(cat "$work/$name.out")"
    elif [ "$(record "$wt")" != registered ]; then fail "at its path Git has: $(record "$wt")"
    elif [ "$(git -C "$wt" rev-parse HEAD)" != "$w_sha" ]; then fail "w is not at its recorded HEAD"
    elif [ "$held_records" != "1 records, 2 refs" ]; then fail "after the kill: $held_records, want 1 records, 2 refs"
    elif [ "$marker" != null ]; then fail "after the kill w has a restored marker: $marker"
    elif [ "$reflog_before" != 0 ]; then fail "after the kill w's HEAD reflog already names the lost commit"
    elif [ "$state" != back ]; then fail "scan lists the record as ${state:-nothing}, want back"
    elif ! grep -qF "back: Git lists it at" "$work/$name.out-list"; then fail "the restore list does not say it is back: $(cat "$work/$name.out-list")"
    elif [ "$freed" != 0 ]; then fail "stats count $freed freed worktrees, want 0: nothing is seeded for a record whose worktree is back"
    elif [ "$again" != 0 ]; then fail "restore of it again exited $again, want 0: $(cat "$work/$name.err-again")"
    elif [ "$checkout" != already_back ]; then fail "restore of it again: checkout is $checkout, want already_back"
    elif [ -z "$marker_after" ]; then fail "restore of it again did not mark w as restored"
    elif [ "$(git -C "$wt" log -g --format=%H HEAD | grep -c "$lost")" = 0 ]; then fail "w's HEAD reflog does not name the commit only its old reflog held"
    elif [ "$(records)" != "0 records, 0 refs" ]; then fail "after restore of it again: $(records), want 0 records, 0 refs"
    elif common_ok; then ok "the record reads as back, and restore of it again marks w and puts its reflog back"; fi
fi

# --- 4f. Another worktree at the path a purge's trash entry came from ------
# w has a commit only its HEAD reflog holds. It is trashed, and a new
# worktree, detached at w's commit, is added at w's original path. A sweep
# purges w's entry; its `git worktree remove` deletes the checkout and the
# `gitdir` file in w's Git directory (all of that directory in the
# reftable run), then fails. The worktree at the original path is another
# one, with its own Git directory: the purge must not take it for w, nor
# say another run put w back. Its removal record and both refs stay, so
# the commit only w's reflog held is kept. Outside the wrapper: Git making
# the new worktree's directory under the name w's had, after w's was
# deleted and before the purge looks; `Written::is_its_git_dir` compares
# the directory's inode for that.
purge_other() { # MODE
    make_repo
    echo lost >>"$wt/README.md"
    setup git -C "$wt" commit -qam lost
    lost=$(git -C "$wt" rev-parse HEAD) || exit 2
    setup git -C "$wt" reset -q --hard HEAD~1
    : >"$work/mode"
    setup "$bin" remove --yes --json --trash "$wt"
    entry=$(find "$repo/.git/residue-trash" -mindepth 1 -maxdepth 1 -type d 2>/dev/null)
    sed 's/^trashed [0-9]* .*/trashed 1767225600 2026-01-01T00:00:00Z/' "$entry/info" >"$work/$name.info" && cat "$work/$name.info" >"$entry/info" || exit 2
    setup git -C "$repo" worktree add -q --detach "$wt" w
    echo "$1" >"$work/mode"
    start ".sweep" remove --all --yes --json --idle 30d "$repo"
    finish "$pid"
    : >"$work/mode"
    out=$work/$name.out.sweep
    result=$(jq -r '.purged[0].result' "$out")
    reason=$(jq -r '.purged[0].reason' "$out")
    holding=$(git -C "$repo" for-each-ref --contains "$lost" refs/residue | wc -l | tr -d ' ')
    if [ "$result" != failed ]; then fail "the purge's result is $result, want failed: $reason"
    elif grep -qF "another run put it back" <<<"$reason"; then fail "the purge took the other worktree for w: $reason"
    elif [ "$(records)" != "1 records, 2 refs" ]; then fail "removal records: $(records), want 1 records, 2 refs"
    elif [ "$holding" = 0 ]; then fail "no ref keeps the commit only w's reflog held"
    elif [ "$(record "$wt")" != registered ] || [ "$(git -C "$wt" rev-parse HEAD)" != "$w_sha" ]; then fail "the other worktree at w's path changed: $(record "$wt")"
    elif common_ok; then ok "the purge keeps w's record and refs: $(cut -c1-110 <<<"$reason")"; fi
}
name=purge-other-worktree
purge_other part-purge
if ! git init -q --ref-format=reftable "$work/reftable.probe.4f" >/dev/null 2>&1; then
    echo "skip purge-other-worktree-reftable: this Git cannot make a reftable repository (Git 2.45 and later)"
else
    name=purge-other-worktree-reftable
    ref_format=reftable purge_other part-purge-all
fi

# --- 4g. A locked worktree whose folder is gone, at the record's path -------
# w is removed, then added again by hand at its path, locked, and its
# folder deleted. Git lists it at its recorded HEAD and never marks a
# locked worktree prunable, but its checkout is gone: the record must read
# as removed, not back, so restore and the app offer to bring it back.
name=back-locked-missing
make_repo
: >"$work/mode"
setup "$bin" remove --yes --json "$wt"
setup git -C "$repo" worktree add -q "$wt" w
setup git -C "$repo" worktree lock "$wt"
rm -rf "$wt"
"$bin" scan --json "$repo" >"$work/$name.scan" 2>/dev/null
state=$(jq -r '[.repositories[].removed[].state] | join(",")' "$work/$name.scan")
if [ "$state" != removed ]; then fail "scan lists the record as ${state:-nothing}, want removed"
elif [ "$(records)" != "1 records, 1 refs" ]; then fail "removal records: $(records), want 1 records, 1 refs"
elif common_ok; then ok "a locked worktree whose folder is gone does not make the record back"; fi

# --- 4h. A worktree at the record's path that residue cannot compare --------
# w is removed and added again by hand at its path, on its branch, and the
# record's HEAD is made one Git does not have, so Git cannot compare the
# two. residue cannot tell whether w is back: the record must read as
# unknown with the reason, never as removed. Stats seed nothing for it,
# and restore at its path refuses and changes nothing. Watch's landed rule
# reads `unknown` as it reads `back` (`watch::back_paths`); this case does
# not run watch.
name=back-unknown
make_repo
: >"$work/mode"
setup "$bin" remove --yes --json "$wt"
setup git -C "$repo" worktree add -q "$wt" w
info=$(find "$repo/.git/residue-removed" -mindepth 2 -maxdepth 2 -name info)
sed 's/^head .*/head 1111111111111111111111111111111111111111/' "$info" >"$work/$name.info" && cat "$work/$name.info" >"$info" || exit 2
rm -f "$XDG_STATE_HOME/residue/ledger.jsonl" "$HOME/Library/Application Support/residue/ledger.jsonl"
"$bin" scan --json "$repo" >"$work/$name.scan" 2>/dev/null
state=$(jq -r '[.repositories[].removed[].state] | join(",")' "$work/$name.scan")
why=$(jq -r '[.repositories[].removed[].back_error] | join(",")' "$work/$name.scan")
"$bin" stats --json >"$work/$name.stats" 2>/dev/null
freed=$(jq -r '.freed.all_time.worktrees // 0' "$work/$name.stats")
(cd "$repo" && "$bin" restore --json w >"$work/$name.out-restore" 2>"$work/$name.err-restore")
restored=$?
if [ "$state" != unknown ]; then fail "scan lists the record as ${state:-nothing}, want unknown"
elif ! grep -qF "could not compare its HEAD with the recorded 111111111111" <<<"$why"; then fail "back_error: $why"
elif [ "$freed" != 0 ]; then fail "stats count $freed freed worktrees, want 0"
elif [ "$restored" != 1 ]; then fail "restore exited $restored, want 1: $(cat "$work/$name.err-restore")"
elif ! grep -qF "so it cannot tell whether that worktree holds what this record keeps" "$work/$name.err-restore"; then fail "restore: $(cat "$work/$name.err-restore")"
elif [ "$(records)" != "1 records, 1 refs" ]; then fail "removal records: $(records), want 1 records, 1 refs"
elif [ "$(record "$wt")" != registered ] || [ "$(git -C "$wt" rev-parse HEAD)" != "$w_sha" ]; then fail "the worktree at w's path changed: $(record "$wt")"
elif common_ok; then ok "the record reads as unknown: $(cut -c1-100 <<<"$why")"; fi

# --- 4i. A purge stopped part way, then its entry restored ------------------
# w holds a commit only its HEAD reflog names. A sweep purges w's week-old
# trash entry. Its Git deletes an ignored file, stops in the checkout at a
# file it cannot delete, goes on to its own directory, as Git's remove
# does, and is stopped there once its logs/ is gone: the sweep and that
# Git are killed together, as Ctrl-C does. The purge's removal record
# stays, and its refs keep the commit through `git gc`. `residue restore`
# of the entry must move w back, say part of it may be gone, put the HEAD
# reflog back from the record, and only then drop the record.
name=purge-stopped-restore
make_repo
setup git -C "$wt" checkout -q --detach
setup git -C "$wt" commit -q --allow-empty -m lone
lone=$(git -C "$wt" rev-parse HEAD) || exit 2
setup git -C "$wt" checkout -q w
: >"$work/mode"
setup "$bin" remove --yes --json --trash "$wt"
entry=$(find "$repo/.git/residue-trash" -mindepth 1 -maxdepth 1 -type d 2>/dev/null)
tinfo=$entry/info
sed 's/^trashed [0-9]* .*/trashed 1767225600 2026-01-01T00:00:00Z/' "$tinfo" >"$work/$name.info" && cat "$work/$name.info" >"$tinfo" || exit 2
echo stop-purge >"$work/mode"
start ".sweep" remove --all --yes --json --idle 30d "$repo"
sweep_pid=$pid
if ! await "$work/at-purge"; then
    finish "$sweep_pid"
    fail "the sweep never reached the purge's git worktree remove (exit $status)"
else
    kill_tree "$sweep_pid"
    finish "$sweep_pid"
    : >"$work/mode"
    stopped_records=$(records)
    stopped_at=$(record "$entry/w")
    git -C "$repo" -c gc.reflogExpire=now -c gc.reflogExpireUnreachable=now gc -q --prune=now >/dev/null 2>&1
    kept=no
    git -C "$repo" cat-file -e "$lone^{commit}" 2>/dev/null && kept=yes
    (cd "$repo" && "$bin" restore --json "$entry" >"$work/$name.out-restore" 2>"$work/$name.err-restore")
    restored=$?
    out=$work/$name.out-restore
    if [ "$stopped_records" != "1 records, 2 refs" ]; then fail "after the stopped purge: $stopped_records, want 1 records, 2 refs"
    elif [ "$stopped_at" != registered ]; then fail "after the stopped purge Git has, in the trash: $stopped_at"
    elif [ "$kept" != yes ]; then fail "git gc lost the commit only the purge's record held"
    elif [ "$restored" != 0 ]; then fail "restore exited $restored: $(cat "$work/$name.err-restore")"
    elif ! jq -e '.restore.notes | any(contains("if its purge had started deleting, part of it is gone"))' "$out" >/dev/null; then fail "restore does not say part of it may be gone: $(jq -c '.restore.notes' "$out")"
    elif [ "$(record "$wt")" != registered ] || [ "$(git -C "$wt" rev-parse HEAD)" != "$w_sha" ]; then fail "w is not back at its HEAD: $(record "$wt")"
    elif ! grep -qx "$lone" <<<"$(git -C "$wt" reflog --format=%H)"; then fail "the restored worktree's reflog does not name the commit only its HEAD reflog held"
    elif [ "$(sums "$wt")" != "$(grep -v '^\./node_modules/' <<<"$w_sum")" ]; then fail "w's tracked files changed, or its deleted ignored file is back"
    elif [ "$(records)" != "0 records, 0 refs" ]; then fail "after restore: $(records), want 0 records, 0 refs"
    elif [ -n "$(ls -A "$repo/.git/residue-trash" 2>/dev/null)" ]; then fail "the trash entry is left after the restore"
    elif common_ok; then ok "restore moves w back, says part may be gone, puts its reflog back, then drops the purge's record"; fi
fi

# --- 4j. 4i, with the purge's record from a release that takes no hold ------
# The stopped purge's removal record is made one that a release taking no
# hold writes, while the PID it names still runs: no hold shows that purge,
# and only that PID says it may still be deleting. Restore of the entry and
# of the record must refuse and change nothing. Once that process ends,
# restore of the entry moves w back and drops the purge's record.
name=purge-unheld-restore
make_repo
: >"$work/mode"
setup "$bin" remove --yes --json --trash "$wt"
entry=$(find "$repo/.git/residue-trash" -mindepth 1 -maxdepth 1 -type d 2>/dev/null)
tinfo=$entry/info
sed 's/^trashed [0-9]* .*/trashed 1767225600 2026-01-01T00:00:00Z/' "$tinfo" >"$work/$name.info" && cat "$work/$name.info" >"$tinfo" || exit 2
echo stop-purge >"$work/mode"
start ".sweep" remove --all --yes --json --idle 30d "$repo"
sweep_pid=$pid
if ! await "$work/at-purge"; then
    finish "$sweep_pid"
    fail "the sweep never reached the purge's git worktree remove (exit $status)"
else
    kill_tree "$sweep_pid"
    finish "$sweep_pid"
    : >"$work/mode"
    record_dir=$(find "$repo/.git/residue-removed" -mindepth 1 -maxdepth 1 -type d 2>/dev/null)
    stopped_sum=$(sums "$entry/w")
    unheld "$record_dir"
    (cd "$repo" && "$bin" restore --json "$entry" >"$work/$name.out-entry" 2>"$work/$name.err-entry")
    by_entry=$?
    (cd "$repo" && "$bin" restore --json "$record_dir" >"$work/$name.out-record" 2>"$work/$name.err-record")
    by_record=$?
    during=$(record "$entry/w")
    during_sum=$(sums "$entry/w")
    during_records=$(records)
    kill "$runner"
    wait "$runner" 2>/dev/null
    forget "$runner"
    (cd "$repo" && "$bin" restore --json "$entry" >"$work/$name.out-restore" 2>"$work/$name.err-restore")
    restored=$?
    want="takes no hold, and its process $runner still runs, so it may still be deleting the worktree"
    if [ -z "$record_dir" ]; then fail "the stopped purge left no removal record"
    elif [ "$by_entry" != 1 ]; then fail "restore of the entry while the record's writer runs exited $by_entry, want 1"
    elif ! grep -qF "$want" "$work/$name.err-entry"; then fail "restore of the entry: $(cat "$work/$name.err-entry")"
    elif [ "$by_record" != 1 ]; then fail "restore of the record while its writer runs exited $by_record, want 1"
    elif ! grep -qF "$want" "$work/$name.err-record"; then fail "restore of the record: $(cat "$work/$name.err-record")"
    elif [ "$during" != registered ] || [ "$during_sum" != "$stopped_sum" ]; then fail "the refused restores changed w, in the trash: $during"
    elif [ "$during_records" != "1 records, 1 refs" ]; then fail "after the refused restores: $during_records, want 1 records, 1 refs"
    elif [ "$restored" != 0 ]; then fail "restore once the writer ended exited $restored: $(cat "$work/$name.err-restore")"
    elif [ "$(record "$wt")" != registered ] || [ "$(git -C "$wt" rev-parse HEAD)" != "$w_sha" ]; then fail "w is not back at its HEAD: $(record "$wt")"
    elif [ "$(records)" != "0 records, 0 refs" ]; then fail "after restore: $(records), want 0 records, 0 refs"
    elif [ -n "$(ls -A "$repo/.git/residue-trash" 2>/dev/null)" ]; then fail "the trash entry is left after the restore"
    elif common_ok; then ok "while a purge's record with no hold names a run that still runs, both restores refuse; once it ends, the entry comes back"; fi
fi

# --- 4k. A purge killed while Git deletes its own directory -----------------
# w holds a commit only its HEAD reflog names. A sweep purges w's trash
# entry; its Git deletes the checkout, then the HEAD in w's Git directory,
# and is killed there with the sweep. Git still lists w in the trash, with
# no files and a Git directory it cannot read. Restore of the entry must
# refuse, change nothing, and name the line that brings w back; that line,
# restore of the purge's record, must clear what is left of w under the
# entry's hold, recreate w at its recorded HEAD with the commit in its
# reflog, empty the trash and drop the record.
name=purge-cut-restore
make_repo
setup git -C "$wt" checkout -q --detach
setup git -C "$wt" commit -q --allow-empty -m lone
lone=$(git -C "$wt" rev-parse HEAD) || exit 2
setup git -C "$wt" checkout -q w
: >"$work/mode"
setup "$bin" remove --yes --json --trash "$wt"
entry=$(find "$repo/.git/residue-trash" -mindepth 1 -maxdepth 1 -type d 2>/dev/null)
tinfo=$entry/info
sed 's/^trashed [0-9]* .*/trashed 1767225600 2026-01-01T00:00:00Z/' "$tinfo" >"$work/$name.info" && cat "$work/$name.info" >"$tinfo" || exit 2
echo cut-purge >"$work/mode"
start ".sweep" remove --all --yes --json --idle 30d "$repo"
sweep_pid=$pid
if ! await "$work/at-purge"; then
    finish "$sweep_pid"
    fail "the sweep never reached the purge's git worktree remove (exit $status)"
else
    kill_tree "$sweep_pid"
    finish "$sweep_pid"
    : >"$work/mode"
    in_trash() { git -C "$repo" worktree list --porcelain | grep -c '^worktree .*/residue-trash/'; }
    listed=$(in_trash)
    record_name=$(basename "$(find "$repo/.git/residue-removed" -mindepth 1 -maxdepth 1 -type d 2>/dev/null)")
    git -C "$repo" -c gc.reflogExpire=now -c gc.reflogExpireUnreachable=now gc -q --prune=now >/dev/null 2>&1
    (cd "$repo" && "$bin" restore --json "$entry" >"$work/$name.out-entry" 2>"$work/$name.err-entry")
    by_entry=$?
    after_entry=$(records)
    listed_after=$(in_trash)
    (cd "$repo" && "$bin" restore --json "$record_name" >"$work/$name.out-restore" 2>"$work/$name.err-restore")
    restored=$?
    if [ "$listed" != 1 ]; then fail "after the kill Git lists $listed worktrees in the trash, want 1"
    elif [ "$by_entry" != 1 ]; then fail "restore of the entry exited $by_entry, want 1: $(cat "$work/$name.err-entry")"
    elif ! grep -qF "a purge of it stopped partway and Git deleted its files; its removal record keeps its HEAD and reflog: \`residue restore $record_name\` recreates it" "$work/$name.err-entry"; then fail "restore of the entry: $(cat "$work/$name.err-entry")"
    elif [ "$after_entry" != "1 records, 2 refs" ] || [ "$listed_after" != 1 ]; then fail "the refused restore changed something: $after_entry, $listed_after listed in the trash"
    elif [ "$restored" != 0 ]; then fail "restore of the record exited $restored: $(cat "$work/$name.err-restore")"
    elif [ "$(jq -r '.restore.checkout' "$work/$name.out-restore")" != checked_out_branch ]; then fail "checkout $(jq -r '.restore.checkout' "$work/$name.out-restore"), want checked_out_branch"
    elif [ "$(record "$wt")" != registered ] || [ "$(git -C "$wt" rev-parse HEAD)" != "$w_sha" ]; then fail "w is not back at its HEAD: $(record "$wt")"
    elif ! grep -qx "$lone" <<<"$(git -C "$wt" reflog --format=%H)"; then fail "the restored worktree's reflog does not name the commit only its HEAD reflog held"
    elif [ "$(records)" != "0 records, 0 refs" ]; then fail "after restore: $(records), want 0 records, 0 refs"
    elif [ "$(in_trash)" != 0 ]; then fail "Git still lists a worktree in the trash"
    elif [ -n "$(ls -A "$repo/.git/residue-trash" 2>/dev/null)" ]; then fail "the trash entry is left after the restore"
    elif common_ok; then ok "restore of the entry names the record's restore, which clears what is left, recreates w with its reflog, and empties the trash"; fi
fi

# --- 4l. A purge by a release that takes no hold, killed alone -------------
# A sweep purges w's trash entry and is held at its `git worktree remove`;
# the sweep alone is killed, and its Git runs on. The purge's record and
# the entry's info are then rewritten, as new files, the way a release
# taking no hold leaves them: neither is held, and the record's PID is
# gone. Only that Git, a `git worktree remove` of w in the trash, shows
# that the purge may still delete it. Restore of the entry and of the
# record must refuse, name that Git and change nothing. Once it exits,
# restore of the entry moves w back and drops the purge's record.
name=purge-killed-unheld-git
make_repo
: >"$work/mode"
setup "$bin" remove --yes --json --trash "$wt"
entry=$(find "$repo/.git/residue-trash" -mindepth 1 -maxdepth 1 -type d 2>/dev/null)
tinfo=$entry/info
sed 's/^trashed [0-9]* .*/trashed 1767225600 2026-01-01T00:00:00Z/' "$tinfo" >"$work/$name.info" && cat "$work/$name.info" >"$tinfo" || exit 2
echo hold-purge >"$work/mode"
start ".sweep" remove --all --yes --json --idle 30d "$repo"
sweep_pid=$pid
if ! await "$work/at-purge"; then
    finish "$sweep_pid"
    fail "the sweep never reached the purge's git worktree remove (exit $status)"
else
    cp "$work/at-purge" "$work/held"
    git_pid=$(cat "$work/at-purge")
    kill -KILL "$sweep_pid"
    finish "$sweep_pid"
    : >"$work/mode"
    record_dir=$(find "$repo/.git/residue-removed" -mindepth 1 -maxdepth 1 -type d 2>/dev/null)
    unheld_file "$record_dir"
    cp "$tinfo" "$work/$name.tinfo" && mv "$work/$name.tinfo" "$tinfo" || exit 2
    stopped_sum=$(sums "$entry/w")
    (cd "$repo" && "$bin" restore --json "$entry" >"$work/$name.out-entry" 2>"$work/$name.err-entry")
    by_entry=$?
    (cd "$repo" && "$bin" restore --json "$record_dir" >"$work/$name.out-record" 2>"$work/$name.err-record")
    by_record=$?
    during=$(record "$entry/w")
    during_sum=$(sums "$entry/w")
    during_records=$(records)
    end_held purge
    ended=$?
    rm -f "$work/held"
    (cd "$repo" && "$bin" restore --json "$entry" >"$work/$name.out-restore" 2>"$work/$name.err-restore")
    restored=$?
    out=$work/$name.out-restore
    want="takes no hold, and process $git_pid, a \`git worktree remove\` of the worktree, still runs, so it may still be deleting it"
    if [ -z "$record_dir" ]; then fail "the held purge left no removal record"
    elif [ "$by_entry" != 1 ]; then fail "restore of the entry while that Git runs exited $by_entry, want 1"
    elif ! grep -qF "$want" "$work/$name.err-entry"; then fail "restore of the entry: $(cat "$work/$name.err-entry")"
    elif [ "$by_record" != 1 ]; then fail "restore of the record while that Git runs exited $by_record, want 1"
    elif ! grep -qF "$want" "$work/$name.err-record"; then fail "restore of the record: $(cat "$work/$name.err-record")"
    elif [ "$during" != registered ] || [ "$during_sum" != "$stopped_sum" ] || [ "$during_sum" != "$w_sum" ]; then fail "the refused restores changed w, in the trash: $during"
    elif [ "$during_records" != "1 records, 1 refs" ]; then fail "after the refused restores: $during_records, want 1 records, 1 refs"
    elif [ "$ended" != 0 ]; then fail "the held git worktree remove did not exit"
    elif [ "$restored" != 0 ]; then fail "restore once that Git exited exited $restored: $(cat "$work/$name.err-restore")"
    elif ! jq -e '.restore.notes | any(contains("if its purge had started deleting, part of it is gone"))' "$out" >/dev/null; then fail "restore does not say part of it may be gone: $(jq -c '.restore.notes' "$out")"
    elif [ "$(record "$wt")" != registered ] || [ "$(sums "$wt")" != "$w_sum" ]; then fail "w is not back whole: $(record "$wt")"
    elif [ "$(records)" != "0 records, 0 refs" ]; then fail "after restore: $(records), want 0 records, 0 refs"
    elif [ -n "$(ls -A "$repo/.git/residue-trash" 2>/dev/null)" ]; then fail "the trash entry is left after the restore"
    elif common_ok; then ok "while a purge's Git outlives a run that takes no hold, both restores name that Git and refuse; once it exits, the entry comes back"; fi
fi

# --- 4m. A purge's record written while a trash restore runs ---------------
# 4i's stopped purge leaves w unlocked in the trash. Its removal record is
# taken out, so the restore reads none, and the restore is held at its
# `git worktree unlock`, right before its move back. By then it must have
# locked w with residue's trash lock, so the `git worktree remove` of w
# that a purge by a release taking no hold runs is refused. The record then
# comes back, as such a purge would write it. The restore must move w
# back, say that part of it may be gone, name that record and keep it;
# restore of the record then finds w back, puts back its reflog and drops
# the record.
name=restore-locks-unlocked
make_repo
setup git -C "$wt" checkout -q --detach
setup git -C "$wt" commit -q --allow-empty -m lone
lone=$(git -C "$wt" rev-parse HEAD) || exit 2
setup git -C "$wt" checkout -q w
: >"$work/mode"
setup "$bin" remove --yes --json --trash "$wt"
entry=$(find "$repo/.git/residue-trash" -mindepth 1 -maxdepth 1 -type d 2>/dev/null)
tinfo=$entry/info
sed 's/^trashed [0-9]* .*/trashed 1767225600 2026-01-01T00:00:00Z/' "$tinfo" >"$work/$name.info" && cat "$work/$name.info" >"$tinfo" || exit 2
echo stop-purge >"$work/mode"
start ".sweep" remove --all --yes --json --idle 30d "$repo"
sweep_pid=$pid
if ! await "$work/at-purge"; then
    finish "$sweep_pid"
    fail "the sweep never reached the purge's git worktree remove (exit $status)"
else
    kill_tree "$sweep_pid"
    finish "$sweep_pid"
    stopped_records=$(records)
    stopped_at=$(record "$entry/w")
    stopped_sum=$(sums "$entry/w")
    record_dir=$(find "$repo/.git/residue-removed" -mindepth 1 -maxdepth 1 -type d 2>/dev/null)
    record_name=$(basename "${record_dir:-none}")
    if [ -n "$record_dir" ]; then
        mv "$record_dir" "$work/$name.record" || exit 2
        echo hold-restore-unlock >"$work/mode"
        start "-restore" restore --json "$entry"
        restore_pid=$pid
        for ((i = 0; i < 600; i++)); do
            [ -e "$work/at-restore-unlock" ] && break
            kill -0 "$restore_pid" 2>/dev/null || break
            sleep 0.1
        done
    fi
    if [ -z "$record_dir" ]; then
        : >"$work/mode"
        fail "the stopped purge left no removal record: $stopped_records"
    elif [ ! -e "$work/at-restore-unlock" ]; then
        finish "$restore_pid"
        : >"$work/mode"
        mv "$work/$name.record" "$record_dir"
        fail "the restore never reached its unlock of w in the trash (exit $status), so it never locked w"
    else
        during=$(record "$entry/w")
        git -C "$repo" worktree remove "$entry/w" >"$work/$name.out-git" 2>&1
        git_removed=$?
        during_sum=$(sums "$entry/w")
        mv "$work/$name.record" "$record_dir" || exit 2
        touch "$work/go-restore-unlock"
        finish "$restore_pid"
        restored=$status
        : >"$work/mode"
        out=$work/$name.out-restore
        back_records=$(records)
        left=$(ls -A "$repo/.git/residue-trash" 2>/dev/null)
        (cd "$repo" && "$bin" restore --json "$record_name" >"$work/$name.out-record" 2>"$work/$name.err-record")
        again=$?
        if [ "$stopped_records" != "1 records, 2 refs" ]; then fail "after the stopped purge: $stopped_records, want 1 records, 2 refs"
        elif [ "$stopped_at" != registered ]; then fail "after the stopped purge Git has, in the trash: $stopped_at, want it unlocked"
        elif [ "$during" != "registered, locked residue: in trash" ]; then fail "at its unlock, the restore had not locked w in the trash: $during"
        elif [ "$git_removed" = 0 ]; then fail "git worktree remove of w in the trash ran while the restore held it"
        elif [ "$during_sum" != "$stopped_sum" ]; then fail "w changed in the trash while the restore held it"
        elif [ "$restored" != 0 ]; then fail "restore exited $restored: $(cat "$work/$name.err-restore")"
        elif ! jq -e '.restore.notes | any(contains("if its purge had started deleting, part of it is gone"))' "$out" >/dev/null; then fail "restore does not say part of it may be gone: $(jq -c '.restore.notes' "$out")"
        elif ! jq -e --arg want "wrote the removal record $record_name while this restore ran" '.restore.notes | any(contains($want))' "$out" >/dev/null; then fail "restore does not name the record written meanwhile: $(jq -c '.restore.notes' "$out")"
        elif ! jq -e --arg want "\`residue restore $record_name --to NEWPATH\` recreates it from that record" '.restore.notes | any(contains($want))' "$out" >/dev/null; then fail "restore does not name the restore to a new path, should Git no longer list w: $(jq -c '.restore.notes' "$out")"
        elif [ "$(record "$wt")" != registered ] || [ "$(git -C "$wt" rev-parse HEAD)" != "$w_sha" ]; then fail "w is not back at its HEAD: $(record "$wt")"
        elif [ "$back_records" != "1 records, 2 refs" ]; then fail "after the restore: $back_records, want the record written meanwhile kept: 1 records, 2 refs"
        elif [ -n "$left" ]; then fail "the trash entry is left after the restore: $left"
        elif [ "$again" != 0 ]; then fail "restore of the record exited $again: $(cat "$work/$name.err-record")"
        elif [ "$(jq -r '.restore.checkout' "$work/$name.out-record")" != already_back ]; then fail "checkout $(jq -r '.restore.checkout' "$work/$name.out-record"), want already_back"
        elif ! jq -e '.restore.notes | any(contains("if its purge had started deleting, part of it is gone"))' "$work/$name.out-record" >/dev/null; then fail "restore of the record does not say part of it may be gone: $(jq -c '.restore.notes' "$work/$name.out-record")"
        elif ! grep -qx "$lone" <<<"$(git -C "$wt" reflog --format=%H)"; then fail "the worktree's reflog does not name the commit only its HEAD reflog held"
        elif [ "$(records)" != "0 records, 0 refs" ]; then fail "after both restores: $(records), want 0 records, 0 refs"
        elif common_ok; then ok "the restore locked w before it read the records, so Git refused a remove; it named the record written meanwhile, whose restore put the reflog back"; fi
    fi
fi

# --- 4n. A trash restore that refuses lifts the lock it put on -------------
# w is unlocked in the trash, and something new is at its path. Restore of
# the entry locks w, then refuses; it must lift that lock again and leave
# w as it was.
name=restore-lifts-its-lock
make_repo
: >"$work/mode"
setup "$bin" remove --yes --json --trash "$wt"
entry=$(find "$repo/.git/residue-trash" -mindepth 1 -maxdepth 1 -type d 2>/dev/null)
setup git -C "$repo" worktree unlock "$entry/w"
before=$(sums "$entry/w")
mkdir -p "$wt" && echo occupied >"$wt/occupied.txt" || exit 2
(cd "$repo" && "$bin" restore --json "$entry" >"$work/$name.out-restore" 2>"$work/$name.err-restore")
restored=$?
if [ "$restored" != 1 ]; then fail "restore exited $restored, want 1"
elif ! grep -qF "exists; move it away first" "$work/$name.err-restore"; then fail "restore: $(cat "$work/$name.err-restore")"
elif [ "$(record "$entry/w")" != registered ]; then fail "after the refusal Git has, in the trash: $(record "$entry/w"), want it unlocked as before"
elif [ "$(sums "$entry/w")" != "$before" ]; then fail "the refused restore changed w in the trash"
elif [ "$(cat "$wt/occupied.txt")" != occupied ]; then fail "the refused restore changed what is at w's path"
elif common_ok; then ok "the refused restore lifted its lock and changed nothing"; fi

# --- 4o. A damaged removal record named like the worktree ------------------
# A record residue cannot read, named like w, from before w went to the
# trash by more than a clock can be off, is another removal's: restore of
# the entry must ignore it. One named for the time since must stop the
# restore, which names the line that sets it aside; once that line has run,
# the restore brings w back.
name=old-damaged-record
make_repo
: >"$work/mode"
old=$repo/.git/residue-removed/w-20250101T000000Z
mkdir -p "$old" && echo "not a record" >"$old/info" || exit 2
setup "$bin" remove --yes --json --trash "$wt"
entry=$(find "$repo/.git/residue-trash" -mindepth 1 -maxdepth 1 -type d 2>/dev/null)
(cd "$repo" && "$bin" restore --json "$entry" >"$work/$name.out-old" 2>"$work/$name.err-old")
by_old=$?
back_old=$(record "$wt")
if [ "$by_old" != 0 ]; then fail "with only the old damaged record, restore exited $by_old: $(cat "$work/$name.err-old")"
elif [ "$back_old" != registered ]; then fail "with only the old damaged record, w is not back: $back_old"
else
    setup "$bin" remove --yes --json --trash "$wt"
    entry=$(find "$repo/.git/residue-trash" -mindepth 1 -maxdepth 1 -type d 2>/dev/null)
    new_name=w-$(date -u +%Y%m%dT%H%M%SZ)
    mkdir "$repo/.git/residue-removed/$new_name" && echo "not a record" >"$repo/.git/residue-removed/$new_name/info" || exit 2
    in_trash_sum=$(sums "$entry/w")
    (cd "$repo" && "$bin" restore --json "$entry" >"$work/$name.out-new" 2>"$work/$name.err-new")
    by_new=$?
    during=$(record "$entry/w")
    during_sum=$(sums "$entry/w")
    aside_line=$(grep -o '`mv [^`]*`' "$work/$name.err-new" | head -1 | tr -d '`')
    [ -n "$aside_line" ] && (cd "$repo" && eval "$aside_line")
    (cd "$repo" && "$bin" restore --json "$entry" >"$work/$name.out-restore" 2>"$work/$name.err-restore")
    restored=$?
    if [ "$by_new" != 1 ]; then fail "with a damaged record named since, restore exited $by_new, want 1"
    elif ! grep -qF "cannot read the removal record $new_name" "$work/$name.err-new" || ! grep -qF "sets it aside, keeping its files and refs" "$work/$name.err-new"; then fail "restore: $(cat "$work/$name.err-new")"
    elif ! grep -qF "residue-damaged-$new_name" <<<"$aside_line"; then fail "the set-aside line is not a move to residue-damaged-$new_name: $aside_line"
    elif [ "$during" != "registered, locked residue: in trash" ] || [ "$during_sum" != "$in_trash_sum" ]; then fail "the refused restore changed w in the trash: $during"
    elif [ ! -s "$repo/.git/residue-damaged-$new_name/info" ]; then fail "the set-aside line did not keep the record's files"
    elif [ "$restored" != 0 ]; then fail "restore once it was set aside exited $restored: $(cat "$work/$name.err-restore")"
    elif [ "$(record "$wt")" != registered ] || [ "$(sums "$wt")" != "$w_sum" ]; then fail "w is not back whole: $(record "$wt")"
    elif [ -n "$(ls -A "$repo/.git/residue-trash" 2>/dev/null)" ]; then fail "the trash entry is left after the restore"
    elif [ "$(records)" != "1 records, 0 refs" ]; then fail "removal records: $(records), want the old damaged one only: 1 records, 0 refs"
    elif common_ok; then ok "the old damaged record is ignored; one named since stops the restore until its printed line sets it aside"; fi
fi

# --- 4p. A trash restore whose worktree's .git is not a regular file --------
# w is in the trash, and its .git is a FIFO, then a symbolic link to a copy
# of the file Git wrote. Restore keeps that .git open while it restores:
# it must refuse at once, say why and change nothing, without waiting on
# the FIFO or following the link. With the file put back, it restores w.
name=restore-opens-no-fifo
make_repo
: >"$work/mode"
setup "$bin" remove --yes --json --trash "$wt"
entry=$(find "$repo/.git/residue-trash" -mindepth 1 -maxdepth 1 -type d 2>/dev/null)
cat "$entry/w/.git" >"$work/$name.dotgit" || exit 2
rm "$entry/w/.git" && mkfifo "$entry/w/.git" || exit 2
start "-fifo" restore --json "$entry"
fifo_pid=$pid
for ((i = 0; i < 150; i++)); do
    kill -0 "$fifo_pid" 2>/dev/null || break
    sleep 0.1
done
if kill -0 "$fifo_pid" 2>/dev/null; then
    kill_tree "$fifo_pid"
    finish "$fifo_pid"
    fifo=blocked
else
    finish "$fifo_pid"
    fifo=$status
fi
fifo_at=$(record "$entry/w")
# Each step runs only while w is still in the trash: a restore that went
# on has moved it back.
link=skipped file=skipped
if [ -d "$entry/w" ]; then
    rm -f "$entry/w/.git" && ln -s "$work/$name.dotgit" "$entry/w/.git" || exit 2
    (cd "$repo" && "$bin" restore --json "$entry" >"$work/$name.out-link" 2>"$work/$name.err-link")
    link=$?
    link_at=$(record "$entry/w")
fi
if [ -d "$entry/w" ]; then
    rm -f "$entry/w/.git" && cat "$work/$name.dotgit" >"$entry/w/.git" || exit 2
    (cd "$repo" && "$bin" restore --json "$entry" >"$work/$name.out-file" 2>"$work/$name.err-file")
    file=$?
fi
if [ "$fifo" = blocked ]; then fail "restore still waited on the FIFO after 15 s"
elif [ "$fifo" != 1 ]; then fail "restore with a FIFO exited $fifo, want 1"
elif ! grep -qF "is a FIFO, not the regular file Git writes there" "$work/$name.err-fifo" || ! grep -qF "so it changed nothing" "$work/$name.err-fifo"; then fail "restore with a FIFO: $(cat "$work/$name.err-fifo")"
elif [ "$fifo_at" != "registered, locked residue: in trash" ]; then fail "after the FIFO refusal Git has, in the trash: $fifo_at"
elif [ "$link" != 1 ]; then fail "restore with a link exited $link, want 1"
elif ! grep -qF "is a symbolic link, not the regular file Git writes there" "$work/$name.err-link"; then fail "restore with a link: $(cat "$work/$name.err-link")"
elif [ "$link_at" != "registered, locked residue: in trash" ]; then fail "after the link refusal Git has, in the trash: $link_at"
elif [ "$file" != 0 ]; then fail "restore with the file put back exited $file: $(cat "$work/$name.err-file")"
elif [ "$(record "$wt")" != registered ] || [ "$(sums "$wt")" != "$w_sum" ]; then fail "w is not back whole: $(record "$wt")"
elif common_ok; then ok "restore refuses at once on a FIFO or a link at .git, changes nothing, and restores w once the file is back"; fi

# --- 4q. A purge's unlock wins over a trash restore's ------------------------
# As 4m, but while the restore is held at its `git worktree unlock`, a
# purge by a release taking no hold unlocks w and removes it, and its
# record comes back. The restore's unlock then fails: it must say that it
# changed nothing and name `residue restore` of that record, which then
# recreates w with the commit only its HEAD reflog held.
name=restore-unlock-raced
make_repo
setup git -C "$wt" checkout -q --detach
setup git -C "$wt" commit -q --allow-empty -m lone
lone=$(git -C "$wt" rev-parse HEAD) || exit 2
setup git -C "$wt" checkout -q w
: >"$work/mode"
setup "$bin" remove --yes --json --trash "$wt"
entry=$(find "$repo/.git/residue-trash" -mindepth 1 -maxdepth 1 -type d 2>/dev/null)
tinfo=$entry/info
sed 's/^trashed [0-9]* .*/trashed 1767225600 2026-01-01T00:00:00Z/' "$tinfo" >"$work/$name.info" && cat "$work/$name.info" >"$tinfo" || exit 2
echo stop-purge >"$work/mode"
start ".sweep" remove --all --yes --json --idle 30d "$repo"
sweep_pid=$pid
if ! await "$work/at-purge"; then
    finish "$sweep_pid"
    fail "the sweep never reached the purge's git worktree remove (exit $status)"
else
    kill_tree "$sweep_pid"
    finish "$sweep_pid"
    record_dir=$(find "$repo/.git/residue-removed" -mindepth 1 -maxdepth 1 -type d 2>/dev/null)
    record_name=$(basename "${record_dir:-none}")
    if [ -n "$record_dir" ]; then
        mv "$record_dir" "$work/$name.record" || exit 2
        echo hold-restore-unlock >"$work/mode"
        start "-restore" restore --json "$entry"
        restore_pid=$pid
        for ((i = 0; i < 600; i++)); do
            [ -e "$work/at-restore-unlock" ] && break
            kill -0 "$restore_pid" 2>/dev/null || break
            sleep 0.1
        done
    fi
    if [ -z "$record_dir" ]; then
        : >"$work/mode"
        fail "the stopped purge left no removal record: $(records)"
    elif [ ! -e "$work/at-restore-unlock" ]; then
        finish "$restore_pid"
        : >"$work/mode"
        mv "$work/$name.record" "$record_dir"
        fail "the restore never reached its unlock of w in the trash (exit $status)"
    else
        git -C "$repo" worktree unlock "$entry/w" >"$work/$name.out-git" 2>&1 &&
            git -C "$repo" worktree remove "$entry/w" >>"$work/$name.out-git" 2>&1
        git_removed=$?
        mv "$work/$name.record" "$record_dir" || exit 2
        touch "$work/go-restore-unlock"
        finish "$restore_pid"
        restored=$status
        : >"$work/mode"
        (cd "$repo" && "$bin" restore --json "$record_name" >"$work/$name.out-record" 2>"$work/$name.err-record")
        again=$?
        if [ "$git_removed" != 0 ]; then fail "the purge's unlock and remove of w failed: $(cat "$work/$name.out-git")"
        elif [ "$restored" != 1 ]; then fail "restore exited $restored, want 1"
        elif ! grep -qF "this restore changed nothing" "$work/$name.err-restore"; then fail "restore does not say it changed nothing: $(cat "$work/$name.err-restore")"
        elif ! grep -qF "\`residue restore $record_name\` recreates it" "$work/$name.err-restore"; then fail "restore does not name the record that keeps w: $(cat "$work/$name.err-restore")"
        elif [ "$again" != 0 ]; then fail "restore of the record exited $again: $(cat "$work/$name.err-record")"
        elif [ "$(record "$wt")" != registered ] || [ "$(git -C "$wt" rev-parse HEAD)" != "$w_sha" ]; then fail "w is not back at its HEAD: $(record "$wt")"
        elif ! grep -qx "$lone" <<<"$(git -C "$wt" reflog --format=%H)"; then fail "the worktree's reflog does not name the commit only its HEAD reflog held"
        elif common_ok; then ok "the restore whose unlock lost says it changed nothing and names the record, whose restore brings w back with its reflog"; fi
    fi
fi

# --- 4r. 4m, and that purge's Git deletes the Git directory after the move -
# As in 4m, the record of a purge by a release taking no hold comes back
# while a trash restore is held at its unlock. Right after the restore's
# move back, that purge's Git, whose delete of the files failed, deletes
# the worktree's Git directory. The files are back at w's path, but Git
# cannot use them, so the restore must exit 1 with no JSON document, say
# that the files stay there and name `residue restore RECORD --to
# NEWPATH`, which must then bring w back at its HEAD.
name=restore-raced-git-dir
make_repo
: >"$work/mode"
setup "$bin" remove --yes --json --trash "$wt"
entry=$(find "$repo/.git/residue-trash" -mindepth 1 -maxdepth 1 -type d 2>/dev/null)
tinfo=$entry/info
sed 's/^trashed [0-9]* .*/trashed 1767225600 2026-01-01T00:00:00Z/' "$tinfo" >"$work/$name.info" && cat "$work/$name.info" >"$tinfo" || exit 2
echo stop-purge >"$work/mode"
start ".sweep" remove --all --yes --json --idle 30d "$repo"
sweep_pid=$pid
if ! await "$work/at-purge"; then
    finish "$sweep_pid"
    fail "the sweep never reached the purge's git worktree remove (exit $status)"
else
    kill_tree "$sweep_pid"
    finish "$sweep_pid"
    record_dir=$(find "$repo/.git/residue-removed" -mindepth 1 -maxdepth 1 -type d 2>/dev/null)
    record_name=$(basename "${record_dir:-none}")
    if [ -n "$record_dir" ]; then
        mv "$record_dir" "$work/$name.record" || exit 2
        echo hold-restore-unlock >"$work/mode"
        start "-restore" restore --json "$entry"
        restore_pid=$pid
        for ((i = 0; i < 600; i++)); do
            [ -e "$work/at-restore-unlock" ] && break
            kill -0 "$restore_pid" 2>/dev/null || break
            sleep 0.1
        done
    fi
    if [ -z "$record_dir" ]; then
        : >"$work/mode"
        fail "the stopped purge left no removal record: $(records)"
    elif [ ! -e "$work/at-restore-unlock" ]; then
        finish "$restore_pid"
        : >"$work/mode"
        mv "$work/$name.record" "$record_dir"
        fail "the restore never reached its unlock of w in the trash (exit $status)"
    else
        mv "$work/$name.record" "$record_dir" || exit 2
        echo drop-after-move >"$work/mode"
        touch "$work/go-restore-unlock"
        finish "$restore_pid"
        restored=$status
        : >"$work/mode"
        err=$work/$name.err-restore
        files=$(ls -A "$wt" 2>/dev/null)
        (cd "$repo" && "$bin" restore --json "$record_name" --to "$repo/.worktrees/w2" >"$work/$name.out-record" 2>"$work/$name.err-record")
        again=$?
        if [ "$restored" != 1 ]; then fail "restore exited $restored, want 1: $(cat "$err")"
        elif [ -s "$work/$name.out-restore" ]; then fail "restore printed a document: $(head -c 400 "$work/$name.out-restore")"
        elif ! grep -qF "Git can no longer use the worktree" "$err" || ! grep -qF "and stay there" "$err"; then fail "restore does not say Git cannot use the files it moved back: $(cat "$err")"
        elif ! grep -qF "\`residue restore $record_name --to NEWPATH\` recreates it from that record" "$err"; then fail "restore does not name the restore of the record to a new path: $(cat "$err")"
        elif [ -z "$files" ]; then fail "the files are not at w's path"
        elif [ "$again" != 0 ]; then fail "restore of the record to a new path exited $again: $(cat "$work/$name.err-record")"
        elif [ "$(record "$repo/.worktrees/w2")" != registered ] || [ "$(git -C "$repo/.worktrees/w2" rev-parse HEAD)" != "$w_sha" ]; then fail "w2 is not w at its HEAD: $(record "$repo/.worktrees/w2")"
        elif [ "$(records)" != "0 records, 0 refs" ]; then fail "after the restore of the record: $(records), want 0 records, 0 refs"
        elif common_ok; then ok "the restore whose worktree Git cannot use exits 1, says where its files stay, and its record brings w back elsewhere"; fi
    fi
fi

# --- 4s. A trash restore's Git move stops after its rename -----------------
# Git's move renames the directory, then records the new path, and may
# stop between the two. w's folder is gone by the time of the restore, so
# the restore makes it again. It must exit 1, say that it made that folder
# and that Git moved the files there, and name the repair; the lines it
# names, run as given, must bring w back there whole.
name=restore-move-stops-after-rename
make_repo
dest=$repo/nest/w
setup mkdir "$repo/nest"
setup git -C "$repo" worktree move "$wt" "$dest"
: >"$work/mode"
setup "$bin" remove --yes --json --trash "$dest"
entry=$(find "$repo/.git/residue-trash" -mindepth 1 -maxdepth 1 -type d 2>/dev/null)
setup rmdir "$repo/nest"
echo die-after-rename >"$work/mode"
start "" restore "$entry"
finish "$pid"
restored=$status
: >"$work/mode"
err=$work/$name.err
follow "$err"
if [ "$restored" != 1 ]; then fail "restore exited $restored, want 1"
elif grep -qF "changed nothing" "$err"; then fail "restore says it changed nothing: $(cat "$err")"
elif ! grep -qF "this restore made the folder " "$err" || ! grep -qF "/nest, which stays" "$err"; then fail "restore does not name the folder it made: $(cat "$err")"
elif ! grep -qF "Git moved its files to " "$err" || ! grep -qF " worktree repair " "$err"; then fail "restore does not say Git moved the files and name the repair: $(cat "$err")"
elif [ -z "$followed" ] || [ -n "$follow_failed" ]; then fail "the lines it names failed: ${follow_failed:-none named}"
elif [ "$(record "$dest")" != registered ] || [ "$(sums "$dest")" != "$w_sum" ]; then fail "w is not back whole at the new path: $(record "$dest")"
elif common_ok; then ok "the restore says Git moved the files and names the repair, which brings w back"; fi

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
# it. LACK is what the reason must say is gone; SUMS what its files are now,
# by default all of them as they were.
part_remove_ok() { # LACK [SUMS]
    if [ "$status" != 3 ]; then fail "exit $status, want 3"
    elif [ "$(jq -r '.results[0].result' "$out")" != failed ]; then fail "result $(jq -r '.results[0].result' "$out"), want failed"
    elif ! grep -qF "but $1" <<<"$reason" || ! grep -q "; its removal record keeps its HEAD and reflog: residue restore " <<<"$reason"; then fail "reason: $reason"
    elif grep -q "Recover with: " <<<"$reason"; then fail "it gives a move back as the recovery: $reason"
    elif [ -z "$aside" ] || [ "$(record "$aside")" != registered ]; then fail "want it at the moved-aside path, which Git lists: ${aside:-none}"
    elif [ "$(sums "$aside")" != "${2:-$w_sum}" ]; then fail "the moved worktree's files changed"
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
    elif ! grep -qF "Git can no longer use it" "$work/$name.err-restore" || ! grep -qF "delete $aside, run \`git -C $main worktree remove $aside\`, then run this restore again" "$work/$name.err-restore"; then fail "restore does not say what to clear: $(cat "$work/$name.err-restore")"
    elif [ "$(records)" != "1 records, 2 refs" ]; then fail "after the refused restore: $(records), want 1 records, 2 refs"
    else
        rm -rf -- "$aside"
        setup git -C "$repo" worktree remove "$aside"
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
# runs. That run holds the record, so restore must say so, change nothing
# and never print an empty command; once it ends, restore says what to
# clear (5b). A PID would not do: PIDs are reused, a hold is not.
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
    held_records=$(records)
    : >"$work/go-after"
    finish "$pid"
    if [ "$restored" != 1 ]; then fail "restore while the removal runs exited $restored, want 1"
    elif ! grep -qF "a residue run, or the \`git worktree remove\` it started, still holds this removal record (\`lsof " "$work/$name.err-restore" || ! grep -qF "it may still be deleting the worktree, or restoring or forgetting the record; run this restore again once it ends" "$work/$name.err-restore"; then fail "restore: $(cat "$work/$name.err-restore")"
    elif grep -qE "put it back with: *$" "$work/$name.err-restore"; then fail "restore prints an empty command: $(cat "$work/$name.err-restore")"
    elif [ "$held_records" != "1 records, 1 refs" ]; then fail "while the removal runs: $held_records, want 1 records, 1 refs"
    elif [ "$status" != 3 ]; then fail "the removal exited $status, want 3"
    elif [ -z "$aside" ] || [ "$(record "$aside")" != registered ]; then fail "want it at the moved-aside path, which Git lists: ${aside:-none}"
    elif [ "$(records)" = "0 records, 0 refs" ]; then fail "the removal record is gone"
    else ok "restore says the removal still holds the record, with no empty command"; fi
fi

# 5e: Git's remove stops in the checkout after it deleted an ignored file,
# before any file of its own directory: Git lists w at the moved-aside
# path, its Git directory is whole, and only the plan's measure of its
# files shows that part of it is gone. The record must stay, and restore
# must move w back, saying part of it may be gone.
name=remove-fails-part-way-files
part_remove part-remove-files
if part_remove_ok "part of its files may be gone: " "$(grep -v '^\./node_modules/' <<<"$w_sum")"; then
    restore_w
    if ! restored_ok moved_back; then :
    elif ! jq -e '.restore.notes | any(contains("if its removal had started deleting, part of it is gone"))' "$work/$name.out-restore" >/dev/null; then fail "restore does not say part of it may be gone: $(jq -c '.restore.notes' "$work/$name.out-restore")"
    elif [ "$(sums "$wt")" != "$(grep -v '^\./node_modules/' <<<"$w_sum")" ]; then fail "the restored worktree's tracked files changed"
    else ok "the record stays though Git lists w with its Git directory whole, and restore moves it back"; fi
fi

# --- 5f. A record restore with --to on another filesystem -------------------
# residue moves, never copies, so it refuses a --to on another filesystem;
# it checks the nearest folder that exists first, so the refusal leaves no
# folder made on the way there, even when the folder --to names went
# after residue read it. The other filesystem is a disk image on
# macOS, which any user may attach, and /dev/shm on Linux.
name=restore-to-other-fs
other=
case $(uname -s) in
Darwin)
    mkdir -p "$work/ovol/.fseventsd" && : >"$work/ovol/.metadata_never_index" && : >"$work/ovol/.fseventsd/no_log" || exit 2
    setup hdiutil create -quiet -fs HFS+ -volname residue-other -layout NONE -format UDRW -srcfolder "$work/ovol" "$work/other.dmg"
    mkdir -p "$work/other" || exit 2
    echo "$work/other" >"$work/mounted-at"
    if attached=$(hdiutil attach -nobrowse -noverify -noautoopen -mountpoint "$work/other" "$work/other.dmg"); then
        echo "${attached%%[[:space:]]*}" >"$work/mounted-dev"
        other=$work/other
    fi
    ;;
Linux)
    if [ -d /dev/shm ] && [ -w /dev/shm ] && [ "$(stat -c %d /dev/shm)" != "$(stat -c %d "$work")" ]; then
        other=$(mktemp -d /dev/shm/residue-recovery.XXXXXX) || exit 2
    fi
    ;;
esac
if [ -z "$other" ]; then
    echo "skip $name: no other filesystem a user may write to here"
else
    part_remove part-remove-files
    if part_remove_ok "part of its files may be gone: " "$(grep -v '^\./node_modules/' <<<"$w_sum")"; then
        # --to needs its parent to exist when residue reads it; the folder
        # goes once the restore holds the record, so the move back would
        # have to make it again.
        mkdir -p "$other/a/b" || exit 2
        echo hold-second-list >"$work/mode"
        start ".r" restore --json --to "$other/a/b/w" "$wt"
        pid_r=$pid
        if ! await "$work/at-list"; then
            finish "$pid_r"
            fail "the restore never reached its second git worktree list (exit $status)"
        else
            : >"$work/mode"
            rmdir "$other/a/b" || exit 2
            touch "$work/go-list"
            finish "$pid_r"
            restored=$status
            if [ "$restored" != 1 ]; then fail "restore --to on another filesystem exited $restored, want 1"
            elif [ -e "$other/a/b" ]; then fail "the refused restore made folders on the other filesystem: $(find "$other/a" | sed "s#$other#OTHER#g" | tr '\n' ' ')"
            elif ! grep -qF "$(cd "$other" && pwd -P)/a is on another filesystem than " "$work/$name.err.r"; then fail "restore does not say the nearest folder of the destination is on another filesystem: $(cat "$work/$name.err.r")"
            elif [ "$(record "$aside")" != registered ] || [ "$(records)" != "1 records, 2 refs" ]; then fail "the refused restore changed w or its record: $(record "$aside"), $(records)"
            elif common_ok; then ok "restore refuses a --to on another filesystem before it makes any folder there"; fi
        fi
    fi
fi
case $other in
/dev/shm/*) rm -rf -- "$other" ;;
"$work/other") unmount_planted || echo "recovery-check: could not unmount the scratch volume at $other" >&2 ;;
esac
rm -f "$work/mounted-at" "$work/mounted-dev"

# --- 6. A restore and a forget of the same removal record -------------------
# w holds a commit only its HEAD reflog names, its removal record pins it,
# and the record is past --keep-removed, so a sweep forgets it. Each run
# holds the record while it acts on it. 6: the restore is held at its `git
# worktree add` while a sweep runs: the sweep must leave the record, and
# the restore must put the saved reflog back, so the commit survives `git
# gc`, and drop the record. 6b: the sweep is held at the `git update-ref`
# that deletes the refs while a restore runs: the restore must refuse and
# change nothing, and the sweep forgets the record. 6c and 6d: a release
# that takes no hold forgets the record during the restore, which this
# script does by hand, after a sweep of this release left it.
forget_setup() {
    make_repo
    setup git -C "$wt" checkout -q --detach
    setup git -C "$wt" commit -q --allow-empty -m lone
    lone=$(git -C "$wt" rev-parse HEAD) || exit 2
    setup git -C "$wt" checkout -q w
    : >"$work/mode"
    setup "$bin" remove --yes --json "$wt"
    [ "$(records)" = "1 records, 2 refs" ] || setup false records
    info=$(find "$repo/.git/residue-removed" -name info)
    sed 's/^removed [0-9]* .*/removed 1767225600 2026-01-01T00:00:00Z/' "$info" >"$work/$name.info" && cat "$work/$name.info" >"$info" || exit 2
}
sweep() { # OUT-SUFFIX: a sweep of the repository that removes nothing and forgets the record
    start "$1" remove --all --yes --json --idle 30d "$repo"
}
# The sweep left the record a restore holds: it skipped it, and says why.
sweep_skipped() { # OUT-FILE
    jq -e '.forgotten | length == 1 and .[0].result == "skipped" and (.[0].reason | contains("a restore, holds it now"))' "$1" >/dev/null
}
# Forget every removal record as a release that takes no hold does: its
# refs, then its directory, whoever holds it.
forget_unheld() {
    git -C "$repo" for-each-ref --format='delete %(refname)' refs/residue | git -C "$repo" update-ref --stdin || exit 2
    rm -rf "$repo/.git/residue-removed"/*
}
restored_reflog_ok() { # w is back, its reflog names the lone commit, git gc keeps it
    git -C "$repo" gc -q --prune=now >/dev/null 2>&1
    if [ "$(record "$wt")" != registered ]; then fail "at its path Git has: $(record "$wt")"
    elif ! grep -qx "$lone" <<<"$(git -C "$wt" reflog --format=%H)"; then fail "the restored reflog does not name the commit only its HEAD reflog held"
    elif ! git -C "$repo" cat-file -e "$lone^{commit}" 2>/dev/null; then fail "git gc lost the commit only its HEAD reflog held"
    elif [ "$(records)" != "0 records, 0 refs" ]; then fail "after both runs: $(records), want 0 records, 0 refs"
    else return 0; fi
    return 1
}

name=restore-forget-race
forget_setup
echo hold-add >"$work/mode"
start ".r" restore --json "$wt"
pid_r=$pid
if ! await "$work/at-add"; then
    finish "$pid_r"
    fail "the restore never reached git worktree add (exit $status)"
else
    : >"$work/mode"
    sweep ".f"
    finish "$pid"
    status_f=$status
    left_f=$(records)
    touch "$work/go-add"
    finish "$pid_r"
    status_r=$status
    notes=$(jq -c '.restore.notes' "$work/$name.out.r")
    if [ "$status_f" != 0 ] || ! sweep_skipped "$work/$name.out.f"; then fail "the sweep: exit $status_f, forgotten: $(jq -c '.forgotten' "$work/$name.out.f"); want 0, skipped as a restore holds it"
    elif [ "$left_f" != "1 records, 2 refs" ]; then fail "after the sweep: $left_f, want 1 records, 2 refs"
    elif [ "$status_r" != 0 ]; then fail "restore exited $status_r: $(cat "$work/$name.err.r")"
    elif grep -q 'stays at' <<<"$notes"; then fail "restore says the record stays: $notes"
    elif restored_reflog_ok && common_ok; then ok "the sweep left the record the restore holds; the restore put the reflog back and dropped it"; fi
fi

name=forget-restore-race
forget_setup
echo hold-update-ref >"$work/mode"
sweep ".f"
pid_f=$pid
if ! await "$work/at-update-ref"; then
    finish "$pid_f"
    fail "the sweep never reached git update-ref (exit $status)"
else
    : >"$work/mode"
    start ".r" restore --json "$wt"
    finish "$pid"
    status_r=$status
    during=$(records)
    touch "$work/go-update-ref"
    finish "$pid_f"
    status_f=$status
    forgot=$(jq -r '[.forgotten[].result] | join(",")' "$work/$name.out.f")
    if [ "$status_r" != 1 ]; then fail "restore while the sweep forgets the record exited $status_r, want 1"
    elif ! grep -qF "another residue run is restoring or forgetting the record" "$work/$name.err.r"; then fail "restore: $(cat "$work/$name.err.r")"
    elif [ "$during" != "1 records, 2 refs" ]; then fail "after the refused restore: $during, want 1 records, 2 refs"
    elif [ -e "$wt" ]; then fail "the refused restore put something at w's path: $(record "$wt")"
    elif [ "$status_f,$forgot" != "0,forgotten" ]; then fail "the sweep: exit $status_f, forgotten: $forgot; want 0, forgotten"
    elif [ "$(records)" != "0 records, 0 refs" ]; then fail "after the sweep: $(records), want 0 records, 0 refs"
    elif common_ok; then ok "restore refused while the sweep held the record; the sweep then forgot it"; fi
fi

# 6c: the record is forgotten, by a release that takes no hold, after the
# restore listed it and before the restore reads it; a sweep of this
# release, run first, leaves it. The restore must change nothing and say
# that another run took the record, never recreate w from a record that
# is gone.
name=forget-before-restore-reads
forget_setup
echo hold-second-list >"$work/mode"
start ".r" restore --json "$wt"
pid_r=$pid
if ! await "$work/at-list"; then
    finish "$pid_r"
    fail "the restore never reached its second git worktree list (exit $status)"
else
    : >"$work/mode"
    sweep ".f"
    finish "$pid"
    status_f=$status
    forget_unheld
    touch "$work/go-list"
    finish "$pid_r"
    status_r=$status
    if [ "$status_f" != 0 ] || ! sweep_skipped "$work/$name.out.f"; then fail "the sweep: exit $status_f, forgotten: $(jq -c '.forgotten' "$work/$name.out.f"); want 0, skipped as a restore holds it"
    elif [ "$status_r" != 1 ]; then fail "restore exited $status_r, want 1"
    elif [ -s "$work/$name.out.r" ]; then fail "restore printed a document: $(head -c 300 "$work/$name.out.r")"
    elif ! grep -q "another residue run forgot or restored this removal record after this restore listed it, so residue changed nothing" "$work/$name.err.r"; then fail "restore: $(cat "$work/$name.err.r")"
    elif [ -e "$wt" ]; then fail "something is at w's path: $(record "$wt")"
    elif [ "$(records)" != "0 records, 0 refs" ]; then fail "$(records), want 0 records, 0 refs"
    elif common_ok; then ok "restore changed nothing: $(sed 's/^residue: //' "$work/$name.err.r" | cut -c1-110)"; fi
fi

# 6d: 6 in a reftable repository, where Git reads no logs/HEAD file, so
# the restore cannot put the reflog back and keeps the record. A release
# that takes no hold forgot it meanwhile, after a sweep of this release
# left it, so the restore must say so and name the commit no ref keeps
# now, never that the record stays.
if ! git init -q --ref-format=reftable "$work/reftable.probe" >/dev/null 2>&1; then
    echo "skip restore-forget-race-reftable: this Git cannot make a reftable repository (Git 2.45 and later)"
else
    name=restore-forget-race-reftable
    ref_format=reftable forget_setup
    echo hold-add >"$work/mode"
    start ".r" restore --json "$wt"
    pid_r=$pid
    if ! await "$work/at-add"; then
        finish "$pid_r"
        fail "the restore never reached git worktree add (exit $status)"
    else
        : >"$work/mode"
        sweep ".f"
        finish "$pid"
        status_f=$status
        forget_unheld
        touch "$work/go-add"
        finish "$pid_r"
        status_r=$status
        notes=$(jq -c '.restore.notes' "$work/$name.out.r")
        if [ "$status_f" != 0 ] || ! sweep_skipped "$work/$name.out.f"; then fail "the sweep: exit $status_f, forgotten: $(jq -c '.forgotten' "$work/$name.out.f"); want 0, skipped as a restore holds it"
        elif [ "$status_r" != 0 ]; then fail "restore exited $status_r: $(cat "$work/$name.err.r")"
        elif grep -q 'stays at' <<<"$notes"; then fail "restore says the record stays: $notes"
        elif ! jq -e --arg lone "$lone" '.restore.notes | any(startswith("another residue run forgot the removal record") and contains("1 commit only the old reflog held is no longer kept") and contains($lone))' "$work/$name.out.r" >/dev/null; then fail "the notes do not name the commit no ref keeps: $notes"
        elif [ "$(record "$wt")" != registered ]; then fail "at its path Git has: $(record "$wt")"
        elif [ "$(records)" != "0 records, 0 refs" ]; then fail "$(records), want 0 records, 0 refs"
        elif common_ok; then ok "restore says the sweep forgot the record and names $lone"; fi
    fi
fi

# --- 7. A `git status` limited to some folders fails ----------------------
# logs/ holds only ignored files and no tracked one, so residue lists it
# with a traditional run limited to it (#120). When that run fails, residue
# runs traditional mode on the whole worktree, as before: logs/ is one
# entry, and it keeps the worktree.
name=limited-status-fails
make_repo
printf '*.log\n' >>"$repo/.git/info/exclude"
mkdir "$wt/logs"
echo a >"$wt/logs/a.log"
before=$(sums "$wt")
echo fail-limited >"$work/mode"
start "" remove --yes --json "$wt"
finish "$pid"
: >"$work/mode"
out=$work/$name.out
calls=$(paste -s -d ' ' - <"$work/statuses" 2>/dev/null)
if [ "$status" != 3 ]; then fail "exit $status, want 3"
elif [ "$(jq -r '.results[0].result' "$out")" != refused ]; then fail "result $(jq -r '.results[0].result' "$out"), want refused"
elif [ "$(jq -r '[.results[0].worktree.ignored[].path] | sort | join(" ")' "$out")" != "logs/ node_modules/" ]; then
    fail "ignored entries: $(jq -c '[.results[0].worktree.ignored[].path]' "$out"), want logs/ and node_modules/"
elif [ "$calls" != "matching limited full" ]; then fail "git status ran: $calls; want matching limited full"
elif [ "$(record "$wt")" != registered ] || [ "$(sums "$wt")" != "$before" ]; then fail "w changed: $(record "$wt")"
elif common_ok; then ok "the whole traditional run answered, and logs/ keeps w"; fi

# Each line in backquotes in scan's text OUT that deletes Git's record of a
# worktree, or makes a branch to keep commits first, one a line; status 0
# when there is one. A line names its repository (`git -C MAIN ...`); the
# note's own mentions of `git gc` and `git worktree prune`, which say when
# Git drops a record, run nothing.
deletes() { # OUT
    grep -o '`[^`]*`' "$1" | grep -E 'git -C .* worktree (remove|prune)|branch keep-|branch -[dD] |update-ref -d|git -C .* gc( |`)'
}

# --- 8. A worktree Git lists whose directory is gone -----------------------
# w, on a detached HEAD that only it holds, is moved by hand. No removal
# record names it, so scan must name no line that deletes Git's record of
# it: no remove, no prune, no branch to keep its commits first. It must
# give the repair first, say when `git gc` drops that record, and name the
# reflog line; the repair it names must then bring w back at that HEAD.
name=stale-names-repair
make_repo
setup git -C "$wt" checkout -q --detach
setup git -C "$wt" commit -q --allow-empty -m lone
lone=$(git -C "$wt" rev-parse HEAD) || exit 2
moved=$work/$name-moved
mv "$wt" "$moved" || exit 2
"$bin" scan "$repo" >"$work/$name.out" 2>&1
scanned=$?
note=$(grep -F 'worktree repair NEWPATH' "$work/$name.out")
setup git -C "$repo" worktree repair "$moved"
if [ "$scanned" != 0 ]; then fail "scan exited $scanned"
elif [ -z "$note" ]; then fail "scan does not give the repair"
elif grep -qF "only Git metadata remains" "$work/$name.out"; then fail "scan says only Git metadata remains"
elif deletes "$work/$name.out"; then fail "scan names a line that deletes Git's record of w: $(deletes "$work/$name.out")"
elif [[ $note != *"worktree repair NEWPATH\` reconnects it"*"\`git gc\` drops Git's record of it, with its HEAD, reflog and index, 3 months by default (\`gc.worktreePruneExpire\`) after its index last changed, never while it is locked; \`git -C "*" reflog show worktrees/w/HEAD\` lists the commits"* ]]; then fail "scan does not give the repair first, then when git gc drops the record and the reflog line: $note"
elif [ "$(record "$moved")" != registered ] || [ "$(git -C "$moved" rev-parse HEAD)" != "$lone" ]; then fail "after the repair, w is not back at its HEAD: $(record "$moved")"
elif common_ok; then ok "scan names no line that deletes Git's record, gives the repair first, and the repair brings w back"; fi

# --- 8b. A hidden worktree whose directory is gone, named by no record ------
# w, on a detached HEAD that only it holds, is under its hidden name, as a
# removal that stopped after its move leaves it, and its directory is then
# gone from there: moved by hand, or on a disk not mounted. No removal
# record names it: `recovery`, which a script may run, must be empty, and
# the text must name no line that deletes Git's record of it, give the
# repair first and say when `git gc` drops that record. The repair must
# then bring w back at its HEAD.
name=gone-aside-no-record
make_repo
setup git -C "$wt" checkout -q --detach
setup git -C "$wt" commit -q --allow-empty -m lone
lone=$(git -C "$wt" rev-parse HEAD) || exit 2
sleep 0 &
dead=$!
wait "$dead"
aside=$repo/.worktrees/.w.residue-removing-$dead
setup git -C "$repo" worktree move "$wt" "$aside"
moved=$work/$name-moved
mv "$aside" "$moved" || exit 2
left=$(interrupted)
"$bin" scan "$repo" >"$work/$name.out" 2>&1
scanned=$?
note=$(grep -F 'worktree repair NEWPATH' "$work/$name.out")
setup git -C "$repo" worktree repair "$moved"
if [ "$scanned" != 0 ]; then fail "scan exited $scanned"
elif ! jq -e 'length == 1 and .[0].present == false and .[0].removal_record == null' <<<"$left" >/dev/null; then fail "scan does not list w as gone with no record: $left"
elif [ "$(jq -r '.[0].recovery' <<<"$left")" != "" ]; then fail "scan's recovery would drop the HEAD only w keeps: $(jq -r '.[0].recovery' <<<"$left")"
elif deletes "$work/$name.out"; then fail "scan names a line that deletes Git's record of w: $(deletes "$work/$name.out")"
elif [[ $note != *"its directory is gone; "*"worktree repair NEWPATH\` reconnects it"*"\`git gc\` drops Git's record of it"*"reflog show worktrees/w/HEAD\`"* ]]; then fail "scan does not give the repair first, then when git gc drops the record: $note"
elif [ "$(record "$moved")" != registered ] || [ "$(git -C "$moved" rev-parse HEAD)" != "$lone" ]; then fail "after the repair, w is not back at its HEAD: $(record "$moved")"
elif common_ok; then ok "scan gives no recovery and no line that deletes for a gone hidden worktree no record names, and the repair it names first brings w back"; fi

# --- 8b2. What `git gc` does with a record, by Git's own rule -------------
# Git drops the record of a worktree whose `.git` is gone at once when it
# has no index, whatever gc.worktreePruneExpire says, and keeps the record
# of one whose `gitdir` file names a `.git` that is there, however old its
# index (`should_prune_worktree`). The note must say which, and Git must
# then do what it says: `git gc` with the default expiry, and `git
# worktree prune`, which drops every one that rule lets go at once.
name=stale-no-index
make_repo
mv "$wt" "$work/$name-moved" || exit 2
rm -f "$repo/.git/worktrees/w/index" || exit 2
"$bin" scan "$repo" >"$work/$name.out" 2>&1
scanned=$?
note=$(grep -F 'worktree repair NEWPATH' "$work/$name.out")
git -C "$repo" gc -q >"$work/$name.out-gc" 2>&1
if [ "$scanned" != 0 ]; then fail "scan exited $scanned"
elif deletes "$work/$name.out"; then fail "scan names a line that deletes Git's record of w: $(deletes "$work/$name.out")"
elif [[ $note != *"worktree repair NEWPATH\` reconnects it"*"the next \`git gc\` or \`git worktree prune\` drops Git's record of it, with its HEAD and reflog, since Git finds no index for it, unless it is locked first; \`git -C "*" reflog show worktrees/w/HEAD\` lists the commits"* ]]; then fail "scan does not say that the next git gc drops the record of w, which has no index: $note"
elif [ -d "$repo/.git/worktrees/w" ]; then fail "git gc kept the record of w, which the note says it drops"
elif common_ok; then ok "scan says the next git gc drops the record of a gone worktree with no index, and git gc does"; fi

name=leftover-dotgit-kept
make_repo
setup git -C "$wt" checkout -q --detach
sleep 0 &
dead=$!
wait "$dead"
aside=$repo/.worktrees/.w.residue-removing-$dead
setup git -C "$repo" worktree move "$wt" "$aside"
rm -f "$repo/.git/worktrees/w/HEAD" || exit 2
touch -t 202001010000 "$repo/.git/worktrees/w/index" || exit 2
"$bin" scan "$repo" >"$work/$name.out" 2>&1
scanned=$?
note=$(grep -F 'no removal record names it' "$work/$name.out")
git -C "$repo" worktree prune >"$work/$name.out-prune" 2>&1
git -C "$repo" gc -q >"$work/$name.out-gc" 2>&1
if [ "$scanned" != 0 ]; then fail "scan exited $scanned"
elif deletes "$work/$name.out"; then fail "scan names a line that deletes Git's record of w: $(deletes "$work/$name.out")"
elif [[ $note != *"Git can no longer use it (Git reads no HEAD in its Git directory), and no removal record names it; Git keeps its record, with its HEAD, reflog and index, while "*"/.w.residue-removing-$dead/.git is there, which its \`gitdir\` file names; \`git -C "*" reflog show worktrees/w/HEAD\` lists the commits its HEAD reflog holds"* ]]; then fail "scan does not say that Git keeps the record while the .git its gitdir file names is there: $note"
elif [ ! -d "$repo/.git/worktrees/w" ]; then fail "git worktree prune or git gc dropped the record of w, which the note says Git keeps"
elif common_ok; then ok "scan says Git keeps the record of a hidden worktree whose .git is there, and git worktree prune and git gc keep it"; fi

# Git's worktrees at a path that ends in one of SUFFIX...
listed_at() { # SUFFIX...
    local s
    for s in "$@"; do
        git -C "$repo" worktree list --porcelain | grep -E "^worktree .*$s\$"
    done
}

# The note scan's text OUT gives under the worktree at a path that ends in
# SUFFIX.
note_of() { # OUT SUFFIX
    grep -A1 -F "$2 " "$1" | tail -1
}

# Run the repair NOTE names, `git -C MAIN worktree repair NEWPATH`, with
# NEWPATH the folder MOVED its worktree was moved to. Status: the repair's,
# or 1 when NOTE names none.
repair() { # NOTE MOVED
    local cmd
    cmd=$(grep -o '`git -C [^`]* worktree repair NEWPATH`' <<<"$1" | head -1)
    [ -n "$cmd" ] || return 1
    cmd=${cmd#\`}
    cmd=${cmd%NEWPATH\`}$(printf '%q' "$2")
    bash -c "$cmd" >>"$work/$name.out-repair" 2>&1
}

# The commits of COMMIT... that the repository no longer has.
gone_commits() { # COMMIT...
    local c
    for c in "$@"; do
        git -C "$repo" cat-file -e "$c^{commit}" 2>/dev/null || printf ' %s' "$c"
    done
}

# The commits of COMMIT... that the reflog line NOTE names does not list,
# run as given, with full hashes.
unlisted() { # NOTE COMMIT...
    local cmd listed c
    cmd=$(grep -o '`git -C [^`]* reflog show [^`]*`' <<<"$1" | head -1)
    shift
    cmd=${cmd#\`}
    cmd=${cmd%\`}
    listed=$([ -n "$cmd" ] && bash -c "$cmd --format=%H" 2>>"$work/$name.out-reflog")
    for c in "$@"; do
        grep -qx "$c" <<<"$listed" || printf ' %s' "$c"
    done
}

# --- 8c. Gone worktrees with commits only their HEAD reflogs reach ---------
# x, detached, made a commit it then reset away, so only its HEAD reflog
# reaches it; y, on branch y, made a detached commit and went back to its
# branch. Both directories are then moved by hand. No removal record
# names them, so scan must name no line that deletes Git's record of
# either; the reflog line in each one's note must list its commit; and
# the repair it names, run for each, must keep both commits through
# `git gc --prune=now` with `gc.worktreePruneExpire=now`.
stale_reflog() {
    make_repo
    setup git -C "$repo" worktree add -q --detach "$repo/.worktrees/x"
    setup git -C "$repo/.worktrees/x" commit -q --allow-empty -m only-in-reflog
    x_only=$(git -C "$repo/.worktrees/x" rev-parse HEAD) || exit 2
    setup git -C "$repo/.worktrees/x" reset -q --hard HEAD~1
    setup git -C "$repo" worktree add -q -b y "$repo/.worktrees/y"
    setup git -C "$repo/.worktrees/y" checkout -q --detach
    setup git -C "$repo/.worktrees/y" commit -q --allow-empty -m detached-then-back
    y_only=$(git -C "$repo/.worktrees/y" rev-parse HEAD) || exit 2
    setup git -C "$repo/.worktrees/y" checkout -q y
    mv "$repo/.worktrees/x" "$work/$name-x" && mv "$repo/.worktrees/y" "$work/$name-y" || exit 2
    : >"$work/mode"
    "$bin" scan "$repo" >"$work/$name.out" 2>&1
    scanned=$?
    x_note=$(note_of "$work/$name.out" /.worktrees/x)
    y_note=$(note_of "$work/$name.out" /.worktrees/y)
    missing=$(unlisted "$x_note" "$x_only")$(unlisted "$y_note" "$y_only")
    repaired=ok
    repair "$x_note" "$work/$name-x" || repaired="x: exit $?"
    repair "$y_note" "$work/$name-y" || repaired="y: exit $?"
    setup git -C "$repo" -c gc.worktreePruneExpire=now gc -q --prune=now
    lost=$(gone_commits "$x_only" "$y_only")
    if [ "$scanned" != 0 ]; then fail "scan exited $scanned"
    elif deletes "$work/$name.out"; then fail "scan names a line that deletes Git's record of a worktree: $(deletes "$work/$name.out")"
    elif [ -n "$missing" ]; then fail "the reflog lines the notes name do not list:$missing"
    elif [ "$repaired" != ok ]; then fail "the repair a note names failed or is missing, $repaired"
    elif [ -n "$lost" ]; then fail "after the repairs and git gc --prune=now, the repository lacks:$lost"
    elif [ "$(record "$work/$name-x")" != registered ] || [ "$(record "$work/$name-y")" != registered ]; then fail "after the repairs Git has x: $(record "$work/$name-x"), y: $(record "$work/$name-y")"
    elif common_ok; then ok "scan names no line that deletes, each reflog line lists its commit, and the repairs keep both through git gc --prune=now"; fi
}
name=stale-keeps-reflog
stale_reflog
if ! git init -q --ref-format=reftable "$work/reftable.probe-stale" >/dev/null 2>&1; then
    echo "skip stale-keeps-reflog-reftable: this Git cannot make a reftable repository (Git 2.45 and later)"
else
    name=stale-keeps-reflog-reftable
    ref_format=reftable stale_reflog
fi

# --- 8d. Two gone worktrees with the same directory name -------------------
# a/wt and b/wt, each detached at a commit only it reaches, are moved by
# hand. Git names their Git directories wt and wt1: each note must name
# its own reflog, which lists its commit, and scan no line that deletes;
# the repairs must keep both commits through `git gc --prune=now`.
name=stale-keeps-same-name
make_repo
for d in a b; do
    mkdir -p "$repo/$d" || exit 2
    setup git -C "$repo" worktree add -q --detach "$repo/$d/wt"
    setup git -C "$repo/$d/wt" commit -q --allow-empty -m "only-on-$d"
done
a_head=$(git -C "$repo/a/wt" rev-parse HEAD) || exit 2
b_head=$(git -C "$repo/b/wt" rev-parse HEAD) || exit 2
mv "$repo/a/wt" "$work/$name-a" && mv "$repo/b/wt" "$work/$name-b" || exit 2
"$bin" scan "$repo" >"$work/$name.out" 2>&1
scanned=$?
a_note=$(note_of "$work/$name.out" /a/wt)
b_note=$(note_of "$work/$name.out" /b/wt)
missing=$(unlisted "$a_note" "$a_head")$(unlisted "$b_note" "$b_head")
repaired=ok
repair "$a_note" "$work/$name-a" || repaired="a: exit $?"
repair "$b_note" "$work/$name-b" || repaired="b: exit $?"
setup git -C "$repo" -c gc.worktreePruneExpire=now gc -q --prune=now
lost=$(gone_commits "$a_head" "$b_head")
if [ "$scanned" != 0 ]; then fail "scan exited $scanned"
elif deletes "$work/$name.out"; then fail "scan names a line that deletes Git's record of a worktree: $(deletes "$work/$name.out")"
elif [[ $a_note != *"reflog show worktrees/wt/HEAD\`"* ]] || [[ $b_note != *"reflog show worktrees/wt1/HEAD\`"* ]]; then fail "the notes do not name each worktree's own reflog: a: $a_note b: $b_note"
elif [ -n "$missing" ]; then fail "the reflog lines the notes name do not list:$missing"
elif [ "$repaired" != ok ]; then fail "the repair a note names failed or is missing, $repaired"
elif [ -n "$lost" ]; then fail "after the repairs and git gc --prune=now, the repository lacks:$lost"
elif [ "$(record "$work/$name-a")" != registered ] || [ "$(record "$work/$name-b")" != registered ]; then fail "after the repairs Git has a: $(record "$work/$name-a"), b: $(record "$work/$name-b")"
elif common_ok; then ok "each gone wt names its own reflog, scan names no line that deletes, and both HEADs survive the repairs and git gc --prune=now"; fi

# --- 8e. A locked worktree whose directory is gone ---------------------------
# p, detached at a commit only it reaches, is locked, as a worktree on a
# portable disk is, and its directory is gone. Git keeps its record, and
# its remove refuses a locked worktree: scan must say it is locked and
# name no remove, and the lines it names must leave Git's record of it.
name=stale-locked-no-remove
make_repo
setup git -C "$repo" worktree add -q --detach "$repo/.worktrees/p"
setup git -C "$repo/.worktrees/p" commit -q --allow-empty -m on-the-disk
p_head=$(git -C "$repo/.worktrees/p" rev-parse HEAD) || exit 2
setup git -C "$repo" worktree lock --reason "portable disk" "$repo/.worktrees/p"
rm -rf "$repo/.worktrees/p" || exit 2
"$bin" scan "$repo" >"$work/$name.out" 2>&1
scanned=$?
note=$(grep -A1 -F "/.worktrees/p " "$work/$name.out" | tail -1)
follow "$work/$name.out"
if [ "$scanned" != 0 ]; then fail "scan exited $scanned"
elif [[ $note != *"it is locked (portable disk)"* ]]; then fail "scan does not say it is locked: $note"
elif [[ $note == *"worktree remove"* ]]; then fail "scan names a remove of a locked worktree: $note"
elif [ -z "$(listed_at /.worktrees/p)" ]; then fail "after the lines scan names, Git no longer lists p"
elif ! git -C "$repo" cat-file -e "$p_head^{commit}" 2>/dev/null; then fail "p's HEAD is gone"
elif common_ok; then ok "scan says the gone worktree is locked and names no remove"; fi

# --- 8f. A move back to a path that is now a directory ---------------------
# w is under its hidden name, as a removal that stopped after its move
# leaves it, with no record, and a new directory is at its old path. Git's
# move into an existing directory puts the worktree inside it, so the
# move back scan gives, run as given, must refuse and leave w where it is,
# and scan's text must say what is at that path and give no move back.
# Once that directory is gone, the same line must bring w back whole.
name=move-back-guard
make_repo
sleep 0 &
dead=$!
wait "$dead"
aside=$repo/.worktrees/.w.residue-removing-$dead
setup git -C "$repo" worktree move "$wt" "$aside"
aside=$(cd "$aside" && pwd -P) || exit 2
mkdir "$wt" && echo new >"$wt/new.txt" || exit 2
line=$(interrupted | jq -r '.[0].recovery')
"$bin" scan "$repo" >"$work/$name.out" 2>&1
text=$(grep -A1 -F ".w.residue-removing-$dead " "$work/$name.out" | tail -1)
bash -c "$line" >"$work/$name.out-move" 2>&1
moved=$?
taken=$(record "$aside")
inside=$(find "$wt" -mindepth 1 -maxdepth 1 -name '.w.residue-removing-*' | head -1)
rm -rf "$wt"
bash -c "$line" >"$work/$name.out-again" 2>&1
again=$?
if [ -z "$line" ]; then fail "scan gives no recovery line"
elif [[ $text != *"/.worktrees/w is a directory now, so it cannot go back there; move that away, then run residue scan again"* ]]; then fail "scan's text does not say a directory is at w's path: $text"
elif [[ $text == *"put it back:"* ]] || [[ $text == *"worktree move"* ]]; then fail "scan's text gives a move back while a directory is at w's path: $text"
elif [ "$moved" = 0 ]; then fail "the move back ran into the new directory: $line"
elif [ -n "$inside" ]; then fail "w was moved inside the new directory: $inside"
elif [ "$taken" != registered ]; then fail "at its hidden path Git has: $taken"
elif [ "$again" != 0 ]; then fail "once the directory is gone, the move back exited $again: $(cat "$work/$name.out-again")"
elif [ "$(record "$wt")" != registered ] || [ "$(sums "$wt")" != "$w_sum" ]; then fail "w is not back whole: $(record "$wt")"
elif common_ok; then ok "scan says a directory is at the old path and gives no move back, the line refuses, and it brings w back once the directory is gone"; fi

# --- 9. A filesystem with no flock(2) ---------------------------------------
# Only a release that takes no hold leaves entries on such a filesystem. An
# interposer makes flock(2) fail with ENOTSUP in residue alone, as there:
# `__interpose` with DYLD_INSERT_LIBRARIES on macOS, LD_PRELOAD on Linux
# with glibc. It is built with cc; elsewhere, or without cc, 9 is skipped.
shim_env=
case $(uname -s) in
Darwin) shim_env=DYLD_INSERT_LIBRARIES shim_flags=-dynamiclib ;;
Linux)
    if getconf GNU_LIBC_VERSION >/dev/null 2>&1 && ldd "$bin" 2>/dev/null | grep -q 'libc\.so'; then
        shim_env=LD_PRELOAD shim_flags="-shared -fPIC"
    fi
    ;;
esac
noflock=
if [ -z "$shim_env" ]; then
    noflock_skip="no flock(2) interposer for $(uname -s) without glibc, or for a static binary"
elif ! command -v cc >/dev/null; then
    noflock_skip="no cc to build the flock(2) interposer"
else
    cat >"$work/noflock.c" <<'C'
#include <errno.h>
#include <sys/file.h>
#ifdef __APPLE__
static int noflock(int fd, int op) { (void)fd; (void)op; errno = ENOTSUP; return -1; }
__attribute__((used)) static struct { const void *replacement; const void *replacee; }
    interpose_flock __attribute__((section("__DATA,__interpose"))) = { (const void *)noflock, (const void *)flock };
#else
int flock(int fd, int op) { (void)fd; (void)op; errno = ENOTSUP; return -1; }
#endif
C
    # shellcheck disable=SC2086
    if cc $shim_flags -o "$work/noflock.so" "$work/noflock.c" >"$work/noflock.cc" 2>&1; then
        noflock=$work/noflock.so
    else
        noflock_skip="cc could not build the flock(2) interposer: $(head -c 300 "$work/noflock.cc")"
    fi
fi
nf() { # ARGS...: residue, where flock(2) fails
    env "$shim_env=$noflock" "$bin" "$@"
}
# The record a removal of the worktree at .worktrees/NAME wrote.
record_of() { # NAME
    basename "$(find "$repo/.git/residue-removed" -mindepth 1 -maxdepth 1 -name "$1-*" | head -1)"
}
# Once: the interposer must take effect, or every case below would pass or
# fail on a filesystem that does give flock(2).
if [ -n "$noflock" ]; then
    name=noflock-probe
    make_repo
    : >"$work/mode"
    setup "$bin" remove --yes --json "$wt"
    (cd "$repo" && nf restore "$(record_of w)") >"$work/$name.out" 2>&1
    if ! grep -qF "gives no flock(2) lock" "$work/$name.out"; then
        noflock=
        noflock_skip="the flock(2) interposer did not take effect: a restore with it did not say the filesystem gives no flock(2) lock: $(head -c 300 "$work/$name.out")"
    fi
fi
# The shells each printed line runs in: /bin/sh, and dash where it is
# installed, which reads no $'...'.
shells=(/bin/sh)
dash=$(command -v dash 2>/dev/null)
[ -n "$dash" ] && [ "$dash" != "$(readlink -f /bin/sh 2>/dev/null)" ] && shells+=("$dash")
# The words SHELL splits LINE into, each in brackets, with stub test and git.
words_in() { # SHELL LINE
    "$1" -c 'test() { printf "[%s]" test "$@"; echo; return 0; }; git() { printf "[%s]" git "$@"; echo; return 0; }; '"$2"
}

# 9: records of worktrees Git no longer lists, then each restored there.
# The restore can hold none of them, so it changes nothing and names the
# `git worktree add` that recreates the worktree by hand, after a guard
# that stops it while anything is at the path. Each line must split into
# the same words in every shell, those naming the path byte for byte, and,
# run as given, must check out the recorded HEAD: d detached, its HEAD
# reflog holding a commit only it reached; g's branch gone, with origin/g
# at another commit, which Git would check out for `worktree add PATH g`;
# r's branch rewritten; b's branch checked out elsewhere; k's branch as it
# was; t's too, at a path with a tab. Once each is back, its restore must
# say so and name no recreate, also r and b on their BRANCH-restored. The
# records keep d's reflog commit through `git gc --prune=now`.
name=noflock-recreate
if [ -z "$noflock" ]; then
    echo "skip $name: $noflock_skip"
else
    make_repo
    base=$(git -C "$repo" rev-parse HEAD) || exit 2
    setup git -C "$repo" worktree add -q --detach "$repo/.worktrees/d"
    setup git -C "$repo/.worktrees/d" commit -q --allow-empty -m only-in-reflog
    x_only=$(git -C "$repo/.worktrees/d" rev-parse HEAD) || exit 2
    setup git -C "$repo/.worktrees/d" reset -q --hard HEAD~1
    want_d=$base
    tab=$(printf 't\tx')
    for b in g r b k t; do
        p=$repo/.worktrees/$b
        [ "$b" = t ] && p=$repo/.worktrees/$tab
        setup git -C "$repo" worktree add -q -b "$b" "$p" "$base"
        setup git -C "$p" commit -q --allow-empty -m "on-$b"
        eval "want_$b=\$(git -C \"\$repo\" rev-parse \"\$b\")" || exit 2
    done
    # residue names each path as the system resolves it, such as
    # /private/var for /var on macOS.
    wts=$(cd "$repo/.worktrees" && pwd -P) || exit 2
    : >"$work/mode"
    for b in d g r b k; do
        setup "$bin" remove --yes --json "$repo/.worktrees/$b"
    done
    setup "$bin" remove --yes --json "$repo/.worktrees/$tab"
    setup git -C "$repo" remote add origin "$repo"
    setup git -C "$repo" update-ref refs/remotes/origin/g "$base"
    setup git -C "$repo" branch -D g
    setup git -C "$repo" branch -f r "$base"
    setup git -C "$repo" worktree add -q "$work/$name-other" b
    wrong=
    n=0
    for b in d g r b k t; do
        p=$repo/.worktrees/$b pc=$wts/$b rec=$(record_of "$b")
        [ "$b" = t ] && p=$repo/.worktrees/$tab pc=$wts/$tab rec=$(record_of t_x)
        (cd "$repo" && nf restore "$rec") >"$work/$name.out-$b" 2>&1
        st=$?
        line=$(sed -n 's/.*recreates the worktree by hand: \(.*\); the record still keeps .*/\1/p' "$work/$name.out-$b")
        first=
        for sh in "${shells[@]}"; do
            words=$(words_in "$sh" "$line" 2>&1)
            [ -n "$first" ] || first=$words
            [ "$words" = "$first" ] || wrong+=" $b: $sh splits the line into other words: $words;"
        done
        case $first in
        "[test][!][-e][$pc]"*"[git][-C]["*"][worktree][add]"*"[$pc]"*) ;;
        *) wrong+=" $b: the line does not guard and name $pc as one word: $first;" ;;
        esac
        # Each line runs for real in one shell, in turn.
        sh=${shells[$((n % ${#shells[@]}))]}
        n=$((n + 1))
        if [ "$st" != 1 ]; then wrong+=" $b: restore exited $st;"
        elif [ -z "$line" ]; then wrong+=" $b: no recreate line: $(head -c 400 "$work/$name.out-$b");"
        elif LC_ALL=C grep -q '[[:cntrl:]]' <<<"$line"; then wrong+=" $b: the line holds a control character;"
        elif ! "$sh" -c "$line" >>"$work/$name.out-ran" 2>&1; then wrong+=" $b: the recreate failed in $sh: $line;"
        else
            eval "want=\$want_$b"
            got=$(git -C "$p" rev-parse HEAD 2>/dev/null)
            [ "$got" = "$want" ] || wrong+=" $b: HEAD $got, want $want: $line;"
        fi
        grep -qF "; the record still keeps the commits of its HEAD reflog" "$work/$name.out-$b" ||
            wrong+=" $b: the note does not say the record keeps the commits of its HEAD reflog;"
    done
    for b in d r b; do
        (cd "$repo" && nf restore "$(record_of "$b")") >"$work/$name.out-again-$b" 2>&1
        st=$?
        if [ "$st" != 1 ]; then wrong+=" $b: the restore once it is back exited $st;"
        elif ! grep -qF "Git lists its worktree at its original path again, and the record stays" "$work/$name.out-again-$b"; then wrong+=" $b: the restore once it is back does not say so: $(head -c 400 "$work/$name.out-again-$b");"
        elif grep -qF "recreates the worktree by hand" "$work/$name.out-again-$b"; then wrong+=" $b: the restore once it is back names a recreate;"
        fi
    done
    setup git -C "$repo" gc -q --prune=now
    if [ -n "$wrong" ]; then fail "$wrong"
    elif [ "$(git -C "$repo" rev-parse r)" != "$base" ]; then fail "the recreate of r moved its rewritten branch"
    elif [ "$(git -C "$repo" rev-parse -q --verify r-restored-2)" != "" ] || [ "$(git -C "$repo" rev-parse -q --verify b-restored-2)" != "" ]; then fail "a restore made a second BRANCH-restored"
    elif ! git -C "$repo" cat-file -e "$x_only^{commit}" 2>/dev/null; then fail "the commit only d's HEAD reflog held is gone"
    elif [ "$(records)" != "6 records, 7 refs" ]; then fail "after the restores: $(records), want every record kept: 6 records, 7 refs"
    elif common_ok; then ok "each recreate line splits alike in ${shells[*]}, checks out the recorded HEAD, and once back each restore says so"; fi
fi

# 9b: an expired removal record and an expired trash entry, which a sweep
# cannot hold, so it keeps both. The plan says so once, in its "kept"
# lists and as `kept` in the JSON, with no warning that repeats it.
name=noflock-kept
if [ -z "$noflock" ]; then
    echo "skip $name: $noflock_skip"
else
    make_repo
    for b in x t; do
        setup git -C "$repo" worktree add -q -b "$b" "$repo/.worktrees/$b"
    done
    : >"$work/mode"
    setup "$bin" remove --yes --json --keep-removed 1m "$repo/.worktrees/x"
    setup "$bin" remove --yes --json --trash "$repo/.worktrees/t"
    for info in "$repo/.git/residue-removed/$(record_of x)/info" "$(find "$repo/.git/residue-trash" -mindepth 2 -maxdepth 2 -name info)"; do
        sed -E 's/^(removed|trashed) [0-9]+ .*/\1 1767225600 2026-01-01T00:00:00Z/' "$info" >"$work/$name.info" && cat "$work/$name.info" >"$info" || exit 2
    done
    nf remove --all --yes --json --idle 52w --keep-trash 1d --keep-removed 1d "$repo" >"$work/$name.out" 2>"$work/$name.err"
    swept=$?
    nf remove --all --yes --idle 52w --keep-trash 1d --keep-removed 1d "$repo" >"$work/$name.out-text" 2>"$work/$name.err-text"
    texted=$?
    if [ "$swept" != 0 ] || [ "$texted" != 0 ]; then fail "the sweeps exited $swept and $texted"
    elif ! jq -e '(.purged | length == 1 and .[0].result == "kept") and (.forgotten | length == 1 and .[0].result == "kept")' "$work/$name.out" >/dev/null; then fail "the JSON does not keep both: $(jq -c '{purged: [.purged[].result], forgotten: [.forgotten[].result]}' "$work/$name.out")"
    elif ! jq -e '.warnings | all(contains("kept the") | not)' "$work/$name.out" >/dev/null; then fail "the JSON warns again: $(jq -c '.warnings' "$work/$name.out")"
    elif grep -qF "warning: kept" "$work/$name.err" "$work/$name.err-text"; then fail "a sweep warns again on stderr: $(grep -hF "warning: kept" "$work/$name.err" "$work/$name.err-text" | head -c 400)"
    elif ! grep -qF "kept removal records (1)" "$work/$name.out-text" || ! grep -qF "kept in the trash (1)" "$work/$name.out-text"; then fail "the text plan does not list both as kept"
    elif [ "$(records)" != "1 records, 1 refs" ] || [ -z "$(ls -A "$repo/.git/residue-trash")" ]; then fail "after the sweeps: $(records), trash: $(ls -A "$repo/.git/residue-trash")"
    elif common_ok; then ok "the sweeps keep the record and the trash entry, said once in the plan and the JSON"; fi
fi

echo "recovery-check: $passes passed, $failures failed"
[ "$failures" -eq 0 ]
