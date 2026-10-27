#!/usr/bin/env bash
# The Settings view driven by the keyboard, against the fake daemon (scripts/m3/test/SettingsKeys.qml):
# real key events through the real panel, client and socket, offscreen. Nothing is shown on any
# screen and nothing real is touched.
#
#   scripts/m3/test/settings-keys.sh          exit 0 when every check passes
set -uo pipefail
# shellcheck source=../lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/../lib.sh"

m3_scratch
m3_harness_dir "$SCR/h" "$M3/test/SettingsKeys.qml"
mv "$SCR/h/SettingsKeys.qml" "$SCR/h/shell.qml"
m3_fake fake --scene ready
say "== the Settings view, by keyboard, against the fake daemon"
timeout 120 bash -c "$(declare -f m3_qs); SCR='$SCR'; m3_qs '$SCR/h'" | grep -E "SETTINGS|ERROR|TypeError|ReferenceError|Binding loop|is not a type" | tee "$SCR/out.log" >&2
grep -q "SETTINGS PASS" "$SCR/out.log"
