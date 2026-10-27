#!/usr/bin/env bash
# Shared helpers for the M3 dev scripts. Source it; it runs nothing.
#
# The host rules these helpers keep:
#   * never the user's own ferryd, its socket, or ~/.config|.local/share|.local/state/ferry
#   * never the live shell: a harness is a separate `quickshell -p`, offscreen, with the Wayland
#     and Hyprland variables unset
#   * everything scratch lives under one mktemp -d, removed on exit

M3=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$M3/../.." && pwd)
OMARCHY=${OMARCHY_PATH:-/usr/share/omarchy}

say() { printf '%s\n' "$*" >&2; }
die() { say "error: $*"; exit 1; }

# m3_scratch: a private scratch dir, removed when the script ends (KEEP=1 keeps it).
m3_scratch() {
  SCR=$(mktemp -d /tmp/ferry-m3.XXXXXX)
  chmod 700 "$SCR"
  mkdir -p "$SCR/run"
  chmod 700 "$SCR/run"
  trap m3_cleanup EXIT
}

M3_CHILDREN=()
m3_cleanup() {
  local pid
  for pid in "${M3_CHILDREN[@]:-}"; do [ -n "$pid" ] && kill "$pid" 2>/dev/null; done
  if [ "${KEEP:-0}" = 1 ]; then say "scratch kept: $SCR"; else rm -rf "$SCR"; fi
}

# m3_harness_dir <dir> [file...]: a quickshell config that can import qs.Commons, qs.Ui and the
# plugin ("ferry"), plus copies of the given files from scripts/m3/harness or scripts/m3/test.
m3_harness_dir() {
  local dir=$1; shift
  [ -d "$OMARCHY/shell/Commons" ] || die "no $OMARCHY/shell/Commons (set OMARCHY_PATH)"
  mkdir -p "$dir"
  ln -s "$OMARCHY/shell/Commons" "$dir/Commons"
  ln -s "$OMARCHY/shell/Ui" "$dir/Ui"
  ln -s "$ROOT/plugin" "$dir/ferry"
  local f
  for f in "$@"; do cp "$f" "$dir/"; done
}

# m3_fake <name> [fake-ferryd args...]: start the fake daemon on $SCR/run/ferry/ferryd.sock.
# Sets FAKE_PID. The socket is under the scratch dir, so the real one cannot be reached.
m3_fake() {
  local name=${1:-fake}; shift || true
  python3 -I "$M3/fake-ferryd.py" --socket "$SCR/run/ferry/ferryd.sock" "$@" > "$SCR/$name.log" 2>&1 &
  FAKE_PID=$!
  M3_CHILDREN+=("$FAKE_PID")
  local _
  for _ in $(seq 50); do [ -S "$SCR/run/ferry/ferryd.sock" ] && return 0; sleep 0.1; done
  cat "$SCR/$name.log" >&2
  die "the fake daemon did not start"
}

# m3_qs <harness dir> [extra env NAME=VALUE...]: run a harness offscreen. Output is filtered of
# colour codes; the caller greps what it wants.
m3_qs() {
  local dir=$1; shift
  env -u HYPRLAND_INSTANCE_SIGNATURE -u WAYLAND_DISPLAY \
    XDG_RUNTIME_DIR="$SCR/run" QT_QPA_PLATFORM=offscreen QSG_RHI_BACKEND=software \
    FERRY_SOCKET="$SCR/run/ferry/ferryd.sock" "$@" \
    quickshell -n -p "$dir" 2>&1 | sed -u -e 's/\x1b\[[0-9;]*m//g'
}

# m3_qs_bg <harness dir> <logfile> [env NAME=VALUE...]: the same, in the background, with the output
# in a file. The pid is the quickshell's own (env execs it), so the cleanup can stop it.
m3_qs_bg() {
  local dir=$1 log=$2; shift 2
  env -u HYPRLAND_INSTANCE_SIGNATURE -u WAYLAND_DISPLAY \
    XDG_RUNTIME_DIR="$SCR/run" QT_QPA_PLATFORM=offscreen QSG_RHI_BACKEND=software \
    FERRY_SOCKET="$SCR/run/ferry/ferryd.sock" "$@" \
    quickshell -n -p "$dir" > "$log" 2>&1 &
  M3_CHILDREN+=("$!")
}

now_ms() { echo $(( $(date +%s%N) / 1000000 )); }
