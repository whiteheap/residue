# Command and configuration reference

Back to the [README](../README.md#reference).

## Where a PATH applies

```sh
residue scan [PATH...]           # every linked worktree of the repositories at or under PATH
residue explain PATH             # every check for one worktree, and why it passed or failed
residue remove PATH...           # confirm, check again, move aside, check, write a removal record, then `git worktree remove` each one
residue remove --all PATH        # the same for every candidate a scan of PATH finds
residue remove --trash PATH...   # move each one to the trash instead; --idle turns this on
residue restore [ENTRY]          # list the trash and the removal records, or put one worktree back (--to PATH: somewhere else)
residue watch [ROOT...]          # stay running; remove a worktree when its branch lands or the disk runs low
residue service install [ROOT...]  # run `residue watch` from login; no ROOT: the usual code folders
```

`scan` with no PATH looks at the current directory. If PATH is inside a repository, residue scans that repository. Otherwise it searches up to four levels below PATH. The search does not follow symbolic links, does not cross into another filesystem, and skips hidden directories, `node_modules`, `target` and `Library`. Linked worktrees come from Git's own list, so they are found wherever they live.

Add `--json` to any command to get one JSON document (schemas `residue.scan/v1`, `residue.explain/v1`, `residue.remove/v1`, `residue.restore/v1`). Each document has `generated_at`, the moment residue started reading, in RFC 3339 UTC. The text output shows the same moment in local time with its UTC offset in its first line (`as of 14:03:12 +02:00`), and every date in the text uses that zone.

Add `--idle DURATION` (`30m`, `12h`, `2d`, `1w`) to `scan`, `explain` or `remove` to also keep every worktree that changed within DURATION. With `remove`, it also turns on `--trash`. [Run it on a schedule](automation.md#run-it-on-a-schedule) explains what counts as a change.

## The regenerable list

When `git worktree remove` deletes a clean worktree, it deletes the worktree's ignored files without asking. But being ignored does not make a file disposable: `.env` files, `.vercel/` project links, local databases and scratch notes are usually ignored too. So residue keeps the worktree unless every ignored entry matches a regenerable rule. The built-in rules name dependency and build-output directories that their tools recreate:

`node_modules` `.pnpm-store` `bower_components` `.next` `.nuxt` `.svelte-kit` `.output` `.turbo` `.parcel-cache` `.vite` `.angular` `.eslintcache` `*.tsbuildinfo` `dist` `build` `coverage` `.nyc_output` `target` `.build` `DerivedData` `.gradle` `.dart_tool` `.venv` `venv` `__pycache__` `*.pyc` `.pytest_cache` `.mypy_cache` `.ruff_cache` `.tox` `.nox` `.DS_Store`

Rules apply only to ignored entries, the ones `.gitignore` or `.git/info/exclude` match. An untracked file that is not ignored always keeps the worktree (the `untracked` check), and no rule or `--regenerable` changes that. To let residue delete such a file, ignore it first.

`scan`, `explain` and the `remove` plan show how many files each ignored entry holds and their size, for example `ignored: .env (1 file, 312 B), .vercel/ (2 files, 1.2 KB)`.

### Rules

A rule is a name or a path.

- **A name**, or `*.ext`, matches one path component at any depth. `cache` matches `web/cache/` but not `web/cache2/`.
- **A path** contains a slash and starts at the worktree root. A leading slash is optional, and a trailing slash is ignored. Each segment is literal or uses `*` for any run of characters within that one segment. `**`, `?`, `[`, `\`, and empty, `.` or `..` segments are rejected. A path matches that entry and everything below it: `web/.tscache` matches `web/.tscache/`, and `/build` matches the top-level `build/` but not `web/build/`.

Git reports a fully ignored directory once, as `dir/`, and residue judges it as that one entry, also when it reads Git's faster `--ignored=matching` list. When a path rule reaches inside such a directory, for example `.local-dev/*.lock` and the entry `.local-dev/`, residue reads the directory. It calls the directory regenerable only if a rule matches every file in it, or one of that file's parent directories. Empty directories do not count. `explain` names the first file that no rule matches. A directory with more than 100,000 entries is not judged file by file, so residue keeps it.

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
residue: note: instead, pass --regenerable RULE, or keep it with `git config --add residue.regenerable RULE`
```

residue never reads rules from a file committed to the repository. A repository you clone must not be able to declare your files disposable, and `git clone` never copies Git config.

For every repository on the machine, put one rule per line in `~/.config/residue/regenerable` (or `$XDG_CONFIG_HOME/residue/regenerable`). Lines starting with `#` are comments. For one run, pass `--regenerable RULE`. An invalid rule in the file or on the command line stops residue with exit status 1.

## Exit status

| Code | Meaning |
| --- | --- |
| 0 | Success. For `remove`, every PATH was removed or moved to the trash and every expired trash entry was purged. For `remove --all`, the same for every candidate, or there was none. A removal record `remove` could not read or forget, or a [stats ledger](#stats) it could not write, never changes the status: it is a warning on stderr and in `warnings`. For `restore`, the worktree is back, or the trash and the removal records were listed; an entry it could not read is listed with its `error`. For `stats`, the report was printed |
| 1 | Error: git missing or too old, unreadable config, unknown PATH for `explain`, an ENTRY that `restore` cannot find or put back (its branch checked out elsewhere, something at its path, more than one entry by that name, another residue run, or the `git worktree remove` it started, holding it, or a worktree it moved back that Git can no longer use because a purge by a release that takes no hold wrote a removal record meanwhile), an ENTRY path that is gone where the nearest folder that exists is the home folder or above it, an ENTRY path with no entry in the repositories at or under the nearest folder that exists, when the current directory is the home folder or above it, a `restore` run from the home folder or above it when none of the usual code folders exists in it, a repository, trash or removal-record folder `restore` could not list, or a stats ledger `stats` could not read (a missing one is empty) |
| 2 | Usage error, including an invalid `--idle`, `--keep-trash` or `--keep-removed` DURATION, `--trash` with `--no-trash`, `remove --all --yes` without `--idle`, `--only-if-upstream-gone` with `--all` or outside `remove`, `--keep-removed` outside `remove` and `watch`, `--removal-reason` outside `remove`, a `--removal-reason` other than `watch-landed` or `watch-low-disk`, `--to` outside `restore` or without an ENTRY, and `residue stats` with anything but `--json` |
| 3 | `remove` did not remove or trash at least one PATH: refused, skipped (including a removal record it could not write, so nothing was deleted), not confirmed, or failed. For `remove --all`: a candidate was skipped, not confirmed or failed, or part of a PATH could not be read. For both: an expired trash entry was skipped or failed, or a trash record could not be read |

## For coding agents

To clean up at the end of a task, run this from outside the worktree:

```sh
residue remove --yes --json "$WORKTREE"
```

Exit status 0 means the worktree is gone. Exit status 3 means it is still there, or that an expired trash entry of its repository could not be purged; `results[].result` says which. The JSON `results[].reason` and `results[].worktree.checks` fields say why, using stable check ids. Do not run it on worktrees another session may still use. Add `--trash` to keep it restorable for `--keep-trash` (default seven days); exit status 0 then means it is in the trash. Without `--trash`, the removal still leaves a removal record, `results[].removal_record`, which `residue restore` takes for `--keep-removed` (default 30 days): it recreates the worktree on its branch and HEAD, without the ignored files the deletion took.

Each PATH gets one `results[].result`:

| Result | Meaning |
| --- | --- |
| `removed` | `git worktree remove` removed it. `reason` is null and `removal_record` is the record's path, which `residue restore` takes |
| `trashed` | It was moved to the trash. `reason` is null and `trash_entry` is the entry's path, which `residue restore` takes |
| `refused` | A check did not pass, so residue did not try to remove it |
| `skipped` | residue did not remove it: nobody confirmed (no `yes` at the prompt, or no terminal and no `--yes`), a check before or after the move aside found a change since the plan, or `--only-if-upstream-gone` found the branch's upstream not gone, or could not read it, at any of its three reads, or residue could not write its removal record (`could not write its removal record, so residue deleted nothing`). A worktree residue moved aside is back in place |
| `failed` | Git refused the removal, or a Git query during the re-check failed. If residue could not move a worktree back, `reason` names where it is and the one command that puts it back. If the removal failed after Git deleted part of the worktree, or its files differ from what the plan measured, it stays under its hidden name, its removal record stays, and `reason` ends with `its removal record keeps its HEAD and reflog: residue restore RECORD` |
| `invalid` | PATH is not a linked worktree residue may remove: the main checkout, a directory inside a worktree, not in a repository, in the trash, or left under a hidden name by an interrupted run |
| `not_attempted` | residue stopped before it reached this PATH |

For every result except `removed` and `trashed`, `reason` is a sentence that says why. With `--all`, `results` lists every linked worktree the scan found, and each one it kept is `refused`. `errors` lists the PATHs, repositories and trash records it could not read. `warnings` lists the removal records it could not read or forget, and a stats ledger it could not write, which it also prints on stderr as `residue: warning: ...`; they never change the exit status.

A remove document also has `trash`, `{"enabled": true, "keep": "7d", "keep_seconds": 604800}`, and `purged`: one object per trash entry the run purged or meant to purge, with `result` (`purged`, `kept` when a check failed or, with no change to the exit status, when the entry is on a filesystem with no flock(2), `skipped`, `failed`, `not_attempted`), `reason`, `entry` (the entry as `residue restore --json` lists it), `worktree` (its checks) and `removal_record` (the record a purge wrote, or null).

It also has `removal_records`, `{"reason": "remove", "by": "remove", "keep": "30d", "keep_seconds": 2592000}`: the reason the run's records name (`remove`, `sweep` for `--all`, `watch_landed`, `watch_low_disk`; a purge's record says `trash_purge`), the command they name as the one that removed the worktree (`by`: `remove` for `residue remove PATH`, `sweep` for `remove --all`, `watch` for the run `residue watch` starts; a purge's record names the run that purged it), and `--keep-removed`. And `forgotten`: one object per removal record older than `--keep-removed` that the run forgot or meant to, with `result` (`forgotten`, `skipped`, `kept`, with no change to the exit status, when the record is on a filesystem with no flock(2), `failed`, `not_attempted`), `reason` and `entry` (the record as `residue restore --json` lists it). Forgetting deletes the record and its refs in one transaction, only while each still names the commit the record names, and only while the run holds the record: one that a restore holds then is `skipped`, and the next run forgets it. A record it could not forget is `failed` here and a warning in `warnings`; it never changes the exit status. A directory with no `info` and no ref, left by a run stopped while it wrote one, is forgotten once it is an hour old. Once a record is forgotten, a commit only its refs held is left to `git gc`: [Restore a removed worktree](../README.md#restore-a-removed-worktree) says when that matters.

Its last field is `stats`, what the run freed and the total so far: [in a remove run](#in-a-remove-run).

`residue restore --json` prints `residue.restore/v1`:

- `restored`: the path a worktree was put back at, or null for a listing.
- `restore`: null for a listing; otherwise `{"kind", "entry", "path", "branch", "head", "checkout", "moved_from", "new_branch", "branch_tip", "not_kept", "notes"}`. `kind` is `trash` or `removed`. `checkout` is `moved_back` (a trash entry, or an interrupted removal, moved back from `moved_from`), `checked_out_branch` (the branch at the recorded HEAD or ahead of it; `branch_tip` is where), `recreated_branch` (the branch was gone), `new_branch` (the branch was rewritten; `new_branch` names `BRANCH-restored` at the recorded HEAD), `detached`, or `already_back` (a `back` record: Git already listed the worktree at its path; restore gave it what it lacked, and `branch_tip` is its HEAD). `not_kept` lists the ignored entries the deletion took, `{"path", "files", "bytes"}`. `notes` are the warnings it also printed on stderr; in a reftable repository, one says that the HEAD reflog of the worktree it recreated was not put back (`its HEAD reflog was not put back: ...`).
- `trash`: one object per entry with `kind` (`trash`), `entry` (its name), `entry_path`, `worktree_path`, `original_path`, `branch`, `head`, `trashed_at`, `allocated_bytes`, `reclaimable_bytes`, `registered` (Git still lists the worktree) and `error` (a record residue could not read).
- `removed`: one object per removal record with `kind` (`removed`), `entry`, `entry_path`, `ref` (`refs/residue/removed/ENTRY`), `state`: `removed` (the worktree is gone; restore recreates it), `interrupted` (the run did not finish and Git still lists the worktree at `aside_path`; part of it may be gone), `back` (Git lists a worktree at `original_path` again, on the recorded branch, or detached when the record is, at the recorded HEAD or ahead of it: a restore that kept the record, or was stopped before it marked the worktree, a `git worktree add` there by hand, or a run that moved it back and could not drop the record; restore of it without `--to` puts back what the worktree lacks of the record, its restored marker, `config.worktree` and HEAD reflog, and drops the record once the worktree holds what it keeps), `unknown` (a worktree is there on the recorded branch, but Git could not compare its HEAD with the recorded one, and `back_error` says why; restore of it without `--to` refuses), `incomplete` (no `info`, as `incomplete` says) or `unreadable` (`error` says why), `original_path`, `aside_path`, `branch`, `head`, `upstream`, `reason` and `by` (as in `removal_records`; `by` is null for a record written without it), `removed_at`, `keep` and `keep_seconds` (the `--keep-removed` the removal that wrote it ran with, such as `"12w"` and `7257600`; no run forgets the record before then; null for a record written by residue 0.0.7 or earlier), `allocated_bytes`, `reclaimable_bytes`, `not_kept`, `worktree_config` (its `config.worktree` was saved), `head_reflog` (its HEAD reflog was saved), `reflog_refs` (`[{"ref", "commit"}]`: the refs `refs/residue/reflog/ENTRY/N` that keep the commits only that reflog held), `incomplete` (true for a directory with no `info`, left by a run stopped while it wrote one; nothing was deleted, and `error` says so), `back_error` (why residue cannot tell whether the worktree is back, for `unknown`; null otherwise), `pid`, `residue_version` and `error`.
- `errors`.

After a restore, `trash` or `removed` holds only the entry it restored, as it was before the restore. A restore that fails prints no document: it exits 1 and says why on stderr. When it changed something before it stopped, such as a folder it made, files Git moved, a lock it lifted, or a worktree Git can no longer use, that message says what and where, and names what brings the worktree back, such as the `git worktree repair` that reconnects the files Git moved, or `residue restore RECORD --to NEWPATH`.

In a scan, each repository has `trash`, its entries in the same shape, `removed`, its removal records in the same shape, and `interrupted`: `{"path", "original_path", "pid", "pid_running", "removal_running", "present", "recovery", "unusable", "removal_record"}` for each worktree an interrupted run left under a hidden name. `removal_running` is true while a residue run, or the `git worktree remove` it started, holds the removal record that names it: that Git goes on after residue is killed alone, so the removal may still be deleting the worktree, `recovery` is empty and `residue restore` of the record refuses. It is also true for a record that residue 0.0.10 or earlier wrote, which takes no hold, while the process it names still runs, while a `git worktree remove` of the worktree runs, or while a process uses the worktree or its Git directory. It is false with no record, and null when residue could not tell, such as when it cannot open the record's `info` or read a record that may name the worktree; `recovery` is then empty too, and the text line says why. `recovery` is the command that puts it back: a `git worktree move`, after `test ! -e ORIGINAL && test ! -L ORIGINAL &&`, so that it fails and changes nothing while anything is at the original path, where Git would move the worktree into a directory. While something is there, the text line says what it is and gives no move back. When its directory is gone, `recovery` is the `git worktree remove` that clears Git's record of it if a removal record names it, since that record keeps its HEAD and reflog, and empty if none does, since that remove would then drop a HEAD and reflog that nothing else keeps. The text line names `git worktree repair` and mounting its disk first. Without a record it names no command that deletes: it says that `git gc` clears Git's record, with its HEAD, reflog and index, 3 months by default (`gc.worktreePruneExpire`) after its index last changed, never while it is locked, and names the `git reflog show worktrees/ID/HEAD` that lists the commits that may go with it. `unusable` is null, or why Git can no longer use the worktree (its `.git` file or its HEAD is gone), which makes Git refuse that move; then `recovery` is `residue restore RECORD`, which says what to clear and recreates it, or empty when no record names it. `recovery` never drops anything that no removal record keeps. `removal_record` is the path of the record that names it, or null. `summary` counts them, as `trash_entries`, `removal_records` and `interrupted_removals`. Each worktree in a scan, explain or remove document has `restored`: `{"unix", "at", "from"}` when `residue restore` put it back, or null, and `restored_error` when residue could not read that; `watch` never removes either kind because its branch landed.

Each repository in a scan also has `free_space`: `{"device", "free_bytes", "total_bytes", "free_inodes", "total_inodes"}` for the filesystem that holds its main checkout (its Git directory when the checkout cannot be read), or null when residue could read neither. `device` is the filesystem's device number (`st_dev`) as a decimal string, such as `"16777234"`, so a JavaScript reader gets it exactly. Repositories with the same `device` share one disk, so show one free-space figure per `device`, as `watch` does. It identifies a filesystem only within one document: it is not stable across reboots or mounts, so never store it or compare it across runs.

A worktree's `rule_sources` is the effective list; the top-level list is what applies to every worktree. Each entry is `{rule, scope, origin}`. A worktree's list starts with the top-level list, `--regenerable` (`command_line`) and then the residue config file (`residue_config`), and continues with its repository's Git config rules in the order Git reads them (`system`, `global`, `local`, `worktree`). Rules add up, so a rule listed twice applies once, and its first entry is the source that wins.

Every scan, explain and remove document has `idle`: `{"threshold": "2d", "threshold_seconds": 172800}`, or null without `--idle`. Each worktree has `last_change`: `{"unix", "at", "source", "what"}`, where `source` is `files`, `git_dir` or `reflog` and `what` is the entry (`src/main.rs`, `.` for the worktree directory itself), the Git directory file (`index`), or the reflog message (`commit: fix login`). It is null with `last_change_error` set when residue could not read it.

Every scan, explain and remove document has `ignored_environment_rules`: the `residue.regenerable` values the environment set and residue ignored, an empty list when there are none, or null when Git could not read that environment. To pass a rule for one run, use `--regenerable`, not `git -c`.

A scan is a snapshot of one moment, `generated_at`. Worktrees created after the scan started are missing from it. Scan again before you act on an old result. `remove` re-checks each path right before it deletes it, so an old scan cannot cause a removal that the current state would refuse.

## Stats

`residue stats` shows how much disk residue has freed on this machine: all time, the last 30 days and the last 7, each split by the command that freed it (`residue remove PATH`, a `remove --all` sweep, `watch`, or a line that does not say), the repositories that freed the most, what went into the trash and what is in it now, and how many worktrees were restored. It reads only its [ledger](#the-ledger) and, for what is in the trash now, the trash folders of the repositories the ledger names. It takes only `--json`, and exits 0, or 1 when it cannot read the ledger; a ledger that does not exist yet is an empty one.

Every size is an estimate: the `reclaimable` size residue measured before the removal, the number the removal record keeps. APFS snapshots, clones and hard links can keep some of the space. A move into the trash frees nothing until a remove run purges the entry, so it is counted on its own, and the purge counts as freed. A restore never subtracts from what was freed; it is counted on its own.

### The ledger

On macOS, `~/Library/Application Support/residue/ledger.jsonl`, whatever `XDG_STATE_HOME` says, so a terminal, the launchd agent and an app find the same file. Elsewhere, `$XDG_STATE_HOME/residue/ledger.jsonl` when `XDG_STATE_HOME` is an absolute path, and `~/.local/state/residue/ledger.jsonl` otherwise. residue creates the folder with mode 0700 and the file with mode 0600. There is one per user, and nothing in it leaves the machine.

Each line is one JSON object, one event:

| Key | Value |
| --- | --- |
| `v` | `1`, the line format |
| `at` | Unix seconds: when the run wrote the line, right after the deletion, move or restore; on a seeded line, the time the record or trash entry names. A reader keeps a time outside 1970 to 9999 at the nearest end |
| `event` | `freed`: a deletion (`remove --no-trash`, `watch`, a sweep with `--no-trash`, or the trash's purge). `trashed`: a move into the trash. `restored`: `residue restore` put a worktree back |
| `by` | `remove`, `sweep` or `watch`, as the removal record names the command. Absent on a `restored` line, and on a `trashed` line taken from a trash entry |
| `reason` | As the record's `reason`: `remove`, `sweep`, `watch_landed`, `watch_low_disk` or `trash_purge` |
| `from` | On a `restored` line: `trash`, `removed` or `interrupted` |
| `bytes` | The reclaimable size. Absent when unknown: the line counts as a worktree and 0 bytes |
| `repo` | The repository's Git common directory |
| `worktree` | The worktree's directory name. Dropped from a line that would otherwise be 512 bytes or longer |
| `entry` | The removal record or trash entry, `NAME-STAMP` |
| `seeded` | `true` on a line taken from a removal record or a trash entry, not written by the run that removed it |

When it is written:

- `remove` appends a line for each worktree it deleted, purged or moved into the trash, once `git worktree remove` or the move succeeded, and nothing for a preview or a run nobody confirmed.
- `restore` appends a `restored` line.
- Seeding: before it acts, a `remove` that will delete or move something, `restore`, and `scan` without `--json` add a `seeded` line for each removal record and trash entry of the repositories they read that the ledger lacks. Not added: a record that `--keep-removed` forgot or a restore took back, removals by residue 0.0.6 or earlier (which wrote no record), an interrupted removal (nothing was deleted), a record whose run may still be going (its process is running and the record is under an hour old), a trash entry whose worktree is gone, and repositories none of those commands reads again. `scan --json`, `explain`, `watch` itself and `stats` never write the ledger; `watch`'s `remove` child does.

Each line is appended with one `write(2)` to the file opened with `O_APPEND`, so runs at the same time (a `watch` child and a terminal) append whole lines and never overwrite each other on a local filesystem; NFS does not promise that. Each event counts once, by `event`, `repo` and `entry`: a removal its run recorded and a later run seeded is one removal. A line residue cannot read, such as one cut short by a crash or a full disk, is skipped and counted (`skipped_lines`); when the file does not end in a newline, the next append starts with one, so only the cut line is lost.

The ledger never decides anything. residue reads and writes it only as a regular file (or a symbolic link to one), opened without waiting: a FIFO, a device or a folder at its path is a ledger it cannot read or write. A ledger residue cannot read or write never changes a removal or a restore: `remove` prints `residue: warning: the stats ledger is not up to date (...); this does not change what was removed` on stderr, lists it in `warnings` and `stats.error`, and exits as it would have. `restore` adds it to its `notes`, or, when it only lists, prints it as a warning. To start over, delete the file; the next run that reads a repository adds its removal records and trash entries again.

### In a remove run

A run that deleted or moved something ends with one line: `freed about 812 MB (about 14.2 GB on this machine since 2026-10-02)`, or `moved about 812 MB to the trash, freed when a remove run purges it after 7d (...)`, or both joined by `and`. The date is local. Its `remove --json` document ends with `stats`:

| Field | Value |
| --- | --- |
| `freed_bytes`, `freed_worktrees` | What this run deleted and purged |
| `trashed_bytes`, `trashed_worktrees` | What this run moved into the trash |
| `total_freed_bytes`, `total_freed_worktrees` | Everything freed in the ledger after this run, or null when the run freed and moved nothing or residue could not read or write the ledger |
| `since` | The ledger's earliest line, as an RFC 3339 time, or null as above |
| `ledger` | The ledger's path, or null when the run wrote nothing |
| `error` | Why the ledger is not up to date, which is also in `warnings`, or null |

A preview has `stats` with zeros and nulls. The `watch` log names what each removal freed: `residue watch: freed about 812 MB (about 14.2 GB on this machine since 2026-10-02)`, with the UTC date, as the log's own times are UTC. `scan` at a terminal ends with `freed about 14.2 GB from 31 worktrees since 2026-10-02; 2.1 GB in the last 7 days · residue stats` once anything was freed. On macOS and Linux, `residue service status` shows a `freed:` line for automatic cleanup: what `watch` freed in the last seven days and when it last removed a worktree.

### `residue stats --json`

`residue.stats/v1`:

- `schema`, `version`, `generated_at`, and `ledger`, the ledger's path.
- `since`: the ledger's earliest line, or null for an empty ledger.
- `estimate_note`: the sentence the text report ends with, that every size is an estimate and why, for a client to show as it is.
- `freed`: `all_time`, `last_30_days` and `last_7_days`, each `{"bytes", "worktrees", "by"}`, where `by` is `{"remove", "sweep", "watch", "other"}`, each `{"bytes", "worktrees"}`; `other` counts lines with no `by`. The windows end at `generated_at`.
- `weeks`: twelve `{"start", "end", "bytes", "worktrees", "by"}`, oldest first, seven days each, the last ending at `generated_at`, with `by` as in `freed`. A line dated later, from a clock that has since moved back, counts in the last.
- `repositories`: every repository with a `freed` line, most bytes first: `{"path", "common_dir", "bytes", "worktrees", "by", "last_freed_at"}`, with `by` as in `freed`. `path` is the folder that holds `.git`, or the Git directory itself for a bare repository.
- `trashed`: `{"bytes", "worktrees"}` moved into the trash, all time.
- `trash_now`: `{"entries", "bytes", "repositories"}`, the trash entries that hold a worktree now, read from the trash folders of the repositories the ledger names.
- `restored`: `{"worktrees", "from_trash", "from_removal_records"}`.
- `last_freed_at`: `{"all", "remove", "sweep", "watch", "other"}`, each a time or null.
- `skipped_lines`: lines it could not read.
- `errors`: trash folders it could not list.
