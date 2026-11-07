# Install residue

Back to the [README](../README.md#install). `git` 2.36 or later must be on `PATH`.

Homebrew, on macOS and Linux:

```sh
brew install whiteheap/tap/residue
```

On Linux, add residue's signed package repository once, and your normal upgrades keep it current. Each repository serves x86_64 and arm64.

Debian and Ubuntu:

```sh
sudo install -d -m 0755 /etc/apt/keyrings
curl -fsSL https://whiteheap.github.io/residue/apt/residue-archive-keyring.gpg | sudo tee /etc/apt/keyrings/residue.gpg >/dev/null
echo "deb [signed-by=/etc/apt/keyrings/residue.gpg] https://whiteheap.github.io/residue/apt stable main" | sudo tee /etc/apt/sources.list.d/residue.list
sudo apt update && sudo apt install residue
```

Fedora, RHEL and their rebuilds (dnf asks you to confirm the key):

```sh
sudo curl -fsSLo /etc/yum.repos.d/residue.repo https://whiteheap.github.io/residue/rpm/residue.repo
sudo dnf install residue
```

Arch Linux, Omarchy and other Arch-based systems:

```sh
curl -fsSL https://whiteheap.github.io/residue/residue-signing-key.asc | sudo pacman-key --add -
sudo pacman-key --lsign-key 1AEAB31435118A1FE575D36EE1553632FC32554A
printf '\n[residue]\nServer = https://whiteheap.github.io/residue/arch/$arch\n' | sudo tee -a /etc/pacman.conf
sudo pacman -Syu residue
```

All three are signed with the key `1AEA B314 3511 8A1F E575  D36E E155 3632 FC32 554A`. Each package installs `/usr/bin/residue` and pulls in Git if it is missing. To install one release without adding a repository, every release also carries `residue_amd64.deb`, `residue.x86_64.rpm` and `residue-x86_64.pkg.tar.zst` (and their arm64 builds) on the [releases page](https://github.com/whiteheap/residue/releases/latest).

Prebuilt binaries for macOS (Apple silicon and Intel), static Linux (x86_64 and arm64) and FreeBSD (x86_64 and aarch64) are on the releases page too. Each archive and package has a SHA-256 checksum in `SHA256SUMS` and a GitHub build-provenance attestation:

```sh
gh attestation verify residue_amd64.deb --repo whiteheap/residue
```

From source, with Rust 1.85 or later:

```sh
cargo install --locked --git https://github.com/whiteheap/residue --tag v0.0.7
```

residue needs `git` 2.36 or later on `PATH`.
