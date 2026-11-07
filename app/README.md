# residue desktop

A desktop app for [residue](https://github.com/whiteheap/residue): see the storage that Git worktrees take, reclaim it, and turn on automatic cleanup, in a window instead of a terminal.

The app is a window over the residue CLI. It never reimplements a check, a size or a deletion: every action runs the residue binary bundled inside the app, with explicit arguments, and reads its JSON.

macOS 13 or later, on Apple silicon and Intel. Linux and Windows are not packaged yet.

## Install

Download [Residue-macos-universal.dmg](https://github.com/whiteheap/residue-desktop/releases/latest/download/Residue-macos-universal.dmg) from the latest release, open it, and drag residue to Applications. The link always serves the newest release. Every release is on the [releases page](https://github.com/whiteheap/residue-desktop/releases).

### First launch

The app is signed ad hoc, not with an Apple Developer ID, and it is not notarized yet. macOS blocks it the first time you open it. To open it once, and from then on like any other app:

1. Open residue from Applications. macOS says it could not verify the app. Click **Done**.
2. Open **System Settings > Privacy & Security**, scroll down to **Security**, and click **Open Anyway** next to the message about residue. The button is there for about an hour after you tried to open the app.
3. macOS asks again: click **Open**, and enter your login password if it asks.

Control-click > Open no longer skips this step: since macOS 15, Apple requires the Privacy & Security setting. Apple's own steps: [Open a Mac app from an unknown developer](https://support.apple.com/guide/mac-help/open-a-mac-app-from-an-unknown-developer-mh40616/mac) and [Safely open apps on your Mac](https://support.apple.com/en-us/102445).

### Check the download

Each release has a `SHA256SUMS` file and a build-provenance attestation. In the folder that holds the DMG and `SHA256SUMS`:

```sh
shasum -a 256 -c SHA256SUMS
gh attestation verify Residue-macos-universal.dmg \
  --repo whiteheap/residue-desktop \
  --signer-workflow whiteheap/residue-desktop/.github/workflows/release.yml \
  --source-ref refs/tags/vX.Y.Z
```

Replace `vX.Y.Z` with the release's tag. The attestation shows that this repository's release workflow built the DMG from that tag.

## What it does

| Screen | What it shows and does |
| --- | --- |
| Worktrees | Every linked worktree under your folders, grouped by repository, with its size and verdict: can go, kept (with the check that keeps it), unknown, or stale. Select the ones that can go and move them to the trash |
| Detail | One worktree: every check and why it passed or failed, its branch, local-only commits and last change |
| Move to trash | Runs residue's plan first and lists exactly the worktrees that move and their sizes. Nothing moves until you confirm |
| Restore | The worktrees in each repository's trash. Restore puts one back where it was, on its branch |
| Automatic cleanup | Turns residue's watch service on or off, with a plain summary before you confirm, its status and recent log lines |
| Folders | The folders to scan. The usual code folders in your home folder (`~/Work`, `~/src`, `~/code` and others) are offered first |

Removal always uses residue's trash (`residue remove --trash`): a worktree stays restorable until a later removal or automatic cleanup in that repository deletes the trash entries older than `--keep-trash`, which neither the app nor automatic cleanup sets, so residue's default applies: 7 days. The branch and its commits always stay. Automatic cleanup is residue's own `residue watch` service, which deletes instead of using the trash; the app says so before you turn it on.

## Automatic cleanup and the residue binary

The switch runs `residue service install FOLDERS` and `residue service uninstall`. On macOS the service is a launchd agent, `com.whiteheap.residue.watch`, that starts `residue watch` now and at every login. It is the same service the CLI installs: the app shows a service you installed from a terminal, and the CLI sees one the app installed.

The service records the absolute path of the binary that installed it, so the app never installs it from the copy inside the app, which would break when the app moves or is deleted. It installs from:

1. a `residue` you installed that is at least as new as the one in the app, such as Homebrew's `/opt/homebrew/bin/residue`; or
2. otherwise a copy of the app's residue at `~/Library/Application Support/com.whiteheap.residue-desktop/bin/residue`, which stays when the app moves or is deleted.

The Automatic cleanup screen shows which binary the service runs and the command that turns it off from a terminal. Deleting the app leaves the service running.

What the screen says the service does comes from the service's own arguments and the version of the binary it runs, which `brew upgrade` can change without the app: residue 0.0.6 never fetches, and 0.0.7 and newer fetch about every 30 minutes unless the service has `--no-fetch`. Where the service sets no option, the screen shows residue's default and says it is one. Saving other folders keeps the service's options as written (`--idle 7d`, `--no-fetch`), and the dialog lists them before you save. If the service has an argument the app does not know, the app does not save, because saving would drop that argument. Change the service from a terminal instead, or turn it off and on to start from residue's defaults.

To use residue from a terminal too:

```sh
brew install whiteheap/tap/residue
```

The [residue README](https://github.com/whiteheap/residue#install) has every other way to install it.

## Build from source

Needs Rust (the version in `rust-toolchain.toml`, with both macOS targets), Node 22.12 or later, pnpm, and an authenticated `gh`, which verifies the sidecar's provenance attestation.

```sh
pnpm install --frozen-lockfile
sh scripts/fetch-sidecar.sh          # residue from sidecar.lock, verified
pnpm tauri dev
pnpm tauri build --target universal-apple-darwin
```

### The residue sidecar

`sidecar.lock` pins the residue release the app bundles and the checksum of each macOS archive. `scripts/fetch-sidecar.sh` downloads them, checks each checksum against `sidecar.lock` and the release's `SHA256SUMS`, verifies the build-provenance attestation from whiteheap/residue's release workflow for that tag, and joins the two architectures into the universal binary Tauri bundles (`src-tauri/binaries/`, never committed). To move to a new residue release, change `version` and the two checksums in `sidecar.lock`, then check that the app's descriptions of residue's behavior still hold.

### Checks

CI runs the same commands on every pull request:

```sh
pnpm typecheck
pnpm lint
(cd src-tauri && cargo fmt --check && cargo clippy --locked --all-targets -- -D warnings)
pnpm tauri build --ci --target universal-apple-darwin
```

### Development builds

These environment variables change what a debug build (`pnpm tauri dev`) does. Release builds ignore them.

- `RESIDUE_DESKTOP_DEV_SERVICE=preview`: the Automatic cleanup switch never runs `residue service install` or `uninstall`. launchd acts on your real login session, and the service label is shared with any residue service already running, so a real install or uninstall would replace or remove it. In preview the switch shows the command it would run and the output of `residue watch --once --dry-run`, which removes nothing. Use it whenever you work on that screen on a machine that runs residue's service.
- `RESIDUE_DESKTOP_DEV_AGENT_FILE=PLIST`, with the preview on: the preview starts as if `PLIST` were the installed service, so you can see how the screen reads a service another residue version wrote. The app only reads the file. Make one in a temporary folder with `plutil -create xml1 PLIST` and `plutil -insert ProgramArguments -json '["/opt/homebrew/bin/residue","watch","--no-fetch","/tmp/x"]' PLIST`.
- `RESIDUE_DESKTOP_DEV_SNAPSHOT_DIR=DIR`: write a name into `DIR/request` and the app saves a picture of its window to `DIR/NAME.tiff`, for checking the UI from a terminal that macOS does not allow to record the screen. `sips -s format png DIR/NAME.tiff --out DIR/NAME.png` converts it.

## Release

1. On a branch, set the same version in `package.json`, `src-tauri/tauri.conf.json` and `src-tauri/Cargo.toml`, and run `cargo check` in `src-tauri` to update `Cargo.lock`. Open a pull request. `main` is protected: a change lands only through a pull request with the `macos-latest` check green.
2. Tag the merge commit on `main` and push the tag:

   ```sh
   git fetch origin
   git tag -a vX.Y.Z -m "residue desktop X.Y.Z" origin/main
   git push origin vX.Y.Z
   ```

The workflow does the rest. `.github/workflows/release.yml` checks that the tag matches the version and points into `main`, builds the universal app, checks its ad-hoc signature, architectures and launch from the mounted DMG, attests the DMG's build provenance, and publishes the release with `Residue-macos-universal.dmg` and `SHA256SUMS`. The notes are `.github/release-notes.md` followed by the pull requests since the last release. A tag with a hyphen (`v1.2.0-rc.1`) is published as a prerelease and never becomes the latest release that the download link serves.

A pull request that changes `release.yml` runs it as a dry run: every step but the release, with the DMG kept as the run's `Residue-macos-universal` artifact. To dry-run any branch by hand:

```sh
gh workflow run release.yml --ref BRANCH
gh run watch
gh run download RUN_ID -n Residue-macos-universal
```

## License

MIT
