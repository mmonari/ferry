#!/usr/bin/env bash
# dev-ferryd.sh: a real ferryd on the HOST for UI work, safe next to the daemon the user runs.
# One instance, or two paired with each other over loopback, so the UI sees a real connected
# peer, a real layout revision and real pairing.
#
#   scripts/m3/dev-ferryd.sh build                         cargo build, debug, CARGO_BUILD_JOBS=4 (ferryd and ferry)
#   scripts/m3/dev-ferryd.sh start [--pair|--two] [--monitors host|none] [--dir DIR]
#   scripts/m3/dev-ferryd.sh status [--dir DIR]            what runs, and `ferry status` of each
#   scripts/m3/dev-ferryd.sh env [--dir DIR] [a|b]         the variables that point a client at one instance
#   scripts/m3/dev-ferryd.sh stop [--dir DIR]
#
#   --pair            two instances, dev-a (UDP 4251) and dev-b (4252), each trusting the other
#   --two             the same two instances, trusting nobody: pair them with the Pair panel (test/pair-real.sh does)
#   --monitors host   the instances read this desktop's monitors from Hyprland (read-only, see below)
#   --monitors none   (default) no Hyprland at all: the instances have no monitors of their own
#   --dir DIR         scratch root, default /tmp/ferry-m3-dev; one folder per instance inside it
#
# Then point the harness or a tool at an instance:
#   eval "$(scripts/m3/dev-ferryd.sh env a)"       # exports XDG_RUNTIME_DIR and FERRY_SOCKET
#   target/debug/ferry status                      # the ferry CLI, on that instance
#
# WHAT IT DOES TO THIS MACHINE, from reading the code (crates/ferryd/src/lib.rs, main.rs, ctl.rs,
# monitors.rs, inject.rs) and from `ferryd --no-input`:
#   * capture is off: no xdg-desktop-portal session, no EIS connection, no barriers, no Hyprland IPC
#     write, no release_input_capture, no cursor reads (`ipc` stays None when `capture_allowed` is false)
#   * injection is off (InjectBackend::Off.start() returns no injector): no virtual keyboard or mouse,
#     no uinput, no keymap upload, nothing sent to Hyprland's virtual-input protocols
#   * the one contact with Hyprland is the monitor watch (monitors.rs): `j/monitors` and the read-only
#     event socket, and only with --monitors host, which links the real hypr runtime dir into the
#     instance's scratch runtime dir. Without it the instance cannot find a compositor.
#   * the files are all under DIR: XDG_CONFIG_HOME, XDG_DATA_HOME, XDG_STATE_HOME, XDG_RUNTIME_DIR and
#     HOME point there, so ~/.config/ferry, ~/.local/share/ferry, ~/.local/state/ferry and
#     $XDG_RUNTIME_DIR/ferry (the user's real daemon, which this never signals) are not touched
#   * the network: a QUIC endpoint on UDP 4251 and 4252, on every interface (ferryd offers no bind
#     option). A stranger's connection is refused for want of a pinned fingerprint, and this host's
#     firewall drops those ports from the LAN. mDNS is switched off in the instance's config.
# `start` refuses to run a ferryd without --no-input, and a DIR that is, or is inside, a real ferry dir.
# FERRY_TARGET_DIR points at a cargo target dir other than this checkout's (a build made elsewhere), and
# FERRY_DEV_PORT_BASE at the UDP port of dev-a (dev-b takes the next one), for when another checkout has its own.
set -uo pipefail
# shellcheck source=lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

BIN=${FERRY_TARGET_DIR:-$ROOT/target}/debug
DIR=/tmp/ferry-m3-dev
PAIR=0 TWO=0 MONITORS=none
cmd=${1:-help}
[ $# -gt 0 ] && shift
POS=()
while [ $# -gt 0 ]; do
  case $1 in
    --pair) PAIR=1 ;;
    --two) TWO=1 ;;
    --monitors) MONITORS=$2; shift ;;
    --dir) DIR=$2; shift ;;
    -h | --help) cmd=help ;;
    *) POS+=("$1") ;;
  esac
  shift
done

# a scratch dir must not be, or sit inside, anything real
guard_dir() {
  local d
  d=$(readlink -m "$DIR")
  case $d in
    "$HOME/.config/ferry"* | "$HOME/.local/share/ferry"* | "$HOME/.local/state/ferry"* | "${XDG_RUNTIME_DIR:-/run/user/$UID}/ferry"*)
      die "$DIR is a real ferry directory" ;;
    /tmp/* | /var/tmp/* | "$HOME/.cache/"*) ;;
    *) die "--dir must be under /tmp, /var/tmp or ~/.cache (it is wiped by 'start')" ;;
  esac
}

# --monitors host reaches the compositor through a symlink inside the instance's runtime dir, and a unix
# socket address holds 107 bytes: a longer --dir makes ferryd find no instance and report no monitors,
# which looks like a Hyprland problem and is not (the default, /tmp/ferry-m3-dev, fits)
guard_socket_path() {
  local sig longest
  [ "$MONITORS" = host ] || return 0
  sig=$(ls "${XDG_RUNTIME_DIR:-/run/user/$UID}/hypr" 2> /dev/null | head -1)
  [ -n "$sig" ] || return 0
  longest="$(readlink -m "$DIR")/a/run/hypr/$sig/.socket.sock"
  [ "${#longest}" -le 107 ] || die "--dir is too long for --monitors host: ${#longest} bytes of socket path, 107 fit. Use a shorter one, e.g. /tmp/fm3"
}

# UDP 4251 and 4252 unless FERRY_DEV_PORT_BASE says otherwise: two checkouts that run dev daemons at once need two bases
inst_port() { case $1 in a) echo "${FERRY_DEV_PORT_BASE:-4251}" ;; b) echo $((${FERRY_DEV_PORT_BASE:-4251} + 1)) ;; esac; }
inst_dir() { echo "$DIR/$1"; }
inst_run() { echo "$DIR/$1/run"; }
inst_sock() { echo "$DIR/$1/run/ferry/ferryd.sock"; }

# run a client (ferry) against one instance: its runtime dir is where the CLI finds the socket
F() {
  local i=$1; shift
  env -i PATH="$PATH" HOME="$(inst_dir "$i")/home" XDG_RUNTIME_DIR="$(inst_run "$i")" "$BIN/ferry" "$@"
}

fp_of() { F "$1" --json id | python3 -c 'import json,sys; print(json.load(sys.stdin)["fingerprint"])'; }

do_build() {
  say "== build (debug, CARGO_BUILD_JOBS=4)"
  (cd "$ROOT" && CARGO_BUILD_JOBS=4 cargo build --locked -p ferryd -p ferry-cli) || die "build failed"
}

start_one() {
  local i=$1 name port d
  name=dev-$i port=$(inst_port "$i") d=$(inst_dir "$i")
  mkdir -p "$d/home" "$d/config/ferry" "$d/data" "$d/state" "$(inst_run "$i")"
  chmod 700 "$d" "$(inst_run "$i")"
  cat > "$d/config/ferry/config.toml" <<TOML
# written by scripts/m3/dev-ferryd.sh
[peer]
name = "$name"
port = $port

[discovery]
mdns = false
advertise = false
TOML
  if [ "$MONITORS" = host ]; then
    # the only way the instance can see Hyprland, and read-only: monitors.rs asks for j/monitors and listens on socket2
    ln -s "${XDG_RUNTIME_DIR:-/run/user/$UID}/hypr" "$(inst_run "$i")/hypr"
  fi
  # --no-input is not an option here: a ferryd without it would capture and inject
  env -i PATH="$PATH" HOME="$d/home" \
    XDG_CONFIG_HOME="$d/config" XDG_DATA_HOME="$d/data" XDG_STATE_HOME="$d/state" XDG_RUNTIME_DIR="$(inst_run "$i")" \
    FERRY_LOG=info setsid "$BIN/ferryd" --no-input > "$d/ferryd.log" 2>&1 < /dev/null &
  echo $! > "$d/ferryd.pid"
  local _
  for _ in $(seq 100); do F "$i" id > /dev/null 2>&1 && return 0; sleep 0.1; done
  tail -5 "$d/ferryd.log" >&2
  die "dev-$i did not come up"
}

do_start() {
  guard_dir
  guard_socket_path
  [ -x "$BIN/ferryd" ] && [ -x "$BIN/ferry" ] || die "no $BIN/ferryd: $0 build"
  [ -e "$DIR" ] && do_stop > /dev/null 2>&1
  rm -rf "$DIR"
  mkdir -p "$DIR"
  chmod 700 "$DIR"
  case $MONITORS in host | none) ;; *) die "--monitors is host or none" ;; esac
  start_one a
  say "dev-a up: $(inst_sock a)"
  if [ "$TWO" = 1 ] && [ "$PAIR" = 0 ]; then
    start_one b
    say "dev-b up: $(inst_sock b)"
  fi
  if [ "$PAIR" = 1 ]; then
    start_one b
    say "dev-b up: $(inst_sock b)"
    local fa fb
    fa=$(fp_of a) fb=$(fp_of b)
    F a trust "$fb" --name dev-b --addr 127.0.0.1:"$(inst_port b)" > /dev/null || die "trust on a failed"
    F b trust "$fa" --name dev-a --addr 127.0.0.1:"$(inst_port a)" > /dev/null || die "trust on b failed"
    local _
    for _ in $(seq 100); do
      F a --json status 2> /dev/null | python3 -c 'import json,sys; d=json.load(sys.stdin); sys.exit(0 if any(p["state"]=="connected" for p in d["peers"]) else 1)' && break
      sleep 0.2
    done
    F a status || true
  fi
  say ""
  say "point a client at an instance:  eval \"\$($0 env a)\""
}

do_status() {
  local i
  for i in a b; do
    [ -f "$(inst_dir "$i")/ferryd.pid" ] || continue
    say "== dev-$i (pid $(cat "$(inst_dir "$i")/ferryd.pid")) $(inst_sock "$i")"
    F "$i" status 2>&1 | head -12
  done
}

do_env() {
  local i=${POS[0]:-a}
  echo "export XDG_RUNTIME_DIR=$(inst_run "$i") FERRY_SOCKET=$(inst_sock "$i")"
}

do_stop() {
  local i pid
  for i in a b; do
    pid=$(cat "$(inst_dir "$i")/ferryd.pid" 2> /dev/null) || continue
    # only ever a pid this script started, and only if it is still our binary
    if [ -n "$pid" ] && [ "$(readlink "/proc/$pid/exe" 2> /dev/null)" = "$BIN/ferryd" ]; then
      kill "$pid" 2> /dev/null
      local _
      for _ in $(seq 30); do kill -0 "$pid" 2> /dev/null || break; sleep 0.1; done
      say "dev-$i stopped"
    fi
  done
}

case $cmd in
  help) sed -n '2,45p' "$0"; exit 0 ;;
  build) do_build ;;
  start) do_start ;;
  status) do_status ;;
  env) do_env ;;
  stop) guard_dir; do_stop ;;
  *) die "unknown command $cmd" ;;
esac
