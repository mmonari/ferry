#!/usr/bin/env bash
# The Pair panel against two REAL ferryd, end to end: dev-a and dev-b (`--no-input`, scratch XDG dirs, UDP
# 4261 and 4262, scripts/m3/dev-ferryd.sh) pair through the real QML panel, with a node script (real.mjs)
# playing the other machine or the terminal. It needs a build of ferryd and ferry that has the event
# stream (stream B and later): without one it says so and passes, so the gate does not depend on it.
#
#   scripts/m3/test/pair-real.sh                 every scenario
#   scripts/m3/test/pair-real.sh dial differ     just those
#   FERRY_TARGET_DIR=~/.cache/x/target scripts/m3/test/pair-real.sh     a build made somewhere else
#
# Scenarios (what each proves is in test/Real.qml):
#   dial     the panel dials, both say yes, then Rename and Unpair on the real daemon
#   differ   the panel says the codes differ
#   refused  the other machine is not accepting: ferryd's error, and no waiting for the event it leaves behind
#   cancel   the panel cancels at the code
#   accept   the panel opens its window; a terminal's connection answers the code, not the panel
#
# Nothing here touches the user's own ferryd, ~/.config/ferry or the live shell: the daemons are the dev ones,
# and the panel runs in a separate offscreen quickshell.
set -uo pipefail
# shellcheck source=../lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/../lib.sh"

TARGET=${FERRY_TARGET_DIR:-$ROOT/target}
if [ ! -x "$TARGET/debug/ferryd" ] || [ ! -x "$TARGET/debug/ferry" ]; then
  say "pair-real: no ferryd and ferry in $TARGET/debug (scripts/m3/dev-ferryd.sh build): skipped"
  exit 0
fi
# a ferryd without `subscribe` has no events for a panel to follow
if ! "$TARGET/debug/ferryd" --help 2>&1 | grep -q -- --no-input; then
  say "pair-real: $TARGET/debug/ferryd has no --no-input: skipped"
  exit 0
fi

SCENARIOS=("$@")
[ ${#SCENARIOS[@]} -gt 0 ] || SCENARIOS=(dial differ refused cancel accept)

export FERRY_TARGET_DIR=$TARGET
# not 4251 and 4252: another checkout's dev daemons may hold them (stream E's Arrange panel does)
export FERRY_DEV_PORT_BASE=${FERRY_DEV_PORT_BASE:-4261}
PORT_B=$((FERRY_DEV_PORT_BASE + 1))
DEV=$M3/dev-ferryd.sh
DIR=/tmp/ferry-m3-real
T=$M3/test
m3_scratch
m3_harness_dir "$SCR/h" "$T/Real.qml"
mv "$SCR/h/Real.qml" "$SCR/h/shell.qml"
trap 'm3_cleanup; "$DEV" stop --dir "$DIR" > /dev/null 2>&1' EXIT

sock() { echo "$DIR/$1/run/ferry/ferryd.sock"; }
rc=0
fail() { say "FAIL: $*"; rc=1; }

# wait_line FILE REGEX SECONDS
wait_line() {
  local _
  for _ in $(seq $(($3 * 10))); do
    grep -qE "$2" "$1" 2> /dev/null && return 0
    sleep 0.1
  done
  return 1
}

start_daemons() {
  "$DEV" stop --dir "$DIR" > /dev/null 2>&1
  if ! "$DEV" start --two --dir "$DIR" > "$SCR/dev.log" 2>&1; then
    cat "$SCR/dev.log" >&2
    fail "the dev daemons did not start"
    return 1
  fi
}

# the panel, on a daemon's socket, as a scenario; the pid is the last child
run_panel() {
  m3_qs_bg "$SCR/h" "$2" FERRY_SOCKET="$1" FERRY_REAL="$3" FERRY_REAL_ADDR="127.0.0.1:$PORT_B"
}

# node real.mjs against a socket, in the background, into a log
run_node() {
  local log=$1
  shift
  node "$T/real.mjs" "$@" > "$log" 2>&1 &
  M3_CHILDREN+=("$!")
}

verdict() {
  local scenario=$1 log=$2 unexpected
  if wait_line "$log" 'REAL (PASS|FAIL)' 120 && grep -q 'REAL PASS' "$log"; then
    say "  panel: $(grep 'REAL PASS' "$log" | sed -e 's/\x1b\[[0-9;]*m//g' -e 's/^.*REAL/REAL/')"
  else
    grep -E 'REAL|TypeError|ReferenceError' "$log" | sed -e 's/\x1b\[[0-9;]*m//g' >&2
    fail "$scenario: the panel did not pass"
  fi
  # QML that warns beyond the three things a bare offscreen run always says is a defect (a binding loop, a function
  # that ran in a context that was gone, a property that does not exist)
  unexpected=$(grep -E "WARN|ERROR" "$log" | grep -vE "window masks|No PanelWindow backend|HYPRLAND_INSTANCE_SIGNATURE" | sed -e 's/\x1b\[[0-9;]*m//g')
  [ -z "$unexpected" ] || { printf '%s\n' "$unexpected" >&2; fail "$scenario: QML warned"; }
}

code_of() { sed -nE "s/.*(REAL|NODE) code=([0-9]{6}).*/\2/p" "$1" | head -1; }

for scenario in "${SCENARIOS[@]}"; do
  say "== $scenario"
  start_daemons || continue
  rm -f "$SCR"/*.log "$SCR/answer-now"
  case $scenario in
    dial | differ | cancel)
      reply=yes
      [ "$scenario" = cancel ] && reply=none
      run_node "$SCR/b.log" "$(sock b)" accept "$reply"
      wait_line "$SCR/b.log" '^NODE ready' 10 || { cat "$SCR/b.log" >&2; fail "$scenario: B never opened its window"; continue; }
      run_panel "$(sock a)" "$SCR/a.log" "$scenario"
      verdict "$scenario" "$SCR/a.log"
      wait_line "$SCR/b.log" '^NODE end' 20 || fail "$scenario: B never saw the end"
      grep -E 'NODE' "$SCR/b.log" | sed 's/^/  /' >&2
      a=$(code_of "$SCR/a.log")
      b=$(code_of "$SCR/b.log")
      [ -n "$a" ] && [ "$a" = "$b" ] && say "  both machines showed the same code ($a)" || fail "$scenario: the codes are '$a' and '$b'"
      case $scenario in
        dial) grep -q 'NODE end stage=paired .*peer=dev-a' "$SCR/b.log" || fail "dial: B did not end paired with dev-a" ;;
        differ) grep -q 'NODE end stage=failed reason=rejected' "$SCR/b.log" || fail "differ: B did not fail with rejected" ;;
        cancel) grep -q 'NODE end stage=failed reason=cancelled' "$SCR/b.log" || fail "cancel: B did not fail with cancelled" ;;
      esac
      if [ "$scenario" = dial ]; then
        run_node "$SCR/peers.log" "$(sock b)" peers
        wait_line "$SCR/peers.log" '^NODE peers' 10 && say "  $(grep '^NODE peers' "$SCR/peers.log")"
      fi
      ;;
    refused)
      run_panel "$(sock a)" "$SCR/a.log" refused
      verdict refused "$SCR/a.log"
      grep 'REAL error=' "$SCR/a.log" | sed -e 's/\x1b\[[0-9;]*m//g' -e 's/^.*REAL/  REAL/' >&2
      ;;
    accept)
      run_panel "$(sock b)" "$SCR/b.log" accept
      wait_line "$SCR/b.log" '^.*REAL listening' 20 || { cat "$SCR/b.log" >&2; fail "accept: the panel never listened"; continue; }
      run_node "$SCR/elsewhere.log" "$(sock b)" answer yes "$SCR/answer-now"
      wait_line "$SCR/elsewhere.log" '^NODE ready' 10 || fail "accept: no second connection"
      run_node "$SCR/a.log.node" "$(sock a)" dial "127.0.0.1:$PORT_B" yes
      wait_line "$SCR/b.log" 'REAL code-shown' 30 || { cat "$SCR/b.log" "$SCR/a.log.node" >&2; fail "accept: the panel never showed a code"; continue; }
      touch "$SCR/answer-now"
      verdict accept "$SCR/b.log"
      grep -h 'NODE' "$SCR/a.log.node" "$SCR/elsewhere.log" | sed 's/^/  /' >&2
      a=$(code_of "$SCR/a.log.node")
      b=$(code_of "$SCR/b.log")
      [ -n "$a" ] && [ "$a" = "$b" ] && say "  both machines showed the same code ($a)" || fail "accept: the codes are '$a' and '$b'"
      ;;
    *) fail "unknown scenario $scenario" ;;
  esac
done

if [ "$rc" = 0 ]; then say "PAIR-REAL PASS"; else say "PAIR-REAL FAIL"; fi
exit "$rc"
