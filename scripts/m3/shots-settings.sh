#!/usr/bin/env bash
# The Settings pictures of docs/m3/shots/, offscreen, against the fake daemon: every state of the view
# in a dark and a light theme, quantized as the other committed pictures are (shots.sh --small). Nothing is shown
# on any screen and nothing real is touched.
#
#   scripts/m3/shots-settings.sh [--out DIR] [--dark THEME] [--light THEME]
#
# Defaults: DIR docs/m3/shots, dark `catppuccin`, light `catppuccin-latte` (any of /usr/share/omarchy/themes).
# Files: settings-<state>-<dark|light>.png for default, error, pending, focus, focus-dropdown, focus-toggle, record, and
# settings-off.png and settings-unsupported.png (no daemon, and a daemon from before M3).
set -uo pipefail

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
OUT=$ROOT/docs/m3/shots DARK=catppuccin LIGHT=catppuccin-latte
while [ $# -gt 0 ]; do
  case $1 in
    --out) OUT=$2; shift ;;
    --dark) DARK=$2; shift ;;
    --light) LIGHT=$2; shift ;;
    -h | --help) sed -n '2,12p' "$0"; exit 0 ;;
    *) echo "shots-settings.sh: unknown argument $1" >&2; exit 2 ;;
  esac
  shift
done

TMP=$(mktemp -d /tmp/ferry-m3-settings-shots.XXXXXX)
trap 'rm -rf "$TMP"' EXIT
mkdir -p "$OUT"

states=(settings settings-error settings-pending settings-focus settings-focus-dropdown settings-focus-toggle settings-record)
only=()
for s in "${states[@]}"; do only+=("$s-$DARK" "$s-$LIGHT"); done
"$HERE/shots.sh" --themes "$DARK,$LIGHT" --only "$(IFS=,; echo "${only[*]}")" --small --out "$TMP/themes" || exit 1
"$HERE/shots.sh" --off --small --out "$TMP/off" || exit 1
"$HERE/shots.sh" --legacy --only settings --small --out "$TMP/legacy" || exit 1

# panel-settings[-state]-<theme>.png -> settings[-state]-<dark|light>.png
for s in "${states[@]}"; do
  name=${s#settings}   # "" or -error ...
  cp "$TMP/themes/panel-$s-$DARK.png" "$OUT/settings${name:-}-dark.png"
  cp "$TMP/themes/panel-$s-$LIGHT.png" "$OUT/settings${name:-}-light.png"
done
cp "$TMP/off/panel-settings-off.png" "$OUT/settings-off.png"
cp "$TMP/legacy/panel-settings.png" "$OUT/settings-unsupported.png"
echo "shots-settings.sh: $(find "$OUT" -name 'settings-*.png' | wc -l) pictures in $OUT" >&2
