# Changelog

## 0.0.1 (2026-11-06)

First release.

- `residue scan [PATH...]` lists the linked worktrees of each repository at or under PATH, with size, a verdict (`candidate`, `keep`, `unknown`, `stale`) and the reason for it.
- `residue explain PATH` shows every check for one worktree, and facts that never block: local-only commits, a gone upstream, the newest change on disk, and size.
- `residue remove PATH...` prints a plan, asks for confirmation (or accepts `--yes`), re-checks against a fresh process snapshot, then runs `git worktree remove` without `--force`. Branches and commits are kept.
- Activity probe with three states (`in_use`, `not_observed`, `unknown`): `lsof` on macOS, `/proc` on Linux.
- Ignored files block removal unless every one matches the regenerable list. Extend the list with `~/.config/residue/regenerable` or `--regenerable`.
- `--json` output for every command: `residue.scan/v1`, `residue.explain/v1`, `residue.remove/v1`.
