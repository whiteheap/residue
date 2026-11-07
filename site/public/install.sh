#!/bin/sh
# Install residue and turn on automatic cleanup.
#
#   curl -fsSL https://residue.whiteheap.com/install.sh | sh
#   curl -fsSL https://residue.whiteheap.com/install.sh | sh -s -- --help
#
# It installs or upgrades residue through the best channel here: Homebrew on
# macOS; the signed apt, dnf or pacman repository on Linux when it can act as
# root; otherwise the release archive, checked against SHA256SUMS and, when gh
# is logged in, its build-provenance attestation. Then it runs `residue
# service install`, and prints what it did and how to undo it. Each command
# that changes the machine is printed (+ ...) before it runs. Everything is in
# functions called on the last line, so a download cut short runs nothing.
# Source: https://github.com/whiteheap/residue/blob/main/site/public/install.sh
set -eu

REPO=whiteheap/residue
PAGES=https://whiteheap.github.io/residue
# The package signing key (docs/installation.md). The two files it is
# published as are pinned by SHA-256; site/check.sh checks both pins against
# packaging/signing-key.asc.
KEY_FPR=1AEAB31435118A1FE575D36EE1553632FC32554A
KEY_ASC_SHA256=f63c31e514d7f8b87a2a61e7fe4833d6708c734fe3093286a505055482471554
KEY_GPG_SHA256=1347e2c9b519414386f5997651a6bae055d0df86eb002b6e2b95ea946172e2cc
# Where dnf finds that key: the repository file names this local file, not a URL.
RPM_KEY=/etc/pki/rpm-gpg/RPM-GPG-KEY-residue
# The first release whose `residue service install` needs no folder (#28).
SERVICE_NO_ROOT_SINCE=0.0.7

usage() {
  cat <<'EOF'
Install residue and turn on automatic cleanup.

Usage: curl -fsSL https://residue.whiteheap.com/install.sh | sh -s -- [OPTIONS]

  --no-service     Install only; do not run `residue service install`
                   (or set RESIDUE_NO_SERVICE=1)
  --archive        Install the release archive, not a package; never sudo
  --prefix DIR     Where the archive puts residue (default ~/.local/bin)
  --version vX.Y.Z Install this release from its archive (default: latest)
  --dry-run        Print the commands that would change anything; run none
  -h, --help       Show this help

--prefix and --version imply --archive. Running it again upgrades.
EOF
}

say() { printf '%s\n' "$*"; }
die() { printf 'install.sh: %s\n' "$*" >&2; exit 1; }
has() { command -v "$1" >/dev/null 2>&1; }
# A terminal to ask on. A subshell, because a failed redirection of a
# special builtin like `:` ends a POSIX shell.
has_tty() { (exec </dev/tty) 2>/dev/null; }

# Print a command, quoted for a shell, then run it unless --dry-run.
run() {
  line='+'
  for arg in "$@"; do
    case $arg in
      '' | *[!A-Za-z0-9_./:=@%+,-]*) arg="'$(printf '%s' "$arg" | sed "s/'/'\\\\''/g")'" ;;
    esac
    line="$line $arg"
  done
  say "$line"
  [ "$dry_run" = 1 ] || "$@"
}

as_root() { if [ -n "$sudo" ]; then run sudo "$@"; else run "$@"; fi; }

# HTTPS only, TLS 1.2 or later, on every redirect too.
fetch() { curl --proto '=https' --tlsv1.2 -fsSL --retry 3 -o "$2" "$1"; }

sha256() {
  if has sha256sum; then sha256sum "$1" | cut -d' ' -f1
  elif has shasum; then shasum -a 256 "$1" | cut -d' ' -f1
  elif has sha256; then sha256 -q "$1"
  else die "no sha256sum, shasum or sha256 to check the download with"
  fi
}

# Download a key file and stop unless it is the pinned key.
fetch_key() {
  fetch "$1" "$2"
  [ "$(sha256 "$2")" = "$3" ] || die "$1 is not the residue signing key $KEY_FPR"
}

parse_args() {
  no_service=${RESIDUE_NO_SERVICE:-0} archive=0 prefix='' tag='' dry_run=0
  while [ $# -gt 0 ]; do
    case $1 in
      --no-service) no_service=1 ;;
      --archive) archive=1 ;;
      --prefix) [ $# -ge 2 ] || die "--prefix needs a directory"; prefix=$2 archive=1; shift ;;
      --version) [ $# -ge 2 ] || die "--version needs a tag, like v0.0.6"; tag=v${2#v} archive=1; shift ;;
      --dry-run) dry_run=1 ;;
      -h | --help) usage; exit 0 ;;
      *) usage >&2; die "unknown option: $1" ;;
    esac
    shift
  done
  case $tag in '' | v[0-9]*.[0-9]*.[0-9]*) ;; *) die "--version takes a tag like v0.0.6, not $tag" ;; esac
  [ -n "${HOME:-}" ] || die "HOME is not set"
  prefix=${prefix:-$HOME/.local/bin}
  case $prefix in /*) ;; *) prefix=$PWD/$prefix ;; esac
}

# Root, passwordless sudo, or sudo that may ask for a password on the
# terminal. Sets $sudo; fails when none of them works.
can_root() {
  sudo=
  [ "$(id -u)" = 0 ] && return 0
  has sudo || return 1
  sudo=sudo
  [ "$dry_run" = 1 ] && return 0
  sudo -n true 2>/dev/null && return 0
  has_tty || return 1
  say "sudo asks for your password to add residue's package repository."
  sudo -v
}

choose_channel() {
  os=$(uname -s)
  channel=archive
  [ "$archive" = 1 ] && return 0
  case $os in
    Darwin) has brew && channel=brew ;;
    Linux)
      if has brew && brew list --formula whiteheap/tap/residue >/dev/null 2>&1; then channel=brew
      elif has apt-get && can_root; then channel=apt
      elif has dnf && can_root; then channel=dnf
      elif has pacman && can_root; then channel=pacman
      fi ;;
  esac
}

install_brew() {
  if brew list --formula whiteheap/tap/residue >/dev/null 2>&1; then verb=upgrade; else verb=install; fi
  run brew "$verb" whiteheap/tap/residue ||
    die "brew $verb failed. To install without Homebrew, add --archive."
  bin=$(brew --prefix)/bin/residue
  updates="brew upgrade"
  undo="brew uninstall residue"
}

install_apt() {
  [ "$dry_run" = 1 ] || fetch_key "$PAGES/apt/residue-archive-keyring.gpg" "$tmp/residue.gpg" "$KEY_GPG_SHA256"
  say "deb [signed-by=/etc/apt/keyrings/residue.gpg] $PAGES/apt stable main" >"$tmp/residue.list"
  as_root install -d -m 0755 /etc/apt/keyrings
  as_root install -m 0644 "$tmp/residue.gpg" /etc/apt/keyrings/residue.gpg
  as_root install -m 0644 "$tmp/residue.list" /etc/apt/sources.list.d/residue.list
  as_root apt-get -q update
  as_root env DEBIAN_FRONTEND=noninteractive apt-get -q install -y residue
  bin=/usr/bin/residue
  updates="apt upgrade"
  undo="${sudo:+sudo }apt-get remove residue; ${sudo:+sudo }rm /etc/apt/sources.list.d/residue.list /etc/apt/keyrings/residue.gpg"
}

install_dnf() {
  [ "$dry_run" = 1 ] || fetch_key "$PAGES/residue-signing-key.asc" "$tmp/residue.asc" "$KEY_ASC_SHA256"
  # packaging/rpm/residue.repo, except that gpgkey names the checked key as a
  # local file. A gpgkey URL would let `dnf -y` import whatever key it serves.
  printf '%s\n' '[residue]' 'name=residue' "baseurl=$PAGES/rpm/\$basearch" 'enabled=1' \
    'gpgcheck=1' 'repo_gpgcheck=1' "gpgkey=file://$RPM_KEY" >"$tmp/residue.repo"
  as_root install -d -m 0755 "${RPM_KEY%/*}"
  as_root install -m 0644 "$tmp/residue.asc" "$RPM_KEY"
  as_root rpm --import "$RPM_KEY"
  as_root install -m 0644 "$tmp/residue.repo" /etc/yum.repos.d/residue.repo
  if rpm -q residue >/dev/null 2>&1; then as_root dnf upgrade -y residue; else as_root dnf install -y residue; fi
  bin=/usr/bin/residue
  updates="dnf upgrade"
  undo="${sudo:+sudo }dnf remove residue; ${sudo:+sudo }rm /etc/yum.repos.d/residue.repo $RPM_KEY"
}

install_pacman() {
  [ "$dry_run" = 1 ] || fetch_key "$PAGES/residue-signing-key.asc" "$tmp/residue.asc" "$KEY_ASC_SHA256"
  as_root pacman-key --add "$tmp/residue.asc"
  as_root pacman-key --lsign-key "$KEY_FPR"
  if ! grep -q '^\[residue\]' /etc/pacman.conf; then
    printf '\n[residue]\nServer = %s/arch/%sarch\n' "$PAGES" '$' >"$tmp/pacman.conf"
    # shellcheck disable=SC2016 # $1 belongs to the inner sh.
    as_root sh -c 'cat "$1" >>/etc/pacman.conf' sh "$tmp/pacman.conf"
  fi
  # Arch supports only full upgrades, so this is `pacman -Syu`, as in the
  # docs. With a terminal, pacman shows the transaction and asks first.
  if [ "$dry_run" = 0 ] && has_tty; then
    as_root pacman -Syu --needed residue </dev/tty
  else
    as_root pacman -Syu --needed --noconfirm residue
  fi
  bin=/usr/bin/residue
  updates="pacman -Syu"
  undo="${sudo:+sudo }pacman -R residue, and remove the [residue] section from /etc/pacman.conf"
}

target() {
  case $(uname -m) in
    x86_64 | amd64) cpu=x86_64 ;;
    arm64 | aarch64) cpu=aarch64 ;;
    *) die "no prebuilt residue for $(uname -m); see https://github.com/$REPO/blob/main/docs/installation.md" ;;
  esac
  # An x86_64 shell under Rosetta still gets the native Apple silicon build.
  [ "$os" = Darwin ] && [ "$(sysctl -n sysctl.proc_translated 2>/dev/null)" = 1 ] && cpu=aarch64
  case $os in
    Darwin) echo "$cpu-apple-darwin" ;;
    Linux) echo "$cpu-unknown-linux-musl" ;;
    FreeBSD) echo "$cpu-unknown-freebsd" ;;
    *) die "no prebuilt residue for $os; see https://github.com/$REPO/blob/main/docs/installation.md" ;;
  esac
}

install_archive() {
  triple=$(target)
  if [ -z "$tag" ]; then
    # The latest release, from the redirect GitHub answers: no API, no limit.
    tag=$(curl --proto '=https' --tlsv1.2 -fsSLI -o /dev/null -w '%{url_effective}' \
      "https://github.com/$REPO/releases/latest")
    tag=${tag##*/}
    case $tag in v[0-9]*) ;; *) die "could not find the latest release of $REPO" ;; esac
  fi
  bin=$prefix/residue
  updates="run this script again"
  undo="rm $bin"
  if [ -x "$bin" ] && [ "$("$bin" --version 2>/dev/null)" = "residue ${tag#v}" ]; then
    say "residue ${tag#v} is already installed at $bin."
    return 0
  fi
  name=residue-$tag-$triple
  base=https://github.com/$REPO/releases/download/$tag
  if [ "$dry_run" = 1 ]; then
    say "+ download $base/$name.tar.gz and SHA256SUMS; check the checksum and attestation"
  else
    say "Downloading $name.tar.gz"
    fetch "$base/$name.tar.gz" "$tmp/$name.tar.gz"
    fetch "$base/SHA256SUMS" "$tmp/SHA256SUMS"
    want=$(awk -v f="$name.tar.gz" '$2 == f { print $1 }' "$tmp/SHA256SUMS")
    [ -n "$want" ] || die "SHA256SUMS of $tag lists no $name.tar.gz"
    [ "$(sha256 "$tmp/$name.tar.gz")" = "$want" ] || die "$name.tar.gz does not match SHA256SUMS; nothing was installed"
    say "Checksum: matches SHA256SUMS"
    if ! has gh; then
      attestation="skipped (gh is not installed)"
    elif ! gh auth status >/dev/null 2>&1; then
      attestation="skipped (gh is not logged in)"
    elif gh attestation verify "$tmp/$name.tar.gz" --repo "$REPO" >/dev/null; then
      attestation="verified, built by $REPO's release workflow"
    else
      die "the build-provenance attestation of $name.tar.gz did not verify; nothing was installed"
    fi
    say "Attestation: $attestation"
    tar -xzf "$tmp/$name.tar.gz" -C "$tmp"
    [ "$("$tmp/$name/residue" --version)" = "residue ${tag#v}" ] || die "the downloaded residue does not run here"
  fi
  run mkdir -p "$prefix"
  # Copy beside it, then rename over it: a running residue keeps its file.
  run cp "$tmp/$name/residue" "$prefix/.residue.new"
  run chmod 0755 "$prefix/.residue.new"
  run mv -f "$prefix/.residue.new" "$bin"
}

# A dotted version is at least another: is $1 >= $2?
at_least() { [ "$(printf '%s\n%s\n' "$1" "$2" | sort -t. -k1,1n -k2,2n -k3,3n | head -n1)" = "$2" ]; }

start_service() {
  # Before 0.0.7, `service install` needs the folder that holds the repositories.
  turn_on="$bin service install"
  if [ "$dry_run" = 0 ]; then
    version=$("$bin" --version)
    version=${version#residue }
    at_least "$version" "$SERVICE_NO_ROOT_SINCE" || turn_on="$turn_on ~/src"
  fi
  if [ "$no_service" = 1 ]; then service="not turned on (--no-service). To turn it on: $turn_on"; return 0; fi
  case $os in Darwin | Linux) ;; *) service="not available on $os. Run \`$bin watch ROOT\` from your own scheduler"; return 0 ;; esac
  # An installed service keeps the folders it was given: never replace it.
  if [ -x "$bin" ] && "$bin" service status >/dev/null 2>&1; then
    service="on already, left as it is, with its own folders. To restart it on this version, run \`$bin service install\` with them"
    return 0
  fi
  if [ "$dry_run" = 1 ]; then run "$bin" service install; service="would be turned on"; return 0; fi
  if [ "$turn_on" != "$bin service install" ]; then
    service="not turned on: residue $version needs the folder that holds your repositories. Run: $turn_on (your folder)"
  elif run "$bin" service install; then
    service="on: \`residue watch\` runs from login ($bin service status)"
  else
    service="NOT turned on: \`$bin service install\` failed above. Run it again with the folder that holds your repositories"
    failed=1
  fi
}

summary() {
  say ""
  if [ "$dry_run" = 1 ]; then say "Dry run: nothing was changed. Cleanup: $service"; return 0; fi
  say "Installed:  $("$bin" --version) at $bin"
  say "Channel:    $channel. Updates: $updates"
  say "Cleanup:    $service"
  say "Undo:       $bin service uninstall; then $undo"
  found=$(command -v residue 2>/dev/null || true)
  # Compare real directories: /usr/sbin may be a link to /usr/bin.
  [ -z "$found" ] || found=$(cd -P "$(dirname "$found")" && pwd)/residue
  if [ -z "$found" ]; then
    say ""
    say "$(dirname "$bin") is not on your PATH. Add it, for example in ~/.profile or your shell's rc file:"
    say "  export PATH=\"$(dirname "$bin"):\$PATH\""
  elif [ "$found" != "$(cd -P "$(dirname "$bin")" && pwd)/residue" ]; then
    say ""
    say "Note: \`residue\` on your PATH is $found, which comes before $bin."
  fi
}

main() {
  parse_args "$@"
  tmp=$(mktemp -d 2>/dev/null || mktemp -d -t residue)
  trap 'rm -rf "$tmp"' EXIT
  trap 'exit 130' INT TERM
  failed=0
  choose_channel
  say "Installing residue through: $channel"
  case $channel in
    brew) install_brew ;;
    apt) install_apt ;;
    dnf) install_dnf ;;
    pacman) install_pacman ;;
    archive) install_archive ;;
  esac
  start_service
  summary
  exit "$failed"
}

main "$@"
