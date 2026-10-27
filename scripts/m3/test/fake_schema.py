#!/usr/bin/env python3
"""The fake daemon's config answers must be the real daemon's, or a settings view built against the
fake is built against a daemon that does not exist.

Checks, with nothing running:
  * SCHEMA in fake-ferryd.py equals the key list the real ferryd serves, which is pinned by the
    snapshot crates/ferryd/src/config_api/snapshots/ferryd__config_api__tests__the_schema_of_every_key.snap
  * config_apply() refuses and accepts what ferryd::config_api::apply and the file loader do, with
    the words and hints the Rust tests (crates/ferryd/src/config_api/tests.rs, tests/config.rs) assert

    python3 -I scripts/m3/test/fake_schema.py        exit 0 when the fake and the snapshot agree
"""

import importlib.util
import json
import os
import sys

sys.dont_write_bytecode = True  # the fake is loaded by path; leave no __pycache__ behind

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.abspath(os.path.join(HERE, "..", "..", ".."))
SNAPSHOT = os.path.join(
    ROOT, "crates", "ferryd", "src", "config_api", "snapshots",
    "ferryd__config_api__tests__the_schema_of_every_key.snap",
)

spec = importlib.util.spec_from_file_location("fake_ferryd", os.path.join(HERE, "..", "fake-ferryd.py"))
fake = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fake)

failures = []


def check(name, ok, detail=""):
    if not ok:
        failures.append(f"{name}: {detail}")


# ---- the schema is the snapshot
with open(SNAPSHOT, encoding="utf-8") as f:
    text = f.read()
real = json.loads(text.split("---\n", 2)[2])["keys"]
check("same number of keys", len(real) == len(fake.SCHEMA), f"{len(real)} real, {len(fake.SCHEMA)} fake")
for want, got in zip(real, fake.SCHEMA):
    check(f"key {want['key']}", want == got, f"\n   real {want}\n   fake {got}")

by_key = {c["key"]: c for c in fake.SCHEMA}


def refuses(key, value, message, hint):
    try:
        fake.config_apply(by_key[key], value)
    except fake.ApiError as e:
        body = e.body()
        check(f"{key} = {value!r} is bad_value", body["code"] == "bad_value", str(body))
        check(f"{key} = {value!r} says {message!r}", message in body["message"], str(body))
        check(f"{key} = {value!r} names the key", key in body["message"], str(body))
        check(f"{key} = {value!r} hints {hint!r}", hint in body.get("hint", ""), str(body))
        return
    failures.append(f"{key} = {value!r} was accepted")


def accepts(key, value, saved):
    try:
        got = fake.config_apply(by_key[key], value)
    except fake.ApiError as e:
        failures.append(f"{key} = {value!r} was refused: {e.body()}")
        return
    check(f"{key} = {value!r} is saved as {saved!r}", got == saved, repr(got))


# the cases of crates/ferryd/src/config_api/tests.rs `every_refusal_is_bad_value_with_a_hint...`
for key, value, message, hint in [
    ("crossing.edge_resistance_px", 201, "out of range", "0 to 200 px"),
    ("crossing.edge_resistance_px", -1, "out of range", "0 to 200 px"),
    ("crossing.edge_resistance_px", 4.5, "whole number", "0 to 200 px"),
    ("crossing.edge_resistance_px", "40", "whole number", "0 to 200 px"),
    ("crossing.edge_resistance_px", True, "whole number", "0 to 200 px"),
    ("crossing.edge_resistance_px", None, "whole number", "0 to 200 px"),
    ("crossing.edge_resistance_px", 2**64 - 1, "whole number", "0 to 200 px"),
    ("safety.lock_check_budget_ms", 250, "out of range", "50 to 100 ms"),
    ("peer.port", 65536, "out of range", "0 to 65535"),
    ("clipboard.max_bytes", -5, "out of range", "0 to 9223372036854775807 bytes"),
    ("crossing.block_while_button_held", "yes", "true or false", "true or false"),
    ("crossing.block_while_button_held", 1, "true or false", "true or false"),
    ("input.capture", "telepathy", "not one of the choices", "one of auto, portal"),
    ("input.capture", 3, "expected a text", "one of auto, portal"),
    ("input.emulation", "uinput", "not one of the choices", "one of wayland"),
    ("crossing.escape_chord", "Ctrl+Escape", "2 modifiers", "at least two modifiers"),
    ("crossing.escape_chord", "Super+Ctrl+Nope", "Nope", "at least two modifiers"),
    ("crossing.escape_chord", 5, "expected a text", "at least two modifiers"),
    ("peer.name", 5, "expected a text", "no whitespace"),
    ("peer.name", "two words", "cannot contain", "no whitespace"),
    ("peer.name", "a:b", "cannot contain", "no whitespace"),
    ("peer.name", "x" * 65, "at most 64", "no whitespace"),
]:
    refuses(key, value, message, hint)

# what is accepted, and the spelling it is saved in
accepts("crossing.edge_resistance_px", 40, 40)
accepts("crossing.edge_resistance_px", 200, 200)
accepts("crossing.block_while_button_held", False, False)
accepts("peer.name", "lap", "lap")
accepts("peer.name", "", "")
accepts("input.capture", "layer-shell", "layer-shell")
accepts("clipboard.max_bytes", 1 << 20, 1 << 20)
accepts("peer.port", 0, 0)
accepts("peer.port", 65535, 65535)
accepts("safety.lock_check_budget_ms", 100, 100)
accepts("crossing.escape_chord", "ctrl+alt+f12", "Ctrl+Alt+F12")
accepts("crossing.escape_chord", "Super+Alt+Esc", "Super+Alt+Escape")
accepts("crossing.escape_chord", "win+shift+del", "Super+Shift+Delete")

# unknown keys: the bare name says which key was meant
try:
    raise fake.config_unknown_key("edge_resistance_px")
except fake.ApiError as e:
    check("a bare key name is answered with the one that was meant",
          "crossing.edge_resistance_px" in e.body()["hint"], str(e.body()))
try:
    raise fake.config_unknown_key("peer.nope")
except fake.ApiError as e:
    check("an unknown key points at the schema", "ferry config schema" in e.body()["hint"], str(e.body()))
    check("an unknown key is unknown_key", e.body()["code"] == "unknown_key", str(e.body()))

if failures:
    print("fake_schema: the fake daemon and the real one disagree:", file=sys.stderr)
    for f in failures:
        print("  - " + f, file=sys.stderr)
    sys.exit(1)
print(f"fake_schema: ok ({len(real)} keys, the real snapshot and the Rust refusals)")
