---
name: residue
description: Remove the Git worktree this session created, once its task is done, with residue, which removes a worktree only when nothing in it would be lost. Use as the last step of a task that ran in a linked worktree, or when the user asks to clean up Git worktrees. Not for a worktree another session or person may still use.
---

# Remove a finished worktree with residue

residue removes a linked Git worktree only when nothing in it would be lost. A change not committed, an untracked file, an ignored file that is not regenerable, such as a `.env`, even inside `build/`, a lock, or a process inside keeps it. Git does the delete, with `git worktree remove`, never `--force`. The branch and its commits stay.

A pass is not a guarantee. A paused session often has no process in its worktree, so residue cannot see that it is still in use. That is why the steps below are for the worktree this session created, and for no other.

## When this applies

- As the last step of a task this session ran in a linked worktree it created, once the task's work is committed.
- When the user asks you to clean up worktrees. For a worktree this session did not create, follow [Other worktrees](#other-worktrees) instead.

If `command -v residue` prints nothing, residue is not installed. Tell the user and leave the worktree; installing it is their choice: <https://github.com/whiteheap/residue-cli#install>.

## Remove the worktree this session created

1. Stop each process you started in the worktree, such as a dev server or a file watcher. A process inside keeps it.
2. If this session locked the worktree, unlock it: `git worktree unlock "$WORKTREE"`.
3. From outside the worktree, such as the main checkout, run:

   ```sh
   residue remove --yes --json "$WORKTREE"
   ```

   `$WORKTREE` is its path, as `git worktree list` prints it. A shell whose current directory is inside the worktree keeps it too.
4. Read the exit status:
   - `0`: the worktree is gone. Report its path. You are done.
   - `3`: read `results[0].result`. `removed` means the worktree is gone and the 3 is about the repository's trash: report `purged[].reason` and `errors`, and you are done. Any other value means residue kept the path: go to [When residue keeps it](#when-residue-keeps-it).
   - `1` or `2`: residue could not run. Report the line it printed on stderr. You are done.

## When residue keeps it

Leave the worktree where it is. Report `results[0].reason` and each check in `results[0].worktree.checks` whose `status` is not `pass`, with its `id` and `detail`, such as `untracked: 1 untracked entry: notes.txt`.

Never remove it another way: no `git worktree remove --force`, no `rm -rf`, no `git worktree prune`. Never delete, reset or stash changes, and never stop a process you did not start, to get past a check.

Fix the cause only when it is yours, then run step 3 again:

| Check | Yours when | Fix |
| --- | --- | --- |
| `activity` | a process you started, or your shell, is inside | Stop the process, or leave the worktree |
| `tracked_changes`, `untracked` | the changes are this task's work | Commit them |
| `locked` | this session set the lock | `git worktree unlock "$WORKTREE"` |

For any other check, or a cause that is not yours, the user decides. A `failed` reason may end with the one command that puts the worktree back: show it to the user.

## Keep a worktree for later

If you stop before the task is done and will come back to it, lock the worktree, so that `residue watch`, the automatic cleanup, keeps it:

```sh
git worktree lock --reason "agent paused: TASK" "$WORKTREE"
```

residue treats every lock as final. Unlock it when you resume.

## Bring one back

`residue restore` lists what residue removed, newest first. `residue restore PATH` recreates a removed worktree at its old path for 30 days by default, on its branch and HEAD, without the ignored files the removal deleted, such as `node_modules/`; the project's install makes them again.

To keep every file, ignored ones included, add `--trash` to the remove. `residue restore PATH` then puts the worktree back whole for 7 days by default, and it frees no disk until a later run purges it.

## Other worktrees

When the user asks you to clean up worktrees:

1. Run `residue scan` in the repository, or `residue scan PATH` for a folder of repositories. It deletes nothing. It lists each linked worktree as `candidate`, `keep`, `unknown` or `stale`, with its size and what kept it.
2. Show the user the candidates. A candidate can still belong to a paused session, so the user decides.
3. Remove only the ones the user names, and only as the user saw them:
   1. `residue remove --json PATH... </dev/null` prints the plan, with `plan` and `as_of`, removes nothing and exits 3: `results[].result` is `skipped` for each one it would remove, and any other value names one it keeps. Show the user both.
   2. Once the user confirms, run `residue remove --yes --json --as-of AS_OF --expect PLAN PATH...` with that `as_of`, that `plan` and the same PATHs, and read the exit status as in step 4. `4` means something changed since that plan and nothing was removed: show the user the plan this run printed, and on a yes run it again with its `as_of` and `plan`.

`residue explain PATH` shows every check for one worktree, and why it passed or failed. Run `residue remove --all`, `residue watch` or `residue service install` only when the user asks for that command.

Every JSON field and exit status: <https://github.com/whiteheap/residue-cli/blob/main/docs/reference.md#for-coding-agents>.
