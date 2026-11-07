# shellcheck shell=sh disable=SC2034 # it sets variables for the scripts that source it
# Sourced by the smoke tests and the race, contract and recovery checks
# (POSIX sh and bash). Sets $partial_probe on the systems whose activity
# probe reads working directories but not open files.
#
# On NetBSD, OpenBSD and DragonFly the probe (src/activity.rs, Seen::limits)
# can show that a worktree is in use, never that it is unused: a process can
# hold a file open inside a worktree while its working directory is
# elsewhere. So a worktree with no use found is activity `unknown`
# (`partial_probe`), which keeps it, and nothing there is ever a removal
# candidate. A case that expects a removal cannot pass there. Each script
# skips such a case with $partial_probe_reason, by name, and asserts instead
# that a clean, idle worktree is kept as `unknown` for that reason.
#
# The gate is the operating system's name, not anything residue reports, so
# a residue that started offering removals there fails these checks.
case $(uname -s) in
NetBSD | OpenBSD | DragonFly) partial_probe=yes ;;
*) partial_probe= ;;
esac
partial_probe_reason="the activity probe on $(uname -s) reads working directories, not open files, so residue keeps every worktree it finds no use of as activity unknown (partial_probe) and removes none"
# What residue prints for such a worktree: the check's short text (as is,
# and as an extended regular expression), and the end of its detail.
partial_probe_short='activity unknown (partial_probe)'
partial_probe_short_re='activity unknown \(partial_probe\)'
partial_probe_detail='not open files, so it cannot show that no process is using it'

skip_partial() { # CASE-NAME
    echo "skip $1: $partial_probe_reason"
}
