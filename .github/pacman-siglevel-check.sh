#!/bin/sh
# What install.sh does to /etc/pacman.conf, witnessed as root in the Arch
# container of .github/workflows/install.yml.
#
#   sh pacman-siglevel-check.sh seed               # before install.sh first runs
#   sh pacman-siglevel-check.sh check INSTALL_SH   # after its dry run and first run
#   sh pacman-siglevel-check.sh full INSTALL_SH    # alone, in a container that may mount
#
# seed adds a [residue] section as an older install.sh and the docs wrote it,
# with no SigLevel. check then shows, as pacman-conf reads each file:
# - the dry run named the change and made none; the run put SIGLEVEL under
#   that header, once, changed nothing else, and kept the file as it was;
# - a second run changes nothing;
# - an older backup is kept, and the next one gets a free name;
# - a SigLevel of the user's own, two [residue] sections, an Include that
#   sets a SigLevel, and a pacman.conf that is a link are left alone, each
#   with a note;
# - on the stock Arch layout, where [extra] is the last section, a header
#   followed by "# comment" stops the script with nothing changed, and an
#   old section gets SIGLEVEL with every other repository's SigLevel the same;
# - pacman takes an unsigned residue.db under the old section and refuses it
#   under the new one;
# - a fresh install writes the section with SIGLEVEL, keeping a backup.
# The dry run's output is in /tmp/dry.log. full puts /etc on a small tmpfs
# and fills it right before install.sh writes pacman.conf: on the edit path
# and on a fresh install, with no page free and with one, the script stops,
# says pacman.conf is unchanged and offers no restore, and pacman.conf is as
# it was, with no backup or new file left.
set -eu

conf=/etc/pacman.conf
bak=/etc/pacman.conf.residue-bak
siglevel='SigLevel = Required DatabaseRequired'
fail() { echo "pacman-siglevel-check: $*" >&2; exit 1; }
sum() { cksum < "$1"; }
run() { # LOG [OPTION]: install.sh, showing its output only when it fails
  log=$1; shift
  sh "$install" --no-service "$@" > "$log" 2>&1 || { cat "$log"; fail "install.sh failed"; }
}
# pacman's reading of a pacman.conf, residue aside: every SigLevel but residue's.
sigs() {
  printf 'global: %s\n' "$(pacman-conf --config "$1" SigLevel | tr '\n' ' ')"
  for repo in $(pacman-conf --config "$1" --repo-list); do
    [ "$repo" = residue ] || printf '%s: %s\n' "$repo" "$(pacman-conf --config "$1" --repo "$repo" SigLevel | tr '\n' ' ')"
  done
}
required() { pacman-conf --config "$conf" --repo residue SigLevel | grep -qx DatabaseRequired; }
# A dry run must leave $conf alone and print NOTE's first words, with no edit.
kept() { # LABEL NOTE
  before=$(sum "$conf")
  run /tmp/kept.log --dry-run
  [ "$(sum "$conf")" = "$before" ] || fail "$1: the dry run changed $conf"
  if grep -q '^+ add ' /tmp/kept.log; then cat /tmp/kept.log; fail "$1: install.sh would edit $conf"; fi
  grep -qF "Note: $2" /tmp/kept.log || { cat /tmp/kept.log; fail "$1: no note"; }
  echo "ok: $1 is left alone, with a note"
}

case ${1:-} in
  seed)
    cp "$conf" /tmp/pacman.conf.stock
    # shellcheck disable=SC2016 # $arch is pacman's.
    printf '\n[residue]\nServer = https://whiteheap.github.io/residue/arch/$arch\n' >> "$conf"
    cp "$conf" /tmp/pacman.conf.old
    exit 0 ;;
  check) install=$2 ;;
  full) install=$2 ;;
  *) fail "usage: $0 seed | check INSTALL_SH | full INSTALL_SH" ;;
esac

if [ "$1" = full ]; then
  pacman-key --init > /dev/null 2>&1
  pacman-key --populate archlinux > /dev/null 2>&1
  if ! { cp -a /etc /root/etc && mount -t tmpfs -o size=64m tmpfs /etc && cp -a /root/etc/. /etc/; }; then
    fail "could not put /etc on a tmpfs"
  fi
  # install.sh writes pacman.conf right after its last pacman-key: this one
  # then fills /etc, leaving $FREE pages free.
  cat > /usr/local/bin/pacman-key <<'WRAP'
#!/bin/sh
/usr/bin/pacman-key "$@" || exit
if [ "$1" = --lsign-key ]; then
  i=0; while [ "$i" -lt "$FREE" ]; do head -c 4096 /dev/zero > "/etc/zz-free-$i"; i=$((i + 1)); done
  dd if=/dev/zero of=/etc/zz-fill bs=4096 2> /dev/null
  rm -f /etc/zz-free-*
fi
WRAP
  chmod +x /usr/local/bin/pacman-key
  cp "$conf" /tmp/pacman.conf.image
  for free in 0 1; do
    for path in edit fresh; do
      cp /tmp/pacman.conf.image "$conf"
      # shellcheck disable=SC2016 # $arch is pacman's.
      [ "$path" = fresh ] || printf '\n[residue]\nServer = https://whiteheap.github.io/residue/arch/$arch\n' >> "$conf"
      before=$(sum "$conf")
      if FREE=$free sh "$install" --no-service > /tmp/full.log 2>&1; then cat /tmp/full.log; fail "$path, $free page free: install.sh did not stop"; fi
      rm -f /etc/zz-*
      grep -q 'could not write /etc/pacman.conf, .* so it is unchanged' /tmp/full.log || { cat /tmp/full.log; fail "$path, $free page free: it stopped for another reason"; }
      if grep -q 'Put it back' /tmp/full.log; then cat /tmp/full.log; fail "$path, $free page free: it offered a restore"; fi
      [ "$(sum "$conf")" = "$before" ] || fail "$path, $free page free: pacman.conf changed"
      if ls /etc/pacman.conf.residue-* 2> /dev/null; then fail "$path, $free page free: a backup or new file was left"; fi
      pacman-conf --repo-list > /dev/null || fail "$path, $free page free: pacman cannot read pacman.conf"
      echo "ok: $path, $free page free: install.sh stops, pacman.conf is as it was, and nothing is left"
    done
  done
  exit 0
fi

# shellcheck disable=SC2016 # $arch is pacman's.
server='Server = https://whiteheap.github.io/residue/arch/$arch'
# The stock Arch file ends with [extra]; the image adds an [options] after it.
awk '/^\[options\]$/ { n++ } n < 2' /tmp/pacman.conf.stock > /tmp/pacman.conf.arch

grep -qF "+ add \"$siglevel\" under [residue] in $conf" /tmp/dry.log || fail "the dry run did not show the change"
[ "$(sum "$bak")" = "$(sum /tmp/pacman.conf.old)" ] || fail "$bak is not the file as it was before the run"
grep -A1 '^\[residue\]$' "$conf" | grep -qxF "$siglevel" || fail "no $siglevel under [residue]"
[ "$(grep -cxF "$siglevel" "$conf")" = 1 ] || fail "$siglevel is in $conf more than once"
[ "$(grep -vxF "$siglevel" "$conf" | cksum)" = "$(sum /tmp/pacman.conf.old)" ] || fail "something else in $conf changed"
required || fail "pacman-conf does not read residue's SigLevel as DatabaseRequired"
[ "$(sigs "$conf")" = "$(sigs /tmp/pacman.conf.old)" ] || fail "another SigLevel changed"
echo "ok: the dry run named the change; the run added it under [residue], once, and $bak keeps the file as it was"

before=$(sum "$conf") kept=$(sum "$bak")
run /tmp/second.log
[ "$(sum "$conf")" = "$before" ] && [ "$(sum "$bak")" = "$kept" ] || fail "a second run changed $conf or $bak"
if grep -q '^Note:' /tmp/second.log; then cat /tmp/second.log; fail "a second run printed a note"; fi
echo "ok: a second run changes nothing"

cp /tmp/pacman.conf.old "$conf"
run /tmp/third.log
[ "$(sum "$bak")" = "$kept" ] || fail "the older backup was overwritten"
[ "$(sum "$bak.1")" = "$(sum /tmp/pacman.conf.old)" ] || fail "the next backup is not $bak.1"
grep -qF "kept as $bak.1" /tmp/third.log || fail "install.sh did not say where the backup is"
echo "ok: an older backup is kept, and the next one is $bak.1"

sed -i "s/^$siglevel\$/SigLevel = Optional TrustAll/" "$conf"
kept "a SigLevel of the user's own" "[residue] in /etc/pacman.conf has a SigLevel of its own"
mine=$(sum "$conf")
run /tmp/mine.log
[ "$(sum "$conf")" = "$mine" ] || fail "install.sh changed a SigLevel of the user's own"
sed -i "s/^SigLevel = Optional TrustAll\$/$siglevel/" "$conf"

cp /tmp/pacman.conf.old "$conf"
printf '\n[residue]\n%s\n' "$server" >> "$conf"
kept "two [residue] sections" "/etc/pacman.conf has more than one [residue] section"

printf 'SigLevel = Never\n%s\n' "$server" > /tmp/residue-include.conf
{ cat /tmp/pacman.conf.stock; printf '\n[residue]\nInclude = /tmp/residue-include.conf\n'; } > "$conf"
kept "an Include that sets a SigLevel" "[residue] in /etc/pacman.conf has a SigLevel of its own"

cp /tmp/pacman.conf.old /etc/pacman.conf.real
ln -sf pacman.conf.real "$conf"
kept "a pacman.conf that is a link" "/etc/pacman.conf is a link to pacman.conf.real"
[ -L "$conf" ] || fail "the link was replaced"
rm "$conf" /etc/pacman.conf.real

{ cat /tmp/pacman.conf.arch; printf '\n[residue] # whiteheap\n%s\n' "$server"; } > "$conf"
before=$(sum "$conf")
if sh "$install" --no-service > /tmp/comment.log 2>&1; then cat /tmp/comment.log; fail "a header followed by a comment did not stop the script"; fi
grep -q 'does not read "\[residue\] # whiteheap" .* as a section' /tmp/comment.log || { cat /tmp/comment.log; fail "it stopped for another reason"; }
[ "$(sum "$conf")" = "$before" ] || fail "a header followed by a comment changed $conf"
echo "ok: on the stock Arch layout, \"[residue] # whiteheap\" stops the script with nothing changed"

{ cat /tmp/pacman.conf.arch; printf '\n[residue]\n%s\n' "$server"; } > "$conf"
cp "$conf" /tmp/pacman.conf.arch-old
run /tmp/arch.log
required || fail "on the stock Arch layout, residue's SigLevel is not DatabaseRequired"
[ "$(sigs "$conf")" = "$(sigs /tmp/pacman.conf.arch-old)" ] || fail "on the stock Arch layout, another SigLevel changed"
pacman -Sy > /tmp/arch-sync.log 2>&1 || { cat /tmp/arch-sync.log; fail "pacman -Sy failed on the stock Arch layout"; }
echo "ok: on the stock Arch layout, the old section gets $siglevel, the other SigLevels stay, and pacman -Sy works"

# residue.db with no residue.db.sig beside it, served from a file, under the
# section as it was and as it is now.
arch=$(uname -m)
mkdir -p "/tmp/unsigned/$arch"
curl -fsSLo "/tmp/unsigned/$arch/residue.db" "https://whiteheap.github.io/residue/arch/$arch/residue.db"
# shellcheck disable=SC2016 # $arch is pacman's.
unsigned='Server = file:///tmp/unsigned/$arch'
for c in old new; do
  if [ "$c" = old ]; then from=/tmp/pacman.conf.old; else from=$conf; fi
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
grep -qF "+ add residue's section at the end of $conf" /tmp/fresh.log || fail "a fresh install did not say what it added"
fresh_bak=$(sed -n 's/.* the file as it is now is kept as //p' /tmp/fresh.log)
if [ -z "$fresh_bak" ] || [ "$(sum "$fresh_bak")" != "$(sum /tmp/pacman.conf.stock)" ]; then
  fail "a fresh install kept no backup of the file as it was"
fi
[ "$(sigs "$conf")" = "$(sigs /tmp/pacman.conf.stock)" ] || fail "a fresh install changed another SigLevel"
echo "ok: a fresh install writes [residue] with $siglevel, and $fresh_bak keeps the file as it was"
