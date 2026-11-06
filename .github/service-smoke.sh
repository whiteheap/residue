#!/bin/sh
# Install, check and uninstall `residue service` on this machine: the
# launchd agent on macOS, the systemd user service on Linux. The service runs
# with --dry-run on an empty folder, so it never removes anything.
#
#   sh .github/service-smoke.sh PATH/TO/residue
set -eu

residue=$1
case $residue in /*) ;; *) residue="$PWD/$residue" ;; esac
root=$(mktemp -d "${TMPDIR:-/tmp}/residue-service.XXXXXX")
trap '"$residue" service uninstall > /dev/null 2>&1 || true; rm -rf "$root"' EXIT

if [ "$(uname)" = Linux ]; then
  # A CI runner has no systemd user manager until lingering is on.
  sudo loginctl enable-linger "$(id -un)"
  XDG_RUNTIME_DIR="/run/user/$(id -u)"
  export XDG_RUNTIME_DIR
  i=0
  until systemctl --user show-environment > /dev/null 2>&1; do
    i=$((i + 1))
    [ "$i" -le 30 ] || { echo "service-smoke: no systemd user manager" >&2; exit 1; }
    sleep 1
  done
fi

"$residue" service install --dry-run "$root"
sleep 5
status=$("$residue" service status)
printf '%s\n' "$status"
printf '%s\n' "$status" | head -1 | grep -Eq ': (running|active)' || { echo "service-smoke: not running" >&2; exit 1; }
printf '%s\n' "$status" | grep -q 'residue watch: watching' || { echo "service-smoke: no log line from watch" >&2; exit 1; }
"$residue" service uninstall
if "$residue" service status > /dev/null; then
  echo "service-smoke: still installed after uninstall" >&2
  exit 1
fi
echo "service-smoke: ok"
