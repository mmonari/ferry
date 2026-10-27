#!/usr/bin/env bash
# The plugin's IPC target, called the way users and the launcher entry call it:
# `omarchy-shell ferry <function>` (the real script, which falls back to `qs ipc` for a plugin's
# IpcHandler), and `ferry ui <view>` when the ferry binary is built. The shell is a standalone one,
# offscreen, in a scratch OMARCHY_PATH and XDG_RUNTIME_DIR, with the real plugin and the fake daemon:
# the live shell is never addressed.
#
#   scripts/m3/test/ipc.sh          exit 0 when every check passes
#   FERRY_BIN=path/to/ferry         the CLI to test `ferry ui` with (default target/debug/ferry)
set -uo pipefail
# shellcheck source=../lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/../lib.sh"

command -v omarchy-shell >/dev/null || die "omarchy-shell is not installed"
m3_scratch
# the scratch OMARCHY_PATH: omarchy-shell wants $OMARCHY_PATH/shell/shell.qml, and addresses that config
mkdir -p "$SCR/omarchy"
m3_harness_dir "$SCR/omarchy/shell" "$M3/test/Ipc.qml"
mv "$SCR/omarchy/shell/Ipc.qml" "$SCR/omarchy/shell/shell.qml"
m3_fake fake --scene ready
m3_qs_bg "$SCR/omarchy/shell" "$SCR/shell.log"

pass=0 fail=0
ok() { pass=$((pass + 1)); say "  ok    $*"; }
bad() { fail=$((fail + 1)); say "  FAIL  $*"; }

# what the scratch shell must be addressed with: never the live one
env_shell=(env -u HYPRLAND_INSTANCE_SIGNATURE -u WAYLAND_DISPLAY XDG_RUNTIME_DIR="$SCR/run" OMARCHY_PATH="$SCR/omarchy")
shell_ipc() { "${env_shell[@]}" omarchy-shell "$@" 2>&1; }
probe() { shell_ipc ferry-probe ui; }

# wait until the probe answers (the shell has loaded the plugin and ferryd said hello)
up=0
for _ in $(seq 100); do
  if [ "$(probe | grep -c '"connected":true')" = 1 ]; then up=1; break; fi
  sleep 0.1
done
[ "$up" = 1 ] || { cat "$SCR/shell.log" >&2; die "the scratch shell did not come up"; }

# expect <what> <json fragment>: the probe says so within 3 s
expect() {
  local what=$1 want=$2 got=''
  for _ in $(seq 30); do
    got=$(probe)
    case $got in *"$want"*) ok "$what"; return 0 ;; esac
    sleep 0.1
  done
  bad "$what (wanted $want, probe says $got)"
}

say "== omarchy-shell ferry <function>"
got=$(shell_ipc ferry state)
[ "$got" = ready ] && ok "state answers ready" || bad "state answered '$got'"

shell_ipc ferry arrange > /dev/null
expect "arrange opens the panel on Arrange" '"panelOpen":true,"view":"arrange"'
shell_ipc ferry arrange > /dev/null
expect "arrange again leaves it open (a launcher entry never closes what it shows)" '"panelOpen":true,"view":"arrange"'
shell_ipc ferry pair > /dev/null
expect "pair switches to Pair" '"view":"pair"'
shell_ipc ferry settings > /dev/null
expect "settings switches to Settings" '"view":"settings"'
shell_ipc ferry panel > /dev/null
expect "panel toggles it closed" '"panelOpen":false'
shell_ipc ferry panel > /dev/null
expect "and open again, on the view it was on" '"panelOpen":true,"view":"settings"'
shell_ipc ferry close > /dev/null
expect "close closes the panel" '"panelOpen":false'

got=$(shell_ipc ferry lock)
expect "lock locks this machine" '"locked":true'
[ "$(shell_ipc ferry state)" = locked ] && ok "and the state says locked" || bad "state after lock: $(shell_ipc ferry state)"
shell_ipc ferry lock > /dev/null
expect "lock again unlocks" '"locked":false'
if shell_ipc ferry release > /dev/null; then ok "release is accepted"; else bad "release failed"; fi
for f in toggle open; do
  out=$(shell_ipc ferry "$f"); rc=$?
  # the popover has no layer-shell window here, so it cannot show; the call must still be known to the target
  case $out in *"not found"*) bad "$f is not a function of the target: $out" ;; *) ok "$f is a function of the target (rc $rc)" ;; esac
done
out=$(shell_ipc ferry nosuch); rc=$?
[ "$rc" != 0 ] && ok "an unknown function fails loudly ($out)" || bad "an unknown function did not fail"

say "== the ui event (a notification action asks for a view)"
shell_ipc ferry close > /dev/null
python3 -I - "$SCR/run/ferry/ferryd.sock" <<'PY'
import json, socket, sys
s = socket.socket(socket.AF_UNIX)
s.connect(sys.argv[1])
s.sendall(b'{"id":1,"method":"fake.ui","params":{"open":"pair"}}\n')
s.settimeout(2)
s.recv(4096)
PY
expect "a ui event {open: pair} opens the Pair panel" '"panelOpen":true,"view":"pair"'

say "== ferry ui <view> (what dist/ferry.desktop runs)"
FERRY_BIN=${FERRY_BIN:-$ROOT/target/debug/ferry}
if [ -x "$FERRY_BIN" ]; then
  shell_ipc ferry close > /dev/null
  # no OMARCHY_PATH in the environment: the CLI must fill it in (a launcher entry may lack it), so
  # point it at the scratch one the way `ferry ui` would find it in a session
  out=$("${env_shell[@]}" "$FERRY_BIN" ui settings 2>&1); rc=$?
  [ "$rc" = 0 ] && ok "ferry ui settings exits 0" || bad "ferry ui settings exited $rc: $out"
  expect "and the Settings panel is open" '"panelOpen":true,"view":"settings"'
  out=$("${env_shell[@]}" "$FERRY_BIN" ui arrange 2>&1); rc=$?
  [ "$rc" = 0 ] && ok "ferry ui arrange exits 0" || bad "ferry ui arrange exited $rc: $out"
  expect "and it moves to Arrange" '"view":"arrange"'
  # a shell where the plugin is not loaded: the CLI says what to do
  out=$(env -u HYPRLAND_INSTANCE_SIGNATURE -u WAYLAND_DISPLAY XDG_RUNTIME_DIR="$SCR/run" OMARCHY_PATH="$SCR/omarchy" "$FERRY_BIN" ui nonsense 2>&1); rc=$?
  [ "$rc" = 2 ] && ok "an unknown view is a usage error (exit 2)" || bad "ferry ui nonsense exited $rc"
else
  say "  skip  ferry ui (no $FERRY_BIN: cargo build -p ferry-cli)"
fi

say "$pass ok, $fail failed"
[ "$fail" = 0 ]
