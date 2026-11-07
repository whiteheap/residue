# Contributing to residue

residue deletes directories. Every change is judged by one question: could it make residue remove something a person would want back? Bug reports, fixes, new regenerable names and platform test reports are all welcome.

## Report a bug

Open an issue with:

- `residue --version`, your operating system and version, and `git --version`
- the command you ran and what you expected
- the output of `residue explain --json PATH` for the worktree involved (remove any paths or branch names you don't want public)

If residue removed, or would remove, a worktree holding uncommitted, untracked or non-regenerable ignored data, a worktree in use, or anything outside the worktree, do not open a public issue. That is a security issue. Report it privately as [SECURITY.md](SECURITY.md) describes.

## Propose a change

Open an issue before you start on anything larger than a small fix, so we can agree on the behavior first.

[AGENTS.md](AGENTS.md#invariants) lists the invariants: deletion only through `git worktree remove` without `--force`, an `unknown` check blocks like a failed one, three activity states, a fresh re-check before every removal and again after the move aside, and one dependency. A change that weakens one of them needs an explicit decision recorded in [CHANGELOG.md](CHANGELOG.md), so raise it in an issue first.

To add a name to the regenerable list, open a pull request that names the tool that recreates the directory and links to its documentation. A name belongs on the list only when deleting it loses nothing that the tool cannot rebuild. A lock file or cache that belongs to one project does not go on the built-in list: that project adds it with `git config --add residue.regenerable PATH`.

## Set up

You need Rust 1.85 or later with `rustfmt` and `clippy`, Git 2.36 or later, and macOS or Linux. `jq` is useful for checking JSON output.

```sh
rustup toolchain install stable --component rustfmt,clippy
git clone https://github.com/whiteheap/residue
cd residue
cargo build
./target/debug/residue --help
```

[AGENTS.md](AGENTS.md#layout) maps each source file to what it owns. The [README](README.md) is the user contract: if your change alters what residue does, update the README in the same pull request.

## Verify a change

Run what CI runs:

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --locked --release
sh .github/smoke.sh ./target/release/residue
sh .github/watch-smoke.sh ./target/release/residue
bash scripts/race-check.sh
bash scripts/contract-check.sh
bash scripts/recovery-check.sh
```

`cargo test` runs the in-crate tests: pure checks of private decision code on synthetic data, such as the verdict fold in `src/inspect.rs`, with no filesystem, Git or processes. The shell scripts run the release binary against real Git repositories and processes. They test different things and neither replaces the other. CI runs `cargo test` on macOS and both Linux runners, and the shell scripts there and on FreeBSD.

CI also runs `.github/service-smoke.sh`, which installs and removes a real login service; run it only on a machine where no `residue service` of yours is installed.

`scripts/race-check.sh` is the race check. For each case it builds a scratch repository with one candidate worktree, starts the release binary's `remove` under a pseudo-terminal (`script`), waits for the prompt, and then changes the worktree: an untracked file, an edit to a tracked file, a new file inside the ignored `node_modules/`, or a new commit, with and without `--trash`. Then it types `yes` and checks for exit status 3, a `changed since you confirmed` reason, and the worktree still in place and registered. Two control cases change nothing and must be removed or moved to the trash. It takes under 15 seconds, needs only bash, git and `script`, cleans up after itself, and takes another binary as its argument: `bash scripts/race-check.sh /path/to/residue`. Run it for any change to `remove`, the checks or the walk, and add a case when you add a way for a worktree to change.

Beyond the smoke tests and the race check, residue has no automated test suite, so every change is also checked by running the release binary on real repositories. This script builds a scratch repository with one linked worktree in each common state. Save it as `~/scratch.sh` and run `bash ~/scratch.sh`:

```sh
set -eu
S=$(mktemp -d)
git init -q -b main "$S/app" && cd "$S/app"
printf 'node_modules/\n.env\n' > .gitignore
echo hello > README.md
git add . && git commit -qm init

# candidate: committed work plus regenerable output
git worktree add -q .worktrees/clean -b clean
mkdir .worktrees/clean/node_modules && echo x > .worktrees/clean/node_modules/a.js

# keep: an ignored file that is not on the regenerable list
git worktree add -q .worktrees/env -b env
echo 'TOKEN=1' > .worktrees/env/.env

# keep: an untracked file
git worktree add -q .worktrees/untracked -b untracked
echo notes > .worktrees/untracked/notes.md

# keep: a modified tracked file
git worktree add -q .worktrees/modified -b modified
echo edit >> .worktrees/modified/README.md

# keep: locked
git worktree add -q .worktrees/locked -b locked
git worktree lock --reason "resume later" .worktrees/locked

# keep: a detached HEAD that no branch contains
git worktree add -q --detach .worktrees/orphan HEAD
git -C .worktrees/orphan commit -q --allow-empty -m "only here"

# keep: in use by a process
git worktree add -q .worktrees/busy -b busy
(cd .worktrees/busy && exec sleep 600) >/dev/null 2>&1 &

echo "S=$S"
echo "stop the busy process when done: kill $!"
```

Paste the `S=...` line it prints into your shell, then run your build against the scratch repository:

```sh
./target/release/residue scan "$S/app"
./target/release/residue explain "$S/app/.worktrees/env"
./target/release/residue remove "$S/app/.worktrees/clean" "$S/app/.worktrees/env"
```

`scan` should show `clean` as the only candidate and give each of the other six worktrees the reason its name describes. `remove` should remove `clean`, keep its branch, refuse `env`, and exit with status 3. Add the states your change touches: a rebase in progress, a submodule, a nested repository, a path with a space, an `assume-unchanged` edit.

If your change touches the Linux side (`/proc`, the musl build, file ownership), check it in a container from a Mac. You need [cargo-zigbuild](https://github.com/rust-cross/cargo-zigbuild) and Docker:

```sh
rustup target add aarch64-unknown-linux-musl
cargo zigbuild --locked --release --target aarch64-unknown-linux-musl
docker run --rm -it \
  -v "$PWD/target/aarch64-unknown-linux-musl/release/residue:/usr/local/bin/residue:ro" \
  -v "$HOME/scratch.sh:/scratch.sh:ro" \
  ubuntu:24.04 bash
```

Inside the container, run as a non-root user, because the activity probe only sees processes the current user can read:

```sh
apt-get update && apt-get install -y git
useradd -m dev && su - dev
git config --global user.name dev && git config --global user.email dev@example.com
```

Then run `bash /scratch.sh` and the same `residue` commands, without the `./target/release/` prefix.

## Open a pull request

1. Branch from `main` and keep one change per pull request.
2. Start each commit message with a type such as `feat:`, `fix:` or `docs:`, as the history does.
3. For a change users will notice, add a line to `CHANGELOG.md` under `## Unreleased` at the top. Create the heading if it isn't there.
4. In the pull request description, list the exact platform, OS version and Git version you ran, and which commands you ran on which worktree states. If you covered a platform or state [docs/platforms-and-testing.md](docs/platforms-and-testing.md) does not list, add it there and adjust the short summary under the README's "Tested on" heading.
5. CI must pass on macOS and both Linux runners.

Check ids (`ignored`, `activity`, ...) and the JSON fields under `residue.scan/v1`, `residue.explain/v1`, `residue.remove/v1` and `residue.restore/v1` are a contract with scripts and coding agents. Add new fields and check ids freely. Do not rename or remove existing ones.

Add a dependency only when hand-written code would be riskier, and say why in the pull request.

## Releases

Maintainers cut releases. The steps are in [AGENTS.md](AGENTS.md#release).

## License

By contributing, you agree that your contributions are licensed under the [MIT License](LICENSE).
