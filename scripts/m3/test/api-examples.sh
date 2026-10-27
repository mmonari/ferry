#!/usr/bin/env bash
# The example clients of docs/api/socket-api.md, run against the fake daemon: the shell ones, the
# Python one and the Quickshell one. Each example is also compared with the text of the document,
# so the page shows what was run. Nothing real is touched.
#
#   scripts/m3/test/api-examples.sh          exit 0 when every check passes
set -uo pipefail
# shellcheck source=../lib.sh
. "$(dirname "${BASH_SOURCE[0]}")/../lib.sh"

EX=$ROOT/docs/api/examples
DOC=$ROOT/docs/api/socket-api.md
pass=0 fail=0
ok() { pass=$((pass + 1)); say "  ok    $*"; }
bad() { fail=$((fail + 1)); say "  FAIL  $*"; }

m3_scratch
m3_fake fake --scene ready
# the examples find the socket the way a client does: from XDG_RUNTIME_DIR
run_env=(env -i PATH="$PATH" XDG_RUNTIME_DIR="$SCR/run")

say "== status.sh (socat and jq)"
if command -v socat > /dev/null && command -v jq > /dev/null; then
  out=$("${run_env[@]}" "$EX/status.sh" 2>&1)
  [ "$(printf '%s' "$out" | jq -r '.summary.state' 2> /dev/null)" = ready ] && ok "prints the status result: state ready" || bad "status.sh printed: $out"
  [ "$(printf '%s\n' "$out" | wc -l)" = 1 ] && ok "and nothing else (the hello line is filtered out)" || bad "status.sh printed more than one line"

  say "== events.sh"
  out=$("${run_env[@]}" timeout 2 "$EX/events.sh" state layout 2> /dev/null)
  [ "$(printf '%s\n' "$out" | jq -r 'select(.event == "state") | .data.summary.state' | head -1)" = ready ] && ok "the first events are the snapshots of the topics asked for" || bad "events.sh printed: $out"
  [ "$(printf '%s\n' "$out" | jq -r '.event' | sort -u | tr '\n' ' ')" = "hello layout state " ] && ok "and only those topics (and the hello)" || bad "events.sh topics: $(printf '%s\n' "$out" | jq -r '.event' | sort -u | tr '\n' ' ')"
  out=$("${run_env[@]}" timeout 2 "$EX/events.sh" 2> /dev/null)
  [ "$(printf '%s\n' "$out" | jq -r '.event' | sort -u | tr '\n' ' ')" = "config hello layout peers state " ] && ok "with no topic named it takes every topic (pairing, notice and ui have nothing to say yet)" || bad "events.sh with no topic: $(printf '%s\n' "$out" | jq -r '.event' | sort -u | tr '\n' ' ')"
else
  say "  skip  socat or jq is not installed"
fi

say "== the netcat line of the document"
nc_line=$(grep -F 'nc -U -N' "$DOC" 2> /dev/null | head -1)
if [ -z "$nc_line" ]; then
  say "  skip  the document has no netcat line yet"
elif command -v nc > /dev/null && nc -h 2>&1 | grep -q -- '-N'; then
  out=$("${run_env[@]}" sh -c "$nc_line" 2>&1)
  [ "$(printf '%s\n' "$out" | sed -n 1p | jq -r .event 2> /dev/null)" = hello ] && ok "prints the hello line first" || bad "nc printed: $out"
  [ "$(printf '%s\n' "$out" | sed -n 2p | jq -r '.result.name' 2> /dev/null)" != null ] && ok "and then the result of the request" || bad "nc printed: $out"
else
  say "  skip  nc is not OpenBSD netcat (no -N)"
fi

say "== client.py"
out=$("${run_env[@]}" python3 -I "$EX/client.py" 2>&1)
case $out in *"hello: desk api 1"*) ok "reads the hello" ;; *) bad "client.py: $out" ;; esac
case $out in *"state {'state': 'ready'}"*) ok "subscribes and prints the first state snapshots" ;; *) bad "client.py: $out" ;; esac
case $out in *"bad_value - crossing.edge_resistance_px: 900 is out of range - 0 to 200 px"*) ok "turns an error answer into an exception with code, message and hint" ;; *) bad "client.py error line: $out" ;; esac

say "== Status.qml (Quickshell Socket)"
mkdir -p "$SCR/qs"
m3_harness_dir "$SCR/qs"
cp "$EX/Status.qml" "$SCR/qs/shell.qml"
log=$(timeout 8 bash -c "$(declare -f m3_qs); SCR='$SCR'; m3_qs '$SCR/qs'" 2>&1 | grep -E 'ferry state|ERROR|TypeError|ReferenceError' | head -5)
case $log in *"ferry state: ready"*) ok "connects, subscribes, and logs the state" ;; *) bad "Status.qml logged: $log" ;; esac

say "== the examples are the text of the document"
if [ -f "$DOC" ]; then
  python3 -I - "$DOC" "$EX" <<'PY' && ok "every example appears in the document, line for line" || bad "an example differs from the document"
import os, sys
doc = open(sys.argv[1], encoding="utf-8").read()
bad = False
for name in sorted(os.listdir(sys.argv[2])):
    text = open(os.path.join(sys.argv[2], name), encoding="utf-8").read().rstrip("\n")
    if text not in doc:
        print(f"docs/api/socket-api.md does not hold docs/api/examples/{name} verbatim", file=sys.stderr)
        bad = True
sys.exit(1 if bad else 0)
PY
else
  say "  skip  $DOC is not written yet"
fi

say "$pass ok, $fail failed"
[ "$fail" = 0 ]
