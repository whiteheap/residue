# Automate residue

Back to [Automate cleanup](../README.md#automate-cleanup). `residue watch` deletes. It does not use the trash. A scheduled `residue remove --all --idle` uses the trash unless you pass `--no-trash`.

## Reclaim storage automatically

See what it would remove right now with `residue watch --once --dry-run` ([Try it first](#try-it-first)), then turn it on:

```sh
residue service install
```

This starts `residue watch` now and at every login, as a launchd agent on macOS or a systemd user service on Linux, and prints the folders it watches, what it removes, how to preview it and how to turn it off (`residue service uninstall`). It never asks a question, so a script or an installer can run it. On a Git older than 2.36 it stops before it installs anything, and a `watch` already running logs that once and waits for a newer Git. It removes a worktree in two cases, and otherwise does nothing:

- **Its work landed.** Its upstream branch was deleted, which is what merging a pull request with branch deletion, or `git push --delete`, does, and the worktree has not changed for an hour (`--grace`). `watch` sees the deletion through its own fetch ([Fetching](#fetching)), or sooner through yours.
- **The disk is low.** Free space on the worktree's filesystem is below 10% (`--min-free`). Then worktrees unchanged for two days (`--idle`) go, largest first, until free space is 5 points above that. While the disk has room, idle worktrees stay. Space a removal frees can take a while to show as free, such as while an APFS local snapshot holds it: for 24 hours, a pass counts what has yet to show toward its target and logs that it waits for it, so it never removes more for that space.

Each removal is `residue remove --yes --idle --no-trash`, so every check in [What makes a worktree a candidate](../README.md#what-makes-a-worktree-a-candidate) runs, and runs again after the worktree is moved aside, right before the deletion. For a branch that landed, `watch` also passes `--only-if-upstream-gone`, so the removal checks the reason itself, from the local refs, three times: at its plan, right before the move aside, and after the move, right before the deletion. If the upstream is back by then (a fetch brought the branch back, say), the worktree stays, back in place, and the log says why. A worktree with no upstream, or on a detached HEAD, never counts as landed. Neither does a local upstream (`branch.NAME.remote` set to `.`), since a deleted local branch is not work that landed on a remote, nor an upstream Git marks `[gone]` whose ref still exists without resolving to a commit, such as a symbolic ref to a missing ref. `watch` deletes instead of using [the trash](../README.md#the-trash): freeing the disk is its job, and a trashed worktree keeps every byte until the purge. The same run purges that repository's trash entries older than seven days. Branches and commits stay, and a locked worktree stays. Each removal first writes a [removal record](../README.md#restore-a-removed-worktree), which says `by watch`, and its log line ends with the command that recreates the worktree, `restore: residue restore RECORD`, for 30 days or the `--keep-removed` you give `watch` ([Removal records](#removal-records)). When the branch has commits on no remote-tracking ref, a landed removal's line says how many stay on it, such as `; 2 commits on no remote-tracking ref stay on branch fix`. A worktree you restore is marked, and `watch` never removes it again because its branch landed, whatever a fetch later shows: you wanted it back after it landed. Disk pressure still removes it, and so does `residue remove`. It works the same for worktrees from any tool (Claude Code, Codex, Cursor, Conductor, `git worktree add`), because it reads Git and the disk, not the agent.

There is no schedule. `watch` sleeps until a fetch or prune changes a repository's `refs/remotes` or `packed-refs` (inotify on Linux, kqueue on macOS and the BSDs), until a repository's own fetch comes due, until a waiting worktree passes `--grace` or `--idle`, or until a filesystem drops below `--min-free`. Free space is read every five minutes, because no system sends an event for it: Linux has none, and on macOS APFS sent none for a 10 GB change. Every six hours `watch` also looks for repositories created under the roots since. kqueue holds an open file for each folder it watches, so on macOS and the BSDs `watch` raises its limit on open files, which launchd sets to 256 for a service, and watches at most 128 fewer folders than that limit, so every pass can still start Git and residue. When there are more, it says so once, and changes in the rest are seen on the next timer. One `watch` runs per user. While none is installed or running, `residue scan` at a terminal ends with a one-line reminder of how to preview it and turn it on: `Automatic cleanup is off. Preview it: residue watch --once --dry-run · turn it on: residue service install`.

### Which folders

With no ROOT, `watch` and `service install` use the usual code folders in your home folder, those that exist: `~/src`, `~/code`, `~/Code`, `~/dev`, `~/Developer`, `~/projects`, `~/Projects`, `~/repos`, `~/git`, `~/github`, `~/GitHub`, `~/work`, `~/Work` and `~/workspace`. On a case-insensitive filesystem `~/code` and `~/Code` are one folder, watched once. Each is searched like any ROOT: four levels deep, on one filesystem, without following symbolic links, skipping hidden folders, `node_modules`, `target` and `Library`. The list leaves out the home folder itself, which holds far more than code, and `Documents`, `Desktop` and `Downloads`, which macOS guards with a permission prompt that a background service cannot answer. If none of the folders exists, both commands print one line that names them and how to pass a ROOT, and exit 2:

```sh
residue service install ~/path/to/repos
```

A ROOT you name replaces the list. `service install` writes the folders it found into the service, so the service watches the same ones at every start, and `residue service status` shows them. A folder created later is picked up when you run `residue service install` again.

### Running install again

`residue service install` replaces the service it finds, so running it twice leaves one service. Without a ROOT, it keeps the ROOTs you named for the service it replaces, and drops any that is no longer a folder, with a note; a service on the usual code folders looks for them again. To change the folders, name them. Options are not kept: each one you do not give goes back to its default, `--dry-run` included, and the command says so when the service it replaced was a dry run.

Installs and `residue service uninstall` run one at a time. One that starts while another runs prints `waiting for another residue service install or uninstall to end`, waits up to 30 seconds, then exits 1.

To replace only the service you looked at, as the Mac app does, pass its fingerprint. `residue service status` prints it, also when no service is installed:

```sh
fp=$(residue service status | sed -n 's/^fingerprint: //p')
residue service install --expect "$fp" ~/src
```

If the service changed since that read, `install` writes nothing, prints one line, and exits 4. Read the status again and decide again. `--expect none` installs only while neither the service nor its file exists. The fingerprint is 32 hex digits. It changes when the service file changes, when the binary the file starts resolves to another file (a `brew upgrade` does that under the same link), and, on macOS, when launchd loads or unloads the agent. It also includes the residue release that reads it, so a fingerprint from another release never matches: read it with the same `residue` that installs. The comparison runs right before the write, under the lock above, so no residue install or uninstall can change the service in between. A program that edits the file without residue still can.

residue's Homebrew formula defines no service, so turn on automatic cleanup with `residue service install`, not `brew services`. If a Homebrew service for residue exists anyway, from a formula of your own, `service install` says so and exits 1: only one `watch` runs per user. Stop that one first with `brew services stop residue`.

### Fetching

To see a branch deleted on the remote, `watch` runs `git fetch --prune --no-tags` in each repository about every 30 minutes (`--fetch-every`), each repository on its own clock, its interval varied by up to a tenth so they do not all fetch at once. It fetches only the remotes that a linked worktree's branch tracks, and none in a repository with no such branch: no other ref decides anything `watch` does. It fetches in the main checkout, with no `FETCH_HEAD` written, no automatic maintenance and no submodules, so a worktree's last change is not touched.

The fetch never prompts. Git runs without a terminal and with `GIT_TERMINAL_PROMPT=0`; SSH runs with `BatchMode=yes` unless you set your own SSH command (`GIT_SSH_COMMAND`, `GIT_SSH` or `core.sshCommand`), which is used as it is, still without a terminal. A fetch that needs a password, a passphrase or a host key answer fails. So does one that takes longer than five minutes. Use an SSH agent or a credential helper for a remote that needs one.

A fetch changes only the remote-tracking refs of the remote it fetches. `watch` gives Git only that remote's fetch refspecs that store under `refs/remotes/`, so tags, notes and every other ref stay as they are, whatever else the remote's refspecs, `fetch.pruneTags` or `remote.NAME.pruneTags` name; your own `git fetch` still uses them all. A refspec that also stores into another remote's `refs/remotes/OTHER/` is left out too, and so are both remotes' refspecs when one name holds the other, such as `origin` and `origin/fork`. A remote with nothing left is not fetched. The log says each once:

```text
2026-11-07T18:41:14Z residue watch: fetch of up in ~/src/app leaves out +refs/heads/*:refs/remotes/origin/* (remote origin's refs)
2026-11-07T18:41:14Z residue watch: not fetching origin in ~/src/app: none of its fetch refspecs stores into refs/remotes/; its refs stay as they were; next try in about 30m
```

A fetch is never a reason to remove: it only refreshes the refs that the scan and the removal read, and every check above still decides. A fetch that fails logs one line, leaves that repository's refs as they were, and is tried again at the next interval; the log says when it works again. A branch the fetch finds deleted is logged too, before any removal it leads to:

```text
2026-11-07T18:41:14Z residue watch: fetched ~/src/app: origin/fix-login is gone from the remote
2026-11-07T18:41:17Z residue watch: removed ~/src/app/.worktrees/fix-login (8.2 KB): origin/fix-login was deleted, unchanged for 1h; restore: residue restore /Users/me/src/app/.git/residue-removed/fix-login-20261107T184117Z
2026-11-07T18:41:17Z residue watch: freed about 8.2 KB (about 14.2 GB on this machine since 2026-10-02)
```

`--no-fetch` turns it off. Then `watch` sees a deleted branch after your own `git fetch --prune`; `git config --global fetch.prune true` makes every fetch prune. `scan`, `explain` and `remove` never fetch.

### Removal records

`residue restore` can recreate a worktree `watch` removed for as long as its [removal record](../README.md#restore-a-removed-worktree) lasts: at least 30 days, or the `--keep-removed DURATION` you pass, with the same syntax and limits as `residue remove --keep-removed` (`12h`, `30d`, `12w`, at most 100 years). `watch` passes it to each removal it starts. Each record that removal writes keeps the value in its `info` (`keep 7257600 12w`), and `residue restore --json` shows it as `keep` and `keep_seconds`. To keep records twelve weeks:

```sh
residue service install --keep-removed 12w
```

`residue service status` then shows `removal records: kept 12w (--keep-removed)`, and the `watching` line at the top of the log carries `· removal records: 12w ·`. A service installed before `watch` took the option shows `removal records: kept 30d (--keep-removed, the default)`. Running `residue service install` again without the option goes back to `30d`, and says so in two lines: `removal records: new ones kept 30d (was 12w); written ones keep their own`, then `to keep 12w: residue service install --keep-removed 12w`.

No run forgets a record before the `--keep-removed` it was written with. A run forgets the records in its repositories that are older than both that and its own `--keep-removed`, so a `residue remove` you run yourself, with its default `30d`, leaves a record written with `12w` until it is twelve weeks old. A record older than both is forgotten by the next removal in its repository, not on the day it expires, so in a repository where nothing more is removed it stays. A record residue 0.0.7 or earlier wrote has no `keep`, and the run's value alone decides. Forgetting a record can lose commits only its refs held: [What `--keep-removed` can lose](../README.md#restore-a-removed-worktree).

### Try it first

```sh
residue watch --once --dry-run             # what it would remove right now
residue service install --dry-run          # a service that only logs what it would remove
residue service status                     # is it running, what it watches, how long it keeps removal records, what it freed, the last lines of its log
residue service uninstall
```

When a `--once` pass removes, or would remove, nothing, it logs one line saying why: no linked worktrees, no branch landed or a check keeps those that did, and each disk's free space against `--min-free`. A dry run still fetches; add `--no-fetch` to leave the refs alone too. The log is `~/Library/Logs/residue/watch.log` on macOS and `journalctl --user -u residue-watch` on Linux. After each removal it logs what the removal freed and the total for this machine, with the UTC date, like the log's own times. `residue service status` has a `freed:` line: what automatic cleanup freed in the last seven days and when it last removed a worktree, such as `freed: about 2.1 GB from 4 worktrees in the last 7 days; last removal 2026-11-07 04:12 +02:00 (5 hours ago)`. `residue stats` has the rest; [Stats](reference.md#stats) says how it is counted. A Linux user service stops when you log out unless you run `loginctl enable-linger "$USER"` once. On other systems, `residue service install` prints the `residue watch` command to run from your system's service manager. [Why `--idle` matters](#why---idle-matters) explains what counts as a change, and why a paused agent session needs a wait. If you would rather sweep at a fixed time, see [Run it on a schedule](#run-it-on-a-schedule).

### After an upgrade

The service runs the new release within about six minutes. Before each pass, and at least every five minutes while it waits, `watch` checks that the path the service starts still names the file it runs. Once an upgrade replaces that file, `watch` logs one line and exits 1 between passes, never during one, and launchd or systemd starts the new release:

```text
residue watch: a new residue is at /opt/homebrew/bin/residue, most likely an upgrade; exiting so the service manager starts the new one
```

Each start's first log line ends with the version, such as `· residue 0.0.11`. A `watch` that 0.0.10 or earlier started has no such check, so its own loop runs on until it restarts, such as at your next login. Each of its passes already runs the new release: it starts them by its own path, which the upgrade points at the new file.

Homebrew on Linux is the exception. There that path is the old version's own file. Unless Homebrew keeps old versions (`HOMEBREW_NO_INSTALL_CLEANUP`), the upgrade deletes it, and the `watch` exits and the service starts the new release. If it keeps them, the `watch` runs old passes until it restarts, so restart the service once. For the one `residue service install` set up:

```sh
systemctl --user restart residue-watch.service
```

For one that `brew services` runs:

```sh
brew services restart residue
```

The restart stops a removal still running, with its Git, as Ctrl-C does, and `residue scan` then shows how to bring that worktree back. A `residue restore` of a record that a run of 0.0.10 or earlier wrote refuses while that run, or a `git worktree remove` of its worktree, still runs, and names the process.

## Run it on a schedule

A daily sweep keeps leaked worktrees from piling up:

```sh
residue remove --all --yes --idle 2d --keep-trash 7d ~/src
```

`--all` treats each PATH as a scan root, the way `scan` does, and takes every candidate it finds. `--idle 2d` keeps every worktree that changed in the last two days. `remove --all --yes` refuses to run without `--idle`.

`--idle` also turns on `--trash`: the sweep moves each candidate to [the trash](../README.md#the-trash) instead of deleting it, so a worktree it took by mistake comes back with `residue restore`. The same run purges the entries older than `--keep-trash`, seven days here, which is also the default, and than the `--keep-trash` each was trashed with. The purge is what frees the disk. A sweep that trashes frees nothing until its entries expire, so with a daily sweep a worktree leaves the disk between nine and eleven days after its last change. For the space sooner, lower `--keep-trash`, or pass `--no-trash` to delete at once.

### Why `--idle` matters

The activity check finds a worktree in use when a process has its working directory or an open file inside it. A coding agent that is waiting for your approval, or for a merge queue, often has neither. Agent harnesses such as Claude Code reset the shell to the main checkout after every command, so between commands no process is inside the worktree. If its work is committed and pushed, every other check passes too. When you run `residue remove` yourself, you see the plan before anything is deleted. A scheduled run has nobody watching, so it needs another sign that someone is still using the worktree: a recent change. Nothing is lost if residue removes it anyway, because the branch and its commits stay. But the paused agent fails on its next command, because its directory is gone, until `residue restore` puts it back: from the trash, or, after a deletion, from its removal record.

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

`remove --all` exits 0 when it moved every candidate to the trash (or removed it, with `--no-trash`), and purged every expired trash entry, including when there was none. A worktree it keeps does not make it fail, and neither does an expired entry it keeps because a check failed. It exits 3 when a candidate or an expired entry was skipped or failed, or when it could not read part of a PATH or a trash record. A removal record it could not read or forget never makes it fail, nor does a trash entry it keeps because the entry holds something residue never deletes: it prints a warning on stderr and lists it in JSON `warnings`, never in `errors`. The log names each one.
