---
name: verify-removal-races
description: Model-check every order of steps of `residue remove`, a trash purge, both kinds of `residue restore`, Git's own `git worktree remove` and the user, to show whether any of them can lose a worktree, a commit or a lock. Use before you merge a change to src/remove.rs, src/trash.rs, src/removed.rs or src/restore.rs, to the worktree move, lock, unlock or remove calls in src/git.rs, or to how src/run.rs and src/watch.rs stop a remove run, and when someone asks whether a removal, purge or restore can lose work.
metadata:
  internal: true
---

# Verify the removal races

`residue remove` checks a worktree, moves it aside, writes a removal record, checks it again and lets Git delete it. A purge, a restore and the user can act while that runs, and Git deletes in steps. Tests try the orders someone thought of. This check tries every order of those steps, for one worktree, with at most one kill or Git timeout, and says which of six rules any order breaks.

It is a TLA+ model of the code, `model/Removal.tla`, checked with TLC. TLC does not read the Rust: the model holds only while it describes the code, which is why the coverage table and the sync commit below exist.

## Run it

```sh
bash .agents/skills/verify-removal-races/check.sh
```

It takes about two minutes and exits 0 when the result matches `model/expected`: every rule holds except any known break it lists, each with its shortest path, and every mutant is caught. At the sync commit it lists none. It exits 1 when the result differs, and 2 when the tools cannot be set up or TLC fails.

The first run downloads TLC and a Java runtime into `~/.cache/residue-tla` (or `$RESIDUE_TLA_TOOLS`) and checks each against its pinned SHA-256. It needs bash, curl, tar, and `shasum` or `sha256sum`. To use your own, set `TLA2TOOLS` to a tla2tools.jar and `JAVA` to a java binary (11 or later); the run says when either is not the pinned one.

| Tool | Version | SHA-256 |
| --- | --- | --- |
| [tla2tools.jar](https://github.com/tlaplus/tlaplus/releases/tag/v1.7.4) | v1.7.4 (TLC 2.19) | `936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88` |
| [Temurin JRE](https://github.com/adoptium/temurin25-binaries/releases/tag/jdk-25.0.5%2B8), macOS arm64 | 25.0.5+8 | `72ebd76b502c849e88dea39afae51baf5f82f2b5b1b6389c28ea707b20451bce` |
| Temurin JRE, macOS x64 | 25.0.5+8 | `806902c796c670969b7b27df73b8f0fe0924a61bd65083aa53064115e49cab2d` |
| Temurin JRE, Linux x64 | 25.0.5+8 | `6fac98f0c8202efc3135a9e9e42aa61963ec42d1c5cbb40caf231e7b14a24ef6` |
| Temurin JRE, Linux arm64 | 25.0.5+8 | `963931723891302ebb2687178532c3d793d71e2577cd058061c7efb396140a86` |

`.github/workflows/model.yml` runs it on Linux when a pull request changes this skill. It is not a required check. It does not run for a change to the Rust alone, because TLC would check the same model and pass: the check catches a bug in a code change only once that change updates the model.

## The rules

Each rule is an invariant in `model/Removal.tla`. TLC checks it in every state every order reaches.

| Rule | What it says | The harm when it breaks |
| --- | --- | --- |
| `NotDeletedOnceBack` | Nothing of a worktree is deleted while it is at its own path. | A worktree the user got back loses files or its Git directory. |
| `OnlyCheckedStateDeleted` | Git deletes a worktree's files only after residue's last check passed on it, at the path Git deletes, unchanged and idle since. | Work made after the check is deleted. |
| `ReflogCommitsKept` | A commit only the worktree's HEAD reflog holds stays held, by its Git directory or by a removal record's refs (AGENTS.md: "no removal, restore or forget by residue makes unreachable a commit the worktree's Git directory kept reachable"). | `git gc` deletes a commit no branch holds. |
| `RestoreTellsTheTruth` | A restore that says the worktree is back leaves Git listing it at its path with its Git directory, and whole unless it says part may be gone. | The user trusts a worktree Git cannot use, or one with files missing. |
| `OnlyOwnLocksLifted` | residue unlocks only a lock it set itself (AGENTS.md: "`git worktree unlock` only for a lock residue set itself"). | The user's `git worktree lock` is gone, and a later run may remove the worktree. |
| `RecordDroppedOnlyWhenWhole` | A remove run or purge that did not delete the worktree drops its removal record only while nothing of it is gone (README: "residue drops the record only while nothing of the worktree is gone"). A restore drops it once the worktree is back and holds what the record keeps. | Part of a worktree is gone with no record to restore it from. |

`TypeOK` checks that every variable keeps its type.

## What it checks, and the bounds

One repository, one worktree W, and these actors, each step one action of the model:

- `rm`: `residue remove --no-trash` on W at its path.
- `pg`: a remove run purging W's expired trash entry.
- `tr`: `residue restore ENTRY` on that trash entry.
- `rr`: `residue restore RECORD` on a removal record.
- Git's `git worktree remove`, as the four steps of `remove_worktree` in Git 2.50's `builtin/worktree.c`: look the worktree up, `git status` in it, delete its files by path, delete its Git directory by id.
- The user: edits W, starts a process in it, or runs `git worktree lock` on it, once each.
- The holds (`src/hold.rs`): the purge holds the trash entry's `info`, each writer holds its removal record's `info` from the moment it is in place, and each `git worktree remove` residue starts inherits its run's holds. A restore takes the hold first and refuses while another process has it; so does a purge.
- In the "old" and "old-orphan" setups, the remove run or the purge is one of a release that takes no hold, 0.0.10 and earlier (`Old`): it holds nothing, its Git inherits nothing, and its record has no `hold` line. While Git still lists the worktree where that record's deletion started, a restore refuses while the PID the record names runs, or while a `git worktree remove` of the worktree does, found by its arguments (`removed::unheld`). Its other steps are this release's. A trash restore keeps such a purge off the worktree three ways: from just after it takes the entry's hold it keeps the worktree's `.git` open, which that purge's re-check sees as a use (`TOpen`); when Git lists the worktree unlocked, it locks it with residue's trash lock before it reads the records, and lifts that lock if it then refuses (`TLock`, `TLift`); and once the worktree has moved, it reads the records again and names a purge's record that came meanwhile (`TForget`).

The faults: a kill of residue with its Git child (Ctrl-C), the 30-minute Git timeout, or an orphaning kill of residue alone, which leaves its Git child deleting. A kill during Git's delete of its own directory leaves it whole, without its `logs/`, or without HEAD or commondir too, by readdir order. Git 2.50.1 moves a worktree whose Git directory lacks `logs/` or `index`, and refuses one that lacks HEAD, commondir or gitdir: probed on 2026-11-09 on macOS by deleting each by hand from a scratch worktree's Git directory before `git worktree move`.

| Setup | Actors | Faults, at most one | Distinct states |
| --- | --- | --- | --- |
| Remove no-faults | rm, rr, user | none | 2,638 |
| Remove faults | rm, rr, user | kill, timeout | 8,044 |
| Remove orphan | rm, rr, user | orphaning kill | 3,722 |
| Remove unraced | rm, user | kill, orphaning kill | 743 |
| Remove after | rm, then rr, user | kill, timeout | 3,479 |
| Trash no-faults | pg, tr, rr, user | none | 52,993 |
| Trash faults | pg, tr, rr, user | kill, timeout | 260,428 |
| Trash orphan | pg, tr, rr, user | orphaning kill | 89,567 |
| Trash unraced | pg, rr, user | kill, orphaning kill | 11,112 |
| Trash after | pg, then tr and rr, user | kill, timeout | 54,202 |
| Remove old | rm of a release that takes no hold, rr, user | kill, timeout | 10,591 |
| Trash old | pg of a release that takes no hold, tr, rr, user | kill, timeout | 496,086 |
| Remove old-orphan | rm of a release that takes no hold, rr, user | orphaning kill | 5,312 |
| Trash old-orphan | pg of a release that takes no hold, tr, rr, user | orphaning kill | 215,556 |

TLC stops at a rule's first break and reports the shortest path, so check.sh checks the other rules again without it. A rule that breaks in a setup is therefore not checked further there: a new way to break it shows only if it is shorter. Two kinds of setup leave the races out, so a rule a race breaks is still checked in the rest. Unraced switches off the restore and the Git timeout. After lets a restore start only once the remove run or purge has stopped, the usual way to recover. Old and old-orphan check what a restore of this release can tell of a run that takes no hold: its record's PID, and a `git worktree remove` of the worktree. In old-orphan that run may be killed alone, so its Git goes on with no hold to show it. In "Trash old" and "Trash old-orphan" the trash restore races that purge throughout, except in the narrow window `LatePurge` names (`Late`); "Not covered" lists what that leaves out. At the sync commit every rule holds in all fourteen.

Assumptions:

- Each Git command, file system call, or read the code makes before it acts is one indivisible step. The time between residue's last check and Git's deletion is the window the README documents; an edit there is one Git's `git status` may not show (an ignored file).
- The user never unlocks residue's own locks, `residue: removing` and `residue: in trash`. Git locks only an unlocked worktree, so while a run holds a worktree residue has locked, only residue changes its lock; that is why a restore that reads the lock under its hold lifts only residue's.
- A hold is `flock(2)` on an open `info`, which the kernel frees once every process that has the file open has exited, however they exit, on one machine. residue runs Git with no fsmonitor daemon and no hooks (`git.rs` `command`), but a program Git starts, such as its `git status` or a filter driver, inherits the holds too and may outlive Git. The model ends Git's hold when Git exits: a program that keeps it longer only makes a restore refuse longer, which breaks no rule.
- Outside the old setups, every run is this version. In them, the restores are this version's.
- One remove run or one purge in each setup, never two; one edit, one process and one lock by the user.

## When a change touches the verified code

Run the check for a change to any function in this table, or for one that adds a step that moves, locks, unlocks or deletes a worktree, or writes or drops a removal record or a trash entry.

| Model actions | Code |
| --- | --- |
| `RRecheck` | `remove.rs` `recheck(original)` |
| `RMove`, `RLock`, `RRecord`, `RUnlock`, `RAfter` | `remove.rs` `execute`: `git::move_worktree` to `.NAME.residue-removing-PID`, `git::lock_worktree(REMOVING)`, `removed::create`, `git::unlock_worktree`, `git::remove_worktree` with the record's hold, and the match on its result |
| `RRecord`, `PRecord` (the hold) | `removed.rs` `write_entry`, `hold.rs` `write_new`: `info.new` is held before its rename |
| `RVerify` | `remove.rs` `verify_moved` |
| `PBCheck`, `PBUnlock`, `PBMove`, `PBStuck` | `remove.rs` `Moved::put_back`, `Moved::stuck` |
| `RSettle`, `PSettle` | `remove.rs` `settle`, `standing`, `Files` |
| `PHold`, `PRecord`, `PRecheck`, `PUndo`, `PAfter`, `PRelock` | `remove.rs` `purge`: `hold::take(trash::info)`, `removed::create`, `recheck(path, ours = trash::LOCK)`, and `settle` with `standing` and no `Files` when the re-check fails; after `git::remove_worktree`, `trash::discard`, whose result does not change the outcome once Git deleted the worktree. A purge in Old re-checks before it writes its record, as 0.0.10 did (`PFirst`). Its step between the hold and the record, `removed::settle_before_purge` over `stopped_purges`, acts only on a record an earlier purge left, which no setup has |
| `PUnlock` | `trash.rs` `purge(item, registered, holds)` |
| `StartGit` | `git.rs` `remove_worktree(main, path, holds)`, `hold.rs` `pass_to` |
| `GFind`, `GExists`, `GStatus`, `GTree`, `GGitdir` | Git's `remove_worktree`, run by `git.rs` `remove_worktree` |
| `GitTimeout` | `git.rs` `REMOVE_TIMEOUT` and `run.rs` `stop` |
| `Kill`, `Orphan` | No code: residue stopped with its Git child, or alone (`run.rs` `stop` on watch's 30-minute deadline for its `residue remove` kills that PID alone); the kernel frees a hold once every process that has it has exited |
| `TList`, `SList` | `restore.rs` `gather`, `choose` |
| `THold`, `TOpen`, `TLock`, `TRead`, `TLift` | `trash.rs` `take`; `restore.rs` `restore_trash`: `keep_open`, `git::worktrees`, `git::lock_worktree(trash::LOCK)` when Git lists it unlocked, `lift`, `removed::stopped_purges` and its `refuse_unheld`: `unheld`, `process::removing`, `process::started` |
| `TCheck`, `TUnlock`, `TMove`, `TRelock` | `restore.rs` `restore_trash`'s refusal when a stopped purge left no files or files Git cannot use (`holds_worktree`, `remove::unusable`); `trash.rs` `restore(item, to, listed, held)`: `holds_worktree`, `was_locked` |
| `TDiscard` | `trash.rs` `discard` |
| `TForget` | `removed.rs` `after_trash_restore`, `finish`; `restore.rs` `settle_trash`: `removed::raced_purges` after the move |
| `SHold`, `SRead` | `removed.rs` `take`; `restore`: `git::worktrees`, `refuse_unheld` when Git lists the record's aside (`unheld`, `process::removing`, `process::started`), `Item::interrupted`, `is_back` |
| `SDest`, `SAfter` | `removed.rs` `restore`, `finish`, `reflog_if_lost`, `holds`, `forget` |
| `SCopies` | `removed.rs` `read_copies` |
| `SMoveBack`, `SUnlock`, `SMove`, `SRelock` | `removed.rs` `restore`'s `purged_to_git_dir`, `move_back`; `remove.rs` `Leftover::of` |
| `SClear` | `removed.rs` `clear_purged`: the entry's hold, `trash::locked_by_residue`, `git::unlock_worktree`, `git::remove_worktree`, `trash::discard` |
| `SRecreate` | `removed.rs` `recreate` |
| `TReport`, `SReport` | `restore.rs` `command`, the `restored ...` line and its notes, `removed::PURGE_STOPPED`, also for a purge's record whose worktree is already back (`removed.rs` `restore`) |

Sync commit: `6394300`, the commit that fixed #135 to #139 (#147). The model was last confirmed to describe the code there, on 2026-11-09. Move it forward only after you confirm, for each covered function a change touched, that the model still describes it. `git log --format='%h %s' 6394300.. -- src/hold.rs src/process.rs src/remove.rs src/trash.rs src/removed.rs src/restore.rs src/git.rs src/run.rs src/watch.rs` lists the changes; read each one after that fix.

## Read a result

Read every result with `/statechart`: give it `model/Removal.tla`, one of the configs (`model/Remove.cfg`, `model/RemoveOrphan.cfg`, `model/Trash.cfg`), and the view `model/view.mjs`. It tells each break as numbered steps in plain words, such as "restore moves it back to its path", with the worktree's state after each, and draws a chart of a setup that passes. For a mutant or another setup, copy a config and set `Broken`, `Faults`, `Off`, `After`, `Old` or `Late` as in that line of check.sh or `model/mutants`.

In check.sh's output:

- `+` against `model/expected` is a new break, or a known one with a new shortest path.
- `-` alone is a known break that no longer happens: a fix, or a model that lost the path.
- `mutant NAME: NOT caught` means the model no longer depends on that guard of the code.
- `no control` means the mutant's rule already breaks in its setup without the mutant, so the line shows nothing.

Each mutant in `model/mutants` removes one guard the code has, to show that a rule depends on it. `A+B` removes two, for a guard a rule needs only once another is gone:

| Mutant | The guard it removes | Rule that breaks |
| --- | --- | --- |
| `NoVerify` | `verify_moved` before the unlock | `OnlyCheckedStateDeleted` |
| `NoFingerprint` | the fingerprint comparison in `verify_moved` | `OnlyCheckedStateDeleted` |
| `PurgeNoRecheck` | `recheck` in `remove.rs` `purge` | `OnlyCheckedStateDeleted` |
| `NoRecord` | no deletion without the record and its refs | `ReflogCommitsKept` |
| `SettleAlwaysUndoes` | `standing` in `settle`: the record goes only while the worktree is whole | `ReflogCommitsKept`, `RecordDroppedOnlyWhenWhole` |
| `StandingIgnoresFiles` | `standing`'s comparison of the files with the plan's once Git ran (`Files`) | `RecordDroppedOnlyWhenWhole` |
| `MoveBackNoNote` | `move_back`'s "part of it is gone" note | `RestoreTellsTheTruth` |
| `PutBackAlwaysUnlocks` | `Moved::put_back(locked)`: unlock only a lock residue set | `OnlyOwnLocksLifted` |
| `PurgeNoHold+TrashRestoreNoUse` | the purge's `hold::take` on the entry, and the trash restore's open `.git` | `NotDeletedOnceBack` |
| `TrashRestoreNoHold+TrashRestoreNoUse` | `trash::take` in `restore_trash`, and its open `.git` | `NotDeletedOnceBack` |
| `RestoreNoHold` | `removed.rs` `take` in `restore` | `NotDeletedOnceBack` |
| `GitNoHold` | `hold::pass_to` in `git::remove_worktree`: Git inherits the holds | `NotDeletedOnceBack` |
| `TrashRestoreStale` | `trash::restore` reading the lock from Git's list under the hold, not the listing's | `OnlyOwnLocksLifted` |
| `RecordRestoreStale` | `removed::restore` reading Git's list under the hold, not the listing's | `OnlyOwnLocksLifted` |
| `TrashRestoreNoNote` | `restore_trash`'s `PURGE_STOPPED` note | `RestoreTellsTheTruth` |
| `TrashForgetsWithoutCheck` | `after_trash_restore`: put back the HEAD reflog, and forget only once the worktree holds what the record keeps | `ReflogCommitsKept` |
| `ClearAnyLock` | `clear_purged` lifting only residue's trash lock | `OnlyOwnLocksLifted` |
| `TrashRestoreNoOldPid` | `stopped_purges`' refusal while the run that wrote a record without a hold runs (`unheld`, `Unheld::Writer`), in the "Trash old" setup | `NotDeletedOnceBack` |
| `RestoreNoOldPid` | `restore`'s refusal while the run that wrote a record without a hold runs (`unheld`, `Unheld::Writer`), in the "Trash old" and "Remove old" setups | `NotDeletedOnceBack` |
| `TrashRestoreNoOldGit` | `stopped_purges`' refusal while a `git worktree remove` of the worktree runs for a record without a hold (`unheld`, `Unheld::Git`), in the "Trash old-orphan" setup | `NotDeletedOnceBack` |
| `RestoreNoOldGit` | `restore`'s refusal while a `git worktree remove` of the worktree runs for a record without a hold (`unheld`, `Unheld::Git`), in the "Remove old-orphan" and "Trash old-orphan" setups | `NotDeletedOnceBack` |
| `TrashRestoreNoUse` | `restore_trash`'s `keep_open`: the worktree's `.git` open from just after the hold, which the re-check of a purge that takes no hold sees as a use, in the "Trash old" setup | `NotDeletedOnceBack` |
| `TrashRestoreNoLock` | `restore_trash`'s lock of a worktree Git lists unlocked, before it reads the records, in the "Trash old" setup | `NotDeletedOnceBack` |
| `TrashRestoreNoReread` | `settle_trash`'s second read of the records once the worktree has moved (`raced_purges`), and its `PURGE_STOPPED` note, in the "Trash old" setup | `RestoreTellsTheTruth` |
| `BackPurgeNoNote` | `removed.rs` `restore`'s `PURGE_STOPPED` note for a purge's record whose worktree is already back, in the "Trash old" setup | `RestoreTellsTheTruth` |

Both refusals apply only while Git still lists the worktree where the record's deletion started. Once it lists it nowhere, that deletion is over, so a restore of a finished removal no longer refuses for a process that reused the record's PID; dropping that condition breaks no rule, it only makes a restore refuse longer. The model also leaves out the activity probe's part (`Unheld::Using`), a process seen using the worktree or its Git directory, which refuses too.

## A bug, or a modeling error

Replay the path step by step against the code. For each action, open its function in the table and check that the code can take that step in that state. A step the code cannot take is a modeling error: fix the model, and say in the change what was wrong. When every step can happen, it is a bug in the code: report it with the steps in the code's words, and the harm in one sentence.

To witness one, build a scratch repository with a temporary `HOME` and run the release binary, never against real repositories or your own `HOME`. Most windows are milliseconds wide; a worktree with 150,000 ignored files under `node_modules/` keeps `git worktree remove` running for seconds. Kill only processes you started.

## Change the model with the code

1. Update the actions for each covered function you changed, from the code, before you rerun. Keep one indivisible step of the code one action.
2. A new guard gets a `Mut("NAME")` switch in the model and a line in `model/mutants`. check.sh then shows that the rule holds with the guard and breaks without it.
3. A fix for a finding removes its lines from `model/expected`, with the reason, in the same change. The rule must then hold in that setup.
4. Never add a line to `model/expected`, or weaken a rule, to make the check pass. A new break is a finding until a replay against the code shows it is a modeling error.
5. Run the check, follow it with `/statechart` for each new or changed line, and move the sync commit.

## Findings, and their fixes

The check found these at `3356770`, the sync commit when the model came in (#134), and each had its lines in `model/expected` until the change that fixed it (#135 to #139) removed them. The replays ran the release binary of residue 0.0.10, and then that of the fix, on macOS 26.3.1 (arm64) with Git 2.50.1, in scratch repositories with a temporary `HOME`.

**1. A restore of a trash entry during its purge deletes the worktree's Git directory and its record.** No fault needed.
1. A remove run purges an expired trash entry: `remove.rs` `purge` rechecks it, `removed::create` writes its record, `trash.rs` `purge` runs `git worktree unlock`, and `git worktree remove TRASH/NAME` starts.
2. `residue restore ENTRY` lists the entry while it is unlocked. `trash.rs` `restore` reads `was_locked = false` from the listing, so it does not unlock, and runs `git worktree move TRASH/NAME ORIGINAL`.
3. Git finds nothing at the old path, or its delete gets ENOENT and counts the files deleted, and deletes `.git/worktrees/ID` by id, then exits 0.
4. `restore_trash` forgets the purge's record and its refs, and prints `restored ... from the trash entry ...`.

Harm: the worktree is back with most of its files, but Git no longer knows it, and a commit only its HEAD reflog held is unreachable. Replayed on the release binary (0.0.10) on macOS: restore exited 0; `git worktree list` showed only the main worktree; `git status` in it said `fatal: not a git repository`; 28,936 of 30,000 `node_modules` files remained; the record and its refs were gone; `git gc --prune=now` deleted the reflog-only commit. The purge failed with exit 3 and pointed at the record restore had just forgotten. Also, `removed.rs` `move_back`'s refusal for a trash aside tells the user to run `residue restore ENTRY`, this racing command.

Fixed (#135): the purge holds the entry's `info` from before its re-check, and its Git inherits the hold; `residue restore ENTRY` takes it first and refuses while anyone has it. Replayed: the restore exited 1 with ``a residue run, or the `git worktree remove` it started, is purging or restoring this trash entry now; run this restore again once it ends``, the purge finished and printed its record's restore command, that restore recreated the worktree, and the reflog-only commit survived `git gc --prune=now`.

**2. A restore can lift the user's lock.** No fault needed. `trash.rs` `restore` (`was_locked`) and `removed.rs` `move_back` (`ours`) decide to unlock from the listing's lock reason. The user runs `git worktree lock` after the listing; Git's `worktree unlock` lifts any reason, so it lifts theirs. The window is between the listing and the unlock, milliseconds. Git has no compare-and-unlock. Model only; not witnessed.

Fixed (#139): both restores take the hold, then read the lock from `git worktree list` again. Git locks only an unlocked worktree, and no other run changes residue's lock while the hold is taken, so the lock read is the lock lifted. The model shows it (`TrashRestoreStale`, `RecordRestoreStale`); not witnessed live.

**3. A Git timeout during the delete drops the record of a worktree with files gone.** `git.rs` `REMOVE_TIMEOUT` (30 minutes), or a kill of Git alone, stops `git worktree remove` after it deleted some files but not the `.git` file. `standing` counts the worktree whole, so `settle` drops the record, and without `--trash` `Moved::put_back` says `it is back in place`. Replayed: residue printed `git worktree remove refused (exit signal): no error output; it is back in place`, 51,277 of 60,000 files remained, Git listed it at its path, and no record was left. Exit 3.

Fixed (#137): once Git ran, `standing` also measures the files against the plan's fingerprint (`Files`), and any difference keeps the record with the worktree aside. Replayed: exit 3 with `Git still lists it at ~/code/.feat.residue-removing-49464, but part of its files may be gone: node_modules/ changed since the plan (60,000 -> 58,822 files); its removal record keeps its HEAD and reflog: residue restore RECORD`; that restore moved it back with its "part of it is gone" note. An edit of the user's during Git's run also keeps the record now, since residue cannot tell it from a deletion; the worktree stays aside until that restore.

**4. A restore after residue alone is killed lets the still-running Git delete the restored worktree's Git directory.** `git worktree remove` runs in residue's process group, and residue sets no signal handler, so a signal to residue's PID alone (`kill PID`, a Force Quit, or watch's 30-minute deadline on its `residue remove`, `run.rs` `stop`) leaves Git deleting.
1. `remove.rs` `execute` moves W to `.NAME.residue-removing-PID`, locks it, writes the record, verifies it, unlocks it and starts `git worktree remove`.
2. residue's PID is killed; Git keeps deleting.
3. `residue restore RECORD`: `move_back` checks `Leftover::running`, the PID in the aside name, which is residue's and dead, so it runs `git worktree move` back to the original path.
4. Git deletes `.git/worktrees/ID` by id and exits.
5. restore prints `restored ... from the interrupted removal ...` and exits 0.

Harm: the worktree is back with most of its files, but Git no longer knows it. Replayed on the release binary: restore exited 0 with a note that it could not find the Git directory; `git worktree list` showed only the main worktree; `git status` in it said `fatal: not a git repository`; 141,598 of 150,000 files remained. The record and its refs stayed, so the reflog-only commit survived `git gc --prune=now`. The model's `ReflogCommitsKept` path needs Git to finish after restore's own checks; live, Git finished first.

Fixed (#136): the record's writer holds its `info`, and `git worktree remove` inherits the hold, so it lasts until Git exits however residue stops. `residue restore RECORD` takes it first and refuses while anyone has it, and `residue scan` reports `removal_running` with no recovery command. Replayed with residue alone killed: `scan --json` showed `"pid_running":false,"removal_running":true` and an empty `recovery`; the restore exited 1 with ``a residue run, or the `git worktree remove` it started, still holds this removal record, so it may still be deleting the worktree; run this restore again once it ends`` while Git ran; once Git exited, the same restore recreated the worktree, and the reflog-only commit survived `git gc --prune=now`.

**5. A restore of a trash entry after its purge was stopped part way says nothing of the files the purge deleted.** Found by the "after" setup: the restore starts only once the purge has stopped.
1. `remove.rs` `purge` rechecks the entry, `removed::create` writes its record, `trash.rs` `purge` unlocks it and starts `git worktree remove`.
2. Ctrl-C stops residue and Git part way through the delete. The entry is left unlocked, with its `.git` file and part of its files.
3. `residue restore ENTRY`: `trash.rs` `restore` finds the worktree in the entry (`holds_worktree`) and moves it back.
4. `restore_trash` forgets the purge's record and prints `restored ... from the trash entry ...` with no note. `removed.rs` `move_back` always adds "if its removal had started deleting, part of it is gone".

Harm: the user gets the worktree back with files missing and no word of it, and a partly deleted `node_modules/` looks installed. Replayed on the release binary: 148,684 of 150,000 ignored files remained; restore exited 0 with `restored ~/code/repo-feat from the trash entry repo-feat-20261108T225724Z` and `on branch feat`, and no note; the purge's record was gone. Git had not reached the tracked files in that directory order; in another order it deletes them too.

Fixed (#138): under the entry's hold, the trash restore reads the records of purges of the entry that Git still lists there and takes their holds. With any, it says `if its purge had started deleting, part of it is gone`, then settles each as `residue restore RECORD` would: puts back the HEAD reflog and `config.worktree` the worktree lacks, and forgets the record only once the worktree holds what it keeps. Replayed: the restore printed that note, 149,260 of 150,000 ignored files remained, and the record was gone. With `logs/` deleted by hand from its Git directory before the restore, as a stop during Git's delete of that directory can leave it: on 0.0.10 the reflog was empty after the restore and `git gc --prune=now` deleted the reflog-only commit; with the fix the reflog was back with 3 entries and the commit survived.

Two changes came from modeling the fixes, not from a break. The Git directory gained the `noreflog` state above, which Finding 5 needs to lose a commit. And the trash restore takes the hold on each stopped purge's record, not only the entry's, so a `residue restore RECORD` cannot settle the same record meanwhile.

The review of #147 added the old setups, `SClear`, and `TCheck`'s refusal. When a purge stopped after Git deleted the worktree's files, `residue restore ENTRY` and `residue restore RECORD` each named the other, and neither finished. Now `residue restore ENTRY` refuses and names the record, and `residue restore RECORD` clears what Git kept, under the record's and the entry's holds, and recreates the worktree. The check has no rule for a refusal that never ends, so this came from a replay, not from a break.

Its second review added the old-orphan setups. For a record without a `hold` line, a restore now also refuses while a `git worktree remove` of the worktree runs, or while the activity probe sees a process using the worktree or its Git directory, and it applies these refusals only while Git still lists the worktree where the record's deletion started. Every rule holds in both.

Its third review added the trash restore's open `.git`, its lock of a worktree Git lists unlocked, its second read of the records once the worktree has moved, and the note of a record restore whose purge's worktree is already back, each with a mutant. In the old setups the trash restore now races the purge throughout, outside the window `LatePurge` names. Every rule holds in all fourteen setups.

A purge writes its removal record before its re-check, as a removal writes its record before its last check, so writing and syncing the record does not lengthen the time between that check and Git's delete. The model has no clock, so no rule shows that time; what it shows is that the new order breaks no rule, that a re-check that fails after the record settles the record as a removal does (`PUndo`), and that the entry's holds are no longer the only guard against a trash restore: a restore that a missing hold lets in either finds the purge's record held and refuses, or opened the worktree's `.git` before that record, which the re-check then sees as a use. So `PurgeNoHold` and `TrashRestoreNoHold` are each checked with `TrashRestoreNoUse` too, and both holds stay in the code. A purge by a release that takes no hold keeps its old order in the model (`PFirst`).

## Not covered

- `remove --trash` moving a worktree into the trash, and clearing empty records or forgetting them after `--keep-removed`.
- `residue watch`'s decisions, the service, and how the restored mark changes later runs.
- Two remove runs at once, and `restore --to`: so also two purges of one entry at once, and a purge after an earlier one of the same entry stopped, which settles that purge's record first (`removed::settle_before_purge`; `scripts/recovery-check.sh` `purge-reset-at-record` and `purge-stopped-before-recheck` check it on the real code).
- A trash restore that a purge's activity probe cannot see, such as one another user runs, which a probe that is not root does not list. The re-check then misses the restore's open `.git` (`TrUses`), and only the holds keep the two apart; the `PurgeNoHold+TrashRestoreNoUse` and `TrashRestoreNoHold+TrashRestoreNoUse` mutants are that case.
- Symbolic links, path canonicalization, and moves across filesystems.
- The stats ledger and the Mac app.
- Reftable repositories, and branches others move during a run.
- Disk errors, except `removed::create` failing.
- Two faults in one run.
- `residue scan`, whose `removal_running` takes a record's hold for an instant: a restore in that instant refuses as busy and says to run it again.
- Of a release that takes no hold, 0.0.10 and earlier: its restores; a purge whose activity probe cannot see the trash restore's open files; and the steps `LatePurge` in the model leaves out, for a purge whose re-check came before a trash restore of this release opened the worktree's `.git` and whose record comes after that restore read the records. After an upgrade, automatic cleanup runs the new release on its own ([After an upgrade](../../../docs/automation.md#after-an-upgrade)), so such a purge needs a run of that release still going.
- A trash restore that finds, once it has moved the worktree, that Git can no longer use it. When a purge of a release that takes no hold deletes the worktree's Git directory during that move, `restore.rs` `restore_trash` checks `git::identity` after the move, exits 1, says where the files stay, and names `residue restore RECORD --to NEWPATH`. `TReport` has no such branch: it reports the worktree back, so a run that reached it would count as `lied`. `scripts/recovery-check.sh` `restore-raced-git-dir` checks that branch on the real code.
- Two unlocks of one worktree. `git worktree unlock` checks for the lock, then deletes it, and a delete that finds no file still exits 0, so in that `LatePurge` window a trash restore and a purge of a release that takes no hold can both unlock, and the restore's move and that purge's `git worktree remove` can then run together. Git deletes its own directory for the worktree even after its delete of the checkout fails. The restore then names the purge's record; when Git can no longer use the moved worktree, it says its files stay at the original path and names `residue restore RECORD --to NEWPATH`, not a restore that would find the path taken.
- Whether a refusal ends. The rules say what must never happen, not that a restore can finish; two restores that each send the user to the other break none of them.
- A program that Git starts keeping a hold after Git exits, a process that reused a record's PID while Git still lists the worktree where that record's deletion started, and a record another user wrote, which a restore not run as root refuses because it cannot see that user's processes: each only makes a restore refuse longer. residue counts a live process with the record's PID as its writer while that process's arguments hold `remove`. Where it cannot read them, on macOS, Linux, FreeBSD and illumos it skips one that started more than ten minutes after the record was written; on NetBSD, OpenBSD and DragonFly it cannot, and `ps -p PID` shows what the process is.
- A filesystem with no flock(2): residue writes no removal record or trash entry there and takes no hold, so it removes and restores nothing. A run plans a trash entry or record an earlier release made there as kept, and a restore of it refuses and names the Git commands that move the worktree back, or recreate it, by hand. The model takes the holds as given.

## Credit

The method follows Lauren Tan's pstack skills, `create-formal-verification-skill` and `statechart`, in [cursor/plugins](https://github.com/cursor/plugins) (MIT).
