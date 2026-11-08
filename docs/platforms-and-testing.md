# Platforms and testing

Back to [Tested on](../README.md#tested-on).

These are the platforms, Git versions, and results from the README table this page replaces, including the 0.0.6 rows added on main. They are records of those runs, not a promise about a platform that is not named.

The README used to say that, apart from the smoke test and `scripts/race-check.sh`, residue had no automated test suite. That sentence does not match [What CI runs](#what-ci-runs): CI also runs `scripts/recovery-check.sh`, `scripts/contract-check.sh`, and `.github/service-smoke.sh` on the platforms named there, and on NetBSD, OpenBSD and DragonFly `scripts/partial-probe.sh` skips removal cases. CHANGELOG.md also records an in-crate verdict-fold test, `cargo test --locked`, on the macOS and Linux runners. This page does not add a pass or a failure beyond those notes.

## Release-binary records

### 0.0.6 on macOS 26.3.1, Apple silicon

**Git:** Apple Git 2.50.1

`scan --json` of a working `~/Work` with 23 linked agent worktrees, run at the same moment as the 0.0.5 release binary: every verdict and check status identical on 22; on the 23rd, 0.0.5 saw the 0.0.6 process itself (`in use by residue[PID]`), whose binary was built inside it. lsof inspected 431 of 666 running processes. `scripts/race-check.sh` 10 passed, `scripts/recovery-check.sh` 8 passed, `scripts/contract-check.sh` 17 passed, `.github/smoke.sh` and `.github/watch-smoke.sh` passed.

### 0.0.5 on macOS 26.3.1 (25D771280a), Apple silicon

**Git:** Apple Git 2.50.1

The CONTRIBUTING states: every check id, status and verdict identical to the 0.0.4 release binary, without `--idle` and under `--idle 2d`. `scripts/race-check.sh`: 7 passed; the 0.0.4 binary, in the case that adds a file to `node_modules/` at the prompt, removed the worktree (exit 0). A `node_modules/` that grew from 1,204 to 1,388 files at the prompt: `skipped .worktrees/big: node_modules/ changed since you confirmed (1,204 -> 1,388 files)`, exit 3. `remove --all --yes --idle 1m` moved four worktrees to the trash (exit 0); `residue restore` by branch, by name, and by entry path from `/` put three back, and the empty trash directory went with the last; an entry whose record was set eight days back was purged by the next `--idle 2d` sweep, its branch kept. A `git` wrapper that sent SIGKILL to residue before the lock, before the unlock, and after deleting the directory: `scan` listed all three under interrupted removals, and each printed recovery command put the worktree back or cleared Git's record. A process starting in the worktree, or a file appearing, right after the move aside: moved back, exit 3. Something at the original path again: `failed`, with a recovery command that worked. `.github/smoke.sh`, `.github/watch-smoke.sh` and `.github/service-smoke.sh` passed.

### 0.0.5 on Ubuntu 24.04.5 LTS, arm64 (Docker, Linux 7.0.14)

**Git:** 2.43.0

Static musl binary as a non-root user: check ids, statuses and verdicts identical to the 0.0.4 musl binary, without `--idle` and under `--idle 2d`; `scripts/race-check.sh` 7 passed under util-linux `script` 2.39.3; `remove --all --yes --idle 1m` moved four worktrees to the trash, `residue restore space` put one back by branch, and an entry set eight days back was purged by the next sweep; a run killed before the unlock and one killed after the directory was deleted were listed by `scan` under interrupted removals, and both recovery commands worked; a process entering right after the move aside (seen through `/proc`) sent the worktree back, exit 3; `.github/smoke.sh` and `.github/watch-smoke.sh` passed.

### 0.0.4 on macOS 26.3.1 (25D771280a), Apple silicon

**Git:** Apple Git 2.50.1

The CONTRIBUTING states with a pushed `clean` branch: every check id, status and verdict identical to the 0.0.3 release binary without `--idle`. Under `--idle 2d`: the fresh clean pushed worktree kept (`changed just now (--idle 2d)`); `touch -t` on its files and Git directory alone still kept it, by its HEAD reflog; a worktree created and committed with `GIT_COMMITTER_DATE` three days back and its mtimes backdated became a `candidate`, and `remove --all --yes --idle 2d` with no terminal removed it (branch kept, exit 0) and kept the other seven. `git gc` in the main checkout (rewrites `logs/HEAD`), three rounds of `git status`, `git diff` and `git log`, and residue's own scans left a backdated worktree a candidate; a checkout, `git add` and an edit made it `keep`. `remove --yes --idle 2d` on a fresh worktree refused (exit 3). Invalid durations and `remove --all --yes` without `--idle` gave exit 2. Rules from `GIT_CONFIG_GLOBAL` and `.git/config` named by scope and file in text and `rule_sources`; an invalid global value named its file. Header and dates with offsets under `TZ` unset, `UTC`, `Asia/Kolkata` and `America/New_York`. The README LaunchAgent plist loaded with `launchctl bootstrap`, run with `launchctl kickstart` from `/`: it removed the idle worktree and logged absolute paths.

### 0.0.4 on Ubuntu 24.04.5 LTS, arm64 (Docker, Linux 7.0.14)

**Git:** 2.43.0

Static musl binary as a non-root user: the same states with check ids, statuses and verdicts identical to the 0.0.3 musl binary without `--idle`; the fresh worktree kept under `--idle 2d`, the backdated one a `candidate`, unchanged by three rounds of `git status`, `git diff` and `git log` and by `git gc`; `remove --all --yes --idle 2d` without a terminal removed it (branch kept, exit 0). The README systemd service and timer passed `systemd-analyze verify` in user and system mode (systemd 255). The timer did not run under a live systemd.

### 0.0.3 on macOS 26.3.1 (25D771280a), Apple silicon

**Git:** Apple Git 2.50.1

Scratch repository with the CONTRIBUTING states plus `.local-dev/*.lock` files: `scan` verdicts identical to 0.0.2 with no rules; `residue.regenerable` set through `GIT_CONFIG_COUNT` (mixed-case key too), `git -c` through a Git alias, and the legacy `GIT_CONFIG` all ignored with the stderr note and `ignored_environment_rules` (0.0.2 made the `.env` worktree a `candidate` under `GIT_CONFIG`; 0.0.3 keeps it); a malformed `GIT_CONFIG_COUNT` giving the "could not check" note and null; an escape character in an ignored value printed escaped; rules in `.git/config`, `$HOME/.gitconfig`, `GIT_CONFIG_GLOBAL` and `GIT_CONFIG_SYSTEM` honored, and skipped under `GIT_CONFIG_NOSYSTEM`; `remove --yes` under `git -c residue.regenerable=.env` refusing `.env`, removing a clean worktree (branch kept), exit 3.

### 0.0.3 on Ubuntu 24.04.5 LTS, arm64 (Docker, Linux 7.0.14)

**Git:** 2.43.0

Static musl binary as a non-root user: the same scratch states; `GIT_CONFIG_COUNT`, `git -c` and legacy `GIT_CONFIG` rules ignored with the note and JSON field; `.git/config` and `GIT_CONFIG_GLOBAL` rules honored; `--regenerable .env` honored; `remove --yes` without a terminal under `git -c` refusing `.env` and removing a clean worktree (branch kept), exit 3.

### 0.0.2 on macOS 26.3.1 (25D771280a), Apple silicon

**Git:** Apple Git 2.50.1

Scratch repositories: a path rule (`web/.tscache`, `/web/`, `web/.tsc*`) matching; `.local-dev/*.lock` expanding a collapsed `.local-dev/` when every file matched (candidate) and when `server.pid` did not (keep, naming the file); the same rules from `git config residue.regenerable` and from the config file; invalid git config values (`logs/**`, an empty value) giving `unknown`; an unreadable directory inside an expanded entry (`unknown`); 100,001 lock files (kept, over the limit); sizes with hard links counted once in `scan`, `explain` and JSON; `generated_at`; `remove --yes` refusing `.env` and `.vercel/` and removing a clean worktree and an expanded `.local-dev/` while the branches stayed. The 0.0.1 verdicts again: locked, modified, untracked, assume-unchanged, nested repository, rebase in progress, unreferenced detached HEAD, a path with a space. A read-only `scan` of a busy agent monorepo with four linked worktrees: 5.3 s.

### 0.0.2 on Ubuntu 24.04.5 LTS, arm64 (Docker, Linux 7.0.14)

**Git:** 2.43.0

Static musl binary as a non-root user with `TZ=Europe/Tallinn`: `scan` with a git-config glob rule and a `--regenerable` path rule, `explain` naming the unmatched file, JSON sizes and `generated_at`, local `as of` time, in-use detection through `/proc`, an invalid git config value, `remove --yes` refusing `.env` and removing a clean worktree without a terminal (branch kept), and `remove` without `--yes` and without a terminal removing nothing.

### 0.0.1 on macOS 26.3.1 (25D771280a), Apple silicon

**Git:** Apple Git 2.50.1

`scan`, `explain` and `remove` on real agent worktrees and on scratch repositories covering every verdict: clean with `node_modules`, `.env`, unreferenced detached HEAD, locked, in use, modified, stale, a path with a space, a path with an escape character, an assume-unchanged edit, a skip-worktree file, a nested repository and a rebase in progress. Removal kept the branches, and the assume-unchanged edit survived.

### 0.0.1 on Ubuntu 24.04.5 LTS, arm64 (Docker, Linux 7.0.14)

**Git:** 2.43.0

Static musl binary as a non-root user: `scan`, in-use detection through `/proc`, hard-link accounting, and `remove` with and without a terminal. As root on another user's repository, Git refused with "dubious ownership" and residue reported the error instead of guessing.

### 0.0.6 on NetBSD 10.2, OpenBSD 7.8, DragonFly 6.4.2 and OmniOS r151054 (illumos), each in a virtual machine on GitHub Actions

**Git:** 2.56.0

Build, `.github/smoke.sh`, `scripts/contract-check.sh` and `scripts/recovery-check.sh` on all four, and `.github/watch-smoke.sh` and `scripts/race-check.sh` on the three BSDs, as root and as an ordinary user. The busy worktree was `in use: sleep[PID]` on each. NetBSD, OpenBSD and DragonFly never offer a worktree for removal on activity grounds: their probe reads working directories, not open files, so activity is `unknown` (`partial_probe`) unless a use is found, and nothing there is a removal candidate. There the clean, idle worktree stayed `unknown` in `scan`, `explain`, `remove --yes`, `remove` under a terminal (no prompt) and `watch` (a landed branch, disk pressure); nothing was moved aside or put in the trash; every case that expects a removal was skipped by name. On illumos, whose probe reads open files, the clean worktree was a candidate and was removed. As an ordinary user the probe inspected 21 of 22 processes on NetBSD and 25 of 26 on DragonFly (their kernels name every process's working directory), and 4 of 40 on OpenBSD and 4 of 52 on illumos (only the user's own).

### 0.0.6 on GitHub Actions `macos-latest`, `ubuntu-24.04` (x86_64), `ubuntu-24.04-arm`, and FreeBSD 14 in a virtual machine

**Git:** runner Git

As 0.0.5, plus `scripts/contract-check.sh` and `scripts/recovery-check.sh` on all four, on FreeBSD as root and as an ordinary user. The release builds FreeBSD x86_64 and aarch64 binaries, each run with `--version` and `.github/smoke.sh` in a FreeBSD 14 virtual machine of its own architecture.

### 0.0.5 on GitHub Actions `macos-latest`, `ubuntu-24.04` (x86_64), `ubuntu-24.04-arm`, and FreeBSD 14 in a virtual machine

**Git:** runner Git

Build, `scan` and `explain` of a temporary worktree, `.github/smoke.sh`, `.github/watch-smoke.sh` and `scripts/race-check.sh`, on FreeBSD as root and as an ordinary user; `.github/service-smoke.sh` on macOS and Linux x86_64.

### 0.0.1 to 0.0.4 on GitHub Actions `macos-latest`, `ubuntu-24.04` (x86_64), `ubuntu-24.04-arm`

**Git:** runner Git

Build, then `scan` and `explain` of a temporary worktree. No removal.

## What was not run

Not run: the Intel macOS binary (built by cross-compiling, never executed), on illumos the watch smoke test and the race check (see below), and Windows (not supported).

## What CI runs

CI runs these scripts on every push: on macOS and Linux x86_64 and arm64, and as root and as an ordinary user in FreeBSD 14, NetBSD 10, OpenBSD 7.8 and DragonFly 6.4 virtual machines. An OmniOS (illumos) virtual machine runs the smoke test and the contract and recovery checks, as root and as an ordinary user; not the watch smoke test, because without inotify or kqueue a running `watch` wakes on its five-minute timer, past that test's two-minute limit, and not the race check, because illumos `script` cannot run a command and the base system has no other way to give residue a terminal. On NetBSD, OpenBSD and DragonFly nothing is a removal candidate, so each script skips every case that expects a removal or a prompt, naming the case and the reason (`scripts/partial-probe.sh`), and checks instead that the clean, idle worktree is kept as `unknown` for the probe's reason (`partial_probe`), with nothing moved aside or put in the trash. The gate is the system's name, so a residue that offered a removal there would fail it.

- `.github/smoke.sh`, in a scratch home folder: a clean candidate, a kept `.env`, a worktree a process is using, a removal that keeps the branch and ends with what it freed, a restore of it from its removal record, and both read back from `residue stats --json`.
- `scripts/race-check.sh`: five changes made while the prompt waits, each skipped; an upstream restored while it waits, kept with and without the trash; and three removals that changed nothing.
- `scripts/recovery-check.sh` (needs `jq`): what `remove` leaves when it is stopped right before `git worktree remove` (still registered under its hidden name, its bytes intact, listed by `scan` with the line that moves it back, and not removed by a later run); when the check after the move fails (back in place); when the move back fails too, or something now occupies the original path (`failed`, locked under its hidden name, with where it is and the line that unlocks and moves it back); and when a second remover of the same worktree runs after the first, with and without the trash (one removes it, the other fails at Git's move, one trash entry at most). The stopped run has written its removal record, so `residue restore` lists it as interrupted and moves it back with every file; the other cases check how many removal records and refs each leaves. A `git` script first on `PATH` stops or fails one Git command at a time; nothing in residue knows about it. Three cases fail `git worktree remove` after it deleted part of the worktree, its HEAD reflog or its HEAD from Git's directory for it, or the `.git` file from its checkout: the worktree stays under its hidden name and the removal record with its two refs stays, with the commit only the reflog held surviving `git gc`; restore then moves it back with every file, or, when Git can no longer use it, says to delete it and run `git worktree prune`, after which restore recreates it. For those two, `scan` prints only the restore of the record, in its text and in `recovery`, and no move Git would refuse. A fourth runs that restore while the removal that left the worktree still runs: restore says to wait for that process and run it again, or, if it is not residue, what to clear. A kill during `git worktree remove` itself is outside it: deletion may then be partial, and residue does not promise to roll it back.
- `scripts/contract-check.sh` (needs `jq`): `remove` with no prompt, one repository per case. A clean worktree removed, in JSON and text; refusals for a staged edit, a modified file, an untracked file, an ignored `.env` and a lock; the main checkout, a directory inside a worktree and a missing path, each `invalid`; no terminal without `--yes`; several targets at once; diagnostics on stderr, never in the JSON; usage errors; and removal records: the record and ref a removal writes and a restore from them, nothing deleted when the record or its ref cannot be written, `--keep-removed` forgetting only an expired record, a record written with `--keep-removed 12w` surviving a plain `remove` at 31 days and forgotten past 12 weeks, with its `keep` in `restore --json`, `watch --keep-removed 1d` passing it to its removal child (a two-day-old 1d record forgotten, the new record keeping `1d`), a commit only the worktree's reflog held surviving `git gc --prune=now` and coming back in the restored reflog, the same for a commit a branch's reflog also named after `git branch -D`, and in a reftable repository (Git 2.45 and later; skipped before) for the lone commit, for one that only a later entry names as its old value once `git gc` expired the entry that made it, and for the commit before an amend that `git reflog delete` left as the oldest entry's old value, after `git branch -D`, each restore saying the HEAD reflog was not put back, a commit only the reflog held surviving `git gc --prune=now` when a removal in files is restored after `git refs migrate --ref-format=reftable` (the restore writes no `logs/HEAD`, says the reflog was not put back and keeps the record) and when a removal in reftable is restored after the migration back to files (skipped on a Git without `git refs migrate`, before 2.46), a removal in reftable that neither copies nor reads a `logs/HEAD` file Git does not read and pins the commit Git's reflog names, an unreadable record that leaves a removal at exit 0 with a warning in `warnings` and none in `errors`, and an hour-old entry with no `info` cleared, and a restore with the branch ahead, rewritten, deleted, checked out elsewhere or detached, and with its path occupied, then `--to`; a name from the home folder, found only in the usual code folders there (a record in `~/Documents` is not listed or found, and the miss names the folders searched), and found from inside `~/Documents`; and the stats ledger: an empty and a damaged ledger, the `stats` object in `remove --json`, a move into the trash and its purge, a restore, seeding from removal records once each, a ledger residue cannot write (the worktree still removed, exit 0, the warning in `warnings` and none in `errors`), a ledger whose last line was cut short, a FIFO at the ledger path, and the text line a run ends with, and none for a preview; and on Linux, a worktree that a `sleep` running elsewhere uses only through a mapped library is `in_use`, and `not_observed` once it exits, with what reading every process's `/proc/PID/maps` cost on that runner. Each case checks the exit status, the JSON result and failing check ids, and afterwards the file bytes, Git's registration, the branch, an unrelated worktree, and that nothing was left moved aside or in the trash.
- `.github/watch-smoke.sh`: `watch` removing a worktree whose branch landed, once and as a running process woken by a `git fetch --prune`, and under disk pressure, without the trash. Then `remove --only-if-upstream-gone` on its own: an upstream that is back, none, a local one, a detached HEAD, a ref Git marks gone that still exists, dirty, locked and recently changed worktrees, and a gone upstream that is removed. Then `watch` through a `git` wrapper that restores the upstream right before each of the removal's three reads, and once during the re-measure before the last read (with `--no-fetch`, since those refs are set by hand). Then `watch` with no ROOT in a scratch home folder: it uses `~/src`, `--no-fetch` keeps a worktree whose branch was deleted on the remote without a local fetch, `watch`'s own fetch prunes that ref without writing `FETCH_HEAD` and the worktree is removed, a remote it cannot reach is logged and its refs and worktree stay, and a home folder with none of the code folders is a usage error. On NetBSD, OpenBSD and DragonFly that case runs too, expecting no removal. And a `watch` removal's log line names its `residue restore` command, its record says `by watch`, the restore brings the worktree back, a fetching `watch` keeps it past `--grace` after its branch landed, and disk pressure still removes it. Its log says what a removal freed, and `residue stats` counts it under `watch`; the script runs in a scratch home folder, so it never writes the caller's ledger. `.github/service-smoke.sh` installs, checks and uninstalls the launchd agent on macOS and the systemd user service on Linux: with a ROOT, again without one (the ROOT is kept), and after an uninstall without one (the usual code folders, marked as such in `service status`; a GitHub runner has `~/work`). It replaces this user's service, so it runs only on CI runners.

## Git behavior the checks rely on

Observed 2026-11-08: Git 2.50.0 to 2.56.0 swap the two reflog expiry defaults that Git documents (`gc.reflogExpire`, 90 days; `gc.reflogExpireUnreachable`, 30 days, for an entry not reachable from the current tip). `git gc` and `git reflog expire` on those versions drop every entry older than 30 days, reachable or not. Git commit 8565827570 (2025-04-08) made the swap, and 60cf500551 (2026-09-24) fixes it on `master`, after 2.56.0. In a reftable repository the commit HEAD left from can then be only the oldest entry's old value, which residue reads as `HEAD@{N}`. The contract case `record-reflog-reftable-expired` dates its entries 60 and 10 days back, so its gap is the same on both defaults. To re-check a Git:

```sh
cd "$(mktemp -d)" && git init -q && GIT_COMMITTER_DATE="$(($(date +%s) - 60 * 86400)) +0000" git -c user.name=x -c user.email=x@example.invalid commit -q --allow-empty -m old && git reflog expire --dry-run --verbose HEAD
```

It prints `keep commit (initial): old` on the documented defaults (Git 2.47.3) and `prune commit (initial): old` on the swapped ones (Git 2.50.1). A release's source says the same: `gh api "repos/git/git/contents/reflog.h?ref=v2.56.0" -H 'Accept: application/vnd.github.raw' | grep -A3 'define REFLOG_EXPIRE_OPTIONS_INIT'` shows `default_expire_total` at 30 days and `default_expire_unreachable` at 90.

Observed 2026-11-08 on Apple Git 2.50.1: Git's directory walk skips an entry whose name is `.git` by `fspathcmp`, which is `strcasecmp` when `core.ignorecase` is set (`git init` sets it on a case-insensitive file system, as macOS has by default), so it skips `.GIT` and every other case of the name at every level, the root included, and `git status` lists nothing inside one. residue's `nested_repo` check therefore blocks on any case of `.git` except the root's own `.git` link (#102); the contract cases `ignored-dotgit-upper-ignorecase`, `ignored-dotgit-upper-case-sensitive`, `ignored-dotgit-plain-folder` and, on a case-sensitive file system only, `ignored-dotgit-upper-root` hold it. The source: `gh api "repos/git/git/contents/dir.c?ref=v2.50.1" -H 'Accept: application/vnd.github.raw' | grep -n 'fspathcmp(cdir->d_name, ".git")'`.
