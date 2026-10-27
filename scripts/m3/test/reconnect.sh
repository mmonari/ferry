#!/usr/bin/env bash
# FR-UI-03 on the host: the widget is `off` while ferryd is away and live again within 2 s of
# ferryd coming back, every time, and it never blocks. Runs the plugin's FerryClient in a bare
# offscreen quickshell against the fake daemon, stops and starts the fake, and times what the
# client reports. Needs no VM and shows nothing on screen.
#
#   scripts/m3/test/reconnect.sh [rounds]        default 4. Exit 0 when every restart was <= 2000 ms.
set -uo pipefail
# shellcheck source=../lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/../lib.sh"

ROUNDS=${1:-4}
LIMIT_MS=2000
m3_scratch
m3_harness_dir "$SCR/h" "$M3/test/Reconnect.qml"
mv "$SCR/h/Reconnect.qml" "$SCR/h/shell.qml"

m3_qs_bg "$SCR/h" "$SCR/qs.log"
sleep 1.5   # the client starts, finds no daemon, and says off

worst=0 fail=0
for round in $(seq "$ROUNDS"); do
  t0=$(now_ms)
  m3_fake "fake$round" --scene ready
  # wait for the connection report, at most 6 s
  got=''
  for _ in $(seq 120); do
    got=$(sed -e 's/\x1b\[[0-9;]*m//g' "$SCR/qs.log" | grep -E "RECONNECT connected=true" | tail -1 | sed -E 's/.* t=([0-9]+)$/\1/')
    [ -n "$got" ] && [ "$got" -ge "$t0" ] && break
    got=''
    sleep 0.05
  done
  if [ -z "$got" ]; then say "round $round: never connected"; fail=1; break; fi
  dt=$((got - t0))
  say "round $round: ferryd started, widget live after ${dt} ms"
  [ "$dt" -gt "$worst" ] && worst=$dt
  [ "$dt" -gt "$LIMIT_MS" ] && fail=1

  # now stop it, as `systemctl --user stop ferryd` does: the socket goes away, the client says off
  t1=$(now_ms)
  kill "$FAKE_PID"; wait "$FAKE_PID" 2>/dev/null
  off=''
  for _ in $(seq 60); do
    off=$(sed -e 's/\x1b\[[0-9;]*m//g' "$SCR/qs.log" | grep -E "RECONNECT connected=false" | tail -1 | sed -E 's/.* t=([0-9]+)$/\1/')
    [ -n "$off" ] && [ "$off" -ge "$t1" ] && break
    off=''
    sleep 0.05
  done
  if [ -z "$off" ]; then say "round $round: never noticed the daemon went away"; fail=1; break; fi
  say "round $round: ferryd stopped, widget off after $((off - t1)) ms"
  sleep 0.4
done

say "worst reconnect: ${worst} ms (limit ${LIMIT_MS} ms)"
if [ "$fail" = 0 ]; then say "PASS"; else say "FAIL"; tail -20 "$SCR/qs.log" >&2; fi
exit "$fail"
