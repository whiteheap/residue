# residue

Coding agents leave Git worktrees behind. Each one is a full checkout with its own `node_modules`, `target` and build output, and a machine that runs agents all day fills its disk with them. residue finds those worktrees, measures them, and removes one only when nothing in it would be lost.

```text
$ residue scan ~/src/app
residue 0.0.1 · activity: lsof inspected 506 of 807 running processes

~/src/app · 4 linked worktrees · 5.6 GB
  candidate   2.1 GB  ~/src/app/.worktrees/fix-login  [fix-login]
                      branch kept · 1 local-only commit · upstream gone · changed 3 days ago
  keep        1.9 GB  ~/src/app/.worktrees/feat-search  [feat-search]
                      in use: node[4211], zsh[4198]
  keep        1.3 GB  ~/src/app/.worktrees/spike  [spike]
                      ignored: .env · 2 untracked
  keep        0.3 GB  ~/src/app/.worktrees/review  (detached 7954cfbb7b3a)
                      unreferenced detached HEAD

disk  41 GB free of 494 GB (8%)  (filesystem of ~/src/app)
1 candidate of 4 linked worktrees · 2.1 GB allocated · about 2.1 GB reclaimable
next: residue explain PATH  ·  residue remove PATH  ·  keep one: git worktree lock PATH
```

`residue scan` only reads. `residue remove` asks before it deletes anything, checks everything again right before it acts, and then hands the deletion to `git worktree remove`, which runs without `--force`. residue never removes something Git itself would refuse to remove.

## Install

Homebrew, on macOS and Linux:

```sh
brew install whiteheap/tap/residue
```

Prebuilt binaries for macOS (Apple silicon and Intel) and static Linux (x86_64 and arm64) are on the [releases page](https://github.com/whiteheap/residue/releases). Each archive has a SHA-256 checksum in `SHA256SUMS` and a GitHub build-provenance attestation:

```sh
gh attestation verify residue-v0.0.1-aarch64-apple-darwin.tar.gz --repo whiteheap/residue
```

From source, with Rust 1.85 or later:

```sh
cargo install --locked --git https://github.com/whiteheap/residue --tag v0.0.1
```

residue needs `git` 2.36 or later on `PATH`.

## Use

```sh
residue scan [PATH...]     # every linked worktree of the repositories at or under PATH
residue explain PATH       # every check for one worktree, and why it passed or failed
residue remove PATH...     # check again, confirm, then `git worktree remove` each one
```

`scan` with no PATH looks at the current directory. If PATH is inside a repository, residue scans that repository. Otherwise it searches up to four levels below PATH. The search does not follow symbolic links, does not cross into another filesystem, and skips hidden directories, `node_modules`, `target` and `Library`. Linked worktrees come from Git's own list, so they are found wherever they live.

Add `--json` to any command to get one JSON document (schemas `residue.scan/v1`, `residue.explain/v1`, `residue.remove/v1`).

## What makes a worktree a candidate

residue runs every check below on every linked worktree. A worktree is a **candidate** only when all of them pass. One failed check keeps it (**keep**). One check residue could not complete also keeps it (**unknown**). The main checkout is never a candidate.

| Check | Keeps the worktree when |
| --- | --- |
| `identity` | The path is not the linked worktree Git lists for this repository |
| `locked` | It is locked with `git worktree lock` |
| `activity` | A process residue can see has its working directory or an open file inside it |
| `tracked_changes` | Any tracked file is staged or modified |
| `untracked` | Any untracked file or directory exists |
| `index_flags` | A tracked file is marked assume-unchanged or skip-worktree, so `git status` cannot see edits to it |
| `ignored` | Any ignored entry is not in the regenerable list (see below) |
| `operation` | A rebase, merge, cherry-pick, revert or bisect is in progress |
| `private_refs` | It has per-worktree refs (`refs/worktree/`, `refs/bisect/`, `refs/rewritten/`) |
| `head` | HEAD is detached and no branch, tag or remote-tracking ref contains it |
| `layout` | It has submodules or is a sparse checkout |
| `nested_repo` | It contains another repository or checkout |
| `mount_point` | It contains a mount point |
| `contents` | residue could not read some of its entries |

Some facts never block, and residue reports each one separately instead of calling a worktree "safe": commits that are on the branch but on no remote-tracking ref, whether the branch's upstream is gone, the newest change on disk, and size. residue does not fetch, so "local-only" means local-only as of your last fetch.

## What removal deletes, and what it keeps

`residue remove` deletes the worktree directory with `git worktree remove`, without `--force`. That deletes:

- the checked-out tracked files. Every one of them is committed, because the `tracked_changes` check passed.
- the ignored entries the plan lists, for example `node_modules/`. Every one of them is in the regenerable list.

It keeps the branch, every commit, the stash, and every other worktree. residue never deletes a branch, never kills a process, and never unlocks anything.

Before it deletes anything, residue prints the plan and asks you to type `yes`. Right before each removal it takes a new process snapshot and runs every check again. It skips the worktree if anything changed: the verdict, HEAD, the branch, a new lock, or the set of ignored files.

If stdin is not a terminal, residue does not delete anything unless you pass `--yes`. Pass `--yes` only from the session that owns the worktree and knows its task is finished, such as an agent's last step or a launcher tearing down a task. Every check still runs. Do not pass `--yes` to a cron job that feeds residue every candidate: an idle session you plan to resume looks exactly like a finished one.

## Keep a worktree

```sh
git worktree lock --reason "resume on Monday" PATH
```

residue treats every lock as final. It is Git's own mechanism, so other tools respect it too.

## The regenerable list

When `git worktree remove` deletes a clean worktree, it deletes the worktree's ignored files without asking. But being ignored does not make a file disposable: `.env` files, local databases and scratch notes are usually ignored too. So residue keeps the worktree unless every ignored entry matches a name on this list. Each name on it is a dependency or build-output directory that its tool recreates:

`node_modules` `.pnpm-store` `bower_components` `.next` `.nuxt` `.svelte-kit` `.output` `.turbo` `.parcel-cache` `.vite` `.angular` `.eslintcache` `*.tsbuildinfo` `dist` `build` `coverage` `.nyc_output` `target` `.build` `DerivedData` `.gradle` `.dart_tool` `.venv` `venv` `__pycache__` `*.pyc` `.pytest_cache` `.mypy_cache` `.ruff_cache` `.tox` `.nox` `.DS_Store`

To add names, put one name or `*.ext` per line in `~/.config/residue/regenerable` (or `$XDG_CONFIG_HOME/residue/regenerable`), or pass `--regenerable NAME`. A name matches one path component, so `cache` matches `web/cache/` but not `web/cache2/`.

## What the activity probe can and cannot see

- **macOS**: one `lsof` snapshot of the working directories and open files of the processes your user can inspect. residue reports how many processes that covered, for example "lsof inspected 506 of 807 running processes".
- **Linux**: `/proc/*/cwd`, `/proc/*/exe` and `/proc/*/fd`, for the processes your user can read.
- **Other Unix systems**: there is no probe. Every worktree reports activity `unknown`, so none is a candidate.

The probe has three answers. `in_use` means a process was seen inside the worktree. `not_observed` means none of the inspected processes was inside it. `unknown` means the probe failed: lsof is missing, it timed out, it failed, or residue could not parse its output. `not_observed` is not proof that a worktree is idle. The probe cannot see processes owned by other users, processes inside containers or virtual machines, processes on other machines that share the disk, or a session that is closed now but that you mean to resume. A process can also open the directory a moment after the snapshot. That is why removal asks a human, and why `--yes` belongs to the session that owns the work.

## Exit status

| Code | Meaning |
| --- | --- |
| 0 | Success. For `remove`, every PATH was removed |
| 1 | Error: git missing or too old, unreadable config, unknown PATH for `explain` |
| 2 | Usage error |
| 3 | `remove` did not remove at least one PATH: refused, skipped, not confirmed, or failed |

## For coding agents

To clean up at the end of a task, run this from outside the worktree:

```sh
residue remove --yes --json "$WORKTREE"
```

Exit status 0 means the worktree is gone. Exit status 3 means it is still there. The JSON `results[].reason` and `results[].worktree.checks` fields say why, using stable check ids. Do not run it on worktrees another session may still use.

## Tested on

These are the exact platforms that ran 0.0.1. residue has no automated test suite yet. Each platform below was checked by running the release binary on real Git repositories.

| Platform | Git | What ran |
| --- | --- | --- |
| macOS 26.3.1 (25D771280a), Apple silicon | Apple Git 2.50.1 | `scan`, `explain` and `remove` on real agent worktrees and on scratch repositories covering every verdict: clean with `node_modules`, `.env`, unreferenced detached HEAD, locked, in use, modified, stale, a path with a space, a path with an escape character, an assume-unchanged edit, a skip-worktree file, a nested repository and a rebase in progress. Removal kept the branches, and the assume-unchanged edit survived. |
| Ubuntu 24.04.5 LTS, arm64 (Docker, Linux 7.0.14) | 2.43.0 | Static musl binary as a non-root user: `scan`, in-use detection through `/proc`, hard-link accounting, and `remove` with and without a terminal. As root on another user's repository, Git refused with "dubious ownership" and residue reported the error instead of guessing. |
| GitHub Actions `macos-latest`, `ubuntu-24.04` (x86_64), `ubuntu-24.04-arm` | runner Git | Build, then `scan` and `explain` of a temporary worktree. No removal. |

Not run: the Intel macOS binary (built by cross-compiling, never executed), FreeBSD, OpenBSD, NetBSD and other Unix systems (no activity probe, so nothing is a candidate), and Windows (not supported).

## Limits

- **Freed space is an estimate.** residue counts each hard-linked file once and excludes files that also have links outside the worktree (such as pnpm's store). It cannot see APFS or Btrfs clones, snapshots, or deleted files a process still holds open, so the space actually freed can be smaller.
- **There is a window between the last check and the deletion.** It lasts milliseconds, but a process can start using the worktree inside it.
- **residue only handles linked worktrees.** It does not touch package-manager stores, Docker build cache or toolchain caches. Use their own commands, which know what is safe: `pnpm store prune`, `docker builder prune`, `npm cache verify`, `go clean -cache`, `uv cache prune`.
- **Stale entries are reported, not cleaned.** For a worktree whose directory is already gone, residue reports the leftover Git metadata as `stale` and tells you to run `git worktree prune`. For an orphaned checkout whose repository is gone, residue lists it and leaves it alone.
- **residue is not a security boundary.** It runs as you, and assumes the other processes running as you are not trying to fool it.

## Other tools

Several tools already address this problem. Pick the one that matches how you work:

- [Claude Code](https://code.claude.com/docs/en/worktrees) sweeps the worktrees it creates for subagents and background tasks, and locks the ones in use.
- [Worktrunk](https://worktrunk.dev) manages the worktree lifecycle (create, switch, merge, remove) and can stop processes tied to a worktree.
- [Agentdisk](https://github.com/laaielichter/Agentdisk) attributes disk use across agent tools and moves what it removes to a quarantine you can restore from.

residue does one job: it removes linked worktrees from any tool, and only when every check above passes. It hands the deletion to Git, reports activity as one of three states on both macOS and Linux, keeps any ignored file that is not on the regenerable list, and depends on one crate (`libc`).

## License

MIT
