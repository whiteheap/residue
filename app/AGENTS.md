# Agent guide

residue desktop is a Tauri 2 app over the [residue](https://github.com/whiteheap/residue) CLI: Rust in `src-tauri/`, React and TypeScript in `src/` (Kumo components, Phosphor icons, Tailwind 4). The README is the user contract. This file is for agents changing the code.

## Invariants

These are why the app can be trusted with someone's worktrees. Do not weaken them without a recorded decision.

- The app never reimplements a check, a verdict, a size or a deletion. Every action runs the residue binary bundled as the sidecar (`src-tauri/src/cli.rs`) with explicit arguments, stdin from `/dev/null` and a deadline, and reads its `--json` document. When residue gives no value, show residue's own words or an unavailable state, never a guess.
- Removal is `residue remove --trash --json PATH...` (`src-tauri/src/lib.rs`, `remove`). The confirmation first shows residue's plan: the same command without `--yes`, which removes and purges nothing because stdin is not a terminal (exit status 3, each ready worktree skipped with verdict `candidate`). Only the user's confirm runs it again with `--yes` and exactly the planned paths. Never `--no-trash`, never `--all`, never a scan root as a path.
- Restore runs only `residue restore --json ENTRY`. Listing the trash runs `residue restore --json` in each folder.
- Automatic cleanup runs `residue service install`, `uninstall` and `status` (`src-tauri/src/service.rs`). It installs from a residue the user installed that is at least as new as the bundled one, or else from the app's own copy in `~/Library/Application Support/com.whiteheap.residue-desktop/bin/`, never from the binary inside the app bundle: the service records the binary's path, and the app can move or be deleted.
- What the window says the service does comes from the service's arguments and the version of the binary it runs (`service.rs`, `watch_settings`), never from the bundled residue: `brew upgrade` changes the service's binary without the app. residue's defaults for options a service does not set are the `DEFAULT_*` constants in `service.rs`, and `FETCH_SINCE` is the first release that fetches. Saving folders passes the service's options on as written; an argument the app does not parse blocks the save (`Plan.blocked`), never a fall back to defaults. Other text about residue's behavior states what the version in `sidecar.lock` does. When `sidecar.lock` moves to a new release, read that release's CHANGELOG entry and update the constants and the text in the same pull request.
- No binary is committed. `scripts/fetch-sidecar.sh` fetches the residue release `sidecar.lock` pins and verifies its checksums and build-provenance attestation.
- Development aids stay out of release builds: `devshot.rs` compiles only for macOS debug builds, and the service preview (`DevPreview`) checks `cfg!(debug_assertions)`.

## Verify a change

On a machine that runs residue's service, its user's data and service are not test fixtures:

- Never run `residue service install` or `uninstall`, `--dry-run` and a scratch `HOME` included. launchd acts on the real login session (`gui/UID`) and the label `com.whiteheap.residue.watch` is shared, so either one replaces or removes that user's service. Run the app with `RESIDUE_DESKTOP_DEV_SERVICE=preview` to exercise the switch: it runs only `residue watch --once --dry-run`, which removes nothing. `residue service status` only reads.
- Remove and restore only in scratch repositories you create in a temporary folder. A scan of real folders only reads, and is fine for a realistic overview.
- To see the UI from a terminal that cannot record the screen, run `pnpm tauri dev` with `RESIDUE_DESKTOP_DEV_SNAPSHOT_DIR=DIR` and write a name into `DIR/request` (README, "Development builds").

Then run what CI runs (README, "Checks"). A change to the release workflow is proved by the dry run its pull request starts (README, "Release"), which publishes nothing.

## Release

README, "Release". Only a `v*` tag pushed from `main` publishes; a manual run and a pull request run are dry runs.
