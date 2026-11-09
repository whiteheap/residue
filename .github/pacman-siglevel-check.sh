#!/bin/sh
# What install.sh does to /etc/pacman.conf, witnessed as root in the Arch
# container of .github/workflows/install.yml.
#
#   sh pacman-siglevel-check.sh seed               # before install.sh first runs
#   sh pacman-siglevel-check.sh check INSTALL_SH   # after its dry run and first run
#
# seed adds a [residue] section as an older install.sh and the docs wrote it,
# with no SigLevel. check then shows that the dry run named the change and
# made none; that the run put SigLevel = Required DatabaseRequired under that
# header, once, changed nothing else and kept the file as it was; that a
# second run changes nothing; that a SigLevel of the user's own is left alone,
# with a note; that pacman takes an unsigned residue.db under the old section
# and refuses it under the new one; and that a fresh install writes the
# section with the line. The dry run's output is in /tmp/dry.log.
set -eu

conf=/etc/pacman.conf
bak=/etc/pacman.conf.residue-bak
siglevel='SigLevel = Required DatabaseRequired'
fail() { echo "pacman-siglevel-check: $*" >&2; exit 1; }
sum() { cksum < "$1"; }
run() { # LOG: install.sh, quietly, showing its output only when it fails
  sh "$install" --no-service > "$1" 2>&1 || { cat "$1"; fail "install.sh failed"; }
}

case ${1:-} in
  seed)
    cp "$conf" /tmp/pacman.conf.stock
    # shellcheck disable=SC2016 # $arch is pacman's.
    printf '\n[residue]\nServer = https://whiteheap.github.io/residue/arch/$arch\n' >> "$conf"
    cp "$conf" /tmp/pacman.conf.old
    exit 0 ;;
  check) install=$2 ;;
  *) fail "usage: $0 seed | check INSTALL_SH" ;;
esac

grep -qF "+ add \"$siglevel\" under [residue] in $conf" /tmp/dry.log || fail "the dry run did not show the change"
[ "$(sum "$bak")" = "$(sum /tmp/pacman.conf.old)" ] || fail "$bak is not the file as it was before the run"
grep -A1 '^\[residue\]$' "$conf" | grep -qxF "$siglevel" || fail "no $siglevel under [residue]"
[ "$(grep -cxF "$siglevel" "$conf")" = 1 ] || fail "$siglevel is in $conf more than once"
[ "$(grep -vxF "$siglevel" "$conf" | cksum)" = "$(sum /tmp/pacman.conf.old)" ] || fail "something else in $conf changed"
echo "ok: the dry run named the change; the run added it under [residue], once, and $bak keeps the file as it was"

before=$(sum "$conf") kept=$(sum "$bak")
run /tmp/second.log
[ "$(sum "$conf")" = "$before" ] && [ "$(sum "$bak")" = "$kept" ] || fail "a second run changed $conf or $bak"
if grep -q '^Note:' /tmp/second.log; then cat /tmp/second.log; fail "a second run printed a note"; fi
echo "ok: a second run changes nothing"

sed -i "s/^$siglevel\$/SigLevel = Optional TrustAll/" "$conf"
mine=$(sum "$conf")
run /tmp/mine.log
[ "$(sum "$conf")" = "$mine" ] || fail "install.sh changed a SigLevel of the user's own"
grep -q '^Note: \[residue\] in /etc/pacman.conf has a SigLevel of your own' /tmp/mine.log || fail "no note for a SigLevel of the user's own"
sed -i "s/^SigLevel = Optional TrustAll\$/$siglevel/" "$conf"
echo "ok: a SigLevel of the user's own is left alone, with a note"

# residue.db with no residue.db.sig beside it, served from a file, under the
# section as it was ($bak) and as it is now.
arch=$(uname -m)
mkdir -p "/tmp/unsigned/$arch"
curl -fsSLo "/tmp/unsigned/$arch/residue.db" "https://whiteheap.github.io/residue/arch/$arch/residue.db"
# shellcheck disable=SC2016 # $arch is pacman's.
unsigned='Server = file:///tmp/unsigned/$arch'
for c in old new; do
  if [ "$c" = old ]; then from=$bak; else from=$conf; fi
  sed "s|^Server = https://whiteheap.github.io/residue/arch/.*|$unsigned|" "$from" > "/tmp/$c-unsigned.conf"
  grep -qxF "$unsigned" "/tmp/$c-unsigned.conf" || fail "could not point $from at the unsigned copy"
done
rm -f /var/lib/pacman/sync/residue.*
pacman --config /tmp/old-unsigned.conf -Sy > /tmp/old-sync.log 2>&1 ||
  { cat /tmp/old-sync.log; fail "the old section did not take the unsigned residue.db, so the next check would show nothing"; }
rm -f /var/lib/pacman/sync/residue.*
if pacman --config /tmp/new-unsigned.conf -Sy > /tmp/new-sync.log 2>&1; then
  cat /tmp/new-sync.log
  fail "pacman took an unsigned residue.db under $siglevel"
fi
grep 'residue\.db\.sig' /tmp/new-sync.log || { cat /tmp/new-sync.log; fail "pacman refused the sync for another reason"; }
rm -f /var/lib/pacman/sync/residue.*
pacman -Sy > /dev/null
echo "ok: pacman takes the unsigned residue.db under the old section and refuses it under $siglevel"

cp /tmp/pacman.conf.stock "$conf"
run /tmp/fresh.log
grep -A1 '^\[residue\]$' "$conf" | grep -qxF "$siglevel" || fail "a fresh install wrote [residue] without $siglevel"
if grep -q '^+ add ' /tmp/fresh.log; then fail "a fresh install edited the section instead of writing it"; fi
echo "ok: a fresh install writes [residue] with $siglevel"
