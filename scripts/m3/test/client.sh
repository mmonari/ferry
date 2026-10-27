#!/usr/bin/env bash
# FerryClient against the fake daemon, end to end, twice: against an M3 daemon (events) and an
# M1 daemon (the polling fallback). Offscreen, nothing on screen, nothing real touched.
#
#   scripts/m3/test/client.sh          exit 0 when every check passes on both
set -uo pipefail
# shellcheck source=../lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/../lib.sh"

rc=0
for mode in events legacy; do
  m3_scratch
  m3_harness_dir "$SCR/h" "$M3/test/Client.qml"
  mv "$SCR/h/Client.qml" "$SCR/h/shell.qml"
  extra=()
  legacy=0
  [ "$mode" = legacy ] && { extra=(--legacy); legacy=1; }
  m3_fake fake --scene ready "${extra[@]}"
  say "== FerryClient against an $( [ $legacy = 1 ] && echo 'M1 (polling)' || echo 'M3 (events)') daemon"
  timeout 60 bash -c "$(declare -f m3_qs); SCR='$SCR'; m3_qs '$SCR/h' FERRY_TEST_LEGACY=$legacy" | grep -E "CLIENT|ERROR|TypeError|ReferenceError" | tee "$SCR/out.log" >&2
  grep -q "CLIENT PASS" "$SCR/out.log" || rc=1
  m3_cleanup
  trap - EXIT
  # shellcheck disable=SC2034  # read by lib.sh
  M3_CHILDREN=()
done
exit $rc
