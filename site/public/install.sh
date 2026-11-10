#!/bin/sh
# Install residue and turn on automatic cleanup.
#
#   curl -fsSL https://residue.whiteheap.com/install.sh | sh
#   curl -fsSL https://residue.whiteheap.com/install.sh | sh -s -- --help
#
# It installs or upgrades residue through the best channel here: Homebrew on
# macOS; the signed apt, dnf or pacman repository on Linux when it can act as
# root; otherwise the release archive, checked against SHA256SUMS and, when gh
# is logged in, its build-provenance attestation from the workflow that builds
# it. When the latest release has no archive for this machine yet (FreeBSD's
# arrive minutes after a release), it takes the newest release that has one
# and says so. Then it runs `residue service install`, and prints what it did
# and how to undo it. Each command that changes the machine is printed (+ ...)
# before it runs. Everything is in functions called on the last line, so a
# download cut short runs nothing.
# Source: https://github.com/whiteheap/residue-cli/blob/main/site/public/install.sh
set -eu

REPO=whiteheap/residue-cli
# Releases up to 0.0.10 were built before the rename, so their attestations
# name the old repository (#127).
OLD_REPO=whiteheap/residue
OLD_REPO_UNTIL=0.0.10
# The package repositories keep the old name's address (#127).
PAGES=https://whiteheap.github.io/residue
# The package signing key (docs/installation.md). The two files it is
# published as are pinned by SHA-256; site/check.sh checks both pins against
# packaging/signing-key.asc.
KEY_FPR=1AEAB31435118A1FE575D36EE1553632FC32554A
KEY_ASC_SHA256=f63c31e514d7f8b87a2a61e7fe4833d6708c734fe3093286a505055482471554
KEY_GPG_SHA256=1347e2c9b519414386f5997651a6bae055d0df86eb002b6e2b95ea946172e2cc
# Where dnf finds that key: the repository file names this local file, not a URL.
RPM_KEY=/etc/pki/rpm-gpg/RPM-GPG-KEY-residue
# residue's pacman section requires the database's signature too, which
# pacman's default, DatabaseOptional, does not.
SIGLEVEL='SigLevel = Required DatabaseRequired'
# /etc/pacman.conf as it was before install.sh added SIGLEVEL to an older
# [residue] section. Not .pacnew, .pacsave or .pacorig: pacdiff takes those
# for pacman's own and offers to merge them.
PACMAN_BAK=/etc/pacman.conf.residue-bak
# The first release whose `residue service install` needs no folder (#28).
SERVICE_NO_ROOT_SINCE=0.0.7
# residue needs Git 2.36 or later: src/git.rs MIN_VERSION, and the packages'
# dependency in packaging/nfpm.yaml.
GIT_NEEDS=2.36
# What the service it turns on removes: `residue watch`'s defaults, since it
# runs `service install` with no options (src/watch.rs, src/cli.rs; site/check.sh
# checks each). The same since 0.0.7, the first release it turns on.
WATCH_GRACE=1h
WATCH_MIN_FREE=10
WATCH_IDLE=2d
KEEP_REMOVED=30d

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

# The same, but it prints the HTTP status and leaves what a 404 means to the
# caller. 000 means no server answered; curl says why on stderr.
fetch_status() { curl --proto '=https' --tlsv1.2 -sSL --retry 3 -o "$2" -w '%{http_code}' "$1" || true; }

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
# terminal. Sets $sudo; fails when none of them works. $1, when given, runs
# right before the password prompt: a check that needs no root, so a refusal
# comes before the user types a password.
can_root() {
  sudo=
  [ "$(id -u)" = 0 ] && return 0
  has sudo || return 1
  sudo=sudo
  [ "$dry_run" = 1 ] && return 0
  sudo -n true 2>/dev/null && return 0
  has_tty || return 1
  [ -z "${1:-}" ] || "$1"
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
      elif has apt-get && can_root apt_git_ok; then channel=apt
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

# The package depends on git (>= 1:2.36), so on a release whose apt offers an
# older Git (Ubuntu 22.04 has 2.34) apt-get would stop after the repository
# is added. Stop before anything under /etc instead, and before sudo asks
# for a password (`can_root apt_git_ok`): it needs no root. With no package
# lists yet there is no candidate to read, and apt-get decides.
apt_git_ok() {
  cand=$(apt-cache policy git 2>/dev/null | sed -n 's/^ *Candidate: *//p')
  case $cand in '' | '(none)') return 0 ;; esac
  dpkg --compare-versions "$cand" ge "1:$GIT_NEEDS" && return 0
  say "install.sh: apt offers Git $cand here, and residue needs Git $GIT_NEEDS or later. Nothing was changed." >&2
  say "Add a newer Git first; on Ubuntu 22.04: sudo add-apt-repository ppa:git-core/ppa, then run this again." >&2
  exit 1
}

install_apt() {
  apt_git_ok
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
  # Read only: what to do about [residue], decided before anything changes.
  pacman_plan
  [ "$dry_run" = 1 ] || fetch_key "$PAGES/residue-signing-key.asc" "$tmp/residue.asc" "$KEY_ASC_SHA256"
  as_root pacman-key --add "$tmp/residue.asc"
  as_root pacman-key --lsign-key "$KEY_FPR"
  case $plan in append | edit) write_pacman_conf ;; esac
  # Arch supports only full upgrades, so this is `pacman -Syu`, as in the
  # docs. With a terminal, pacman shows the transaction and asks first.
  if [ "$dry_run" = 0 ] && has_tty; then
    as_root pacman -Syu --needed residue </dev/tty
  else
    as_root pacman -Syu --needed --noconfirm residue
  fi
  bin=/usr/bin/residue
  updates="pacman -Syu"
  undo="${sudo:+sudo }pacman -R residue, and remove the [residue] section from /etc/pacman.conf${edited:+ ($edited is that file as it was before install.sh changed it)}"
}

# What to do about the [residue] section of /etc/pacman.conf. Sets $plan:
# append (there is none), edit (residue's own, with no SigLevel: add
# SIGLEVEL), or keep, with a $note when what is kept does not require
# residue's database signature. pacman reads the file as pacman-conf prints
# it: it trims each line, skips a blank one or one that starts with #, and
# takes # nowhere else as a comment. Each change is checked with pacman-conf
# on a copy first, and anything that stops the script stops here, before
# anything changes.
pacman_plan() {
  plan=keep edited=
  [ -r /etc/pacman.conf ] || die "/etc/pacman.conf is not readable by $(id -un), so install.sh cannot check it. Nothing was changed."
  stray=$(awk '{ s = $0; gsub(/^[[:space:]]+|[[:space:]]+$/, "", s) }
    s ~ /^\[residue\]/ && s != "[residue]" { print s; exit }' /etc/pacman.conf)
  [ -z "$stray" ] || die "pacman does not read \"$stray\" in /etc/pacman.conf as a section: # starts a comment only at the start of a line. Make that line [residue], then run this again. Nothing was changed."
  headers=$(awk '{ s = $0; gsub(/^[[:space:]]+|[[:space:]]+$/, "", s) } s == "[residue]" { n++ } END { print n + 0 }' /etc/pacman.conf)
  printf '\n[residue]\n%s\nServer = %s/arch/%sarch\n' "$SIGLEVEL" "$PAGES" '$' >"$tmp/residue.section"
  if ! has pacman-conf; then
    if [ "$headers" = 0 ]; then plan=append; cat /etc/pacman.conf "$tmp/residue.section" >"$tmp/pacman.conf"
    else note="this pacman has no pacman-conf to check /etc/pacman.conf with, so [residue] was left as it is. For pacman to require residue's database signature, add this line under [residue]: $SIGLEVEL"
    fi
    return 0
  fi
  pacman-conf --config /etc/pacman.conf --repo-list >"$tmp/repos" 2>"$tmp/pacman-conf.err" ||
    die "pacman cannot read /etc/pacman.conf: $(head -n 1 "$tmp/pacman-conf.err"). Fix it, then run this again. Nothing was changed."
  sections=$(grep -cx residue "$tmp/repos") || true
  if [ "$sections" -gt 1 ] || [ "$headers" -gt 1 ]; then
    note="/etc/pacman.conf has more than one [residue] section, left as they are. For pacman to require residue's database signature, keep one, with this line under it: $SIGLEVEL"
  elif [ "$sections" = 0 ]; then
    [ "$headers" = 0 ] || die "pacman does not read [residue] in /etc/pacman.conf as a repository. Fix it, then run this again. Nothing was changed."
    cat /etc/pacman.conf "$tmp/residue.section" >"$tmp/pacman.conf"
    reads_as_intended /etc/pacman.conf "$tmp/pacman.conf" ||
      die "pacman would not read residue's section as intended at the end of /etc/pacman.conf, so nothing was changed."
    plan=append
  elif [ "$headers" = 0 ]; then
    note="[residue] is in a file /etc/pacman.conf includes, left as it is. For pacman to require residue's database signature, add this line under [residue] there: $SIGLEVEL"
  elif sig=$(pacman-conf --config /etc/pacman.conf --repo residue SigLevel 2>/dev/null) && [ -n "$sig" ]; then
    # A SigLevel of the user's own, in the section or a file it includes.
    printf '%s\n' "$sig" | grep -qx DatabaseRequired ||
      note="[residue] in /etc/pacman.conf has a SigLevel of its own, left as it is, and it does not require residue's database signature. This line under [residue] would: $SIGLEVEL"
  elif [ -L /etc/pacman.conf ]; then
    note="/etc/pacman.conf is a link to $(readlink /etc/pacman.conf), and install.sh does not edit through a link, so [residue] was left as it is. For pacman to require residue's database signature, add this line under [residue] there: $SIGLEVEL"
  else
    # residue's own section, from an older install.sh or the docs, with no
    # SigLevel: SIGLEVEL goes right under its header, and nothing else
    # changes. A second run finds it and changes nothing.
    awk -v line="$SIGLEVEL" '{ print; s = $0; cr = (s ~ /\r$/) ? "\r" : ""; gsub(/^[[:space:]]+|[[:space:]]+$/, "", s) }
      s == "[residue]" { print line cr }' /etc/pacman.conf >"$tmp/pacman.conf"
    reads_as_intended /etc/pacman.conf "$tmp/pacman.conf" ||
      die "with \"$SIGLEVEL\" under [residue], pacman would not read /etc/pacman.conf as intended, so nothing was changed. Add the line there by hand."
    plan=edit
  fi
}

# Does pacman read the pacman.conf $2 as $1, but with residue's database
# signature required? The global SigLevel, and each other repository and
# its SigLevel, must be the same in both, and residue's must name
# DatabaseRequired.
reads_as_intended() {
  pacman-conf --config "$2" --repo residue SigLevel 2>/dev/null | grep -qx DatabaseRequired || return 1
  before=$(pacman_sigs "$1") && after=$(pacman_sigs "$2") && [ "$before" = "$after" ]
}

# pacman's reading of the pacman.conf $1, residue aside: the global SigLevel,
# then each other repository's, one line each.
pacman_sigs() {
  repos=$(pacman-conf --config "$1" --repo-list 2>/dev/null) || return 1
  printf 'global: %s\n' "$(pacman-conf --config "$1" SigLevel 2>/dev/null | tr '\n' ' ')"
  for repo in $repos; do
    [ "$repo" = residue ] || printf '%s: %s\n' "$repo" "$(pacman-conf --config "$1" --repo "$repo" SigLevel 2>/dev/null | tr '\n' ' ')"
  done
}

# Put $tmp/pacman.conf, the file pacman_plan checked with pacman-conf, in
# place of /etc/pacman.conf, keeping the file as it was as PACMAN_BAK, or
# PACMAN_BAK.1 and so on, never over an older one. The new file is written
# beside it with its mode and owner, then renamed over it: pacman never reads
# a half-written file, and a write that fails, as on a full disk, removes
# what it wrote and leaves /etc/pacman.conf as it was. A link stays a link:
# the file it points at is replaced, by a rename in that file's directory.
write_pacman_conf() {
  conf=/etc/pacman.conf real=/etc/pacman.conf bak=$PACMAN_BAK n=0
  if [ -L "$conf" ]; then
    real=$(readlink -f "$conf") || die "cannot tell what the link $conf points at. Nothing was changed."
  fi
  while [ -e "$bak" ] || [ -L "$bak" ]; do n=$((n + 1)); bak=$PACMAN_BAK.$n; done
  cp "$conf" "$tmp/pacman.conf.before"
  case $plan in
    append) say "+ add residue's section at the end of $conf; the file as it is now is kept as $bak" ;;
    edit) say "+ add \"$SIGLEVEL\" under [residue] in $conf; the file as it is now is kept as $bak" ;;
  esac
  [ "$real" = "$conf" ] || say "  ($conf is a link to $real, which is replaced; the link stays)"
  # shellcheck disable=SC2016 # $1 to $4 belong to the inner sh.
  as_root sh -c 'cp -p "$1" "$2" && cp -p "$1" "$3" && cat "$4" >"$3" && mv -f "$3" "$1" || { rm -f "$2" "$3"; exit 1; }' \
    sh "$real" "$bak" "$real.residue-new" "$tmp/pacman.conf" ||
    die "could not write $conf, as the error above says, so it is unchanged."
  edited=$bak
  # A pacman before 5.1 has no pacman-conf to check with.
  { [ "$dry_run" = 0 ] && has pacman-conf; } || return 0
  reads_as_intended "$tmp/pacman.conf.before" "$conf" && return 0
  # shellcheck disable=SC2016 # $1 to $3 belong to the inner sh.
  as_root sh -c 'cp -p "$1" "$2" && mv -f "$2" "$3" || { rm -f "$2"; exit 1; }' sh "$bak" "$real.residue-new" "$real" ||
    die "pacman does not read the new $conf as intended, and putting it back failed. Put it back with: ${sudo:+sudo }cp -p $bak $real"
  die "pacman does not read the new $conf as intended, so it was put back from $bak. Add residue's section by hand, as docs/installation.md shows."
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

# Does release $tag list this machine's archive? Downloads its SHA256SUMS to
# $tmp/SHA256SUMS, sets $name, $base and $want (the archive's checksum), and
# fails when SHA256SUMS has no line for it. A FreeBSD archive arrives some
# minutes after its release is published (packages.yml), with that line.
listed() {
  name=residue-$tag-$triple
  base=https://github.com/$REPO/releases/download/$tag
  code=$(fetch_status "$base/SHA256SUMS" "$tmp/SHA256SUMS")
  case $code in
    200) ;;
    404) if [ "$exact" = 1 ]; then die "there is no release $tag of residue. The releases: https://github.com/$REPO/releases"; fi
      die "release $tag has no SHA256SUMS right now; it may be in the middle of an update. Run this script again in a few minutes." ;;
    *) die "could not download $base/SHA256SUMS (HTTP $code). Nothing was installed." ;;
  esac
  want=$(awk -v f="$name.tar.gz" '$2 == f { print $1 }' "$tmp/SHA256SUMS")
  [ -n "$want" ]
}

# Set $older to the newest release other than $latest whose assets include
# this machine's archive, from one GitHub API call that lists releases, newest
# first. $latest is skipped: packages.yml uploads its archives a second before
# the SHA256SUMS that names them, so $latest can list the archive while
# `listed` fails. Empty when the call fails ($api is its HTTP status) or none
# of them has it.
find_older() {
  older=
  api=$(curl --proto '=https' --tlsv1.2 -sSL -o "$tmp/releases.json" -w '%{http_code}' \
    "https://api.github.com/repos/$REPO/releases?per_page=10") || true
  [ "$api" = 200 ] || return 0
  url=$(grep -o "\"browser_download_url\": *\"https://github.com/$REPO/releases/download/v[0-9][0-9.]*/residue-v[0-9][0-9.]*-$triple\.tar\.gz\"" \
    "$tmp/releases.json" | grep -Fv "/download/$latest/" | head -n 1)
  url=${url%/*}
  older=${url##*/}
}

install_archive() {
  triple=$(target)
  if [ -n "$tag" ]; then
    exact=1
  else
    exact=0
    # The latest release, from the redirect GitHub answers: no API, no limit.
    tag=$(curl --proto '=https' --tlsv1.2 -fsSLI -o /dev/null -w '%{url_effective}' \
      "https://github.com/$REPO/releases/latest")
    tag=${tag##*/}
    case $tag in v[0-9]*) ;; *) die "could not find the latest release of $REPO" ;; esac
  fi
  if ! listed; then
    # --version is exact: never another release.
    [ "$exact" = 0 ] || die "residue $tag has no archive for $triple. The releases and their archives: https://github.com/$REPO/releases"
    # The latest release has no archive for this machine yet. The apt, dnf and
    # pacman repositories still serve the previous release then, so install
    # the newest release that has one, as they do.
    latest=$tag
    late=
    case $triple in *-freebsd) late=" FreeBSD archives are built after a release is published and are usually there within half an hour." ;; esac
    find_older
    if [ -z "$older" ]; then
      if [ "$api" = 200 ]; then why="none of the 10 newest releases has one"
      elif [ "${api:-000}" = 000 ]; then why="GitHub's API did not answer which earlier release has one"
      else why="GitHub's API did not say which earlier release has one (HTTP $api)"
      fi
      say "install.sh: residue $latest has no archive for $triple yet, and $why.$late" >&2
      say "Run this script again later, or install an earlier release now by its tag (https://github.com/$REPO/releases):" >&2
      say "  curl -fsSL https://residue.whiteheap.com/install.sh | sh -s -- --version vX.Y.Z" >&2
      exit 1
    fi
    tag=$older
    listed || die "SHA256SUMS of $tag lists no $name.tar.gz"
    say "residue $latest has no archive for $triple yet.$late Using $tag, the newest release that has one. To update to $latest, run this script again later."
  fi
  # Who attests the archive: release.yml at the tag for the macOS and Linux
  # ones, packages.yml on main for FreeBSD's (docs/installation.md).
  case $triple in
    *-freebsd) workflow=packages.yml ref=refs/heads/main ;;
    *) workflow=release.yml ref=refs/tags/$tag ;;
  esac
  bin=$prefix/residue
  updates="run this script again"
  undo="rm $bin"
  if [ -x "$bin" ] && [ "$("$bin" --version 2>/dev/null)" = "residue ${tag#v}" ]; then
    say "residue ${tag#v} is already installed at $bin."
    return 0
  fi
  if [ "$dry_run" = 1 ]; then
    say "+ download $base/$name.tar.gz; check it against SHA256SUMS and its attestation from $workflow at $ref"
  else
    say "Downloading $name.tar.gz"
    code=$(fetch_status "$base/$name.tar.gz" "$tmp/$name.tar.gz")
    case $code in
      200) ;;
      # SHA256SUMS names it, so it is seconds away.
      404) die "$name.tar.gz is not on release $tag yet, though its SHA256SUMS names it. Run this script again in a minute." ;;
      *) die "could not download $base/$name.tar.gz (HTTP $code). Nothing was installed." ;;
    esac
    [ "$(sha256 "$tmp/$name.tar.gz")" = "$want" ] || die "$name.tar.gz does not match SHA256SUMS; nothing was installed"
    say "Checksum: matches SHA256SUMS"
    # Up to OLD_REPO_UNTIL the old name first, then REPO, for an archive
    # attested again after the rename (a packages.yml re-run). Each verify
    # takes seconds, so a later release tries only REPO. gh's reasons are kept
    # for the error. A gh without `gh attestation` (before 2.49) stops with
    # a plain message, where it stopped with gh's own error before.
    if ! has gh; then
      attestation="skipped (gh is not installed)"
    elif ! gh auth status >/dev/null 2>&1; then
      attestation="skipped (gh is not logged in)"
    elif ! gh attestation verify --help 2>/dev/null | grep -q -- --cert-identity; then
      die "this gh cannot check which workflow built $name.tar.gz; gh 2.49 or later can. Update gh, then run this again. Nothing was installed."
    elif at_least "$OLD_REPO_UNTIL" "${tag#v}" && attested_by "$OLD_REPO"; then
      attestation="verified, built by $OLD_REPO's $workflow at $ref ($REPO's name up to $OLD_REPO_UNTIL)"
    elif attested_by "$REPO"; then
      attestation="verified, built by $REPO's $workflow at $ref"
    else
      cat "$tmp/attestation.err" >&2
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

# Did $1's $workflow at $ref attest the archive, on a GitHub-hosted runner?
# --repo alone would accept any workflow in that repository, at any ref.
attested_by() {
  gh attestation verify "$tmp/$name.tar.gz" --repo "$1" --deny-self-hosted-runners \
    --cert-identity "https://github.com/$1/.github/workflows/$workflow@$ref" >/dev/null 2>>"$tmp/attestation.err"
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
  # The service would stop at every pass on an older Git. The apt, dnf and
  # pacman packages depend on a new enough one, so a dry run through them has
  # none to check yet; Homebrew's formula and the archive bring none.
  if { [ "$dry_run" = 0 ] || [ "$channel" = brew ] || [ "$channel" = archive ]; } && ! git_ok; then
    if [ -n "$git_v" ]; then git_found="git here is $git_v"
    elif has git; then git_found="\`git --version\` does not say which Git this is"
    else git_found="no git is on PATH"
    fi
    if [ "$dry_run" = 1 ]; then
      service="would not be turned on: residue needs Git $GIT_NEEDS or later, and $git_found"
      return 0
    fi
    service="NOT turned on: residue needs Git $GIT_NEEDS or later, and $git_found. Install a newer Git, then run: $turn_on"
    failed=1
    return 0
  fi
  if [ "$dry_run" = 1 ]; then run "$bin" service install; service="would be turned on"; return 0; fi
  if [ "$turn_on" != "$bin service install" ]; then
    service="not turned on: residue $version needs the folder that holds your repositories. Run: $turn_on (your folder)"
  elif run "$bin" service install; then
    service="on: \`residue watch\` runs from login ($bin service status)"
    turned_on=1
  else
    service="NOT turned on: \`$bin service install\` failed above; fix what it names, then run it again"
    failed=1
  fi
}

# Is `git --version` at least $GIT_NEEDS? Sets $git_v to its major.minor,
# empty when there is no git or its version does not read.
git_ok() {
  git_v=$(git --version 2>/dev/null | sed -n 's/^git version \([0-9][0-9]*\.[0-9][0-9]*\).*/\1/p') || true
  [ -n "$git_v" ] && at_least "$git_v.0" "$GIT_NEEDS.0"
}

summary() {
  say ""
  if [ "$dry_run" = 1 ]; then say "Dry run: nothing was changed. Cleanup: $service"; return 0; fi
  say "Installed:  $("$bin" --version) at $bin"
  say "Channel:    $channel. Updates: $updates"
  say "Cleanup:    $service"
  if [ "$turned_on" = 1 ]; then
    say "Removes:    a worktree whose upstream branch is gone, once unchanged $WATCH_GRACE;"
    say "            below $WATCH_MIN_FREE% free, worktrees unchanged $WATCH_IDLE, largest first"
    say "Deletes:    without the trash; \`residue restore\` brings one back for $KEEP_REMOVED"
    say "Preview:    $bin watch --once --dry-run. Turn it off: $bin service uninstall"
  fi
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

# Stop before any channel on a machine residue has no build for: a package
# manager would add residue's repository first and fail after.
check_machine() {
  case $(uname -s) in
    MINGW* | MSYS* | CYGWIN*) die "residue does not run on Windows; run this inside WSL" ;;
  esac
  case $(uname -m) in
    x86_64 | amd64 | arm64 | aarch64) ;;
    *) die "no prebuilt residue for $(uname -m); to build it from source: https://github.com/$REPO/blob/main/docs/installation.md#from-source" ;;
  esac
}

main() {
  parse_args "$@"
  check_machine
  has curl || die "curl is needed to download residue; install it, then run this again"
  tmp=$(mktemp -d 2>/dev/null || mktemp -d -t residue)
  trap 'rm -rf "$tmp"' EXIT
  trap 'exit 130' INT TERM
  failed=0 turned_on=0 note=
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
  if [ -n "$note" ]; then say ""; say "Note: $note"; fi
  exit "$failed"
}

main "$@"
