#!/usr/bin/env bash
# The gates for the plugin stream (there is no Rust here): what runs before a commit.
#
#   scripts/m3/check.sh            validate the manifest, lint the QML, run the node tests, shellcheck
#   scripts/m3/check.sh --full     also: FerryClient against the fake (events and legacy), the Arrange panel, the popover
#                                  and the Pair panel driven against it and, when ferryd is built, against two real dev
#                                  daemons, the 2 s reconnect, the Settings view by keyboard, the IPC target, the API
#                                  reference's example clients, a picture run
#
# All of it is offscreen and touches nothing outside a scratch directory.
set -uo pipefail
# shellcheck source=lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

FULL=0
[ "${1:-}" = --full ] && FULL=1
fail=0
step() { say "== $*"; }
run() { "$@" || { say "FAILED: $*"; fail=1; }; }

step "omarchy plugin validate"
command -v omarchy >/dev/null && run omarchy plugin validate "$ROOT/plugin" || say "(omarchy is not installed: skipped)"

# Qt 6's linter, and only that one. On this machine `qmllint` on PATH is Qt 5's (Arch's
# qt5-declarative owns /usr/bin/qmllint): it knows no `: void` annotation, which every IpcHandler
# function needs, and it exits 255 without printing a word. A gate that took that for a result
# would be wrong in both directions, so a linter that does not say "qmllint 6" is not used.
find_qmllint() {
  local c
  for c in "${QMLLINT:-}" /usr/lib/qt6/bin/qmllint "$(command -v qmllint6 2>/dev/null)" "$(command -v qmllint 2>/dev/null)"; do
    if [ -n "$c" ] && [ -x "$c" ] && "$c" --version 2>&1 | grep -q '^qmllint 6'; then
      echo "$c"
      return 0
    fi
  done
  return 1
}

# What the linter says that is not a defect of ours. Each entry is a fact about the tool or about a
# window type, with the reason, so the list cannot grow by habit:
#   Fx.qml           qmllint 6.12 does not read the sibling qmldir for a file given on its command
#                    line, so it reports a singleton as "not declared" for any file it is handed
#                    alone. The declaration is in qmldir, plugin.test.mjs fails the build when a
#                    `pragma Singleton` file is missing from it, and the real shell loads Fx through it.
#   PanelHost.qml    a PanelWindow is a layer-shell window; qmllint has no backend to create it.
#                    (Popover.qml and PanelHost.qml are loaded by URL for the same reason.)
#   Popover.qml      `focusTarget: body.item`: Loader.item is typed QObject, focusTarget wants an
#                    Item. The Loader only ever builds PopoverContent, an Item.
LINT_ALLOW='ferry/Fx\.qml::? Type Fx not declared as singleton|ferry/PanelHost\.qml:[0-9]+:[0-9]+: Type PanelWindow is not creatable|ferry/Popover\.qml:[0-9]+:[0-9]+: Cannot assign binding of type QObject to QQuickItem'

lint_qml() {
  local ql lint out rc gated notes
  if ! ql=$(find_qmllint); then
    say "(no Qt 6 qmllint: skipped. Set QMLLINT, or install qt6-declarative; /usr/bin/qmllint here is Qt 5's)"
    return 0
  fi
  lint=$(mktemp -d /tmp/ferry-m3-lint.XXXXXX)
  out=$lint/lint.out
  mkdir "$lint/qs"
  # The same names the shell resolves: `qs.` is the shell root, and `ferry` is the plugin folder, which
  # the harness and test files import by path (they sit beside it, as they do in a harness directory).
  ln -s "$OMARCHY/shell/Commons" "$lint/qs/Commons"
  ln -s "$OMARCHY/shell/Ui" "$lint/qs/Ui"
  ln -s "$ROOT/plugin" "$lint/ferry"
  cp "$M3"/harness/*.qml "$M3"/test/*.qml "$lint/"
  # The cd is in a subshell, the verdict is not: run() inside it would set `fail` where nobody reads it.
  (cd "$lint" && "$ql" -I "$lint" -I /usr/lib/qt6/qml ferry/*.qml ferry/views/*.qml ferry/panels/*.qml ferry/settings/*.qml ./*.qml) > "$out" 2>&1
  rc=$?
  # Gated: errors, and the warnings that mean a file will not load (a type or an import that does not
  # resolve). Not gated: [missing-property] and [unqualified]. The shell's own files draw the same
  # ones, because Style.* and Commons.Color.* groups are typed QObject to the linter.
  gated=$(grep -E '^Error:|^Warning: .*\[(import|unresolved-type|uncreatable-type|incompatible-type|syntax|compiler)\]$' "$out" | grep -vE "$LINT_ALLOW")
  notes=$(grep -cE '^(Warning|Info): ' "$out")
  known=$(grep -cE "$LINT_ALLOW" "$out")
  if [ "$rc" != 0 ] || [ -n "$gated" ]; then
    [ -n "$gated" ] && printf '%s\n' "$gated" >&2
    [ -z "$gated" ] && head -n 20 "$out" >&2
    say "FAILED: qmllint (exit $rc)"
    fail=1
  else
    say "qmllint: no errors, nothing unresolved ($notes notes in all: $known known and explained above, the rest style and not gated)"
  fi
  rm -rf "$lint"
}

step "qmllint (Qt 6: syntax, imports and types, against the shell's Commons and Ui)"
lint_qml

step "node tests (Model.js, Strings.js, Glyphs.js, the plugin's structure)"
run node --test "$M3"/test/*.test.mjs

step "shellcheck"
if command -v shellcheck >/dev/null; then
  run shellcheck --severity=warning "$M3"/*.sh "$M3"/test/*.sh
else
  say "(shellcheck is not installed: skipped)"
fi

step "python"
run python3 -I -m py_compile "$M3/fake-ferryd.py" "$M3/harness/render-themes.py"
# the fake daemon's config answers are the real daemon's (schema snapshot and the Rust refusals)
run python3 -I "$M3/test/fake_schema.py"
find "$M3" -name __pycache__ -type d -prune -exec rm -rf {} +

if [ "$FULL" = 1 ]; then
  step "FerryClient against the fake daemon (events, then an M1 daemon)"
  run "$M3/test/client.sh"
  step "the popover and the Pair panel, driven: rename, unpair, both pairing flows, cancel, timeout"
  run "$M3/test/ui.sh"
  step "the Pair panel against two real ferryd (skipped when none is built: scripts/m3/dev-ferryd.sh build)"
  run "$M3/test/pair-real.sh"
  step "reconnect within 2 s (FR-UI-03)"
  run "$M3/test/reconnect.sh" 3
  step "the Arrange panel against the fake daemon (events, then an M1 daemon)"
  run "$M3/test/arrange.sh"
  step "the Settings view by keyboard (FR-UI-10, FR-UI-14)"
  run "$M3/test/settings-keys.sh"
  step "the IPC target through omarchy-shell and ferry ui (FR-UI-13, FR-UI-18)"
  run "$M3/test/ipc.sh"
  step "the example clients of the API reference, run as the page shows them"
  run "$M3/test/api-examples.sh"
  step "pictures"
  run "$M3/shots.sh" --only ready,away,attention,popover-rename,pair-dial-code,pair-accept-failed-listening,arrange-three,arrange-snap --out /tmp/ferry-m3-check-shots
fi

if [ "$fail" = 0 ]; then say "ALL GREEN"; else say "SOMETHING FAILED"; fi
exit "$fail"
