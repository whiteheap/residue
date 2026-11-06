# Changelog

## Unreleased

- A signed apt repository for Debian and Ubuntu at https://whiteheap.github.io/residue/apt, so `sudo apt install residue` works and `apt upgrade` keeps residue current. It holds the latest release for amd64 and arm64, and every release updates it.

## 0.0.3 (2026-11-07)

Feedback from running 0.0.2 on an agent monorepo: a user set `residue.regenerable` through `GIT_CONFIG_COUNT`/`GIT_CONFIG_KEY_<n>`/`GIT_CONFIG_VALUE_<n>`, the environment form of `git -c`, and residue ignored it without a word.

- Decision: residue still ignores `residue.regenerable` set through the environment (`git -c` and `--config-env`, which Git passes on as `GIT_CONFIG_PARAMETERS`; `GIT_CONFIG_COUNT` with its pairs; the legacy `GIT_CONFIG`). Every subprocess and CI step inherits the environment, so it must not widen what residue deletes. `--regenerable` is the explicit rule for one run, and Git config files are the deliberate rule for a repository.
- It no longer ignores them silently. When the environment sets the key, residue prints a note on stderr naming the values, and points at `--regenerable` and `git config --add residue.regenerable`. Every JSON document gains `ignored_environment_rules`: the ignored values, an empty list when there are none, or null when Git could not read that environment (for example `GIT_CONFIG_COUNT=1` with no `GIT_CONFIG_KEY_0`). Git parses the variables itself, so quoting and key case follow Git. The schemas stay at v1. residue runs no extra Git process when none of the variables is set.
- Fix: 0.0.2 did not remove the legacy `GIT_CONFIG` variable. It makes `git config` read one file in place of every config file, so an inherited `GIT_CONFIG` replaced the repository's own rules, and a file it named with `residue.regenerable = .env` made a worktree holding `.env` a `candidate`. residue now removes `GIT_CONFIG` from its Git commands and reports the values in that file as ignored. It has no effect on other Git commands.
- `GIT_CONFIG_GLOBAL`, `GIT_CONFIG_SYSTEM` and `GIT_CONFIG_NOSYSTEM` are unchanged and documented: they choose which config files Git reads, and residue reads the same files as your Git. A user who relocates the global config keeps its `residue.regenerable` rules, as in 0.0.2. `GIT_CONFIG_NOSYSTEM` skips the system file.
- `--help` and the README say which sources count. AGENTS.md: after each release, upgrade this machine's Homebrew install.
- Linux packages on every release, for x86_64 and arm64: `.deb` for Debian and Ubuntu, `.rpm` for Fedora and RHEL, and a pacman package for Arch and Omarchy. Each is attested and listed in `SHA256SUMS`. v0.0.2 has them too.
- `packaging/aur/PKGBUILD` for the `residue-bin` AUR package.

## 0.0.2 (2026-11-06)

Feedback from running 0.0.1 on a busy agent monorepo: every worktree there held `.local-dev/` lock files, `.cargo/config.toml` and `web/.tscache/`, so nothing was a candidate unless each user added those names to a config file that then applied to every repository.

- Rules per repository from Git config: `git config --add residue.regenerable RULE`. residue reads them with `git config --get-all` in each worktree. An invalid value makes the `ignored` check `unknown` and names the value. It does not stop the scan. `explain` lists the values. residue never reads rules from a file committed to the repository, because a cloned repository must not be able to declare your files disposable.
- Path rules: a rule with a slash is a path from the worktree root (`web/.tscache`, `.local-dev/*.lock`). `*` matches within one segment. A path rule matches that entry and everything below it. Path rules work in the config file, `--regenerable` and Git config. Name rules are unchanged.
- Git reports a fully ignored directory once, as `dir/`. When a path rule reaches inside one, residue judges each file in it during the walk it already makes. The directory is regenerable only if a rule matches every file in it. `explain` names the first file that no rule matches. Above 100,000 entries, the directory is kept.
- Ignored entries show their file count and apparent size in the `scan` line, in `explain` (up to 8 entries), and on the `deletes ignored:` line of the `remove` plan, for example `ignored: .local-dev/ (2 files, 0 B), .vercel/ (2 files, 1.2 KB) (+5 more; 7 entries hold 31 files, 3.1 MB)`. Each hard-linked file counts once. JSON `ignored[]` entries gain `files` and `bytes` (null when not measured). The schemas stay at v1.
- The same walk sizes the ignored entries, judges collapsed directories and runs the safety checks. `git status` now runs before the walk. The order of the checks in the report is unchanged.
- Snapshot time: the text output of `scan`, `explain` and `remove` starts with `as of HH:MM:SS` in local time. Every JSON document has `generated_at` in RFC 3339 UTC. `explain` text now starts with the same header line as `scan`.
- `remove` still inspects each worktree twice, once for the plan and once right before deletion. Measured on macOS 26.3.1 (Apple silicon) with a 1.0 GB worktree of 46,572 entries (a pnpm `node_modules` and a Cargo `target`), over three rounds: `residue explain` took 0.65 to 0.83 s, `git worktree remove` of an identical copy took 2.69 to 3.04 s, and `residue remove --yes` took 3.68 to 4.34 s. The plan inspection is 13 to 22% of `remove --yes` (about 17% on average), under the 20% bar for removing it under `--yes`, so `remove` is unchanged.
- README: the `results[].result` values, a scan as a snapshot, `.vercel/` as a common non-regenerable entry, rules and their sources, the cost of the walk, and the platforms that ran 0.0.2.

## 0.0.1 (2026-11-06)

First release.

- `residue scan [PATH...]` lists the linked worktrees of each repository at or under PATH, with size, a verdict (`candidate`, `keep`, `unknown`, `stale`) and the reason for it.
- `residue explain PATH` shows every check for one worktree, and facts that never block: local-only commits, a gone upstream, the newest change on disk, and size.
- `residue remove PATH...` prints a plan, asks for confirmation (or accepts `--yes`), re-checks against a fresh process snapshot, then runs `git worktree remove` without `--force`. Branches and commits are kept.
- Activity probe with three states (`in_use`, `not_observed`, `unknown`): `lsof` on macOS, `/proc` on Linux.
- Ignored files block removal unless every one matches the regenerable list. Extend the list with `~/.config/residue/regenerable` or `--regenerable`.
- `--json` output for every command: `residue.scan/v1`, `residue.explain/v1`, `residue.remove/v1`.
