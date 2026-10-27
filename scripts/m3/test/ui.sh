#!/usr/bin/env bash
# The popover and the Pair panel as a user drives them, against the fake daemon: the real QML and the real
# FerryClient, rename, unpair, both pairing flows, a cancel, a timeout, a panel opened in the middle of a
# pairing. Offscreen, nothing on screen, nothing real touched.
#
#   scripts/m3/test/ui.sh          exit 0 when every check passes
set -uo pipefail
# shellcheck source=../lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/../lib.sh"

m3_scratch
m3_harness_dir "$SCR/h" "$M3/test/Ui.qml"
mv "$SCR/h/Ui.qml" "$SCR/h/shell.qml"
m3_fake fake --scene ready
say "== the popover and the Pair panel against the fake daemon"
timeout 120 bash -c "$(declare -f m3_qs); SCR='$SCR'; m3_qs '$SCR/h'" > "$SCR/raw.log" 2>&1
grep -E "UI |ERROR|TypeError|ReferenceError|Cannot read|is not defined" "$SCR/raw.log" | tee "$SCR/out.log" >&2
# Anything QML warns about that is not one of the three things a bare offscreen run always says is a defect: a binding
# loop, a function that ran in a context that was gone, a property that does not exist.
unexpected=$(grep -E "WARN|ERROR" "$SCR/raw.log" | grep -vE "window masks|No PanelWindow backend|HYPRLAND_INSTANCE_SIGNATURE")
if [ -n "$unexpected" ]; then
  say "FAIL: QML warned:"
  printf '%s\n' "$unexpected" >&2
  exit 1
fi
grep -q "UI PASS" "$SCR/out.log"
