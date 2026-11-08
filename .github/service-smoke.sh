#!/bin/sh
# Install, check and uninstall `residue service` on this machine: the
# launchd agent on macOS, the systemd user service on Linux. The service runs
# with --dry-run, so it never removes anything. It replaces and then
# uninstalls this user's residue service: run it on a CI runner, not on a
# machine whose service you use.
#
#   1. install with a ROOT (an empty folder) and --keep-removed 12w:
#      running, logging, and status shows the root and the 12w, which the
#      running watch logs too.
#   2. install again without a ROOT keeps that root, and --keep-removed
#      goes back to its default, 30d, with a note that says so.
#   3. after uninstall, install without a ROOT uses the usual code folders
#      in the home folder and marks them as such (a GitHub runner has
#      ~/work), and installing again finds them again. With none of the
#      folders it must be the one-line usage error, exit 2.
#   4. uninstall, and status says it is gone.
#
#   sh .github/service-smoke.sh PATH/TO/residue
set -eu

residue=$1
case $residue in /*) ;; *) residue="$PWD/$residue" ;; esac
root=$(mktemp -d "${TMPDIR:-/tmp}/residue-service.XXXXXX")
root=$(cd "$root" && pwd -P)
trap '"$residue" service uninstall > /dev/null 2>&1 || true; rm -rf "$root"' EXIT

fail() {
  echo "service-smoke: $1" >&2
  [ -n "${2-}" ] && printf '%s\n' "$2" >&2
  exit 1
}

if [ "$(uname)" = Linux ]; then
  # A CI runner has no systemd user manager until lingering is on.
  sudo loginctl enable-linger "$(id -un)"
  XDG_RUNTIME_DIR="/run/user/$(id -u)"
  export XDG_RUNTIME_DIR
  i=0
  until systemctl --user show-environment > /dev/null 2>&1; do
    i=$((i + 1))
    [ "$i" -le 30 ] || fail "no systemd user manager"
    sleep 1
  done
fi

# 1. With a ROOT.
"$residue" service install --dry-run --keep-removed 12w "$root"
sleep 5
status=$("$residue" service status)
printf '%s\n' "$status"
printf '%s\n' "$status" | head -1 | grep -Eq ': (running|active)' || fail "not running"
printf '%s\n' "$status" | grep -q 'residue watch: watching' || fail "no log line from watch"
printf '%s\n' "$status" | grep -qx "watches: $root" || fail "status does not show the root" "$status"
printf '%s\n' "$status" | grep -q "^runs: .* --keep-removed 12w " || fail "the service does not record --keep-removed" "$status"
printf '%s\n' "$status" | grep -qx "removal records: kept 12w (--keep-removed)" || fail "status does not show --keep-removed" "$status"
printf '%s\n' "$status" | grep -q "residue watch: watching .* · removal records: 12w ·" \
  || fail "the running watch did not get --keep-removed" "$status"

# 2. Again without a ROOT: the root a user named is kept.
out=$("$residue" service install --dry-run)
printf '%s\n' "$out"
printf '%s\n' "$out" | grep -q "watches: $root (kept from the service it replaces" || fail "install without a ROOT did not keep the root" "$out"
printf '%s\n' "$out" | grep -qx "removal records: new ones kept 30d (was 12w); written ones keep their own" \
  || fail "install did not say --keep-removed went back to its default" "$out"
printf '%s\n' "$out" | grep -qx "to keep 12w: residue service install --keep-removed 12w" \
  || fail "install did not say how to keep the old --keep-removed" "$out"
status=$("$residue" service status)
printf '%s\n' "$status" | grep -qx "watches: $root" || fail "status lost the kept root" "$status"
printf '%s\n' "$status" | grep -qx "removal records: kept 30d (--keep-removed)" \
  || fail "--keep-removed did not go back to its default" "$status"

# 3. A new install without a ROOT: the usual code folders.
"$residue" service uninstall
set +e
out=$("$residue" service install --dry-run --no-fetch 2>&1)
st=$?
set -e
printf '%s\n' "$out"
case $st in
0)
  printf '%s\n' "$out" | grep -q "watches: .*(the code folders in your home folder; name ROOTs to choose others)" \
    || fail "install without a ROOT did not say it uses the code folders" "$out"
  printf '%s\n' "$out" | grep -q "turn it off: residue service uninstall" || fail "install did not say how to turn it off" "$out"
  sleep 5
  status=$("$residue" service status)
  printf '%s\n' "$status"
  printf '%s\n' "$status" | head -1 | grep -Eq ': (running|active)' || fail "the default-root service is not running" "$status"
  printf '%s\n' "$status" | grep -q "^watches: ~/.*(the code folders at install" \
    || fail "status does not show the default roots as such" "$status"
  printf '%s\n' "$status" | grep -q "^runs: .* --no-fetch --dry-run /" || fail "the service does not record its roots" "$status"
  out=$("$residue" service install --dry-run --no-fetch)
  printf '%s\n' "$out" | grep -q "(the code folders in your home folder; name ROOTs to choose others)" \
    || fail "a second install without a ROOT did not look for the code folders again" "$out"
  ;;
2)
  printf '%s\n' "$out" | grep -q "none of the usual code folders exists in your home folder" \
    || fail "exit 2 without the no-folders message" "$out"
  echo "service-smoke: no code folder in $HOME, so step 3 checked the usage error only"
  ;;
*) fail "install without a ROOT: exit $st" "$out" ;;
esac

# 4. Uninstall.
"$residue" service uninstall
if "$residue" service status > /dev/null; then
  fail "still installed after uninstall"
fi
echo "service-smoke: ok"
