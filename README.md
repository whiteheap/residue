# residue

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/assets/logo-dark.png">
  <img src="docs/assets/logo.png" width="112" alt="Residue mark: three flat sheets stacked like linked worktrees, and a small square set apart beneath them.">
</picture>

**Coding agents leave Git worktrees on disk; residue removes one only when every check passes.**

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/assets/how-it-works-dark.png">
  <img src="docs/assets/how-it-works.png" width="420" alt="Chart. A repository leads to its linked worktrees, then checks, then a report. Read only ends at scan and explain, which read and stop. A separate arrow, you or watch, enters remove, then fresh checks. Block, unknown, or changed leads to stays. All pass leads to git remove without --force, or to the trash for restore. Scan does not lead to deletion.">
</picture>

A repository's linked worktrees are checked, then reported. `scan` and `explain` only read that report and stop. `remove` is separate: you run it, or `watch` chooses paths for it. Fresh checks leave the worktree in place on a block, an unknown, or a change. When every check passes, `git worktree remove` deletes it without `--force`, or the trash holds it for `restore`. A pass is not a guarantee, and age alone does not mean inactive.

Coding agents leave Git worktrees behind. Each one is a full checkout with its own `node_modules`, `target` and build output, and a machine that runs agents all day fills its disk with them. residue finds those worktrees, measures them, and removes one only when nothing in it would be lost.

```text
$ residue scan ~/src/app
residue 0.0.5 · as of 14:03:12 +02:00 · activity: lsof inspected 506 of 807 running processes

~/src/app · 4 linked worktrees · 5.6 GB
  candidate   2.1 GB  ~/src/app/.worktrees/fix-login  [fix-login]
                      branch kept · 1 local-only commit · upstream gone · changed 3 days ago
  keep        1.9 GB  ~/src/app/.worktrees/feat-search  [feat-search]
                      in use: node[4211], zsh[4198]
  keep        1.3 GB  ~/src/app/.worktrees/spike  [spike]
                      ignored: .env (1 file, 312 B) · 2 untracked
  keep        0.3 GB  ~/src/app/.worktrees/review  (detached 7954cfbb7b3a)
                      unreferenced detached HEAD

disk  41 GB free of 494 GB (8%)  (filesystem of ~/src/app)
1 candidate of 4 linked worktrees · 2.1 GB allocated · about 2.1 GB reclaimable
next: residue explain PATH  ·  residue remove PATH  ·  keep one: git worktree lock PATH
```

`residue scan` only reads. `residue remove` asks before it deletes anything, checks everything again right before it acts, moves the worktree aside and checks once more, and then hands the deletion to `git worktree remove`, which runs without `--force`. It deletes what you saw, or nothing, and never removes something Git itself would refuse to remove. A scheduled sweep moves worktrees to a trash first, so `residue restore` can put one back until the trash is purged.

## Install

Homebrew, on macOS and Linux:

```sh
brew install whiteheap/tap/residue
```

On Linux, add residue's signed package repository once, and your normal upgrades keep it current. Each repository serves x86_64 and arm64.

Debian and Ubuntu:

```sh
sudo install -d -m 0755 /etc/apt/keyrings
curl -fsSL https://whiteheap.github.io/residue/apt/residue-archive-keyring.gpg | sudo tee /etc/apt/keyrings/residue.gpg >/dev/null
echo "deb [signed-by=/etc/apt/keyrings/residue.gpg] https://whiteheap.github.io/residue/apt stable main" | sudo tee /etc/apt/sources.list.d/residue.list
sudo apt update && sudo apt install residue
```

Fedora, RHEL and their rebuilds (dnf asks you to confirm the key):

```sh
sudo curl -fsSLo /etc/yum.repos.d/residue.repo https://whiteheap.github.io/residue/rpm/residue.repo
sudo dnf install residue
```

Arch Linux, Omarchy and other Arch-based systems:

```sh
curl -fsSL https://whiteheap.github.io/residue/residue-signing-key.asc | sudo pacman-key --add -
sudo pacman-key --lsign-key 1AEAB31435118A1FE575D36EE1553632FC32554A
printf '\n[residue]\nServer = https://whiteheap.github.io/residue/arch/$arch\n' | sudo tee -a /etc/pacman.conf
sudo pacman -Syu residue
```

All three are signed with the key `1AEA B314 3511 8A1F E575  D36E E155 3632 FC32 554A`. Each package installs `/usr/bin/residue` and pulls in Git if it is missing. To install one release without adding a repository, every release also carries `residue_amd64.deb`, `residue.x86_64.rpm` and `residue-x86_64.pkg.tar.zst` (and their arm64 builds) on the [releases page](https://github.com/whiteheap/residue/releases/latest).

Prebuilt binaries for macOS (Apple silicon and Intel), static Linux (x86_64 and arm64) and FreeBSD (x86_64 and aarch64) are on the releases page too. Each archive and package has a SHA-256 checksum in `SHA256SUMS` and a GitHub build-provenance attestation:

```sh
gh attestation verify residue_amd64.deb --repo whiteheap/residue
```

From source, with Rust 1.85 or later:

```sh
cargo install --locked --git https://github.com/whiteheap/residue --tag v0.0.5
```

residue needs `git` 2.36 or later on `PATH`.

## Use

```sh
residue scan [PATH...]           # every linked worktree of the repositories at or under PATH
residue explain PATH             # every check for one worktree, and why it passed or failed
residue remove PATH...           # confirm, check again, move aside, check, then `git worktree remove` each one
residue remove --all PATH        # the same for every candidate a scan of PATH finds
residue remove --trash PATH...   # move each one to the trash instead; --idle turns this on
residue restore [ENTRY]          # list the trash, or put one worktree back
residue watch ROOT...            # stay running; remove a worktree when its branch lands or the disk runs low
residue service install ROOT...  # run `residue watch` from login
```

`scan` with no PATH looks at the current directory. If PATH is inside a repository, residue scans that repository. Otherwise it searches up to four levels below PATH. The search does not follow symbolic links, does not cross into another filesystem, and skips hidden directories, `node_modules`, `target` and `Library`. Linked worktrees come from Git's own list, so they are found wherever they live.

Add `--json` to any command to get one JSON document (schemas `residue.scan/v1`, `residue.explain/v1`, `residue.remove/v1`, `residue.restore/v1`). Each document has `generated_at`, the moment residue started reading, in RFC 3339 UTC. The text output shows the same moment in local time with its UTC offset in its first line (`as of 14:03:12 +02:00`), and every date in the text uses that zone.

Add `--idle DURATION` (`30m`, `12h`, `2d`, `1w`) to `scan`, `explain` or `remove` to also keep every worktree that changed within DURATION. With `remove`, it also turns on `--trash`. [Run it on a schedule](#run-it-on-a-schedule) explains what counts as a change.

## What makes a worktree a candidate

residue runs every check below on every linked worktree. A worktree is a **candidate** only when all of them pass. One failed check keeps it (**keep**). One check residue could not complete also keeps it (**unknown**). The main checkout is never a candidate.

| Check | Keeps the worktree when |
| --- | --- |
| `identity` | The path is not the linked worktree Git lists for this repository |
| `locked` | It is locked with `git worktree lock` |
| `activity` | A process residue can see has its working directory or an open file inside it |
| `idle` | Only with `--idle DURATION`: it changed within DURATION, for example `changed 3 hours ago (--idle 2d)` |
| `tracked_changes` | Any tracked file is staged or modified |
| `untracked` | Any untracked file or directory exists |
| `index_flags` | A tracked file is marked assume-unchanged or skip-worktree, so `git status` cannot see edits to it |
| `ignored` | Any ignored entry matches no regenerable rule (see below) |
| `operation` | A rebase, merge, cherry-pick, revert or bisect is in progress |
| `private_refs` | It has per-worktree refs (`refs/worktree/`, `refs/bisect/`, `refs/rewritten/`) |
| `head` | HEAD is detached and no branch, tag or remote-tracking ref contains it |
| `layout` | It has submodules or is a sparse checkout |
| `nested_repo` | It contains another repository or checkout |
| `mount_point` | It contains a mount point |
| `contents` | residue could not read some of its entries |

Some facts never block, and residue reports each one separately instead of calling a worktree "safe": commits that are on the branch but on no remote-tracking ref, whether the branch's upstream is gone, the last change and what made it, and size. residue does not fetch, so "local-only" means local-only as of your last fetch.

## What removal deletes, and what it keeps

`residue remove` deletes the worktree directory with `git worktree remove`, without `--force`. That deletes:

- the checked-out tracked files. Every one of them is committed, because the `tracked_changes` check passed.
- the ignored entries the plan lists, with their file counts and sizes, for example `node_modules/ (41530 files, 816 MB)`. Every one of them matches a regenerable rule.

It keeps the branch, every commit, the stash, and every other worktree. residue never deletes a branch, never kills a process, and never lifts a lock it did not set.

Before it deletes anything, residue prints the plan and asks you to type `yes`. The plan records what it showed you for each worktree: HEAD, the branch, the lock, the staged, modified and untracked files, the file count, size and newest modification time of every ignored entry, and the modification times of the worktree's index and HEAD reflog. Right before each removal, residue takes a new process snapshot, runs every check again, and compares. Any difference skips the worktree, and the reason names it:

```text
skipped .worktrees/big: node_modules/ changed since you confirmed (1,204 -> 1,388 files)
```

residue deletes what you saw, or nothing. With `--yes` there is no prompt, so the reason says `changed since the plan`.

Then residue moves the worktree aside with `git worktree move`, to a hidden sibling named `.NAME.residue-removing-PID`, and locks it. The move is one rename on the same filesystem; a worktree that is a mount point is skipped, never copied. After the move, nothing can reach the worktree by its old path, so residue checks once more at the new path: a new process snapshot must see nobody inside it, and the record must still match. Only then does `git worktree remove` delete it. If anything differs, residue moves the worktree back, unlocks it, and the reason ends with `it is back in place`.

The move narrows the window between the last check and the deletion; it does not close it. A program that opens the old path after the move finds nothing there, and a process that was already inside shows up in the second snapshot. What remains: from that last check until Git finishes deleting, a process that opens the hidden directory by its new name, or one the probe cannot see (another user's, or one in a container or virtual machine), can still write into it, and that write is deleted with the worktree. Git checks again for modified and untracked files before it starts deleting, but not for ignored ones.

If residue is killed between the move and the end of the deletion, the worktree stays under its hidden name and Git still lists it. The next `residue scan` lists it under `interrupted removals`, with the one command that puts it back:

```text
interrupted removals (a residue run moved these aside to delete them and stopped; if it stopped during the delete, part may be gone):
  .worktrees/.fix-login.residue-removing-4242  was .worktrees/fix-login
    put it back: git -C /Users/me/src/app worktree unlock /Users/me/src/app/.worktrees/.fix-login.residue-removing-4242 && git -C /Users/me/src/app worktree move /Users/me/src/app/.worktrees/.fix-login.residue-removing-4242 /Users/me/src/app/.worktrees/fix-login
```

If its directory is already gone, the command clears Git's record with `git worktree remove` instead. If the process that moved it still runs, the line says so: it may still be removing it. residue never removes a worktree under a hidden name, by sweep or by PATH, so nothing touches it until you recover it.

If stdin is not a terminal, residue does not delete anything unless you pass `--yes`. Pass `--yes` only from the session that owns the worktree and knows its task is finished, such as an agent's last step or a launcher tearing down a task, or from a scheduled `remove --all --idle` as [Run it on a schedule](#run-it-on-a-schedule) describes. Every check still runs. Without `--idle`, a session that is paused looks exactly like a finished one, so `remove --all --yes` refuses to run without it.

## Keep a worktree

```sh
git worktree lock --reason "resume on Monday" PATH
```

residue treats every lock as final. It is Git's own mechanism, so other tools respect it too.

## Reclaim storage automatically

```sh
residue service install ~/src
```

This starts `residue watch ~/src` now and at every login, as a launchd agent on macOS or a systemd user service on Linux. It removes a worktree in two cases, and otherwise does nothing:

- **Its work landed.** Its upstream branch was deleted, which is what merging a pull request with branch deletion, or `git push --delete`, does, and the worktree has not changed for an hour (`--grace`). residue never fetches, so it notices after your next `git fetch --prune`. `git config --global fetch.prune true` makes every fetch prune.
- **The disk is low.** Free space on the worktree's filesystem is below 10% (`--min-free`). Then worktrees unchanged for two days (`--idle`) go, largest first, until free space is 5 points above that. While the disk has room, idle worktrees stay.

Each removal is `residue remove --yes --idle --no-trash`, so every check in [What makes a worktree a candidate](#what-makes-a-worktree-a-candidate) runs, and runs again after the worktree is moved aside, right before the deletion. For a branch that landed, `watch` also passes `--only-if-upstream-gone`, so the removal checks the reason itself, from the local refs, three times: at its plan, right before the move aside, and after the move, right before the deletion. If the upstream is back by then (a fetch brought the branch back, say), the worktree stays, back in place, and the log says why. A worktree with no upstream, or on a detached HEAD, never counts as landed. Neither does a local upstream (`branch.NAME.remote` set to `.`), since a deleted local branch is not work that landed on a remote, nor an upstream Git marks `[gone]` whose ref still exists without resolving to a commit, such as a symbolic ref to a missing ref. `watch` deletes instead of using [the trash](#the-trash): freeing the disk is its job, and a trashed worktree keeps every byte until the purge. The same run purges that repository's trash entries older than seven days. Branches and commits stay, and a locked worktree stays. It works the same for worktrees from any tool (Claude Code, Codex, Cursor, Conductor, `git worktree add`), because it reads Git and the disk, not the agent.

There is no schedule. `watch` sleeps until a fetch or prune changes a repository's `refs/remotes` or `packed-refs` (inotify on Linux, kqueue on macOS and the BSDs), until a waiting worktree passes `--grace` or `--idle`, or until a filesystem drops below `--min-free`. Free space is read every five minutes, because no system sends an event for it. Every six hours `watch` also looks for repositories created under the roots since. One `watch` runs per user.

Try it without removing anything first:

```sh
residue watch --once --dry-run ~/src       # what it would remove right now
residue service install --dry-run ~/src    # a service that only logs what it would remove
residue service status                     # is it running, and the last lines of its log
residue service uninstall
```

The log is `~/Library/Logs/residue/watch.log` on macOS and `journalctl --user -u residue-watch` on Linux. A Linux user service stops when you log out unless you run `loginctl enable-linger "$USER"` once. On other systems, `residue service install` prints the `residue watch` command to run from your system's service manager. [Why `--idle` matters](#why---idle-matters) explains what counts as a change, and why a paused agent session needs a wait. If you would rather sweep at a fixed time, see [Run it on a schedule](#run-it-on-a-schedule).

## The trash

`residue remove --trash` moves each worktree into the trash instead of deleting it. `--idle` turns it on, so a scheduled sweep uses the trash unless you pass `--no-trash`.

```sh
residue remove --trash ~/src/app/.worktrees/fix-login
residue restore               # list the trash of the repositories at or under this directory
residue restore fix-login     # put one back
```

Each repository has its own trash, `residue-trash/` in its Git common directory: `.git/residue-trash/` in the main checkout. Each entry is a directory named after the worktree and the time it was trashed, `fix-login-20261107T040000Z/`, that holds the worktree under its own name and a record, `info`, with its original path, branch, HEAD, the time it was trashed and its size. The trash lives there because:

- a move into it is one rename on the repository's filesystem, the usual place for linked worktrees, so trashing takes no time and no extra space;
- it belongs to the repository: every worktree of the repository finds it, `git status` never shows it, and deleting the repository deletes its trash;
- the worktree stays registered with Git, and its record moves with it, so `residue restore` and the purge are plain Git commands.

A trashed worktree is locked with the reason `residue: in trash`, and `git worktree list` still shows it. Its branch stays checked out there, so `git switch` to that branch in another worktree refuses until you restore the worktree or the purge removes it. residue refuses to trash a worktree that is on another filesystem than its repository, because the move would be a copy; remove it with `--no-trash`, or keep it.

`residue restore NAME` takes the entry name, the worktree's directory name or branch when only one entry matches, or the path of an entry. It unlocks the worktree and moves it back with `git worktree move`, which updates Git's record, then drops the entry. It refuses when something exists at the original path again. A restore counts as a change, so a sweep under `--idle` does not trash the worktree again right away.

The trash does not free disk space: a trashed worktree takes all the space it took before. The purge frees it. Every `remove` run purges the trash entries older than `--keep-trash DURATION` (default `7d`) in the repositories it covers, and lists them in its plan before you confirm. The purge runs every check on each entry again, except `--idle`, since `--keep-trash` is the trash's only clock, and deletes it with `git worktree remove`, without `--force`. An entry that someone changed in the trash stays, and the run says why. A preview, or a run you do not confirm, purges nothing.

## Run it on a schedule

A daily sweep keeps leaked worktrees from piling up:

```sh
residue remove --all --yes --idle 2d --keep-trash 7d ~/src
```

`--all` treats each PATH as a scan root, the way `scan` does, and takes every candidate it finds. `--idle 2d` keeps every worktree that changed in the last two days. `remove --all --yes` refuses to run without `--idle`.

`--idle` also turns on `--trash`: the sweep moves each candidate to [the trash](#the-trash) instead of deleting it, so a worktree it took by mistake comes back with `residue restore`. The same run purges the entries older than `--keep-trash`, seven days here, which is also the default. The purge is what frees the disk. A sweep that trashes frees nothing until its entries expire, so with a daily sweep a worktree leaves the disk between nine and eleven days after its last change. For the space sooner, lower `--keep-trash`, or pass `--no-trash` to delete at once.

### Why `--idle` matters

The activity check finds a worktree in use when a process has its working directory or an open file inside it. A coding agent that is waiting for your approval, or for a merge queue, often has neither. Agent harnesses such as Claude Code reset the shell to the main checkout after every command, so between commands no process is inside the worktree. If its work is committed and pushed, every other check passes too. When you run `residue remove` yourself, you see the plan before anything is deleted. A scheduled run has nobody watching, so it needs another sign that someone is still using the worktree: a recent change. Nothing is lost if residue removes it anyway, because the branch and its commits stay. But the paused agent fails on its next command, because its directory is gone, until `residue restore` puts it back from the trash.

A worktree's last change is the newest of:

- any file, directory or symbolic link in the checkout, ignored ones included, so an edit, a new or deleted file, or a build writing `target/` counts;
- the files in the worktree's own Git directory (`.git/worktrees/NAME/`): `index`, `HEAD`, `ORIG_HEAD`, `FETCH_HEAD`, `COMMIT_EDITMSG`. Git writes them on `add`, `commit`, `checkout`, `reset`, `stash`, `fetch` and `pull`;
- the time recorded in the newest entry of the worktree's HEAD reflog: commits, checkouts, resets, rebases and merges.

residue uses modification times. `explain` and the JSON field `last_change` say which entry set the time, for example `last change  2026-11-07 09:12 +02:00 (5 hours ago; index in its Git directory)`. residue does not use the modification time of the reflog file itself, because `git gc` in any worktree of a repository rewrites every worktree's reflog file. residue's own Git commands never write the index, so a scan never resets the clock.

`--idle` sees writes, not reads. `git status`, `git diff`, `git log`, reading a file and searching the code leave no trace. `git status` rewrites the index only when it has to refresh a file's stat data, which means the file changed. If a session may sit without writing anything for longer than the idle time, lock its worktree:

```sh
git worktree lock --reason "agent paused, resume Monday" PATH
```

A lock keeps a worktree through every sweep until you run `git worktree unlock`.

### Preview the next run

Run the same command without `--yes`. residue prints the plan and the reason it keeps each other worktree. Without a terminal it removes nothing:

```sh
residue remove --all --idle 2d ~/src
residue scan --idle 2d ~/src     # the same verdicts, as a scan
```

### macOS: launchd

Save this as `~/Library/LaunchAgents/com.whiteheap.residue.plist`. Replace `YOU` with your user name and `src` with the folder that holds your repositories. launchd does not expand `~`. On an Intel Mac, Homebrew installs to `/usr/local/bin`, so use `/usr/local/bin/residue` and put `/usr/local/bin` first in `PATH`.

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>com.whiteheap.residue</string>
  <key>ProgramArguments</key>
  <array>
    <string>/opt/homebrew/bin/residue</string>
    <string>remove</string>
    <string>--all</string>
    <string>--yes</string>
    <string>--idle</string>
    <string>2d</string>
    <string>--keep-trash</string>
    <string>7d</string>
    <string>/Users/YOU/src</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key>
    <string>/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin</string>
  </dict>
  <key>StartCalendarInterval</key>
  <dict>
    <key>Hour</key>
    <integer>4</integer>
    <key>Minute</key>
    <integer>0</integer>
  </dict>
  <key>StandardOutPath</key>
  <string>/Users/YOU/Library/Logs/residue.log</string>
  <key>StandardErrorPath</key>
  <string>/Users/YOU/Library/Logs/residue.log</string>
</dict>
</plist>
```

`PATH` must find `git`: Homebrew's if you have it, otherwise `/usr/bin/git`. residue runs `lsof` as `/usr/sbin/lsof`. Load the job, run it once now, and read the log:

```sh
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/com.whiteheap.residue.plist
launchctl kickstart gui/$(id -u)/com.whiteheap.residue
cat ~/Library/Logs/residue.log
```

The job then runs every day at 04:00. If the Mac is asleep then, launchd runs it when the Mac wakes. The log names each worktree it moved to the trash, with the `residue restore` command that puts it back, and each entry it purged. To stop it: `launchctl bootout gui/$(id -u)/com.whiteheap.residue`, then delete the file.

### Linux: systemd

Save these two files in `~/.config/systemd/user/`. `%h` is your home directory. With Homebrew on Linux, the binary is `/home/linuxbrew/.linuxbrew/bin/residue`.

`residue.service`:

```ini
[Unit]
Description=Move idle Git worktrees to the residue trash and purge old entries

[Service]
Type=oneshot
ExecStart=/usr/bin/residue remove --all --yes --idle 2d --keep-trash 7d %h/src
```

`residue.timer`:

```ini
[Unit]
Description=Run residue every day at 04:00

[Timer]
OnCalendar=*-*-* 04:00:00
Persistent=true

[Install]
WantedBy=timers.target
```

```sh
systemctl --user daemon-reload
systemctl --user enable --now residue.timer
systemctl --user start residue.service     # run it once now
journalctl --user -u residue.service       # the log
```

`Persistent=true` runs a missed sweep at the next boot. A user timer runs only while you are logged in, unless you run `loginctl enable-linger`.

### Exit status of a sweep

`remove --all` exits 0 when it moved every candidate to the trash (or removed it, with `--no-trash`) and purged every expired trash entry, including when there was none. A worktree it keeps does not make it fail, and neither does an expired entry it keeps because a check failed. It exits 3 when a candidate or an expired entry was skipped or failed, or when it could not read part of a PATH or a trash record. The log names each one.

## The regenerable list

When `git worktree remove` deletes a clean worktree, it deletes the worktree's ignored files without asking. But being ignored does not make a file disposable: `.env` files, `.vercel/` project links, local databases and scratch notes are usually ignored too. So residue keeps the worktree unless every ignored entry matches a regenerable rule. The built-in rules name dependency and build-output directories that their tools recreate:

`node_modules` `.pnpm-store` `bower_components` `.next` `.nuxt` `.svelte-kit` `.output` `.turbo` `.parcel-cache` `.vite` `.angular` `.eslintcache` `*.tsbuildinfo` `dist` `build` `coverage` `.nyc_output` `target` `.build` `DerivedData` `.gradle` `.dart_tool` `.venv` `venv` `__pycache__` `*.pyc` `.pytest_cache` `.mypy_cache` `.ruff_cache` `.tox` `.nox` `.DS_Store`

Rules apply only to ignored entries, the ones `.gitignore` or `.git/info/exclude` match. An untracked file that is not ignored always keeps the worktree (the `untracked` check), and no rule or `--regenerable` changes that. To let residue delete such a file, ignore it first.

`scan`, `explain` and the `remove` plan show how many files each ignored entry holds and their size, for example `ignored: .env (1 file, 312 B), .vercel/ (2 files, 1.2 KB)`.

### Rules

A rule is a name or a path.

- **A name**, or `*.ext`, matches one path component at any depth. `cache` matches `web/cache/` but not `web/cache2/`.
- **A path** contains a slash and starts at the worktree root. A leading slash is optional, and a trailing slash is ignored. Each segment is literal or uses `*` for any run of characters within that one segment. `**`, `?`, `[`, `\`, and empty, `.` or `..` segments are rejected. A path matches that entry and everything below it: `web/.tscache` matches `web/.tscache/`, and `/build` matches the top-level `build/` but not `web/build/`.

Git reports a fully ignored directory once, as `dir/`. When a path rule reaches inside such a directory, for example `.local-dev/*.lock` and the entry `.local-dev/`, residue reads the directory. It calls the directory regenerable only if a rule matches every file in it, or one of that file's parent directories. Empty directories do not count. `explain` names the first file that no rule matches. A directory with more than 100,000 entries is not judged file by file, so residue keeps it.

### Where rules come from

For one repository, add rules to its git config. This is the place for a project's own lock files and caches:

```sh
git config --add residue.regenerable '.local-dev/*.lock'
git config --add residue.regenerable web/.tscache
git config --get-all residue.regenerable    # list them
```

Without a scope option, `git config` writes the repository's `.git/config`, which every worktree of the repository shares. residue reads the key with `git config --get-all`, so a value in your global Git config applies to every repository. `explain` names the scope and file of each rule, for example `global config (~/.gitconfig) adds ".env"; this repository's config (~/src/app/.git/config) adds "web/.tscache"`, and `git config --show-origin --get-all residue.regenerable` lists the same. In JSON, `rule_sources` lists every rule with its scope and file; [For coding agents](#for-coding-agents) describes the two lists. If a value is invalid, the `ignored` check is `unknown`, so no worktree of that repository is a candidate until you fix it. `explain` names the value.

Only Git config files count: the repository's own config, your global config and the system config, including files you choose with `GIT_CONFIG_GLOBAL` and `GIT_CONFIG_SYSTEM`. `GIT_CONFIG_NOSYSTEM` skips the system file, as it does for Git. residue ignores `residue.regenerable` set through the environment: `git -c` and `--config-env` (which pass it on as `GIT_CONFIG_PARAMETERS`), `GIT_CONFIG_COUNT` with `GIT_CONFIG_KEY_<n>` and `GIT_CONFIG_VALUE_<n>`, and the legacy `GIT_CONFIG`. Every subprocess and CI step inherits the environment, so it must not widen what residue deletes. When the environment sets the key, residue prints a note on stderr and lists the values in the JSON field `ignored_environment_rules`:

```text
residue: note: ignored residue.regenerable set through the environment (git -c, GIT_CONFIG_COUNT, GIT_CONFIG_PARAMETERS or GIT_CONFIG): "web/.tscache"
residue: note: residue reads rules only from Git config files, the residue config file and --regenerable. Pass --regenerable RULE for one run, or run `git config --add residue.regenerable RULE` in the repository to keep the rule
```

residue never reads rules from a file committed to the repository. A repository you clone must not be able to declare your files disposable, and `git clone` never copies Git config.

For every repository on the machine, put one rule per line in `~/.config/residue/regenerable` (or `$XDG_CONFIG_HOME/residue/regenerable`). Lines starting with `#` are comments. For one run, pass `--regenerable RULE`. An invalid rule in the file or on the command line stops residue with exit status 1.

## What the activity probe can and cannot see

- **macOS**: one `lsof` snapshot of the working directories and open files of the processes your user can inspect. residue reports how many processes that covered, for example "lsof inspected 506 of 807 running processes".
- **Linux**: `/proc/*/cwd`, `/proc/*/exe` and `/proc/*/fd`, for the processes your user can read.
- **FreeBSD**: one `procstat -f` snapshot from the base system, of the working directories and open files of the processes your user can inspect. procstat is built with the running kernel. A packaged lsof is not used: in testing on FreeBSD 14 it listed processes without resolving their working directories, which would have hidden a worktree in use. A process whose working directory the kernel cannot name does not count as inspected. Every release since 0.0.5 has FreeBSD binaries for x86_64 and aarch64, `residue-vX.Y.Z-x86_64-unknown-freebsd.tar.gz` and `residue-vX.Y.Z-aarch64-unknown-freebsd.tar.gz`, on the [releases page](https://github.com/whiteheap/residue/releases). Each is built from the tag's source, run and smoke-tested in a FreeBSD 14 virtual machine of its own architecture; the aarch64 machine is emulated on an x86_64 runner.
- **Other Unix systems** (NetBSD, OpenBSD, DragonFly, illumos): residue builds, but there is no probe yet. Every worktree reports activity `unknown`, so none is a candidate.

The probe has three answers. `in_use` means a process was seen inside the worktree. `not_observed` means none of the inspected processes was inside it. `unknown` means the probe failed: lsof is missing, it timed out, it failed, or residue could not parse its output. `not_observed` is not proof that a worktree is idle. The probe cannot see processes owned by other users, processes inside containers or virtual machines, processes on other machines that share the disk, or a session that is closed now but that you mean to resume. A process can also open the directory a moment after the snapshot; residue moves the worktree aside and looks again before it deletes, which narrows that gap but does not close it. That is why removal asks a human, why `--yes` belongs to the session that owns the work, and why a scheduled sweep needs `--idle`.

## Exit status

| Code | Meaning |
| --- | --- |
| 0 | Success. For `remove`, every PATH was removed or moved to the trash, and every expired trash entry was purged. For `remove --all`, the same for every candidate, or there was none. For `restore`, the worktree is back, or the trash was listed |
| 1 | Error: git missing or too old, unreadable config, unknown PATH for `explain`, an ENTRY that `restore` cannot find or put back, or a trash `restore` could not list completely |
| 2 | Usage error, including an invalid `--idle` or `--keep-trash` DURATION, `--trash` with `--no-trash`, `remove --all --yes` without `--idle`, and `--only-if-upstream-gone` with `--all` or outside `remove` |
| 3 | `remove` did not remove or trash at least one PATH: refused, skipped, not confirmed, or failed. For `remove --all`: a candidate was skipped, not confirmed or failed, or part of a PATH could not be read. For both: an expired trash entry was skipped or failed, or a trash record could not be read |

## For coding agents

To clean up at the end of a task, run this from outside the worktree:

```sh
residue remove --yes --json "$WORKTREE"
```

Exit status 0 means the worktree is gone. Exit status 3 means it is still there. The JSON `results[].reason` and `results[].worktree.checks` fields say why, using stable check ids. Do not run it on worktrees another session may still use. Add `--trash` to keep it restorable for `--keep-trash` (default seven days); exit status 0 then means it is in the trash.

Each PATH gets one `results[].result`:

| Result | Meaning |
| --- | --- |
| `removed` | `git worktree remove` removed it. `reason` is null |
| `trashed` | It was moved to the trash. `reason` is null and `trash_entry` is the entry's path, which `residue restore` takes |
| `refused` | A check did not pass, so residue did not try to remove it |
| `skipped` | residue did not remove it: nobody confirmed (no `yes` at the prompt, or no terminal and no `--yes`), a check before or after the move aside found a change since the plan, or `--only-if-upstream-gone` found the branch's upstream not gone, or could not read it, at any of its three reads. A worktree residue moved aside is back in place |
| `failed` | Git refused the removal, or a Git query during the re-check failed. If residue could not move a worktree back, `reason` names where it is and the one command that puts it back |
| `invalid` | PATH is not a linked worktree residue may remove: the main checkout, a directory inside a worktree, not in a repository, in the trash, or left under a hidden name by an interrupted run |
| `not_attempted` | residue stopped before it reached this PATH |

For every result except `removed` and `trashed`, `reason` is a sentence that says why. With `--all`, `results` lists every linked worktree the scan found, and each one it kept is `refused`. `errors` lists the PATHs, repositories and trash records it could not read.

A remove document also has `trash`, `{"enabled": true, "keep": "7d", "keep_seconds": 604800}`, and `purged`: one object per trash entry the run purged or meant to purge, with `result` (`purged`, `kept` when a check failed, `skipped`, `failed`, `not_attempted`), `reason`, `entry` (the entry as `residue restore --json` lists it) and `worktree` (its checks).

`residue restore --json` prints `residue.restore/v1`: `restored`, the path a worktree was put back at, or null for a listing; `trash`, one object per entry with `entry` (its name), `entry_path`, `worktree_path`, `original_path`, `branch`, `head`, `trashed_at`, `allocated_bytes`, `reclaimable_bytes`, `registered` (Git still lists the worktree) and `error` (a record residue could not read); and `errors`.

In a scan, each repository has `trash`, its entries in the same shape, and `interrupted`: `{"path", "original_path", "pid", "pid_running", "present", "recovery"}` for each worktree an interrupted run left under a hidden name, where `recovery` is the command that puts it back. `summary` counts both, as `trash_entries` and `interrupted_removals`.

A worktree's `rule_sources` is the effective list; the top-level list is what applies to every worktree. Each entry is `{rule, scope, origin}`. A worktree's list starts with the top-level list, `--regenerable` (`command_line`) and then the residue config file (`residue_config`), and continues with its repository's Git config rules in the order Git reads them (`system`, `global`, `local`, `worktree`). Rules add up, so a rule listed twice applies once, and its first entry is the source that wins.

Every scan, explain and remove document has `idle`: `{"threshold": "2d", "threshold_seconds": 172800}`, or null without `--idle`. Each worktree has `last_change`: `{"unix", "at", "source", "what"}`, where `source` is `files`, `git_dir` or `reflog` and `what` is the entry (`src/main.rs`, `.` for the worktree directory itself), the Git directory file (`index`), or the reflog message (`commit: fix login`). It is null with `last_change_error` set when residue could not read it.

Every scan, explain and remove document has `ignored_environment_rules`: the `residue.regenerable` values the environment set and residue ignored, an empty list when there are none, or null when Git could not read that environment. To pass a rule for one run, use `--regenerable`, not `git -c`.

A scan is a snapshot of one moment, `generated_at`. Worktrees created after the scan started are missing from it. Scan again before you act on an old result. `remove` re-checks each path right before it deletes it, so an old scan cannot cause a removal that the current state would refuse.

## Tested on

These are the exact platforms and versions that ran. Apart from the smoke tests and the race check (`scripts/race-check.sh`) that CI runs, residue has no automated test suite. Each row below was checked by running a release binary on real Git repositories.

| Version | Platform | Git | What ran |
| --- | --- | --- | --- |
| 0.0.5 | macOS 26.3.1 (25D771280a), Apple silicon | Apple Git 2.50.1 | The CONTRIBUTING states: every check id, status and verdict identical to the 0.0.4 release binary, without `--idle` and under `--idle 2d`. `scripts/race-check.sh`: 7 passed; the 0.0.4 binary, in the case that adds a file to `node_modules/` at the prompt, removed the worktree (exit 0). A `node_modules/` that grew from 1,204 to 1,388 files at the prompt: `skipped .worktrees/big: node_modules/ changed since you confirmed (1,204 -> 1,388 files)`, exit 3. `remove --all --yes --idle 1m` moved four worktrees to the trash (exit 0); `residue restore` by branch, by name, and by entry path from `/` put three back, and the empty trash directory went with the last; an entry whose record was set eight days back was purged by the next `--idle 2d` sweep, its branch kept. A `git` wrapper that sent SIGKILL to residue before the lock, before the unlock, and after deleting the directory: `scan` listed all three under interrupted removals, and each printed recovery command put the worktree back or cleared Git's record. A process starting in the worktree, or a file appearing, right after the move aside: moved back, exit 3. Something at the original path again: `failed`, with a recovery command that worked. `.github/smoke.sh`, `.github/watch-smoke.sh` and `.github/service-smoke.sh` passed. |
| 0.0.5 | Ubuntu 24.04.5 LTS, arm64 (Docker, Linux 7.0.14) | 2.43.0 | Static musl binary as a non-root user: check ids, statuses and verdicts identical to the 0.0.4 musl binary, without `--idle` and under `--idle 2d`; `scripts/race-check.sh` 7 passed under util-linux `script` 2.39.3; `remove --all --yes --idle 1m` moved four worktrees to the trash, `residue restore space` put one back by branch, and an entry set eight days back was purged by the next sweep; a run killed before the unlock and one killed after the directory was deleted were listed by `scan` under interrupted removals, and both recovery commands worked; a process entering right after the move aside (seen through `/proc`) sent the worktree back, exit 3; `.github/smoke.sh` and `.github/watch-smoke.sh` passed. |
| 0.0.4 | macOS 26.3.1 (25D771280a), Apple silicon | Apple Git 2.50.1 | The CONTRIBUTING states with a pushed `clean` branch: every check id, status and verdict identical to the 0.0.3 release binary without `--idle`. Under `--idle 2d`: the fresh clean pushed worktree kept (`changed just now (--idle 2d)`); `touch -t` on its files and Git directory alone still kept it, by its HEAD reflog; a worktree created and committed with `GIT_COMMITTER_DATE` three days back and its mtimes backdated became a `candidate`, and `remove --all --yes --idle 2d` with no terminal removed it (branch kept, exit 0) and kept the other seven. `git gc` in the main checkout (rewrites `logs/HEAD`), three rounds of `git status`, `git diff` and `git log`, and residue's own scans left a backdated worktree a candidate; a checkout, `git add` and an edit made it `keep`. `remove --yes --idle 2d` on a fresh worktree refused (exit 3). Invalid durations and `remove --all --yes` without `--idle` gave exit 2. Rules from `GIT_CONFIG_GLOBAL` and `.git/config` named by scope and file in text and `rule_sources`; an invalid global value named its file. Header and dates with offsets under `TZ` unset, `UTC`, `Asia/Kolkata` and `America/New_York`. The README LaunchAgent plist loaded with `launchctl bootstrap`, run with `launchctl kickstart` from `/`: it removed the idle worktree and logged absolute paths. |
| 0.0.4 | Ubuntu 24.04.5 LTS, arm64 (Docker, Linux 7.0.14) | 2.43.0 | Static musl binary as a non-root user: the same states with check ids, statuses and verdicts identical to the 0.0.3 musl binary without `--idle`; the fresh worktree kept under `--idle 2d`, the backdated one a `candidate`, unchanged by three rounds of `git status`, `git diff` and `git log` and by `git gc`; `remove --all --yes --idle 2d` without a terminal removed it (branch kept, exit 0). The README systemd service and timer passed `systemd-analyze verify` in user and system mode (systemd 255). The timer did not run under a live systemd. |
| 0.0.3 | macOS 26.3.1 (25D771280a), Apple silicon | Apple Git 2.50.1 | Scratch repository with the CONTRIBUTING states plus `.local-dev/*.lock` files: `scan` verdicts identical to 0.0.2 with no rules; `residue.regenerable` set through `GIT_CONFIG_COUNT` (mixed-case key too), `git -c` through a Git alias, and the legacy `GIT_CONFIG` all ignored with the stderr note and `ignored_environment_rules` (0.0.2 made the `.env` worktree a `candidate` under `GIT_CONFIG`; 0.0.3 keeps it); a malformed `GIT_CONFIG_COUNT` giving the "could not check" note and null; an escape character in an ignored value printed escaped; rules in `.git/config`, `$HOME/.gitconfig`, `GIT_CONFIG_GLOBAL` and `GIT_CONFIG_SYSTEM` honored, and skipped under `GIT_CONFIG_NOSYSTEM`; `remove --yes` under `git -c residue.regenerable=.env` refusing `.env`, removing a clean worktree (branch kept), exit 3. |
| 0.0.3 | Ubuntu 24.04.5 LTS, arm64 (Docker, Linux 7.0.14) | 2.43.0 | Static musl binary as a non-root user: the same scratch states; `GIT_CONFIG_COUNT`, `git -c` and legacy `GIT_CONFIG` rules ignored with the note and JSON field; `.git/config` and `GIT_CONFIG_GLOBAL` rules honored; `--regenerable .env` honored; `remove --yes` without a terminal under `git -c` refusing `.env` and removing a clean worktree (branch kept), exit 3. |
| 0.0.2 | macOS 26.3.1 (25D771280a), Apple silicon | Apple Git 2.50.1 | Scratch repositories: a path rule (`web/.tscache`, `/web/`, `web/.tsc*`) matching; `.local-dev/*.lock` expanding a collapsed `.local-dev/` when every file matched (candidate) and when `server.pid` did not (keep, naming the file); the same rules from `git config residue.regenerable` and from the config file; invalid git config values (`logs/**`, an empty value) giving `unknown`; an unreadable directory inside an expanded entry (`unknown`); 100,001 lock files (kept, over the limit); sizes with hard links counted once in `scan`, `explain` and JSON; `generated_at`; `remove --yes` refusing `.env` and `.vercel/` and removing a clean worktree and an expanded `.local-dev/` while the branches stayed. The 0.0.1 verdicts again: locked, modified, untracked, assume-unchanged, nested repository, rebase in progress, unreferenced detached HEAD, a path with a space. A read-only `scan` of a busy agent monorepo with four linked worktrees: 5.3 s. |
| 0.0.2 | Ubuntu 24.04.5 LTS, arm64 (Docker, Linux 7.0.14) | 2.43.0 | Static musl binary as a non-root user with `TZ=Europe/Tallinn`: `scan` with a git-config glob rule and a `--regenerable` path rule, `explain` naming the unmatched file, JSON sizes and `generated_at`, local `as of` time, in-use detection through `/proc`, an invalid git config value, `remove --yes` refusing `.env` and removing a clean worktree without a terminal (branch kept), and `remove` without `--yes` and without a terminal removing nothing. |
| 0.0.1 | macOS 26.3.1 (25D771280a), Apple silicon | Apple Git 2.50.1 | `scan`, `explain` and `remove` on real agent worktrees and on scratch repositories covering every verdict: clean with `node_modules`, `.env`, unreferenced detached HEAD, locked, in use, modified, stale, a path with a space, a path with an escape character, an assume-unchanged edit, a skip-worktree file, a nested repository and a rebase in progress. Removal kept the branches, and the assume-unchanged edit survived. |
| 0.0.1 | Ubuntu 24.04.5 LTS, arm64 (Docker, Linux 7.0.14) | 2.43.0 | Static musl binary as a non-root user: `scan`, in-use detection through `/proc`, hard-link accounting, and `remove` with and without a terminal. As root on another user's repository, Git refused with "dubious ownership" and residue reported the error instead of guessing. |
| 0.0.5 | GitHub Actions `macos-latest`, `ubuntu-24.04` (x86_64), `ubuntu-24.04-arm`, and FreeBSD 14 in a virtual machine | runner Git | Build, `scan` and `explain` of a temporary worktree, `.github/smoke.sh`, `.github/watch-smoke.sh` and `scripts/race-check.sh`, on FreeBSD as root and as an ordinary user; `.github/service-smoke.sh` on macOS and Linux x86_64. |
| 0.0.1 to 0.0.4 | GitHub Actions `macos-latest`, `ubuntu-24.04` (x86_64), `ubuntu-24.04-arm` | runner Git | Build, then `scan` and `explain` of a temporary worktree. No removal. |

Not run: the Intel macOS binary (built by cross-compiling, never executed), NetBSD, OpenBSD, DragonFly and illumos (they build, but have no activity probe, so nothing is a candidate), and Windows (not supported).

CI runs four scripts on macOS, Linux x86_64 and arm64, and FreeBSD 14 as root and as an ordinary user, on every push:

- `.github/smoke.sh`: a clean candidate, a kept `.env`, a worktree a process is using, and a removal that keeps the branch.
- `scripts/race-check.sh`: five changes made while the prompt waits, each skipped; an upstream restored while it waits, kept with and without the trash; and three removals that changed nothing.
- `scripts/contract-check.sh` (needs `jq`): `remove` with no prompt, one repository per case. A clean worktree removed, in JSON and text; refusals for a staged edit, a modified file, an untracked file, an ignored `.env` and a lock; the main checkout, a directory inside a worktree and a missing path, each `invalid`; no terminal without `--yes`; several targets at once; diagnostics on stderr, never in the JSON; and usage errors. Each case checks the exit status, the JSON result and failing check ids, and afterwards the file bytes, Git's registration, the branch, an unrelated worktree, and that nothing was left moved aside or in the trash.
- `.github/watch-smoke.sh`: `watch` removing a worktree whose branch landed, once and as a running process woken by a `git fetch --prune`, and under disk pressure, without the trash. Then `remove --only-if-upstream-gone` on its own: an upstream that is back, none, a local one, a detached HEAD, a ref Git marks gone that still exists, dirty, locked and recently changed worktrees, and a gone upstream that is removed. Then `watch` through a `git` wrapper that restores the upstream right before each of the removal's three reads, and once during the re-measure before the last read. `.github/service-smoke.sh` installs, checks and uninstalls the launchd agent on macOS and the systemd user service on Linux.

## Limits

- **Work landed means the local refs say so.** residue never fetches: a branch deleted on the remote counts once a `git fetch --prune` reaches the local refs, and one that comes back counts once a fetch brings it back. An upstream counts as gone only when its remote-tracking ref is absent. A loose ref that is a dangling symbolic link reads as absent to Git and so to residue, and a `git remote rename` in progress makes its branches look gone until it finishes; `watch` scans only after the refs have been quiet for 20 seconds, at most two minutes.
- **Freed space is an estimate.** residue counts each hard-linked file once and excludes files that also have links outside the worktree (such as pnpm's store). It cannot see APFS or Btrfs clones, snapshots, or deleted files a process still holds open, so the space actually freed can be smaller.
- **residue reads every entry of every worktree it inspects.** That walk is the check behind `nested_repo`, `mount_point` and `contents`, and the sizes come from the same walk. So there is no option to skip it. On a 1.0 GB worktree with 46,572 entries (macOS, Apple silicon, warm cache), `residue explain` took 0.65 to 0.83 s, and `git worktree remove` of an identical copy took 2.7 to 3.0 s.
- **The last check is not the deletion.** residue moves a worktree aside and checks it again before Git deletes it, which leaves a window from that check to the end of the deletion. A process that opens the hidden directory by its new name, or one the probe cannot see, can write into it then, and the write is deleted with it. [What removal deletes](#what-removal-deletes-and-what-it-keeps) has the detail. A sweep uses the trash, so what it takes by mistake stays restorable until the purge.
- **residue only handles linked worktrees.** It does not touch package-manager stores, Docker build cache or toolchain caches. Use their own commands, which know what is safe: `pnpm store prune`, `docker builder prune`, `npm cache verify`, `go clean -cache`, `uv cache prune`.
- **Stale entries are reported, not cleaned.** For a worktree whose directory is already gone, residue reports the leftover Git metadata as `stale` and tells you to run `git worktree prune`. For an orphaned checkout whose repository is gone, residue lists it and leaves it alone.
- **residue is not a security boundary.** It runs as you, and assumes the other processes running as you are not trying to fool it.

## Other tools

Several tools already address this problem. Pick the one that matches how you work:

- [Claude Code](https://code.claude.com/docs/en/worktrees) sweeps the worktrees it creates for subagents and background tasks, and locks the ones in use.
- [Worktrunk](https://worktrunk.dev) manages the worktree lifecycle (create, switch, merge, remove) and can stop processes tied to a worktree.
- [Agentdisk](https://github.com/laaielichter/Agentdisk) attributes disk use across agent tools and moves what it removes to a quarantine you can restore from.

residue does one job: it removes linked worktrees from any tool, and only when every check above passes. It hands the deletion to Git, keeps what a scheduled sweep takes in a trash you can restore from until it expires, reports activity as one of three states on both macOS and Linux, keeps any ignored file that is not on the regenerable list, and depends on one crate (`libc`).

## Contributing

Bug reports, fixes and test reports from other platforms are welcome. [CONTRIBUTING.md](CONTRIBUTING.md) has the setup, the checks to run, and a script that builds a scratch repository covering each verdict.

## License

MIT
