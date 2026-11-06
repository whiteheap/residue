# Agent guide

residue is a Rust CLI that judges and removes Git linked worktrees. The README is the user contract. This file is for agents changing the code.

## Invariants

These are the reasons people trust the tool. Do not weaken them without an explicit decision recorded in the CHANGELOG.

- Deletion only through `git worktree remove` without `--force` (`git::remove_worktree`). Never `rm`, never `--force`, never branch deletion, never `git worktree unlock`, never killing processes.
- A worktree is removable only when every check in `inspect::worktree` passes. An `unknown` check blocks, the same as a failed one.
- Activity has three states (`in_use`, `not_observed`, `unknown`). A probe failure is `unknown`, never `not_observed`.
- `remove` re-inspects against a fresh activity snapshot right before acting, and skips the worktree on any change.
- Regenerable rules come only from the built-in list, the user's config file, `--regenerable` and Git config (`residue.regenerable`). Never from a file committed to the repository: a cloned repository must not be able to declare a user's files disposable.
- Every string from the filesystem, Git or another process passes through `output::clean` before it reaches a terminal.
- Git runs through `git::command`: argv only, optional locks off, prompts off, hooks and fsmonitor disabled, inherited `GIT_DIR`-style variables removed.
- One dependency (`libc`). Add another only when hand-written code would be riskier.

## Layout

| File | Owns |
| --- | --- |
| `src/cli.rs` | Argument parsing and help text |
| `src/commands.rs` | `scan`, `explain`, and resolving a PATH to one linked worktree |
| `src/remove.rs` | The remove plan, confirmation, re-check and execution |
| `src/inspect.rs` | Every check and the verdict |
| `src/activity.rs` | The process probe (`lsof` on macOS, `/proc` on Linux) |
| `src/git.rs` | Hardened Git invocation and porcelain parsers |
| `src/fsinfo.rs` | Disk usage walk and free space |
| `src/policy.rs` | Regenerable rules (names and paths) for ignored entries |
| `src/discover.rs` | Finding repositories under a root |
| `src/output.rs`, `src/json.rs` | Terminal text and JSON documents |
| `src/run.rs` | Child processes with a deadline and an output cap |

## Verify a change

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo build --release
```

Then run the binary on real repositories. Make a scratch repository with worktrees in the states your change touches; [CONTRIBUTING.md](CONTRIBUTING.md#verify-a-change) has a script that builds one with each common state. The README's "Tested on" table lists the states each version covered. Run `scan`, `explain` and `remove`, and record the exact platform and Git version that ran. For Linux from a Mac: `cargo zigbuild --release --target aarch64-unknown-linux-musl`, then run the binary in an `ubuntu:24.04` container as a non-root user.

## Release

1. Bump `version` in `Cargo.toml`, run `cargo build` to update `Cargo.lock`, and add a `## X.Y.Z (date)` section to `CHANGELOG.md`.
2. Commit, then tag `vX.Y.Z` and push the tag. `.github/workflows/release.yml` builds four targets, attests provenance, and publishes the GitHub release with `SHA256SUMS`.
3. Update `Formula/residue.rb` in [whiteheap/homebrew-tap](https://github.com/whiteheap/homebrew-tap) with the new tag in the four URLs and the four checksums from `SHA256SUMS`. The formula has no `version` line: Homebrew reads it from the URL, and `brew audit --strict` rejects a redundant one. Then run `brew install whiteheap/tap/residue`, `brew test whiteheap/tap/residue` and `brew audit --strict --online whiteheap/tap/residue`.
