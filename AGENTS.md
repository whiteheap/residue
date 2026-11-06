# Agent guide

residue is a Rust CLI that judges and removes Git linked worktrees. The README is the user contract. This file is for agents changing the code.

## Invariants

These are the reasons people trust the tool. Do not weaken them without an explicit decision recorded in the CHANGELOG.

- Deletion only through `git worktree remove` without `--force` (`git::remove_worktree`). Never `rm`, never `--force`, never branch deletion, never `git worktree unlock`, never killing processes.
- A worktree is removable only when every check in `inspect::worktree` passes. An `unknown` check blocks, the same as a failed one.
- Activity has three states (`in_use`, `not_observed`, `unknown`). A probe failure is `unknown`, never `not_observed`.
- `remove` re-inspects against a fresh activity snapshot right before acting, and skips the worktree on any change.
- Regenerable rules come only from the built-in list, the user's config file, `--regenerable` and Git config files (`residue.regenerable`). Never from a file committed to the repository: a cloned repository must not be able to declare a user's files disposable. Never from the environment (`git -c`, `GIT_CONFIG_COUNT`, `GIT_CONFIG_PARAMETERS`, the legacy `GIT_CONFIG`): every subprocess inherits it, so it must not widen what residue deletes. `git::environment_regenerable` reports those values on stderr and in `ignored_environment_rules`, so they are never ignored silently.
- Every string from the filesystem, Git or another process passes through `output::clean` before it reaches a terminal.
- Git runs through `git::command`: argv only, optional locks off, prompts off, hooks and fsmonitor disabled, inherited `GIT_DIR`-style variables, `GIT_CONFIG`, `GIT_CONFIG_PARAMETERS` and `GIT_CONFIG_COUNT` removed. `GIT_CONFIG_GLOBAL`, `GIT_CONFIG_SYSTEM` and `GIT_CONFIG_NOSYSTEM` stay, so residue reads the same config files as the user's Git.
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
| `packaging/` | Linux packages (`nfpm.yaml`, `linux-packages.sh`), the apt repository (`apt-repo.sh`, `apt/` holds its public key) and the AUR package (`aur/`) |

## Verify a change

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo build --release
```

Then run the binary on real repositories. Make a scratch repository with worktrees in the states your change touches; [CONTRIBUTING.md](CONTRIBUTING.md#verify-a-change) has a script that builds one with each common state. The README's "Tested on" table lists the states each version covered. Run `scan`, `explain` and `remove`, and record the exact platform and Git version that ran. For Linux from a Mac: `cargo zigbuild --release --target aarch64-unknown-linux-musl`, then run the binary in an `ubuntu:24.04` container as a non-root user.

## Release

1. Bump `version` in `Cargo.toml`, run `cargo build` to update `Cargo.lock`, and turn `## Unreleased` in `CHANGELOG.md` into a `## X.Y.Z (date)` section (add the section if there is no `## Unreleased`). The release notes come from that section.
2. Commit, then tag `vX.Y.Z` and push the tag. `.github/workflows/release.yml` builds four targets, attests provenance, and publishes the GitHub release with `SHA256SUMS`. Its last job runs `.github/workflows/packages.yml`, which builds `.deb`, `.rpm` and Arch packages from the two Linux archives, attests them, and adds them to the release with an updated `SHA256SUMS`. When the tag is the latest release, its `apt` job then rebuilds the signed apt repository from the two `.deb` files and deploys it to GitHub Pages at https://whiteheap.github.io/residue/apt. The repository holds only the latest release. To package a release made before that workflow existed, run `gh workflow run packages.yml -f tag=vX.Y.Z`.
3. Update `Formula/residue.rb` in [whiteheap/homebrew-tap](https://github.com/whiteheap/homebrew-tap) with the new tag in the four URLs and the four checksums from `SHA256SUMS`. The formula has no `version` line: Homebrew reads it from the URL, and `brew audit --strict` rejects a redundant one. Then run `brew install whiteheap/tap/residue`, `brew test whiteheap/tap/residue` and `brew audit --strict --online whiteheap/tap/residue`. After the tap update, upgrade this machine's install, so the copy we dogfood is always the latest release:

   ```sh
   brew update && HOMEBREW_NO_AUTO_UPDATE=1 HOMEBREW_NO_INSTALL_CLEANUP=1 brew upgrade whiteheap/tap/residue
   ```

   When residue is not installed, use `brew install` in place of `brew upgrade`, with the same two variables. Use exactly this form: the machine's brew hook allows a tap upgrade only this way. Auto-update and install-time cleanup could change other formulae in the shared `/opt/homebrew`. The formula is one static binary with no `depends_on`, so the upgrade itself changes nothing else in the prefix.
4. Update the AUR package `residue-bin`, which lets Arch and Omarchy users run `yay -S residue-bin`. It is not on the AUR yet: the first push creates it. Run `packaging/aur/update.sh vX.Y.Z` (needs `gh` and Docker), commit `packaging/aur/`, then push the same `PKGBUILD` and `.SRCINFO` to the AUR. That needs an AUR account with your SSH key:

   ```sh
   git clone ssh://aur@aur.archlinux.org/residue-bin.git /tmp/residue-bin
   cp packaging/aur/PKGBUILD packaging/aur/.SRCINFO /tmp/residue-bin/
   git -C /tmp/residue-bin add PKGBUILD .SRCINFO
   git -C /tmp/residue-bin commit -m "Update to vX.Y.Z" && git -C /tmp/residue-bin push
   ```

### The apt signing key

The apt repository is signed with an ed25519 key, fingerprint `1AEAB31435118A1FE575D36EE1553632FC32554A`, with no expiry date: an expired key would break `apt update` for every user at once. The public key is `packaging/apt/residue-archive-keyring.asc`, and the `apt` job checks each signature against it before deploying. The armored private key is the `APT_SIGNING_KEY` secret of the `github-pages` environment, so only that job can read it, and the `github-pages` environment only deploys from `main` and `v*` tags. A backup is in the maintainer's macOS keychain, item `Whiteheap residue apt signing key` (base64 of the armored key).

To rotate it, generate a new key, replace `packaging/apt/residue-archive-keyring.asc`, update the secret with `gh secret set APT_SIGNING_KEY --env github-pages --repo whiteheap/residue`, update the fingerprint in the README, and tell users to fetch the keyring again. Their `apt update` fails until they do.
