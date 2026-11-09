# Contributing to residue

residue deletes directories. Every change is judged by one question: could it make residue remove something a person would want back? Bug reports, fixes, new regenerable names and platform test reports are all welcome.

## Report a bug

Open a [bug report](https://github.com/whiteheap/residue-cli/issues/new?template=bug_report.yml) with:

- `residue --version`, your operating system and version, and `git --version`
- the command you ran and what you expected
- the output of `residue explain --json PATH` for the worktree involved (remove any paths or branch names you don't want public)

If residue removed, or would remove, a worktree holding uncommitted, untracked or non-regenerable ignored data, a worktree in use, or anything outside the worktree, do not open a public issue. That is a security issue. Report it privately as [SECURITY.md](SECURITY.md) describes.

## Propose a change

Open an issue before you start on anything larger than a small fix, so we can agree on the behavior first. Include a sketch of the change, such as a diagram, a mockup or a before and after.

A step that each contributor or each release repeats by hand belongs in a script, a check or a workflow in this repository. When a step failed or a mistake got through, the change that fixes it also adds what catches it next time.

[AGENTS.md](AGENTS.md#invariants) lists the invariants: deletion only through `git worktree remove` without `--force`, an `unknown` check blocks like a failed one, three activity states, a fresh re-check before every removal and again after the move aside, and one dependency. A change that weakens one of them needs an explicit decision recorded in [CHANGELOG.md](CHANGELOG.md), so raise it in an issue first.

To add a name to the regenerable list, open a pull request that names the tool that recreates the directory and links to its documentation. A name belongs on the list only when deleting it loses nothing that the tool cannot rebuild. A lock file or cache that belongs to one project does not go on the built-in list: that project adds it with `git config --add residue.regenerable PATH`.

## Set up

You need Rust 1.85 or later with `rustfmt` and `clippy`, Git 2.36 or later, and macOS or Linux. `jq` is useful for checking JSON output.

```sh
rustup toolchain install stable --component rustfmt,clippy
git clone https://github.com/whiteheap/residue-cli
cd residue-cli
cargo build
./target/debug/residue --help
```

[AGENTS.md](AGENTS.md#layout) maps each source file to what it owns. The [README](README.md) is the user contract: if your change alters what residue does, update the README in the same pull request.

The Mac app is [whiteheap/residue-ui](https://github.com/whiteheap/residue-ui), with its own setup and checks. It bundles one residue-cli release that it pins and reads that release's JSON documents and exit statuses, so it gets a change made here only once residue-ui pins a release that has it. To try a change here in the app first, build the app [with a residue from your checkout](https://github.com/whiteheap/residue-ui/blob/main/CONTRIBUTING.md#a-residue-from-your-checkout).

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
sudo bash scripts/contained-check.sh   # Linux and FreeBSD, as root
```

`cargo test` runs the in-crate tests: checks of private code, such as the verdict fold in `src/inspect.rs` on synthetic data, and the size walk in `src/fsinfo.rs` and the stats ledger in `src/stats.rs` on scratch folders in the system's temporary directory. None of them runs Git. The shell scripts run the release binary against real Git repositories and processes. They test different things and neither replaces the other. CI runs `cargo test` on macOS and both Linux runners, and the shell scripts there and in the FreeBSD, NetBSD, OpenBSD, DragonFly and illumos virtual machines. On NetBSD, OpenBSD and DragonFly nothing is a removal candidate (their probe does not read open files), so the scripts skip every case that expects a removal or a prompt, by name, and check that the clean worktree is kept as `unknown` instead; `scripts/partial-probe.sh` holds that gate. A case you add that expects a removal needs the same gate.

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
docker run --rm -it --pid=host \
  -v "$PWD/target/aarch64-unknown-linux-musl/release/residue:/usr/local/bin/residue:ro" \
  -v "$HOME/scratch.sh:/scratch.sh:ro" \
  ubuntu:24.04 bash
```

`--pid=host` lets residue see every process of the Docker machine: in a PID namespace of its own it cannot see the processes outside the container, so it keeps every worktree as `unknown` (`hidden_processes`). Inside the container, run as a non-root user, because the activity probe only sees processes the current user can read:

```sh
apt-get update && apt-get install -y git
useradd -m dev && su - dev
git config --global user.name dev && git config --global user.email dev@example.com
```

Then run `bash /scratch.sh` and the same `residue` commands, without the `./target/release/` prefix.

## State machines

residue stores no state field. Its store is Git and the filesystem, and each lifecycle's state is read from what is there: which directory a worktree is in, its lock reason, whether Git lists it, and which record files and refs exist.

| Lifecycle | Its states are read in | Its moves are made by |
| --- | --- | --- |
| A worktree being removed: linked, moved aside (`.NAME.residue-removing-PID`, locked `residue: removing`), in the trash (locked `residue: in trash`), deleted | `discover::Repo::new` | `remove::execute`, `Moved::put_back`, `remove::purge`, `trash::restore`, `removed::restore` |
| A removal record (`removed::State`): incomplete, unreadable, interrupted, back (Git lists its worktree at its original path again, on its branch at or ahead of the recorded HEAD), unknown (a worktree there on its branch whose HEAD Git could not compare), removed | `removed::list`, `removed::Item::state` (`removed::is_back`) | `removed::create`, `Written::undo`, `removed::forget`, `removed::restore` (a `back` one gets what it lacks, then `forget`) |
| A trash entry: being made (no `info` yet), holds its worktree, holds only its record (a purge clears it an hour after it was made), holds something residue never deletes beside its record or with no `info`, holds its worktree while Git lists that worktree elsewhere, or has a `keep` line this release cannot read (every run keeps it, with a warning, `trash::Item::stranded`, `remove::expired`) | `trash::list`, `trash::Item` | `trash::create`, `trash::restore`, `trash::purge`, `trash::discard` (which removes nothing from an entry that holds more than its record) |
| The restored marker | `removed::marked` | `removed::mark` |
| A hold on a trash entry or a removal record (flock(2) on its `info`): free, held by a run, held by the `git worktree remove` it started, which keeps it after residue is killed; none where the filesystem has no flock(2) | `hold::take` (`Busy`; `Unsupported` with no flock(2), where no run removes or restores), `hold::held`, `removed::Item::activity` (`Idle`, `Held`, `Unheld` for a record without the `hold` line whose worktree something may still be deleting, `removed::unheld`) | `hold::take`, `hold::write_new`, `hold::pass_to`; the kernel frees it when the last process with the file exits |

The stats ledger only appends one line per event, so it has no moves. `watch` keeps its timers in memory.

A change that adds a state or a move:

- reads the state where the table says, and again right before it acts, never from an earlier listing;
- makes each move one Git command (`git worktree move`, `lock`, `remove`, `update-ref` with old values) or one `rename(2)`, `mkdir(2)` or `O_EXCL` create, so a second writer's move fails. That failure is reported, never taken as success;
- writes a record ahead of a deletion only as intent: what Git and the filesystem show decides the state, and a record whose deletion did not happen is undone or reads as interrupted;
- adds the state to this table and to the owner's `Item`, and to the JSON `state`, with an issue in [residue-ui](https://github.com/whiteheap/residue-ui/issues) for its `src/lib/types.ts` when the app reads it, and adds a `scripts/recovery-check.sh` or `scripts/race-check.sh` case for the move and for another writer that gets there first.

## Performance

residue runs beside your editor, your builds and your coding agents, and its service runs all day, so the CPU and memory it spends are the user's. A pull request that touches a hot path, or adds a dependency, a thread, a child process or a timer, gives in its description: the commands below with every run of both builds, the machine, warm or cold cache, how you checked that the output is the same, and what you did not measure. A change that makes a number worse says why it is worth it.

### Measure a change

This fixture holds four candidate worktrees of 27,000 entries each: 2,000 tracked files and 25,000 in `node_modules/`. It takes about 12 seconds and 440 MB of disk. It gives residue a `HOME` of its own, so its `git config --global` lines write only there, your Git config does not move the numbers, and `scan` writes nothing to your stats ledger. Save it as `~/perf.sh` and run `sh ~/perf.sh`:

```sh
set -eu
P=$(mktemp -d)
mkdir "$P/home"
export HOME="$P/home" XDG_STATE_HOME="$P/state"
git config --global user.name perf
git config --global user.email perf@example.com
git init -q -b main "$P/app" && cd "$P/app"
echo node_modules/ > .gitignore
mkdir src && for i in $(seq 2000); do echo "$i" > "src/$i.txt"; done
git add . && git commit -qm init

# four candidates, each 2,000 tracked files and 25,000 in node_modules/
for w in 1 2 3 4; do
  git worktree add -q ".worktrees/w$w" -b "w$w"
  for d in $(seq 250); do
    mkdir -p ".worktrees/w$w/node_modules/$d"
    for f in $(seq 100); do echo x > ".worktrees/w$w/node_modules/$d/$f.js"; done
  done
done
echo "P=$P"
```

Paste the `P=...` line it prints. Then build main in a second worktree and run the two builds in turn, three rounds, from your checkout:

```sh
git worktree add -q --detach ../residue-main origin/main
cargo build --locked --release --manifest-path ../residue-main/Cargo.toml
cargo build --locked --release
BASE=$(cd ../residue-main && pwd)/target/release/residue NEW=$PWD/target/release/residue
(
  export HOME="$P/home" XDG_STATE_HOME="$P/state"
  t() { /usr/bin/time -l "$@" 2>&1 >/dev/null | grep -E ' real |resident|peak'; }
  for i in 1 2 3; do
    for R in "$BASE" "$NEW"; do
      echo "$R  load: $(sysctl -n vm.loadavg)"
      t "$R" --version
      t "$R" stats --json
      t "$R" explain "$P/app/.worktrees/w1"
      t "$R" scan "$P/app"
    done
  done
  for R in "$BASE" "$NEW"; do
    rm -f "$P/trace"; GIT_TRACE2_EVENT="$P/trace" "$R" scan "$P/app" >/dev/null
    echo "$R  git processes: $(grep -c '"event":"version"' "$P/trace")"
  done
)
```

`user` and `sys` include the git processes residue waited for, and `maximum resident set size` is the largest of residue and those processes; `peak memory footprint` is residue's own. On Linux, use `/usr/bin/time -v` with `grep -E 'Elapsed|seconds|Maximum resident'`, and `cat /proc/loadavg`. `scan` and `explain` also read the open files of every process residue can see, so their CPU grows with what else runs; compare builds on one machine, never with another machine's numbers.

### Main's numbers

Main at c61d629 on 2026-11-08: Apple M4 Max, 14 cores, 36 GB, macOS 26.3.1, Apple Git 2.50.1, warm cache after one unrecorded round, 1-minute load average 7.3 at every run, 670 running processes. Cold cache and Linux were not measured. Output checked: `scan` lists the four worktrees as candidates with 442 MB reclaimable, and `explain` passes every check.

| Command | Wall time, 3 runs | User CPU | System CPU | residue's peak footprint | Largest resident set | Git processes |
| --- | --- | --- | --- | --- | --- | --- |
| `--version` | 0.00, 0.00, 0.00 s | 0.00 s | 0.00 s | 1.1 MB | 1.6 MB | 0 |
| `stats --json`, empty ledger | 0.00, 0.00, 0.00 s | 0.00 s | 0.00 s | 1.1 to 1.2 MB | 2.0 MB | 0 |
| `explain` of `w1` | 0.24, 0.24, 0.26 s | 0.07 s each | 0.16, 0.15, 0.17 s | 6.5 to 6.7 MB | 8.5 to 8.6 MB | 13 |
| `scan` of the four | 0.32, 0.34, 0.35 s | 0.14 s each | 0.70, 0.77, 0.76 s | 8.9 to 9.2 MB | 9.9 to 10.3 MB | 40 |

`watch` as the service runs it, `--grace 1h --idle 2d --min-free 10` (its defaults), with the fixture's folder as ROOT: at rest it spent under 0.01 s of CPU in 5 minutes, held 1.5 to 2.2 MB resident and ran no child process, at load averages of 5.5 and 13.9. For scale, the 0.0.9 release's service on a real code folder of the same Mac spent 0.07 s in those 5 minutes, at 3.3 to 3.5 MB. For each build, read it twice, 5 minutes apart, with `ps -o rss=,time= -p PID`:

```sh
(
  export HOME="$P/home" XDG_STATE_HOME="$P/state"
  "$NEW" watch --grace 1h --idle 2d --min-free 10 "$P" 2>"$P/watch.log" & w=$!
  sleep 30; ps -o rss=,time= -p $w; sleep 300; ps -o rss=,time= -p $w
  kill $w
)
```

The website's home page (`site/`) and every file it loads come to 39,125 bytes with Brotli, not counting headers: HTML 6,355, `style.css` 4,446, `hero.js` 5,920, two fonts 8,480 and 8,612, and four SVGs 5,312, one of them the how-it-works drawing, which loads lazily. Measured on 2026-11-08 with `curl -s -o /dev/null -H 'Accept-Encoding: br' -w '%{size_download}\n' URL` for each file. With the hero on screen, the page's main thread is busy 135 to 161 ms in 10 s (median 148; headless Chrome at 1440 by 900, [#87](https://github.com/whiteheap/residue-cli/pull/87)): the change in `TaskDuration` from the Chrome DevTools Protocol's `Performance.getMetrics` over 10 s after the page settles.

Found something slow, in residue, a library or a tool it runs? Open an issue with the measurement and the command that repeats it, before there is a fix.

## Open a pull request

1. Branch from `main` and keep one change per pull request.
2. Start each commit message with a type such as `feat:`, `fix:` or `docs:`, as the history does.
3. For a change users will notice, add a line to `CHANGELOG.md` under `## Unreleased` at the top. Create the heading if it isn't there.
4. In the pull request description, list the exact platform, OS version and Git version you ran, and which commands you ran on which worktree states. If you covered a platform or state [docs/platforms-and-testing.md](docs/platforms-and-testing.md) does not list, add it there and adjust the short summary under the README's "Tested on" heading.
5. For a change to a hot path, or one that adds a dependency, a thread, a child process or a timer, add every run of both builds from [Performance](#performance).
6. CI must pass on macOS and both Linux runners.

Check ids (`ignored`, `activity`, ...) and the JSON fields under `residue.scan/v1`, `residue.explain/v1`, `residue.remove/v1` and `residue.restore/v1` are a contract with scripts, coding agents and the Mac app. Add new fields and check ids freely. Do not rename or remove existing ones.

Add a dependency only when hand-written code would be riskier, and say why in the pull request.

## Releases

Maintainers cut releases. The steps are in [AGENTS.md](AGENTS.md#release).

## License

By contributing, you agree that your contributions are licensed under the [MIT License](LICENSE).
