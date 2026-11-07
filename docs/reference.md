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

## Exit status

| Code | Meaning |
| --- | --- |
| 0 | Success. For `remove`, every PATH was removed or moved to the trash and every expired trash entry was purged. For `remove --all`, the same for every candidate, or there was none. A removal record `remove` could not read or forget never changes the status: it is a warning on stderr and in `errors`. For `restore`, the worktree is back, or the trash and the removal records were listed; an entry it could not read is listed with its `error` |
| 1 | Error: git missing or too old, unreadable config, unknown PATH for `explain`, an ENTRY that `restore` cannot find or put back (its branch checked out elsewhere, something at its path, more than one entry by that name), an ENTRY path that is gone where the nearest folder that exists is the home folder or above it, or a repository, trash or removal-record folder `restore` could not list |
| 2 | Usage error, including an invalid `--idle`, `--keep-trash` or `--keep-removed` DURATION, `--trash` with `--no-trash`, `remove --all --yes` without `--idle`, `--only-if-upstream-gone` with `--all` or outside `remove`, `--keep-removed` or `--removal-reason` outside `remove`, a `--removal-reason` other than `watch-landed` or `watch-low-disk`, and `--to` outside `restore` or without an ENTRY |
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
| `failed` | Git refused the removal, or a Git query during the re-check failed. If residue could not move a worktree back, `reason` names where it is and the one command that puts it back |
| `invalid` | PATH is not a linked worktree residue may remove: the main checkout, a directory inside a worktree, not in a repository, in the trash, or left under a hidden name by an interrupted run |
| `not_attempted` | residue stopped before it reached this PATH |

For every result except `removed` and `trashed`, `reason` is a sentence that says why. With `--all`, `results` lists every linked worktree the scan found, and each one it kept is `refused`. `errors` lists the PATHs, repositories and trash records it could not read, and the removal records it could not read or forget, which it also prints on stderr as `residue: warning: ...`.

A remove document also has `trash`, `{"enabled": true, "keep": "7d", "keep_seconds": 604800}`, and `purged`: one object per trash entry the run purged or meant to purge, with `result` (`purged`, `kept` when a check failed, `skipped`, `failed`, `not_attempted`), `reason`, `entry` (the entry as `residue restore --json` lists it), `worktree` (its checks) and `removal_record` (the record a purge wrote, or null).

It also has `removal_records`, `{"reason": "remove", "by": "remove", "keep": "30d", "keep_seconds": 2592000}`: the reason the run's records name (`remove`, `sweep` for `--all`, `watch_landed`, `watch_low_disk`; a purge's record says `trash_purge`), the command they name as the one that removed the worktree (`by`: `remove` for `residue remove PATH`, `sweep` for `remove --all`, `watch` for the run `residue watch` starts; a purge's record names the run that purged it), and `--keep-removed`. And `forgotten`: one object per removal record older than `--keep-removed` that the run forgot or meant to, with `result` (`forgotten`, `skipped`, `failed`, `not_attempted`), `reason` and `entry` (the record as `residue restore --json` lists it). Forgetting deletes the record and its refs in one transaction, only while each still names the commit the record names. A record it could not forget is `failed` here and a warning in `errors`; it never changes the exit status. A directory with no `info` and no ref, left by a run stopped while it wrote one, is forgotten once it is an hour old. Once a record is forgotten, a commit only its refs held is left to `git gc`: [Restore a removed worktree](../README.md#restore-a-removed-worktree) says when that matters.

`residue restore --json` prints `residue.restore/v1`:

- `restored`: the path a worktree was put back at, or null for a listing.
- `restore`: null for a listing; otherwise `{"kind", "entry", "path", "branch", "head", "checkout", "moved_from", "new_branch", "branch_tip", "not_kept", "notes"}`. `kind` is `trash` or `removed`. `checkout` is `moved_back` (a trash entry, or a removal that stopped before deleting, moved back from `moved_from`), `checked_out_branch` (the branch at the recorded HEAD or ahead of it; `branch_tip` is where), `recreated_branch` (the branch was gone), `new_branch` (the branch was rewritten; `new_branch` names `BRANCH-restored` at the recorded HEAD) or `detached`. `not_kept` lists the ignored entries the deletion took, `{"path", "files", "bytes"}`. `notes` are the warnings it also printed on stderr.
- `trash`: one object per entry with `kind` (`trash`), `entry` (its name), `entry_path`, `worktree_path`, `original_path`, `branch`, `head`, `trashed_at`, `allocated_bytes`, `reclaimable_bytes`, `registered` (Git still lists the worktree) and `error` (a record residue could not read).
- `removed`: one object per removal record with `kind` (`removed`), `entry`, `entry_path`, `ref` (`refs/residue/removed/ENTRY`), `state` (`removed`, or `interrupted` when the run stopped before deleting and the worktree is still at `aside_path`), `original_path`, `aside_path`, `branch`, `head`, `upstream`, `reason` and `by` (as in `removal_records`; `by` is null for a record written without it), `removed_at`, `allocated_bytes`, `reclaimable_bytes`, `not_kept`, `worktree_config` (its `config.worktree` was saved), `head_reflog` (its HEAD reflog was saved), `reflog_refs` (`[{"ref", "commit"}]`: the refs `refs/residue/reflog/ENTRY/N` that keep the commits only that reflog held), `incomplete` (true for a directory with no `info`, left by a run stopped while it wrote one; nothing was deleted, and `error` says so), `pid`, `residue_version` and `error`.
- `errors`.

After a restore, `trash` or `removed` holds only the entry it restored, as it was before the restore.

In a scan, each repository has `trash`, its entries in the same shape, `removed`, its removal records in the same shape, and `interrupted`: `{"path", "original_path", "pid", "pid_running", "present", "recovery"}` for each worktree an interrupted run left under a hidden name, where `recovery` is the command that puts it back. `summary` counts them, as `trash_entries`, `removal_records` and `interrupted_removals`. Each worktree in a scan, explain or remove document has `restored`: `{"unix", "at", "from"}` when `residue restore` put it back, or null, and `restored_error` when residue could not read that; `watch` never removes either kind because its branch landed.

A worktree's `rule_sources` is the effective list; the top-level list is what applies to every worktree. Each entry is `{rule, scope, origin}`. A worktree's list starts with the top-level list, `--regenerable` (`command_line`) and then the residue config file (`residue_config`), and continues with its repository's Git config rules in the order Git reads them (`system`, `global`, `local`, `worktree`). Rules add up, so a rule listed twice applies once, and its first entry is the source that wins.

Every scan, explain and remove document has `idle`: `{"threshold": "2d", "threshold_seconds": 172800}`, or null without `--idle`. Each worktree has `last_change`: `{"unix", "at", "source", "what"}`, where `source` is `files`, `git_dir` or `reflog` and `what` is the entry (`src/main.rs`, `.` for the worktree directory itself), the Git directory file (`index`), or the reflog message (`commit: fix login`). It is null with `last_change_error` set when residue could not read it.

Every scan, explain and remove document has `ignored_environment_rules`: the `residue.regenerable` values the environment set and residue ignored, an empty list when there are none, or null when Git could not read that environment. To pass a rule for one run, use `--regenerable`, not `git -c`.

A scan is a snapshot of one moment, `generated_at`. Worktrees created after the scan started are missing from it. Scan again before you act on an old result. `remove` re-checks each path right before it deletes it, so an old scan cannot cause a removal that the current state would refuse.
