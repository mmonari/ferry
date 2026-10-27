#!/usr/bin/env bash
# The Arrange panel's own QML against the fake daemon, end to end, twice: against an M3 daemon and
# against one from before layout.set (the plugin must explain itself, not fail). Offscreen, nothing
# on screen, nothing real touched. The checks are in Arrange.qml.
#
#   scripts/m3/test/arrange.sh          exit 0 when every check passes on both
set -uo pipefail
# shellcheck source=../lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/../lib.sh"

rc=0
for mode in events legacy; do
  m3_scratch
  m3_harness_dir "$SCR/h" "$M3/test/Arrange.qml"
  mv "$SCR/h/Arrange.qml" "$SCR/h/shell.qml"
  extra=()
  legacy=0
  [ "$mode" = legacy ] && { extra=(--legacy); legacy=1; }
  m3_fake fake --scene ready --fixture three "${extra[@]}"
  say "== the Arrange panel against an $( [ $legacy = 1 ] && echo 'M1 (no layout.set)' || echo 'M3') daemon"
  timeout 90 bash -c "$(declare -f m3_qs); SCR='$SCR'; m3_qs '$SCR/h' FERRY_TEST_LEGACY=$legacy" | grep -E "ARRANGE|ERROR|TypeError|ReferenceError|Binding loop" | tee "$SCR/out.log" >&2
  grep -q "ARRANGE PASS" "$SCR/out.log" || rc=1
  m3_cleanup
  trap - EXIT
  # shellcheck disable=SC2034  # read by lib.sh
  M3_CHILDREN=()
done
exit $rc
