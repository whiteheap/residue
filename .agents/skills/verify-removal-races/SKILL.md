---
name: verify-removal-races
description: Model-check every order of steps of `residue remove`, a trash purge, both kinds of `residue restore`, Git's own `git worktree remove` and the user, to show whether any of them can lose a worktree, a commit or a lock. Use before you merge a change to src/remove.rs, src/trash.rs, src/removed.rs or src/restore.rs, to the worktree move, lock, unlock or remove calls in src/git.rs, or to how src/run.rs and src/watch.rs stop a remove run, and when someone asks whether a removal, purge or restore can lose work.
---

# Verify the removal races

`residue remove` checks a worktree, moves it aside, writes a removal record, checks it again and lets Git delete it. A purge, a restore and the user can act while that runs, and Git deletes in steps. Tests try the orders someone thought of. This check tries every order of those steps, for one worktree, with at most one kill or Git timeout, and says which of six rules any order breaks.

It is a TLA+ model of the code, `model/Removal.tla`, checked with TLC. TLC does not read the Rust: the model holds only while it describes the code, which is why the coverage table and the sync commit below exist.

## Run it

```sh
bash .agents/skills/verify-removal-races/check.sh
```

It takes about a minute and exits 0 when the result matches `model/expected`: every rule holds except the known breaks it lists, each with its shortest path, and every mutant is caught. It exits 1 when the result differs, and 2 when the tools cannot be set up or TLC fails.

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
| `RecordDroppedOnlyWhenWhole` | A removal record is dropped only while nothing of the worktree is gone (README: "residue drops the record only while nothing of the worktree is gone"). | Part of a worktree is gone with no record to restore it from. |

`TypeOK` checks that every variable keeps its type.

## What it checks, and the bounds

One repository, one worktree W, and these actors, each step one action of the model:

- `rm`: `residue remove --no-trash` on W at its path.
- `pg`: a remove run purging W's expired trash entry.
- `tr`: `residue restore ENTRY` on that trash entry.
- `rr`: `residue restore RECORD` on a removal record.
- Git's `git worktree remove`, as the four steps of `remove_worktree` in Git 2.50's `builtin/worktree.c`: look the worktree up, `git status` in it, delete its files by path, delete its Git directory by id.
- The user: edits W, starts a process in it, or runs `git worktree lock` on it, once each.

The faults: a kill of residue with its Git child (Ctrl-C), the 30-minute Git timeout, or an orphaning kill of residue alone, which leaves its Git child deleting.

| Setup | Actors | Faults, at most one | Distinct states |
| --- | --- | --- | --- |
| Remove no-faults | rm, rr, user | none | 4,534 |
| Remove faults | rm, rr, user | kill, timeout | 10,329 |
| Remove orphan | rm, rr, user | orphaning kill | 7,012 |
| Remove unraced | rm, user | kill, orphaning kill | 717 |
| Remove after | rm, then rr, user | kill, timeout | 2,457 |
| Trash no-faults | pg, tr, rr, user | none | 90,600 |
| Trash faults | pg, tr, rr, user | kill, timeout | 227,001 |
| Trash unraced | pg, rr, user | kill, orphaning kill | 6,712 |
| Trash after | pg, then tr and rr, user | kill, timeout | 6,992 |

TLC stops at a rule's first break and reports the shortest path, so check.sh checks the other rules again without it. A rule that breaks in a setup is therefore not checked further there: a new way to break it shows only if it is shorter. Two kinds of setup check the rules again without the races the findings need. Unraced switches off the restore and the Git timeout, and every rule holds there. After lets a restore start only once the remove run or purge has stopped, the usual way to recover; there only Findings 3 and 5 break.

Assumptions:

- Each Git command, file system call, or read the code makes before it acts is one indivisible step. The time between residue's last check and Git's deletion is the window the README documents; an edit there is one Git's `git status` may not show (an ignored file).
- The user never unlocks residue's own locks, `residue: removing` and `residue: in trash`.
- One remove run or one purge in each setup, never two; one edit, one process and one lock by the user.

## When a change touches the verified code

Run the check for a change to any function in this table, or for one that adds a step that moves, locks, unlocks or deletes a worktree, or writes or drops a removal record or a trash entry.

| Model actions | Code |
| --- | --- |
| `RRecheck` | `remove.rs` `recheck(original)` |
| `RMove`, `RLock`, `RRecord`, `RUnlock`, `RAfter` | `remove.rs` `execute`: `git::move_worktree` to `.NAME.residue-removing-PID`, `git::lock_worktree(REMOVING)`, `removed::create`, `git::unlock_worktree`, `git::remove_worktree` and the match on its result |
| `RVerify` | `remove.rs` `verify_moved` |
| `PBCheck`, `PBUnlock`, `PBMove`, `PBStuck` | `remove.rs` `Moved::put_back`, `Moved::stuck` |
| `RSettle`, `PSettle` | `remove.rs` `settle`, `standing` |
| `PRecheck`, `PRecord`, `PAfter`, `PRelock` | `remove.rs` `purge`: `recheck(path, ours = trash::LOCK)`, `removed::create` |
| `PUnlock` | `trash.rs` `purge` |
| `GFind`, `GExists`, `GStatus`, `GTree`, `GGitdir` | Git's `remove_worktree`, run by `git.rs` `remove_worktree` |
| `GitTimeout` | `git.rs` `REMOVE_TIMEOUT` and `run.rs` `stop` |
| `Kill`, `Orphan` | No code: residue stopped with its Git child, or alone (`run.rs` `stop` on watch's 30-minute deadline for its `residue remove` kills that PID alone) |
| `TList`, `SList` | `restore.rs` `gather`, `choose`; `removed.rs` `Item::interrupted`, `is_back` |
| `TCheck`, `TUnlock`, `TMove`, `TRelock` | `trash.rs` `restore`: `holds_worktree`, `was_locked` |
| `TDiscard` | `trash.rs` `discard` |
| `TForget` | `restore.rs` `restore_trash`, `removed::forget` |
| `SDest`, `SAfter` | `removed.rs` `restore`, `holds`, `forget` |
| `SCopies` | `removed.rs` `read_copies` |
| `SMoveBack`, `SUnlock`, `SMove`, `SRelock` | `removed.rs` `move_back`; `remove.rs` `Leftover::of`, `running` |
| `SRecreate` | `removed.rs` `recreate` |
| `TReport`, `SReport` | `restore.rs` `command`, the `restored ...` line and its notes |

Sync commit: `3356770f658493f1f84dfe0236da09b3faf64ea8`. The model was last confirmed to describe the code above at that commit, on 2026-11-09. Move it forward only after you confirm, for each covered function a change touched, that the model still describes it. `git diff 3356770f65 -- src/remove.rs src/trash.rs src/removed.rs src/restore.rs src/git.rs src/run.rs src/watch.rs` lists what to read.

## Read a result

Read every result with `/statechart`: give it `model/Removal.tla`, one of the configs (`model/Remove.cfg`, `model/RemoveOrphan.cfg`, `model/Trash.cfg`), and the view `model/view.mjs`. It tells each break as numbered steps in plain words, such as "restore moves it back to its path", with the worktree's state after each, and draws a chart of a setup that passes. For a mutant or another setup, copy a config and set `Broken`, `Faults`, `Off` or `After` as in that line of check.sh or `model/mutants`.

In check.sh's output:

- `+` against `model/expected` is a new break, or a known one with a new shortest path.
- `-` alone is a known break that no longer happens: a fix, or a model that lost the path.
- `mutant NAME: NOT caught` means the model no longer depends on that guard of the code.
- `no control` means the mutant's rule already breaks in its setup without the mutant, so the line shows nothing.

Each mutant in `model/mutants` removes one guard the code has, to show that a rule depends on it:

| Mutant | The guard it removes | Rule that breaks |
| --- | --- | --- |
| `NoVerify` | `verify_moved` before the unlock | `OnlyCheckedStateDeleted` |
| `NoFingerprint` | the fingerprint comparison in `verify_moved` | `OnlyCheckedStateDeleted` |
| `PurgeNoRecheck` | `recheck` in `remove.rs` `purge` | `OnlyCheckedStateDeleted` |
| `NoRecord` | no deletion without the record and its refs | `ReflogCommitsKept` |
| `SettleAlwaysUndoes` | `standing` in `settle`: the record goes only while the worktree is whole | `ReflogCommitsKept`, `RecordDroppedOnlyWhenWhole` |
| `MoveBackWhileRunning` | `Leftover::running` in `move_back` | `NotDeletedOnceBack` |
| `MoveBackNoNote` | `move_back`'s "part of it is gone" note | `RestoreTellsTheTruth` |
| `PutBackAlwaysUnlocks` | `Moved::put_back(locked)`: unlock only a lock residue set | `OnlyOwnLocksLifted` |

## A bug, or a modeling error

Replay the path step by step against the code. For each action, open its function in the table and check that the code can take that step in that state. A step the code cannot take is a modeling error: fix the model, and say in the change what was wrong. When every step can happen, it is a bug in the code: report it with the steps in the code's words, and the harm in one sentence.

To witness one, build a scratch repository with a temporary `HOME` and run the release binary, never against real repositories or your own `HOME`. Most windows are milliseconds wide; a worktree with 150,000 ignored files under `node_modules/` keeps `git worktree remove` running for seconds. Kill only processes you started.

## Change the model with the code

1. Update the actions for each covered function you changed, from the code, before you rerun. Keep one indivisible step of the code one action.
2. A new guard gets a `Mut("NAME")` switch in the model and a line in `model/mutants`. check.sh then shows that the rule holds with the guard and breaks without it.
3. A fix for a finding removes its lines from `model/expected`, with the reason, in the same change. The rule must then hold in that setup.
4. Never add a line to `model/expected`, or weaken a rule, to make the check pass. A new break is a finding until a replay against the code shows it is a modeling error.
5. Run the check, follow it with `/statechart` for each new or changed line, and move the sync commit.

## Findings at the sync commit

Each finding has its lines in `model/expected`. The replays ran residue 0.0.10, the release binary, on macOS 26.3.1 (arm64) with Git 2.50.1, in scratch repositories with a temporary `HOME`.

**1. A restore of a trash entry during its purge deletes the worktree's Git directory and its record.** No fault needed.
1. A remove run purges an expired trash entry: `remove.rs` `purge` rechecks it, `removed::create` writes its record, `trash.rs` `purge` runs `git worktree unlock`, and `git worktree remove TRASH/NAME` starts.
2. `residue restore ENTRY` lists the entry while it is unlocked. `trash.rs` `restore` reads `was_locked = false` from the listing, so it does not unlock, and runs `git worktree move TRASH/NAME ORIGINAL`.
3. Git finds nothing at the old path, or its delete gets ENOENT and counts the files deleted, and deletes `.git/worktrees/ID` by id, then exits 0.
4. `restore_trash` forgets the purge's record and its refs, and prints `restored ... from the trash entry ...`.

Harm: the worktree is back with most of its files, but Git no longer knows it, and a commit only its HEAD reflog held is unreachable. Replayed on the release binary (0.0.10) on macOS: restore exited 0; `git worktree list` showed only the main worktree; `git status` in it said `fatal: not a git repository`; 28,936 of 30,000 `node_modules` files remained; the record and its refs were gone; `git gc --prune=now` deleted the reflog-only commit. The purge failed with exit 3 and pointed at the record restore had just forgotten. Also, `removed.rs` `move_back`'s refusal for a trash aside tells the user to run `residue restore ENTRY`, this racing command.

**2. A restore can lift the user's lock.** No fault needed. `trash.rs` `restore` (`was_locked`) and `removed.rs` `move_back` (`ours`) decide to unlock from the listing's lock reason. The user runs `git worktree lock` after the listing; Git's `worktree unlock` lifts any reason, so it lifts theirs. The window is between the listing and the unlock, milliseconds. Git has no compare-and-unlock. Model only; not witnessed.

**3. A Git timeout during the delete drops the record of a worktree with files gone.** `git.rs` `REMOVE_TIMEOUT` (30 minutes), or a kill of Git alone, stops `git worktree remove` after it deleted some files but not the `.git` file. `standing` counts the worktree whole, so `settle` drops the record, and without `--trash` `Moved::put_back` says `it is back in place`. Replayed: residue printed `git worktree remove refused (exit signal): no error output; it is back in place`, 51,277 of 60,000 files remained, Git listed it at its path, and no record was left. Exit 3.

**4. A restore after residue alone is killed lets the still-running Git delete the restored worktree's Git directory.** `git worktree remove` runs in residue's process group, and residue sets no signal handler, so a signal to residue's PID alone (`kill PID`, a Force Quit, or watch's 30-minute deadline on its `residue remove`, `run.rs` `stop`) leaves Git deleting.
1. `remove.rs` `execute` moves W to `.NAME.residue-removing-PID`, locks it, writes the record, verifies it, unlocks it and starts `git worktree remove`.
2. residue's PID is killed; Git keeps deleting.
3. `residue restore RECORD`: `move_back` checks `Leftover::running`, the PID in the aside name, which is residue's and dead, so it runs `git worktree move` back to the original path.
4. Git deletes `.git/worktrees/ID` by id and exits.
5. restore prints `restored ... from the interrupted removal ...` and exits 0.

Harm: the worktree is back with most of its files, but Git no longer knows it. Replayed on the release binary: restore exited 0 with a note that it could not find the Git directory; `git worktree list` showed only the main worktree; `git status` in it said `fatal: not a git repository`; 141,598 of 150,000 files remained. The record and its refs stayed, so the reflog-only commit survived `git gc --prune=now`. The model's `ReflogCommitsKept` path needs Git to finish after restore's own checks; live, Git finished first.

**5. A restore of a trash entry after its purge was stopped part way says nothing of the files the purge deleted.** Found by the "after" setup: the restore starts only once the purge has stopped.
1. `remove.rs` `purge` rechecks the entry, `removed::create` writes its record, `trash.rs` `purge` unlocks it and starts `git worktree remove`.
2. Ctrl-C stops residue and Git part way through the delete. The entry is left unlocked, with its `.git` file and part of its files.
3. `residue restore ENTRY`: `trash.rs` `restore` finds the worktree in the entry (`holds_worktree`) and moves it back.
4. `restore_trash` forgets the purge's record and prints `restored ... from the trash entry ...` with no note. `removed.rs` `move_back` always adds "if its removal had started deleting, part of it is gone".

Harm: the user gets the worktree back with files missing and no word of it, and a partly deleted `node_modules/` looks installed. Replayed on the release binary: 148,684 of 150,000 ignored files remained; restore exited 0 with `restored ~/code/repo-feat from the trash entry repo-feat-20261108T225724Z` and `on branch feat`, and no note; the purge's record was gone. Git had not reached the tracked files in that directory order; in another order it deletes them too.

## Not covered

- `remove --trash` moving a worktree into the trash, and clearing empty records or forgetting them after `--keep-removed`.
- `residue watch`'s decisions, the service, and how the restored mark changes later runs.
- Two remove runs at once, and `restore --to`.
- Symbolic links, path canonicalization, and moves across filesystems.
- The stats ledger and the Mac app.
- Reftable repositories, and branches others move during a run.
- Disk errors, except `removed::create` failing.
- Two faults in one run, and a restore of a trash entry after its purge was killed alone.

## Credit

The method follows Lauren Tan's pstack skills, `create-formal-verification-skill` and `statechart`, in [cursor/plugins](https://github.com/cursor/plugins) (MIT).
