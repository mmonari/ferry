#!/usr/bin/env bash
# shots.sh: the fast dev loop. Draws ferry's own QML offscreen, against the fake daemon, with the
# shell's real Commons and Ui and the real theme, and saves PNGs. Nothing is shown on any screen,
# nothing touches the live shell, the real ferryd or Hyprland (it reads the theme files).
#
#   scripts/m3/shots.sh                          every state, every panel, the stock theme set
#   scripts/m3/shots.sh --only away,attention    just those steps (names are the PNG file names)
#   scripts/m3/shots.sh --themes catppuccin,flexoki-light,tokyo-night
#   scripts/m3/shots.sh --off                    the widget with no daemon at all
#   scripts/m3/shots.sh --daemon SOCKET          a real daemon, as it is (scripts/m3/dev-ferryd.sh), instead of the fake
#   options: --out DIR (default /tmp/ferry-m3-shots)   --scale N (default 2)   --legacy (an M1 daemon)
#            --keep (leave the scratch dir, whose harness/ you can run by hand)
#            --small (128 colours, no dither: 40 KB a picture instead of 120, for the ones that are committed)
#   a full run is about 70 steps and takes 3 to 4 minutes; SHOTS_TIMEOUT (seconds, default 420) is the limit
#
# What it does, in order: renders the themes to JSON, builds a scratch harness dir (symlinks to the
# shell's Commons and Ui, to plugin/, and copies of the harness files), starts scripts/m3/fake-ferryd.py
# on a scratch socket, runs `quickshell -p` offscreen with the Wayland and Hyprland variables
# unset, and stops the fake. The harness drives the fake through its fake.* methods.
#
# Works offscreen: layout, text, colours, theme switching, focus rings, animation end states, and
# the PNGs. Does not: a real pointer (a drag is simulated by calling the commit the drag ends with),
# layer-shell placement, the shell's tooltip and popout switching. Those need the VM
# (scripts/m3/vm-plugin.sh).
set -uo pipefail

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
OMARCHY=${OMARCHY_PATH:-/usr/share/omarchy}
OUT=/tmp/ferry-m3-shots SCALE=2 ONLY='' THEMES='' OFF=0 LEGACY=0 KEEP=0 SMALL=0 DAEMON=''


while [ $# -gt 0 ]; do
  case $1 in
    --out) OUT=$2; shift ;;
    --scale) SCALE=$2; shift ;;
    --only) ONLY=$2; shift ;;
    --themes) THEMES=$2; shift ;;
    --off) OFF=1 ;;
    --daemon) DAEMON=$2; shift ;;
    --legacy) LEGACY=1 ;;
    --keep) KEEP=1 ;;
    --small) SMALL=1 ;;
    -h | --help) sed -n '2,23p' "$0"; exit 0 ;;
    *) echo "shots.sh: unknown argument $1" >&2; exit 2 ;;
  esac
  shift
done

command -v quickshell >/dev/null || { echo "shots.sh: quickshell is not installed" >&2; exit 1; }
[ "$SMALL" = 0 ] || command -v magick >/dev/null || { echo "shots.sh: --small needs ImageMagick (magick)" >&2; exit 1; }
[ -d "$OMARCHY/shell/Commons" ] || { echo "shots.sh: no $OMARCHY/shell/Commons (set OMARCHY_PATH)" >&2; exit 1; }

SCR=$(mktemp -d /tmp/ferry-m3-harness.XXXXXX)
chmod 700 "$SCR"
mkdir -p "$SCR/run" "$SCR/h" "$OUT"
chmod 700 "$SCR/run"
FAKE_PID=''
cleanup() {
  [ -n "$FAKE_PID" ] && kill "$FAKE_PID" 2>/dev/null
  if [ "$KEEP" = 1 ]; then echo "shots.sh: scratch kept: $SCR" >&2; else rm -rf "$SCR"; fi
}
trap cleanup EXIT

# the harness dir: the shell's modules, the plugin, and the harness's own two files
ln -s "$OMARCHY/shell/Commons" "$SCR/h/Commons"
ln -s "$OMARCHY/shell/Ui" "$SCR/h/Ui"
ln -s "$ROOT/plugin" "$SCR/h/ferry"
cp "$HERE/harness/shell.qml" "$HERE/harness/Stage.qml" "$SCR/h/"

SOCK="$SCR/run/ferry/ferryd.sock"
if [ -n "$DAEMON" ]; then
  # an external daemon: its socket, a theme, and only the steps that need no steering
  [ -S "$DAEMON" ] || { echo "shots.sh: $DAEMON is not a socket" >&2; exit 1; }
  case $(readlink -f "$DAEMON") in "${XDG_RUNTIME_DIR:-/run/user/$UID}/ferry/"*) echo "shots.sh: that is the real ferryd's socket" >&2; exit 1 ;; esac
  SOCK=$DAEMON
  THEMES=${THEMES:-current}
  python3 -I "$HERE/harness/render-themes.py" "$SCR/themes.json" ${THEMES//,/ } || exit 1
  ONLY=live,live-arrange
elif [ "$OFF" = 0 ]; then
  [ -n "$THEMES" ] || THEMES=current
  python3 -I "$HERE/harness/render-themes.py" "$SCR/themes.json" ${THEMES//,/ } || exit 1
  fake_args=(--socket "$SOCK" --scene ready)
  [ "$LEGACY" = 1 ] && fake_args+=(--legacy)
  python3 -I "$HERE/fake-ferryd.py" "${fake_args[@]}" > "$SCR/fake.log" 2>&1 &
  FAKE_PID=$!
  for _ in $(seq 50); do [ -S "$SOCK" ] && break; sleep 0.1; done
  [ -S "$SOCK" ] || { echo "shots.sh: the fake daemon did not start:" >&2; cat "$SCR/fake.log" >&2; exit 1; }
else
  # nothing listens here: the widget must say it is off
  THEMES=${THEMES:-current}
  python3 -I "$HERE/harness/render-themes.py" "$SCR/themes.json" ${THEMES//,/ } 2> /dev/null || true
  ONLY=off
fi

env -u HYPRLAND_INSTANCE_SIGNATURE -u WAYLAND_DISPLAY \
  XDG_RUNTIME_DIR="$SCR/run" QT_QPA_PLATFORM=offscreen QSG_RHI_BACKEND=software \
  FERRY_SOCKET="$SOCK" FERRY_HARNESS_OUT="$OUT" FERRY_HARNESS_SCALE="$SCALE" \
  FERRY_HARNESS_ONLY="$ONLY" FERRY_HARNESS_OFF="$OFF" FERRY_HARNESS_THEMES="$SCR/themes.json" \
  timeout "${SHOTS_TIMEOUT:-420}" quickshell -n -p "$SCR/h" 2>&1 | sed -e 's/\x1b\[[0-9;]*m//g' | grep -E "harness:|ERROR|WARN|error|Error|Cannot|not defined|undefined|TypeError|ReferenceError" | grep -v "does not support setting window masks"
status=${PIPESTATUS[0]}
if [ "$SMALL" = 1 ]; then
  for f in "$OUT"/*.png; do
    [ -e "$f" ] && magick "$f" -dither None -colors 128 "PNG8:$f.small" && mv "$f.small" "$f"
  done
fi
echo "shots.sh: quickshell exited $status; pictures in $OUT" >&2
exit "$status"
