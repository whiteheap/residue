# The residue Mac app

This folder holds the Mac app: a window over the residue CLI that lists the linked worktrees under your code folders, moves the ones that can go to the trash, restores them from the trash or from a removal record, and turns automatic cleanup on or off. What it does for users, the download and the first launch are in the [README](../README.md#mac-app), which is the contract. This file is for building and changing the app.

The app never reimplements a check, a size or a deletion. Every action runs the residue binary bundled inside the app, with explicit arguments, and reads its JSON. That binary is built from this repository, at the same commit as the app, and has the same version. It is Tauri 2: Rust in `src-tauri/`, React and TypeScript in `src/` (Kumo components, Phosphor icons, Tailwind 4). [AGENTS.md](AGENTS.md) has the invariants.

The app's history before it moved here is in the archived [whiteheap/residue-desktop](https://github.com/whiteheap/residue-desktop) repository, up to commit `180862f`.

## Build from source

You need Rust (the version in `rust-toolchain.toml`; rustup installs it with both macOS targets), Node 22.12 or later, and pnpm (the version in `package.json`'s `packageManager`). Run these in `app/`:

```sh
pnpm install --frozen-lockfile
pnpm tauri dev
sh scripts/sidecar.sh
pnpm tauri build --target universal-apple-darwin
```

`pnpm tauri dev` builds residue first. `pnpm tauri build` does not: run `sh scripts/sidecar.sh` before it, so the app carries residue from your checkout. The bundle is `src-tauri/target/universal-apple-darwin/release/bundle/macos/residue.app`, next to a DMG in `bundle/dmg/`. The DMG step arranges its window through Finder, which can time out from a terminal that macOS has not allowed to control Finder; with `CI=true`, as on GitHub Actions, it skips that step.

### The bundled residue

Tauri's `externalBin` takes `src-tauri/binaries/residue-cli-<target>` and copies it into the bundle as `Contents/MacOS/residue-cli`, beside the app's own executable, `residue`; a `tauri dev` run puts it beside `src-tauri/target/debug/residue`. The folder is never committed. `scripts/sidecar.sh` fills it:

- With no argument, it builds residue from the repository root with `cargo build --locked --release` for `aarch64-apple-darwin` and `x86_64-apple-darwin`, and joins the two with `lipo` into `residue-cli-universal-apple-darwin`. It runs cargo in `app/`, so rustup takes `app/rust-toolchain.toml`; the binaries go to the root's `target/`.
- `--from ARM64_BINARY X86_64_BINARY` joins two binaries that are already built. The release passes the binaries of its two macOS archives.
- `--check` only checks what is there. `tauri build` runs it, so a build never bundles a missing binary or one of another version.

Each way checks that every binary holds its architectures and that the universal one reports the version in the root `Cargo.toml`.

### One version

The app's version is residue's: the `version` in the root `Cargo.toml`. `tauri.conf.json`, `package.json` and `src-tauri/Cargo.toml` hold none. `pnpm tauri` runs `scripts/tauri.sh`, which passes that version to `tauri dev`, `build` and `bundle` as a `--config` merge, so it becomes the bundle's `CFBundleShortVersionString` and the version the window shows. CI and the release check that both the bundle and the bundled residue report it.

The app's crate stays out of the CLI. The root `Cargo.toml` excludes `app/` from the residue package, and `src-tauri/Cargo.toml` is a workspace of its own, so `cargo build`, `fmt`, `clippy` and `test` at the root never build or read it. Because cargo searches a whole Git repository for packages, `cargo install --git` needs the package name, `residue`, as the README's command has it.

## Checks

`.github/workflows/app.yml` runs these on a pull request that changes `app/`, that workflow, or the CLI the app bundles (`src/`, `Cargo.toml`, `Cargo.lock`). It is not a required check.

```sh
sh scripts/sidecar.sh
pnpm typecheck
pnpm lint
pnpm build
(cd src-tauri && cargo fmt --check && cargo clippy --locked --all-targets -- -D warnings)
pnpm tauri build --ci --target universal-apple-darwin -- --locked
```

Then it checks the bundle: the identifier `com.whiteheap.residue`, the name `residue`, the version, both architectures in the app and in the bundled residue, residue's `--version`, and the signature.

## Development builds

These environment variables change what a debug build (`pnpm tauri dev`) does. Release builds ignore them.

- `RESIDUE_APP_DEV_SERVICE=preview`: the Automatic cleanup switch never runs `residue service install` or `uninstall`. launchd acts on your real login session, and the service label is shared with any residue service already running, so a real install or uninstall would replace or remove it. In preview the switch shows the command it would run and the output of `residue watch --once --dry-run`, which removes nothing. Use it whenever you work on that screen on a machine that runs residue's service.
- `RESIDUE_APP_DEV_AGENT_FILE=PLIST`, with the preview on: the preview reads `PLIST` as the installed service's agent file, so you can see how the screen reads a service another residue version wrote. It reads the file again on every read, as it reads the real one, until you use the switch: change the file while the save dialog is open, and Save refuses and shows what changed. The app only reads the file. Make one in a temporary folder with `plutil -create xml1 PLIST` and `plutil -insert ProgramArguments -json '["/opt/homebrew/bin/residue","watch","--no-fetch","/tmp/x"]' PLIST`.
- `RESIDUE_APP_DEV_SNAPSHOT_DIR=DIR`: write a name into `DIR/request` and the app saves a picture of its window to `DIR/NAME.tiff`, for checking the UI from a terminal that macOS does not allow to record the screen. `sips -s format png DIR/NAME.tiff --out DIR/NAME.png` converts it.

## Release

The app is released with the CLI, from the same tag: [AGENTS.md](../AGENTS.md#release) at the root has the steps, and nothing in them is specific to the app. On a `v*` tag, the `residue-macos.dmg` job in `.github/workflows/release.yml` extracts residue from the release's two macOS archives, joins them with `scripts/sidecar.sh --from`, builds the universal app, and checks the app inside the DMG: identifier, version, architectures, that its residue is the archives' binaries (by each architecture's UUID), the ad-hoc signature, and that it starts. The DMG goes into the release as `residue-macos.dmg`, into `SHA256SUMS` and into the provenance attestation. `packages.yml` keeps its line when it rewrites `SHA256SUMS`.

A pull request that changes `release.yml` or `app/scripts/`, and `gh workflow run release.yml --ref BRANCH`, are dry runs: they build and check the archives and the DMG and keep them as the run's artifacts, and attest and publish nothing.

### Signing and notarization

The app is signed ad hoc (`signingIdentity` `"-"` in `tauri.conf.json`) and not notarized, so macOS asks users to allow it once (README, "Mac app"). Once there is a Developer ID, sign and notarize in `release.yml` where its comment says, with the certificate and an App Store Connect API key as secrets of an environment only `v*` tags can use. Then update the ad-hoc check in that job and the README's first-launch steps.
