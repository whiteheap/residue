# Agent guide

residue is a Rust CLI that judges and removes Git linked worktrees. The README is the user contract. This file is for agents changing the code.

## Invariants

These are the reasons people trust the tool. Do not weaken them without an explicit decision recorded in the CHANGELOG.

- Deletion only through `git worktree remove` without `--force` (`git::remove_worktree`). Never `rm`, never `--force`, never branch deletion, never killing processes. `git worktree unlock` only for a lock residue set itself (`remove::REMOVING`, `trash::LOCK`), never anyone else's. Moves only through `git worktree move` (one rename by Git, which updates only that worktree's record and refuses another filesystem); never `mv`, never a copy, never `git worktree repair`, which rewrites every worktree's record.
- A worktree is removable only when every check in `inspect::worktree` passes. An `unknown` check blocks, the same as a failed one.
- Activity has three states (`in_use`, `not_observed`, `unknown`). A probe failure is `unknown`, never `not_observed`. So is finding no use with a probe that does not read open files (`activity::Seen::limits`, set on NetBSD, OpenBSD and DragonFly): it can show a use, never its absence, so `Snapshot::evidence` answers `unknown` (`partial_probe`) and nothing there is a candidate. A new backend that reads less than working directories and open files sets `limits`; never clear it to make worktrees removable.
- `remove` deletes what the plan showed or nothing. The plan records an `inspect::Fingerprint` (HEAD, tracked and untracked changes, each ignored entry's file count, size and newest mtime, the `index` and `logs/HEAD` mtimes). Right before acting it re-inspects against a fresh activity snapshot and compares; any change skips the worktree, with a reason that names it. Then it moves the worktree aside (`.NAME.residue-removing-PID`, or into the trash), locks it, and checks again at the new path (Git's record, a fresh snapshot, `Fingerprint::again`) before `git worktree remove`. On any doubt it moves the worktree back. Never claim this is atomic: the README documents the window that remains.
- `watch` deletes nothing itself. It only chooses which paths to pass to a `residue remove --yes --idle --no-trash` child process, so every removal it makes is one `remove` would make. Its reason travels with the paths: a work-landed removal passes `--only-if-upstream-gone`, which the child checks from fresh local refs at the plan, in the re-check before the move, and last in `verify_moved` right before the deletion, so a reason that went stale between the scan and the deletion keeps the worktree. `inspect::upstream` counts an upstream as gone only when its ref is absent; Git's `[gone]` for a ref that exists but does not resolve to a commit is an error, never gone. Work landed also needs the upstream to be a remote-tracking ref (`Upstream::remote`), in both `watch` (from `upstream_ref`) and the child. It reads `scan --json` and `remove --json`, so the JSON schemas are its contract too.
- `watch` fetches only through `git::fetch`, from `watch::Fetcher`: `git fetch --prune --no-tags --no-write-fetch-head --no-auto-maintenance --no-recurse-submodules` of the remotes that a linked worktree's branch tracks, in the main checkout, in a new session with no terminal, with prompts off (`GIT_TERMINAL_PROMPT=0`, `SSH_ASKPASS_REQUIRE=never`, `GCM_INTERACTIVE=never`), SSH `BatchMode=yes` unless the user set an SSH command, and a deadline. A fetch only refreshes the remote-tracking refs that the scan and the child read: it is never itself a reason to remove, and a failed one is logged and changes nothing. It never writes `FETCH_HEAD` or anything else in a worktree's Git directory, whose files are the idle clock. `scan`, `explain` and `remove` never fetch, so the child's three upstream reads stay local.
- Without a ROOT, `watch` and `service install` use `watch::DEFAULT_ROOTS` that exist in the home folder: never the home folder itself or a folder above it, never `Documents`, `Desktop` or `Downloads` (macOS asks a background process for permission there). `service install` writes the resolved roots into the service, and `service status` shows them. Run again without a ROOT, it keeps the roots a user named; only a service carrying `service::DEFAULT_MARK` looks for the default ones again.
- `remove --all --yes` (an unattended sweep) refuses to run without `--idle`. `remove` without `--all` takes only worktree paths, never a scan root. `--idle` on `remove` implies `--trash` unless `--no-trash`.
- The `idle` check's last change is the newest of the checkout's entries, the files directly in the worktree's Git directory, and the timestamp inside the newest HEAD reflog entry (`inspect::last_change`). Never the mtime of anything under `logs/`: `git gc` rewrites every worktree's `logs/HEAD`. A last change residue cannot read is `unknown`.
- Regenerable rules come only from the built-in list, the user's config file, `--regenerable` and Git config files (`residue.regenerable`). Never from a file committed to the repository: a cloned repository must not be able to declare a user's files disposable. Never from the environment (`git -c`, `GIT_CONFIG_COUNT`, `GIT_CONFIG_PARAMETERS`, the legacy `GIT_CONFIG`): every subprocess inherits it, so it must not widen what residue deletes. `git::environment_regenerable` reports those values on stderr and in `ignored_environment_rules`, so they are never ignored silently.
- Every string from the filesystem, Git or another process passes through `output::clean` before it reaches a terminal.
- Git runs through `git::command`: argv only, optional locks off, prompts off, hooks and fsmonitor disabled, inherited `GIT_DIR`-style variables, `GIT_CONFIG`, `GIT_CONFIG_PARAMETERS` and `GIT_CONFIG_COUNT` removed. `GIT_CONFIG_GLOBAL`, `GIT_CONFIG_SYSTEM` and `GIT_CONFIG_NOSYSTEM` stay, so residue reads the same config files as the user's Git.
- A worktree in the trash leaves it only through `trash::restore` (`git worktree move` back) or the purge (`trash::purge`: a re-check, then `git worktree remove` without `--force`). `trash::discard` removes only an entry's record and empty directories, never recursively. The purge is part of the confirmed plan, so a preview or an unconfirmed run purges nothing. A worktree in the trash or under a hidden `.residue-removing-` name is never a target of `remove`, by PATH or by sweep (`discover::Repo::new` sorts them out).
- One dependency (`libc`). Add another only when hand-written code would be riskier.

## Layout

| File | Owns |
| --- | --- |
| `src/cli.rs` | Argument parsing and help text |
| `src/commands.rs` | `scan`, `explain`, and resolving a PATH to one linked worktree |
| `src/remove.rs` | The remove plan, confirmation, re-check, move aside, execution, the trash purge, and interrupted-removal recovery (`Leftover`) |
| `src/trash.rs` | The trash: entries and their records, `residue restore`, and the purge |
| `src/inspect.rs` | Every check and the verdict |
| `src/activity.rs` | The process probe (`lsof` on macOS, `/proc` on Linux, `procstat` on FreeBSD, `ps` plus a working-directory sysctl per process on NetBSD, OpenBSD and DragonFly, `/proc/PID/path` on illumos) |
| `src/git.rs` | Hardened Git invocation, the non-interactive fetch `watch` runs, and porcelain parsers |
| `src/fsinfo.rs` | Disk usage walk and free space |
| `src/policy.rs` | Regenerable rules (names and paths) for ignored entries |
| `src/discover.rs` | Finding repositories under a root |
| `src/output.rs`, `src/json.rs` | Terminal text and JSON documents |
| `src/run.rs` | Child processes with a deadline and an output cap |
| `src/watch.rs` | `residue watch`: when to remove (branch landed, disk low), by running `scan --json` and `remove --yes --idle --no-trash`; when to fetch (`Fetcher`); the default roots |
| `src/events.rs` | Waiting for file changes: inotify on Linux, kqueue on macOS and the BSDs |
| `src/jsonread.rs` | Reading back residue's own JSON documents, for `watch` |
| `src/service.rs` | `residue service`: the launchd agent and the systemd user service that run `watch`, reading an installed one back (`status`, kept roots), and the line `scan` ends with at a terminal while no `watch` runs (`hint`) |
| `scripts/race-check.sh` | The race check: changes a worktree while the release binary's prompt waits, under a pseudo-terminal |
| `scripts/partial-probe.sh` | Sourced by the five test scripts: `$partial_probe` on NetBSD, OpenBSD and DragonFly, where nothing is a candidate, and the named reason each removal-expecting case is skipped with |
| `packaging/` | Linux packages (`nfpm.yaml`, `linux-packages.sh`), the signed apt, dnf and pacman repositories (`apt-repo.sh`, `rpm-repo.sh` with `rpm/residue.repo`, `arch-repo.sh`), their public key (`signing-key.asc`), and the AUR package (`aur/`) |
| `site/` | The one-page website: static HTML, CSS, one script (`hero.js`) and two self-hosted fonts in `site/public/`, its Cloudflare Workers Static Assets config, `site/check.sh` and `site/smoke.sh`. Live at https://residue.whiteheap.com: `.github/workflows/site.yml` checks it on pull requests and deploys it when a change to `site/` lands on `main`; `site/README.md` has the checks, the token and the manual fallback. The README stays the contract: commands on the page are copied from it |

## Verify a change

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --release
sh .github/smoke.sh ./target/release/residue
sh .github/watch-smoke.sh ./target/release/residue
bash scripts/race-check.sh ./target/release/residue
bash scripts/contract-check.sh ./target/release/residue
bash scripts/recovery-check.sh ./target/release/residue
```

CI runs the format, lint, test and build steps, both smoke tests, the race, contract and recovery checks on macOS, Linux x86_64 and arm64, and the build, both smoke tests and the race, contract and recovery checks in FreeBSD 14, NetBSD 10, OpenBSD 7.8 and DragonFly 6.4 virtual machines as root and as an ordinary user. An OmniOS (illumos) virtual machine runs the build, `.github/smoke.sh` and the contract and recovery checks, as root and as an ordinary user: illumos has no change events for the watch smoke test's two-minute limit, and its `script` cannot drive the race check. On NetBSD, OpenBSD and DragonFly each script skips the cases that expect a removal or a prompt, by name (`scripts/partial-probe.sh`), and checks that a clean idle worktree is kept as `unknown` (`partial_probe`). NetBSD and OpenBSD run the race check through `.github/script-adapter/script`, which translates the `script -q FILE CMD...` call into their `script -c` form. It runs `.github/service-smoke.sh` (install, status, uninstall) on macOS and Linux x86_64. Never run that script, or `residue service install` or `uninstall`, `--dry-run` included, on a machine whose service you use: they replace or remove this user's service.

Then run the binary on real repositories. Make a scratch repository with worktrees in the states your change touches; [CONTRIBUTING.md](CONTRIBUTING.md#verify-a-change) has a script that builds one with each common state. The README's "Tested on" table lists the states each version covered. Run `scan`, `explain` and `remove`, and record the exact platform and Git version that ran. For Linux from a Mac: `cargo zigbuild --release --target aarch64-unknown-linux-musl`, then run the binary in an `ubuntu:24.04` container as a non-root user.

## Release

1. On a branch, bump `version` in `Cargo.toml`, run `cargo build` to update `Cargo.lock`, turn `## Unreleased` in `CHANGELOG.md` into a `## X.Y.Z (date)` section (add the section if there is no `## Unreleased`), and update the `--tag` in the README's `cargo install` line. The release notes come from that section. Open a pull request. `main` is protected: a change lands only through a pull request, with the `macos-latest`, `ubuntu-24.04` and `ubuntu-24.04-arm` checks green, and never by a direct or force push. Wait for `freebsd-14` too, though the ruleset does not require it. Squash merge it.
2. Tag the merge commit on `main`, not the branch: `git fetch origin`, check that `origin/main` is the squash commit, then `git tag -s vX.Y.Z -m "residue X.Y.Z" origin/main` and `git push origin vX.Y.Z`. `.github/workflows/release.yml` builds four targets, attests provenance, and publishes the GitHub release with `SHA256SUMS`. Its last job starts `.github/workflows/packages.yml` as a separate run on `main` (check that run too), which builds `.deb`, `.rpm` and Arch packages from the two Linux archives and the FreeBSD x86_64 and aarch64 archives, each built and smoke-tested in a FreeBSD 14 virtual machine of its architecture from the tag's source (aarch64 emulated, so it is the slowest leg), attests them, and adds them to the release with an updated `SHA256SUMS`. A failed FreeBSD build never holds back the Linux packages or the other FreeBSD archive; check that both archives are there. `gh workflow run packages.yml --ref BRANCH -f tag=vX.Y.Z -f dry_run=true` checks a change to that workflow without uploading or deploying anything. When the tag is the latest release, its `repositories` job then rebuilds the signed apt, dnf and pacman repositories from those packages and deploys them to GitHub Pages at https://whiteheap.github.io/residue/ (`apt/`, `rpm/`, `arch/`). They hold only the latest release, so users get it with `apt upgrade`, `dnf upgrade` or `pacman -Syu` and no release needs a manual step. To package a release made before that workflow existed, run `gh workflow run packages.yml -f tag=vX.Y.Z`.
3. Update `Formula/residue.rb` in [whiteheap/homebrew-tap](https://github.com/whiteheap/homebrew-tap) with the new tag in the four URLs and the four checksums from `SHA256SUMS`. The formula has no `version` line: Homebrew reads it from the URL, and `brew audit --strict` rejects a redundant one. Then run `brew install whiteheap/tap/residue`, `brew test whiteheap/tap/residue` and `brew audit --strict --online whiteheap/tap/residue`. After the tap update, upgrade this machine's install, so the copy we dogfood is always the latest release:

   ```sh
   brew update && HOMEBREW_NO_AUTO_UPDATE=1 HOMEBREW_NO_INSTALL_CLEANUP=1 brew upgrade whiteheap/tap/residue
   ```

   When residue is not installed, use `brew install` in place of `brew upgrade`, with the same two variables. Use exactly this form: the machine's brew hook allows a tap upgrade only this way. Auto-update and install-time cleanup could change other formulae in the shared `/opt/homebrew`. The formula is one static binary with no `depends_on`, so the upgrade itself changes nothing else in the prefix.
4. Optional, once [#1](https://github.com/whiteheap/residue/issues/1) is done: update the AUR package `residue-bin`. Arch and Omarchy users already get each release from the pacman repository; the AUR adds `yay -S residue-bin` for people who have not added it. It is not on the AUR yet, because AUR registration is closed: the first push creates it. Run `packaging/aur/update.sh vX.Y.Z` (needs `gh` and Docker), commit `packaging/aur/`, then push the same `PKGBUILD` and `.SRCINFO` to the AUR. That needs an AUR account with your SSH key:

   ```sh
   git clone ssh://aur@aur.archlinux.org/residue-bin.git /tmp/residue-bin
   cp packaging/aur/PKGBUILD packaging/aur/.SRCINFO /tmp/residue-bin/
   git -C /tmp/residue-bin add PKGBUILD .SRCINFO
   git -C /tmp/residue-bin commit -m "Update to vX.Y.Z" && git -C /tmp/residue-bin push
   ```

### The package signing key

The apt, dnf and pacman repositories, and the `.rpm` files in the dnf repository, are signed with one ed25519 key, fingerprint `1AEAB31435118A1FE575D36EE1553632FC32554A`, with no expiry date: an expired key would break every user's upgrades at once. The public key is `packaging/signing-key.asc`, published as https://whiteheap.github.io/residue/residue-signing-key.asc and, for apt, as `apt/residue-archive-keyring.gpg`. The `repositories` job checks every signature against it before deploying. The armored private key is the `PACKAGE_SIGNING_KEY` secret of the `github-pages` environment, so only that job can read it, and the `github-pages` environment only deploys from `main` and `v*` tags. A backup is in the maintainer's macOS keychain, item `Whiteheap residue apt signing key` (base64 of the armored key).

To rotate it, generate a new key, replace `packaging/signing-key.asc`, update the secret with `gh secret set PACKAGE_SIGNING_KEY --env github-pages --repo whiteheap/residue`, update the fingerprint in the README, and tell users to fetch the key again. Their upgrades fail until they do.
