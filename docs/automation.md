# Automate residue

Back to [Automate cleanup](../README.md#automate-cleanup). `residue watch` deletes. It does not use the trash. A scheduled `residue remove --all --idle` uses the trash unless you pass `--no-trash`.

## Reclaim storage automatically

```sh
residue service install ~/src
```

This starts `residue watch ~/src` now and at every login, as a launchd agent on macOS or a systemd user service on Linux. It removes a worktree in two cases, and otherwise does nothing:

- **Its work landed.** Its upstream branch was deleted, which is what merging a pull request with branch deletion, or `git push --delete`, does, and the worktree has not changed for an hour (`--grace`). residue never fetches, so it notices after your next `git fetch --prune`. `git config --global fetch.prune true` makes every fetch prune.
- **The disk is low.** Free space on the worktree's filesystem is below 10% (`--min-free`). Then worktrees unchanged for two days (`--idle`) go, largest first, until free space is 5 points above that. While the disk has room, idle worktrees stay.

Each removal is `residue remove --yes --idle --no-trash`, so every check in [What makes a worktree a candidate](../README.md#what-makes-a-worktree-a-candidate) runs, and runs again after the worktree is moved aside, right before the deletion. For a branch that landed, `watch` also passes `--only-if-upstream-gone`, so the removal checks the reason itself, from the local refs, three times: at its plan, right before the move aside, and after the move, right before the deletion. If the upstream is back by then (a fetch brought the branch back, say), the worktree stays, back in place, and the log says why. A worktree with no upstream, or on a detached HEAD, never counts as landed. Neither does a local upstream (`branch.NAME.remote` set to `.`), since a deleted local branch is not work that landed on a remote, nor an upstream Git marks `[gone]` whose ref still exists without resolving to a commit, such as a symbolic ref to a missing ref. `watch` deletes instead of using [the trash](../README.md#the-trash): freeing the disk is its job, and a trashed worktree keeps every byte until the purge. The same run purges that repository's trash entries older than seven days. Branches and commits stay, and a locked worktree stays. It works the same for worktrees from any tool (Claude Code, Codex, Cursor, Conductor, `git worktree add`), because it reads Git and the disk, not the agent.

There is no schedule. `watch` sleeps until a fetch or prune changes a repository's `refs/remotes` or `packed-refs` (inotify on Linux, kqueue on macOS and the BSDs), until a waiting worktree passes `--grace` or `--idle`, or until a filesystem drops below `--min-free`. Free space is read every five minutes, because no system sends an event for it. Every six hours `watch` also looks for repositories created under the roots since. One `watch` runs per user.

Try it without removing anything first:

```sh
residue watch --once --dry-run ~/src       # what it would remove right now
residue service install --dry-run ~/src    # a service that only logs what it would remove
residue service status                     # is it running, and the last lines of its log
residue service uninstall
```

The log is `~/Library/Logs/residue/watch.log` on macOS and `journalctl --user -u residue-watch` on Linux. A Linux user service stops when you log out unless you run `loginctl enable-linger "$USER"` once. On other systems, `residue service install` prints the `residue watch` command to run from your system's service manager. [Why `--idle` matters](#why---idle-matters) explains what counts as a change, and why a paused agent session needs a wait. If you would rather sweep at a fixed time, see [Run it on a schedule](#run-it-on-a-schedule).

## Run it on a schedule

A daily sweep keeps leaked worktrees from piling up:

```sh
residue remove --all --yes --idle 2d --keep-trash 7d ~/src
```

`--all` treats each PATH as a scan root, the way `scan` does, and takes every candidate it finds. `--idle 2d` keeps every worktree that changed in the last two days. `remove --all --yes` refuses to run without `--idle`.

`--idle` also turns on `--trash`: the sweep moves each candidate to [the trash](../README.md#the-trash) instead of deleting it, so a worktree it took by mistake comes back with `residue restore`. The same run purges the entries older than `--keep-trash`, seven days here, which is also the default. The purge is what frees the disk. A sweep that trashes frees nothing until its entries expire, so with a daily sweep a worktree leaves the disk between nine and eleven days after its last change. For the space sooner, lower `--keep-trash`, or pass `--no-trash` to delete at once.

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
