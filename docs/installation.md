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

Right after a release, its FreeBSD archives are still being built, usually for under half an hour. In that window the script installs the newest release that has this machine's archive, names both versions, and tells you to run it again later to update; the apt, dnf and pacman repositories serve the previous release in that window too. If it cannot tell which release that is, for example because GitHub's API did not answer, it stops and prints the command to install a release by its tag.

Then it runs `residue service install`, which starts `residue watch` now and at every login ([Automate cleanup](../README.md#automate-cleanup)). From 0.0.7 that needs no folder: it watches the usual code folders in your home folder. An older residue needs the folder that holds your repositories, so the script prints the command to run instead. A service you installed before is left as it is, with its own folders. On FreeBSD and other systems without launchd or systemd, the script says so and installs nothing more.

It ends with a summary: the version and path it installed, the channel and how updates arrive, whether cleanup is on, and how to undo it.

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
- The release archive must match its line in the release's `SHA256SUMS`, or nothing is installed. When `gh` is installed and logged in, the archive's [build-provenance attestation](#release-archives-and-packages) must verify against `whiteheap/residue-cli` too, or `whiteheap/residue` for 0.0.10 and earlier; otherwise the summary says it was skipped and why.
- The signing key it adds for apt, dnf or pacman must match the SHA-256 pinned in the script, the key `1AEA B314 3511 8A1F E575  D36E E155 3632 FC32 554A`. Each package manager trusts only that key for residue's repository: apt through `signed-by`, pacman through `--lsign-key`, and dnf through the repository file the script writes, whose `gpgkey` names the checked key as the local file `/etc/pki/rpm-gpg/RPM-GPG-KEY-residue`, so dnf never imports a key from a URL.
- Every command that changes the machine is printed (`+ ...`) before it runs.
- The whole script is functions called on its last line, so a download cut short runs nothing.

To undo it, the summary prints the commands for the channel it used. In short:

```sh
residue service uninstall
brew uninstall residue                    # Homebrew
sudo apt-get remove residue               # apt; then remove /etc/apt/sources.list.d/residue.list and /etc/apt/keyrings/residue.gpg
sudo dnf remove residue                   # dnf; then remove /etc/yum.repos.d/residue.repo and /etc/pki/rpm-gpg/RPM-GPG-KEY-residue
sudo pacman -R residue                    # pacman; then remove the [residue] section from /etc/pacman.conf
rm ~/.local/bin/residue                   # archive, or the --prefix you chose
```

## Install by hand

### Homebrew

On macOS and Linux:

```sh
brew install whiteheap/tap/residue
```

### Linux package repositories

On Linux, add residue's signed package repository once, and your normal upgrades keep it current. Each repository serves x86_64 and arm64.

#### Debian and Ubuntu


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
printf '\n[residue]\nServer = https://whiteheap.github.io/residue/arch/$arch\n' | sudo tee -a /etc/pacman.conf
sudo pacman -Syu residue
```

All three are signed with the key `1AEA B314 3511 8A1F E575  D36E E155 3632 FC32 554A`. Each package installs `/usr/bin/residue` and pulls in Git if it is missing. To install one release without adding a repository, every release also carries `residue_amd64.deb`, `residue.x86_64.rpm` and `residue-x86_64.pkg.tar.zst` (and their arm64 builds) on the [releases page](https://github.com/whiteheap/residue-cli/releases/latest).

### Release archives and packages

Prebuilt binaries for macOS (Apple silicon and Intel), static Linux (x86_64 and arm64) and FreeBSD (x86_64 and aarch64) are on the releases page too. So is the Mac app, `residue-macos.dmg`, from 0.0.8 on (README, [Mac app](../README.md#mac-app)). Each archive, package and DMG has a SHA-256 checksum in `SHA256SUMS` and a GitHub build-provenance attestation:

```sh
gh attestation verify residue_amd64.deb --repo whiteheap/residue-cli
```

For 0.0.10 and earlier, built before the rename, use `--repo whiteheap/residue`.

### From source

With Rust 1.85 or later:

```sh
cargo install --locked --git https://github.com/whiteheap/residue-cli --tag v0.0.10 residue
```

The last word is the package name. Cargo searches the whole repository for packages, and from 0.0.8 on the repository also holds the Mac app's, so without the name `cargo install` stops with "multiple packages with binaries found".

residue needs `git` 2.36 or later on `PATH`.
