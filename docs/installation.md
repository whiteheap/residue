# Install residue

Back to the [README](../README.md#install). `git` 2.36 or later must be on `PATH`.

## The install script

```sh
curl -fsSL https://residue.whiteheap.com/install.sh | sh
```

The script is [`site/public/install.sh`](../site/public/install.sh), served byte for byte from this repository (`site/smoke.sh` checks it after every deploy). To read it before it runs:

```sh
curl -fsSL https://residue.whiteheap.com/install.sh -o install.sh
less install.sh
sh install.sh --dry-run
sh install.sh
```

It installs residue through the first channel that fits, or upgrades it when it is already there, so a second run is an upgrade:

| Machine | Channel | Updates arrive with |
| --- | --- | --- |
| macOS with Homebrew | `brew install whiteheap/tap/residue` (or `brew upgrade`) | `brew upgrade` |
| Linux with Homebrew, where residue is a Homebrew formula already | `brew upgrade whiteheap/tap/residue` | `brew upgrade` |
| Debian, Ubuntu | the signed apt repository, as in [the apt steps](#debian-and-ubuntu) | `apt upgrade` |
| Fedora, RHEL and their rebuilds | the signed dnf repository, as in [the dnf steps](#fedora-rhel-and-their-rebuilds) | `dnf upgrade` |
| Arch Linux, Omarchy | the signed pacman repository, as in [the pacman steps](#arch-linux-omarchy-and-other-arch-based-systems), then `pacman -Syu residue` | `pacman -Syu` |
| macOS without Homebrew, FreeBSD, Alpine, Linux without root, or `--archive` | the release archive into `~/.local/bin` | running the script again |

After an upgrade, automatic cleanup runs the new release within about six minutes ([After an upgrade](automation.md#after-an-upgrade)).

A package repository needs root: the script uses it when it runs as root, when `sudo` works without a password, or when `sudo` can ask for yours on the terminal. Otherwise it falls back to the archive, which needs no root. On Arch, `pacman -Syu` upgrades the whole system, as Arch requires; with a terminal, pacman shows the transaction and asks first.

The script stops before it changes anything on Windows, where it points to [WSL](#windows-and-wsl), on a CPU residue has no build for, where it links to [building from source](#from-source), and without `curl`. On Debian and Ubuntu it also stops, before it asks for a password or writes under `/etc`, when apt offers a Git older than 2.36, as on Ubuntu 22.04, and names `sudo add-apt-repository ppa:git-core/ppa`.

Right after a release, its FreeBSD archives are still being built, usually for under half an hour. In that window the script installs the newest release that has this machine's archive, names both versions, and tells you to run it again later to update; the apt, dnf and pacman repositories serve the previous release in that window too. If it cannot tell which release that is, for example because GitHub's API did not answer, it stops and prints the command to install a release by its tag.

Then it runs `residue service install`, which starts `residue watch` now and at every login ([Automate cleanup](../README.md#automate-cleanup)). From 0.0.7 that needs no folder: it watches the usual code folders in your home folder. An older residue needs the folder that holds your repositories, so the script prints the command to run instead. A service you installed before is left as it is, with its own folders. On FreeBSD and other systems without launchd or systemd, the script says so and installs nothing more. With a Git older than 2.36, it installs residue but leaves cleanup off, names the Git it found, and prints the command that turns cleanup on once Git is newer. When `residue service install` fails, the summary says to fix what it named and run it again.

It ends with a summary: the version and path it installed, the channel and how updates arrive, whether cleanup is on, and how to undo it. With cleanup on, it also says what cleanup removes, that it deletes without the trash, for how long `residue restore` brings a worktree back, and how to preview it.

| Option | Effect |
| --- | --- |
| `--no-service` | Install only; do not run `residue service install`. Same as `RESIDUE_NO_SERVICE=1` |
| `--archive` | Use the release archive, never a package manager or `sudo` |
| `--prefix DIR` | Install the archive's `residue` into DIR (default `~/.local/bin`). Implies `--archive` |
| `--version vX.Y.Z` | Install that release's archive instead of the latest, and no other: when that release has no archive for this machine, the script says so and stops. Implies `--archive` |
| `--dry-run` | Print the commands that would change anything, and run none. It still reads what is installed, which release is the latest and that release's `SHA256SUMS` |
| `-h`, `--help` | Show the options |

Pass options after `sh -s --`:

```sh
curl -fsSL https://residue.whiteheap.com/install.sh | sh -s -- --no-service
```

What it checks:

- Every download is HTTPS only, TLS 1.2 or later, including redirects.
- The release archive must match its line in the release's `SHA256SUMS`, or nothing is installed. When `gh` is installed and logged in, the archive's [build-provenance attestation](#release-archives-and-packages) must also show that the workflow that builds it built it, on a GitHub-hosted runner: `release.yml` at the release's tag for macOS and Linux, `packages.yml` on `main` for FreeBSD, in `whiteheap/residue-cli`, or `whiteheap/residue` for 0.0.10 and earlier. The summary names the workflow and ref it checked. A `gh` older than 2.49 cannot check this, so the script stops and asks you to update it. Without `gh`, or logged out, the summary says the check was skipped and why.
- The signing key it adds for apt, dnf or pacman must match the SHA-256 pinned in the script, the key `1AEA B314 3511 8A1F E575  D36E E155 3632 FC32 554A`. apt trusts it for residue's repository alone, through `signed-by`. dnf does too, through the repository file the script writes, whose `gpgkey` names the checked key as the local file `/etc/pki/rpm-gpg/RPM-GPG-KEY-residue`, so dnf never imports a key from a URL. pacman has no key per repository: `--lsign-key` trusts the key for every repository, and the `[residue]` section accepts what any key pacman trusts signed. The section the script writes, or adds that line to, requires a signature on the database as well as on each package (`SigLevel = Required DatabaseRequired`), which pacman's default does not.
- Every command that changes the machine is printed (`+ ...`) before it runs.
- The whole script is functions called on its last line, so a download cut short runs nothing.

To undo it, the summary prints the commands for the channel it used; [Uninstall](#uninstall) has them all.

## Install by hand

### Homebrew

On macOS and Linux:

```sh
brew install whiteheap/tap/residue
```

### Linux package repositories

On Linux, add residue's signed package repository once, and your normal upgrades keep it current. Each repository serves x86_64 and arm64.

#### Debian and Ubuntu

The package needs Git 2.36 or later: Debian 12 and Ubuntu 24.04 ship it. Ubuntu 22.04 ships Git 2.34, so add a newer Git there first with `sudo add-apt-repository ppa:git-core/ppa`. The install script checks this before it adds the repository.

```sh
sudo install -d -m 0755 /etc/apt/keyrings
curl -fsSL https://whiteheap.github.io/residue/apt/residue-archive-keyring.gpg | sudo tee /etc/apt/keyrings/residue.gpg >/dev/null
echo "deb [signed-by=/etc/apt/keyrings/residue.gpg] https://whiteheap.github.io/residue/apt stable main" | sudo tee /etc/apt/sources.list.d/residue.list
sudo apt update && sudo apt install residue
```

#### Fedora, RHEL and their rebuilds

dnf asks you to confirm the key:

```sh
sudo curl -fsSLo /etc/yum.repos.d/residue.repo https://whiteheap.github.io/residue/rpm/residue.repo
sudo dnf install residue
```

#### Arch Linux, Omarchy and other Arch-based systems

```sh
curl -fsSL https://whiteheap.github.io/residue/residue-signing-key.asc | sudo pacman-key --add -
sudo pacman-key --lsign-key 1AEAB31435118A1FE575D36EE1553632FC32554A
printf '\n[residue]\nSigLevel = Required DatabaseRequired\nServer = https://whiteheap.github.io/residue/arch/$arch\n' | sudo tee -a /etc/pacman.conf
sudo pacman -Syu residue
```

`SigLevel = Required DatabaseRequired` makes pacman refuse residue's database unless it is signed; under pacman's default it would take an unsigned one. To a `[residue]` section with no `SigLevel`, from an older install script or these steps, the install script adds that line under the header, and `--dry-run` shows the change. It first checks with `pacman-conf` that pacman would then require residue's database signature and read every other `SigLevel` as before. Whether it adds the line or, on a fresh install, the whole section, it writes the new file beside `/etc/pacman.conf` with its mode and owner and renames it into place, so a full disk leaves the file as it was. It keeps the file as it was in `/etc/pacman.conf.residue-bak`, or `.residue-bak.1` and so on when an older one is there. When `/etc/pacman.conf` is a link, a fresh section goes into the file it points at and the link stays. It leaves the file as it is, with a note, when the section has a `SigLevel` of its own (in an `Include` too), when there are two `[residue]` sections, when `[residue]` is in an included file, and when the section to edit is in a linked `/etc/pacman.conf`. pacman takes `#` as a comment only at the start of a line, so it does not read `[residue] # note` as a section; the script stops on such a line before it changes anything, as it does when it cannot read `/etc/pacman.conf`.

All three are signed with the key `1AEA B314 3511 8A1F E575  D36E E155 3632 FC32 554A`. Each package installs `/usr/bin/residue` and pulls in Git if it is missing. To install one release without adding a repository, every release also carries `residue_amd64.deb`, `residue.x86_64.rpm` and `residue-x86_64.pkg.tar.zst`, and for arm64 `residue_arm64.deb`, `residue.aarch64.rpm` and `residue-aarch64.pkg.tar.zst`, on the [releases page](https://github.com/whiteheap/residue-cli/releases/latest).

### Release archives and packages

Prebuilt binaries for macOS (Apple silicon and Intel), static Linux (x86_64 and arm64) and FreeBSD (x86_64 and aarch64) are on the releases page too. Releases 0.0.8 to 0.0.10 also carry the Mac app, `residue-macos.dmg`, which now releases from [residue-ui](https://github.com/whiteheap/residue-ui) (README, [Mac app](../README.md#mac-app)). Each archive and package has a SHA-256 checksum in `SHA256SUMS` and a GitHub build-provenance attestation:

```sh
gh attestation verify residue-vX.Y.Z-x86_64-unknown-linux-musl.tar.gz --repo whiteheap/residue-cli \
  --cert-identity https://github.com/whiteheap/residue-cli/.github/workflows/release.yml@refs/tags/vX.Y.Z
gh attestation verify residue_amd64.deb --repo whiteheap/residue-cli \
  --cert-identity https://github.com/whiteheap/residue-cli/.github/workflows/packages.yml@refs/heads/main
```

`--cert-identity` names the workflow and the ref that must have built the file; `--repo` alone would accept any workflow in the repository, at any ref. The macOS and static Linux archives come from `release.yml` at the release's tag, as in the first command. The packages and the FreeBSD archives come from `packages.yml` on `main`, as in the second: it builds the packages from the Linux archives `release.yml` attested and the FreeBSD archives from the tag's source. For 0.0.10 and earlier, built before the rename, put `whiteheap/residue` in place of `whiteheap/residue-cli` in both places.

The macOS binaries need macOS 11 or later on Apple silicon and 10.12 or later on Intel, and the FreeBSD ones FreeBSD 14. The Linux binaries are static (musl) and need no libc. On a Mac, a binary from an archive your browser downloaded is blocked the first time it runs, because it is not notarized: run `xattr -d com.apple.quarantine residue` on it, or download the archive with `curl`, which does not mark it.

### From source

With Rust 1.85 or later, from [crates.io](https://crates.io/crates/residue), where each release from 0.0.12 on is published from its tag:

```sh
cargo install --locked residue
```

Run it again to upgrade. Or build a release from its tag:

```sh
cargo install --locked --git https://github.com/whiteheap/residue-cli --tag v0.0.14 residue
```

The last word is the package name. Cargo searches the whole repository for packages, and from 0.0.8 to 0.0.10 the repository also held the Mac app's, so at those tags `cargo install` without the name stops with "multiple packages with binaries found".

residue needs `git` 2.36 or later on `PATH`.

## Windows and WSL

Windows is not supported: residue builds only for Unix-like systems, and the install script stops in Git Bash. Under WSL 2, install it inside WSL as on Linux; that is untested. The probe there sees Linux processes only, so a Windows program working in a repository, through `\\wsl$` or under `/mnt/c`, is not seen: keep repositories you use from Windows programs locked, or out of residue's folders. Automatic cleanup needs systemd, which WSL starts when `/etc/wsl.conf` has `systemd=true` under `[boot]`.

## Uninstall

Turn the service off before you remove the package. Removed first, the service goes on starting a binary that is gone, about once a minute.

```sh
residue service uninstall
brew uninstall residue                    # Homebrew
sudo apt-get remove residue               # apt; then remove /etc/apt/sources.list.d/residue.list and /etc/apt/keyrings/residue.gpg
sudo dnf remove residue                   # dnf; then remove /etc/yum.repos.d/residue.repo and /etc/pki/rpm-gpg/RPM-GPG-KEY-residue
sudo pacman -R residue                    # pacman; then remove the [residue] section from /etc/pacman.conf
rm ~/.local/bin/residue                   # archive, or the --prefix you chose
```

If residue is already gone, stop the service by hand. On macOS:

```sh
launchctl bootout gui/$(id -u)/com.whiteheap.residue.watch
rm ~/Library/LaunchAgents/com.whiteheap.residue.watch.plist
```

On Linux:

```sh
systemctl --user disable --now residue-watch.service
rm ~/.config/systemd/user/residue-watch.service
systemctl --user daemon-reload
```

### Remove everything

residue leaves a trash, removal records and refs in each repository it removed from, and a ledger, logs and rules in your home folder. First run `residue restore` in each repository and put back what you want: a trash entry is a whole worktree, locked, that keeps its disk until it is purged, and a removal record is what brings a deleted worktree back. Then, in each repository, from the main checkout:

```sh
for wt in .git/residue-trash/*/*/; do git worktree unlock "$wt" && git worktree remove "$wt"; done
git for-each-ref --format='delete %(refname)' refs/residue/ | git update-ref --stdin   # a commit only these refs held is lost at the next git gc
rm -rf .git/residue-trash .git/residue-removed
git config --unset-all residue.regenerable                                              # the rules you added, if any
```

Once per user, with `--global` for rules you added there:

```sh
rm -rf ~/.config/residue ~/.local/state/residue ~/Library/Application\ Support/residue ~/Library/Logs/residue
git config --global --unset-all residue.regenerable
```

Your branches, commits and stashes stay.
