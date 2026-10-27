#!/usr/bin/env python3
"""fake-ferryd: a stand-in for ferryd that speaks the whole socket API of docs/api/socket-api.md,
so every UI screen can be built and photographed without a daemon, a compositor or a
second machine.

    fake-ferryd.py --socket /tmp/x/ferryd.sock                     one scene, stays put (ready)
    fake-ferryd.py --socket ... --scene away                       start in a scene
    fake-ferryd.py --socket ... --cycle 4                          walk through every widget state, 4 s each
    fake-ferryd.py --socket ... --legacy                           an M1 daemon: no subscribe, no summary, no config.*
    fake-ferryd.py --socket ... --jitter                           move the latencies, as a real link does

Scenes: idle-first idle ready away controlled locked attention lost off.  `off` makes the daemon
disappear for a few seconds (the socket is closed and unlinked), as `systemctl --user stop ferryd`
does, so the widget's reconnect can be watched.

While it runs, anything that speaks JSON lines can steer it with `fake.*` methods:

    {"id":1,"method":"fake.scene","params":{"name":"away"}}
    {"id":2,"method":"fake.notice","params":{"kind":"peer_lost","title":"lap is lost","peer":"lap"}}
    {"id":3,"method":"fake.ui","params":{"open":"pair"}}
    {"id":3,"method":"fake.edge","params":{"monitor":"HDMI-A-1","side":"bottom","start":285,"end":1635}}   (a crossing over that edge)
    {"id":4,"method":"fake.layout_by","params":{"peer":"lap","x":-1800,"y":300}}   (lap applies a layout; "by" names another author)
    {"id":4,"method":"fake.fixture","params":{"name":"four"}}                       (two three four hidpi stack untrusted unplaced lonely)
    {"id":5,"method":"fake.peer","params":{"name":"lap","state":"offline"}}
    {"id":6,"method":"fake.pair_arrive","params":{"name":"tower"}}                 (a dialer reaches an open window)
    {"id":7,"method":"fake.dark","params":{"seconds":3}}
    {"id":8,"method":"fake.world"}                                                 (the raw state, for debugging)
    {"id":9,"method":"fake.silent"}                                                (never answers: a call timeout)
    {"id":18,"method":"fake.config_external","params":{"key":"crossing.edge_dwell_ms","value":150}}   (a change made by `ferry config set`)
    {"id":19,"method":"fake.config_readonly","params":{"on":true}}                 (config.toml cannot be written: config.set answers read_only)
    {"id":20,"method":"fake.restart","params":{"seconds":1.2}}                     (ferryd restarts: saved restart keys are in force, no longer pending)
    {"id":21,"method":"fake.config_reset"}                                         (every key back to its default)
    {"id":10,"method":"fake.pair_next","params":{"other":"mismatch"}}              (how the other machine behaves next, see below)
    {"id":11,"method":"fake.pair_reset"}                                           (drop any pairing, silently, and the script)
    {"id":15,"method":"fake.reset","params":{"scene":"ready"}}                      (a fresh world, no pairing, the default script)
    {"id":16,"method":"fake.pair_answer","params":{"match":true}}                  (the answer comes from another connection)
    {"id":17,"method":"fake.pair_elsewhere","params":{"role":"acceptor"}}          (a pairing some other connection opened: nobody here may cancel it)
    {"id":12,"method":"fake.unpaired_by","params":{"peer":"lap"}}                  (lap revoked this machine: toast and pin gone)
    {"id":13,"method":"fake.untrusted","params":{"name":"tower2"}}                 (a peer in the layout that is not paired here)
    {"id":14,"method":"fake.discover","params":{"peers":[{"name":"workshop","addr":"192.0.2.124:4243"}]}}

Pairing is simulated as ferryd does it (design m3 sections 2.3 and 9, stream B): `pair.start` dials, `pair.accept`
opens a window, a `pairing` event follows every stage to every subscriber of the `pairing` topic (and to the opener
when it has not subscribed, as in M1), `pair.answer` is accepted from any connection and the first answer wins,
`pair.cancel` too (pair-polish: from any connection, with `cancelled_by` on the end), and a dial that fails ends
with a `failed` event of its own for the subscribers. The other machine is a script, `fake.pair_next`, which keeps until it is changed:

    other        confirm (default) | mismatch | silent | cancel | drop | version | bad_reveal | protocol
    connect_ms   how long the exchange before the code takes (700)
    other_ms     how long after the code the other person acts (1500); `silent` never does
    code         the six digits (418207)       name, addr, fingerprint    who the other machine is (workshop)

Dial addresses that behave: 10.255.x.x or "unreachable" in it -> peer_unreachable (after 1.2 s); 10.254.x.x or
"closed" in it -> pairing_not_accepting; this machine's own address or name -> bad_params. Anything else dials.
`--pair-window-s N` shortens the 120 s window and the attempt, so a timeout can be watched in seconds.
`--discover` adds the M4 method `discover.list`, which M3's ferryd does not have.

The layout is the real daemon's, down to the errors: `layout.set` answers `overlap` ("lap overlaps desk",
the moved peer first), `stale_layout`, `bad_params`, `unknown_peer` and `peer_monitors_unknown`, in the order
ferryd checks them, makes one revision however many peers move, spends none when nothing changes, and every
change is a `layout` event. Links follow ferry_core::geom::derive_links and only join this machine and the
peers paired here. `--fixture NAME` starts with one of the layouts above and `--dump-layout NAME` prints that
layout as JSON and exits (the node tests compare its links with plugin/Geometry.js).

It never listens on, and refuses to be started on, the real $XDG_RUNTIME_DIR/ferry/ferryd.sock
(a lab VM, where that path is the VM's own, passes --allow-real-socket).
Python 3 standard library only.
"""

import argparse
import asyncio
import copy
import hashlib
import json
import os
import signal
import sys
import time

API = 1
VERSION = "0.1.0-fake"
COORD_CAP = 1 << 24  # ferry_core::wire::caps::COORD: an origin is at most this many px from zero
LAYOUT_PEERS = 4  # caps::LAYOUT_PEERS: a layout holds this many peers, this machine included
TOPICS = ["state", "peers", "layout", "pairing", "config", "notice", "ui", "edge"]
LEGACY_METHODS = [
    "status", "release", "id", "trust", "layout.get", "layout.place", "lock",
    "pair.accept", "pair.start", "pair.answer", "pair.cancel", "peers", "unpair", "doctor",
]
NEW_METHODS = LEGACY_METHODS + [
    "subscribe", "unsubscribe", "layout.set", "config.schema", "config.get", "config.set", "peer.rename",
]
NOTICE_RATE_S = 30.0  # contract 3: one peer_lost / peer_back toast per peer per 30 s
SCENES = ["idle-first", "idle", "ready", "away", "controlled", "locked", "attention", "lost", "off"]


def fingerprint(name):
    return hashlib.sha256(("ferry-fake:" + name).encode()).hexdigest()


def peer_name_problem(name):
    """ferry-core `validate_peer_name`: not empty, at most 64 characters, no whitespace, control character or ':'."""
    name = str(name)
    if name == "":
        return "a peer name cannot be empty"
    if len(name) > 64:
        return "a peer name is at most 64 characters"
    for c in name:
        if c.isspace() or ord(c) < 32 or 127 <= ord(c) < 160 or c == ":":
            return f"a peer name cannot contain {c!r} (no whitespace, control characters or ':')"
    return None


def addr_ok(text):
    """ferryd `parse_host_port`: host, host:port, ip:port, ::1, [::1] and [::1]:port."""
    import re
    s = text.strip()
    if not s:
        return False
    if s.startswith("["):
        m = re.fullmatch(r"\[([0-9a-fA-F:.]+)\](?::(\d{1,5}))?", s)
        return bool(m) and (m.group(2) is None or int(m.group(2)) <= 65535)
    if s.count(":") > 1:
        return bool(re.fullmatch(r"[0-9a-fA-F:.]+", s))
    host, _, port = s.partition(":")
    if _ and not (port.isdigit() and int(port) <= 65535):
        return False
    return bool(re.fullmatch(r"[A-Za-z0-9._-]{1,253}", host))


def now_iso(minus_s=0):
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(time.time() - minus_s))


# ------------------------------------------------------------------------------ config table
#
# The real daemon's `config.schema`, key for key (crates/ferryd/src/config.rs `KEYS`, pinned by the
# snapshot crates/ferryd/src/config_api/snapshots/ferryd__config_api__tests__the_schema_of_every_key.snap).
# scripts/m3/test/fake_schema.py fails when this table and that snapshot differ, so the fake cannot drift.

SCHEMA = [
    {"key": "peer.name", "type": "name", "default": "", "applies": "restart", "doc": "This machine's name on the other machines and in the layout. Empty uses the hostname"},
    {"key": "peer.port", "type": "int", "default": 4243, "min": 0, "max": 65535, "applies": "restart", "doc": "The UDP port the other machines connect to; open it in the firewall. 0 lets the kernel pick one"},
    {"key": "crossing.edge_resistance_px", "type": "int", "default": 0, "min": 0, "max": 200, "unit": "px", "applies": "live", "doc": "How far to push into an edge before the pointer crosses. 0 crosses on contact"},
    {"key": "crossing.edge_dwell_ms", "type": "int", "default": 0, "min": 0, "max": 65535, "unit": "ms", "slider_max": 1000, "applies": "live", "doc": "How long to push into an edge before the pointer crosses. 0 turns the delay off"},
    {"key": "crossing.corner_dead_zone_px", "type": "int", "default": 8, "min": 0, "max": 200, "unit": "px", "applies": "live", "doc": "The stretch at each end of an edge link that acts as a wall"},
    {"key": "crossing.block_while_button_held", "type": "bool", "default": True, "applies": "live", "doc": "Do not cross back while a mouse button is held on the other machine, so a drag stays where it began"},
    {"key": "crossing.escape_chord", "type": "chord", "default": "Super+Ctrl+Escape", "applies": "setup", "doc": "The keys that always give input back: at least two modifiers and one key. Run `ferry setup` to update the Hyprland bind"},
    {"key": "crossing.go_chord", "type": "chord", "default": "Super+Ctrl+Alt", "applies": "setup", "doc": "These modifiers with an arrow key move the keyboard and mouse to the machine on that side, or back. Run `ferry setup` to update the Hyprland binds"},
    {"key": "crossing.edge_glow", "type": "bool", "default": True, "applies": "live", "doc": "Light the edge on both machines for a moment when the pointer crosses it, in each machine's theme colour"},
    {"key": "input.capture", "type": "enum", "default": "auto", "choices": ["auto", "portal", "direct", "layer-shell", "off"], "applies": "restart", "doc": "How input is captured on this machine. off makes it a target only"},
    {"key": "input.emulation", "type": "enum", "default": "wayland", "choices": ["wayland"], "applies": "restart", "doc": "How input is injected when this machine is a target"},
    {"key": "input.scroll_invert", "type": "bool", "default": False, "applies": "restart", "doc": "Invert the scroll direction on targets", "available": False},
    {"key": "clipboard.enabled", "type": "bool", "default": True, "applies": "restart", "doc": "Share the clipboard between the machines", "available": False},
    {"key": "clipboard.max_bytes", "type": "int", "default": 33554432, "min": 0, "max": 9223372036854775807, "unit": "bytes", "applies": "restart", "doc": "The largest clipboard item that is shared", "available": False},
    {"key": "discovery.mdns", "type": "bool", "default": True, "applies": "restart", "doc": "Look for other machines on the network", "available": False},
    {"key": "discovery.advertise", "type": "bool", "default": True, "applies": "restart", "doc": "Let other machines on the network find this one", "available": False},
    {"key": "safety.lock_check_budget_ms", "type": "int", "default": 50, "min": 50, "max": 100, "unit": "ms", "applies": "restart", "doc": "How long the screen-lock check may take before a crossing counts the screen as locked. A slow machine may need more"},
    {"key": "notify.enabled", "type": "bool", "default": True, "applies": "live", "doc": "Show ferry's notifications on the desktop: the pairing code, a machine lost or back, a replaced layout, a problem that needs you. Off, they still reach the bar plugin"},
]

# The escape chord grammar of crates/ferry-core/src/chord.rs: modifier names (any of the aliases),
# one trigger key from a fixed table, saved in the canonical spelling.
CHORD_MODIFIERS = {"super": "Super", "meta": "Super", "logo": "Super", "win": "Super", "mod4": "Super",
                   "ctrl": "Ctrl", "control": "Ctrl", "alt": "Alt", "mod1": "Alt", "shift": "Shift"}
CHORD_KEYS = (["Escape"] + list("1234567890") + ["Backspace"] + list("QWERTYUIOPASDFGHJKLZXCVBNM") + ["Space"]
              + [f"F{n}" for n in range(1, 13)] + ["Home", "End", "Delete", "Pause"])
CHORD_ALIASES = {"esc": "Escape", "del": "Delete"}
CONFIG_PATH = "/home/user/.config/ferry/config.toml"


def rust_debug(text):
    """A string as Rust's {:?} prints it, for the cases a message can hold."""
    return json.dumps(text, ensure_ascii=False)


def chord_canonical(text):
    """EscapeChord::parse(text).to_string(), or the ChordError's words."""
    parts = [x.strip() for x in text.split("+")]
    if len(parts) > 4:
        return None, "the escape chord has at most 4 keys"
    mods, trigger = [], None
    for part in parts:
        mod = CHORD_MODIFIERS.get(part.lower())
        if mod:
            if mod not in mods:
                mods.append(mod)
            continue
        key = next((k for k in CHORD_KEYS if k.lower() == part.lower()), None) or CHORD_ALIASES.get(part.lower())
        if key is None:
            return None, f"unknown key name {rust_debug(part)}"
        if trigger is not None:
            return None, "the escape chord needs exactly one non-modifier key"
        trigger = key
    if trigger is None:
        return None, "the escape chord needs exactly one non-modifier key"
    if len(mods) < 2:
        return None, "the escape chord needs at least 2 modifiers"
    return "+".join(mods + [trigger]), None


def go_chord_canonical(text):
    """GoChord::parse(text).to_string(), or the ChordError's words (BL-023)."""
    parts = [x.strip() for x in text.split("+")]
    if len(parts) > 3:
        return None, "the escape chord has at most 4 keys"
    mods = []
    for part in parts:
        mod = CHORD_MODIFIERS.get(part.lower())
        if mod:
            if mod not in mods:
                mods.append(mod)
            continue
        key = next((k for k in CHORD_KEYS if k.lower() == part.lower()), None) or CHORD_ALIASES.get(part.lower())
        if key is not None or part.lower() in ("left", "right", "up", "down"):
            return None, "the go chord is modifiers only (the arrow keys are its keys)"
        return None, f"unknown key name {rust_debug(part)}"
    if len(mods) < 2:
        return None, "the escape chord needs at least 2 modifiers"
    return "+".join(mods), None


def peer_name_refusal(name):
    """ferry_core::peer::validate_peer_name, for a name that is not empty (empty is the hostname)."""
    if name == "":
        return None
    if len(name) > 64:
        return "a peer name is at most 64 characters"
    for c in name:
        if c.isspace() or c == ":" or ord(c) < 32 or 127 <= ord(c) < 160:
            return f"a peer name cannot contain {c!r} (no whitespace, control characters or ':')"
    return None


def config_allowed(spec):
    """The hint of every refusal: what the key accepts, in a few words."""
    t = spec["type"]
    if t == "bool":
        return "true or false"
    if t == "int":
        unit = f" {spec['unit']}" if spec.get("unit") else ""
        return f"{spec['min']} to {spec['max']}{unit}"
    if t == "name":
        return "no whitespace, control characters or ':', at most 64 characters; empty uses the hostname"
    if t == "chord" and spec["key"] == "crossing.go_chord":
        return "two or three modifiers (Super, Ctrl, Alt, Shift) and no key, such as Super+Ctrl+Alt"
    if t == "chord":
        return "at least two modifiers (Super, Ctrl, Alt, Shift) and one key, such as Super+Ctrl+Escape"
    return "one of " + ", ".join(spec["choices"])


def config_apply(spec, value):
    """ferryd::config_api::apply plus the loader's own rules: the value as it is saved, or a bad_value."""
    key, t = spec["key"], spec["type"]

    def wrong(expected):
        return ApiError("bad_value", f"{key}: expected {expected}, got {json.dumps(value, ensure_ascii=False)}",
                        config_allowed(spec))

    if t == "bool":
        if not isinstance(value, bool):
            raise wrong("true or false")
        return value
    if t == "int":
        if isinstance(value, bool) or not isinstance(value, int) or not -2**63 <= value < 2**63:
            raise wrong("a whole number")
        if value < spec["min"] or value > spec["max"]:
            raise ApiError("bad_value", f"{key}: {value} is out of range", config_allowed(spec))
        return value
    if not isinstance(value, str):
        raise wrong("a text")
    if t == "name":
        why = peer_name_refusal(value)
        if why:
            raise ApiError("bad_value", f"{key}: {why}", config_allowed(spec))
        return value
    if t == "chord":
        parse = go_chord_canonical if key == "crossing.go_chord" else chord_canonical
        canonical, why = parse(value)
        if why:
            raise ApiError("bad_value", f"{key}: {rust_debug(value)}: {why}", config_allowed(spec))
        return canonical
    if value not in spec["choices"]:
        raise ApiError("bad_value", f"{key}: {rust_debug(value)} is not one of the choices", config_allowed(spec))
    return value


def config_unknown_key(key):
    meant = [c["key"] for c in SCHEMA if c["key"].split(".", 1)[1] == key]
    hint = (f"did you mean {meant[0]}? `ferry config schema` lists the keys" if len(meant) == 1
            else "`ferry config schema` lists the keys")
    return ApiError("unknown_key", f"there is no config key {rust_debug(key)}", hint)


class ApiError(Exception):
    def __init__(self, code, message, hint=None):
        super().__init__(message)
        self.code = code
        self.message = message
        self.hint = hint

    def body(self):
        e = {"code": self.code, "message": self.message}
        if self.hint:
            e["hint"] = self.hint
        return e


# ---------------------------------------------------------------------------------- fixtures
#
# Layouts to draw. Monitors are local to their peer (logical pixels after scale); an
# `origin` places the peer's whole group on the plane, a `shift` is the anchor shift of
# M3 design 4.1 (the monitors sit at local + shift + origin), `trusted: False` is a peer that is placed
# in the layout without being paired here, `origin: None` is a peer the layout has not placed.
# The first four are the cases of the plan: 2, 3 and 4 peers (the daemon holds four at most), mixed HiDPI
# and a vertical stack.

def mon(name, w, h, x=0, y=0, scale=1.0, desc=None, transform=0):
    return {"name": name, "description": desc if desc is not None else f"{name} (fake)", "x": x, "y": y,
            "w": w, "h": h, "scale": scale, "transform": transform}


DESK = [mon("HDMI-A-1", 1920, 1080, 0, 0, desc="Dell Inc. DELL U2412M"),
        mon("DVI-D-1", 1920, 1080, 1920, 0, desc="Dell Inc. DELL U2412M")]
LAP = [mon("eDP-1", 1800, 1125, scale=1.6, desc="BOE 0x0BCA")]  # 2880x1800 at scale 1.6
TOWER = [mon("DP-1", 2560, 1440, desc="LG Electronics 27GL850")]
MINI = [mon("eDP-1", 1366, 768, desc="Chimei Innolux 0x15F7")]

FIXTURES = {
    # nothing paired: this machine is not placed, as on a fresh daemon
    "empty": {"me": DESK, "me_origin": None, "peers": [], "rev": 0, "rev_by": "@me"},
    # a fresh pair of machines with the same two monitors and nothing placed, as scripts/m3/dev-ferryd.sh
    # --pair --monitors host starts (run the fake with --name dev-a)
    "pair": {"me": DESK, "me_origin": None, "rev": 0, "rev_by": "@me", "peers": [
        {"name": "dev-b", "monitors": DESK, "origin": None}]},
    # journey J3 done: lap left of desk, tops aligned
    "two": {"me": DESK, "me_origin": (0, 0), "rev": 14, "peers": [
        {"name": "lap", "monitors": LAP, "origin": (-1800, 0)}]},
    "three": {"me": DESK, "me_origin": (0, 0), "rev": 14, "peers": [
        {"name": "lap", "monitors": LAP, "origin": (-1800, 0)},
        {"name": "tower", "monitors": TOWER, "origin": (3840, -180)}]},
    # the most a layout holds (caps::LAYOUT_PEERS): a netbook below the desk, as in the first-light drill
    "four": {"me": DESK, "me_origin": (0, 0), "rev": 14, "peers": [
        {"name": "lap", "monitors": LAP, "origin": (-1800, 0)},
        {"name": "tower", "monitors": TOWER, "origin": (3840, -180)},
        {"name": "mini", "monitors": MINI, "origin": (277, 1080)}]},
    # scales 2, 1.6, 1.25 and 1 side by side, none the height of the next: partial seams and walls
    "hidpi": {"me": [mon("DP-1", 1920, 1080, scale=2.0, desc="Dell Inc. DELL U2723QE")], "me_origin": (0, 0), "rev": 21, "peers": [
        {"name": "lap", "monitors": LAP, "origin": (-1800, -22)},
        {"name": "tower", "monitors": [mon("DP-2", 2752, 1152, scale=1.25, desc="Samsung Electric Company C34J79x")], "origin": (1920, -36)},
        {"name": "mini", "monitors": MINI, "origin": (4672, 200)}]},
    # a vertical stack: lap above desk, tower (two monitors one over the other) below it, a portrait monitor beside
    "stack": {"me": DESK, "me_origin": (0, 0), "rev": 9, "peers": [
        {"name": "lap", "monitors": LAP, "origin": (1020, -1125)},
        {"name": "tower", "monitors": [mon("DP-1", 1920, 1080, 0, 0, desc="LG Electronics 27GL850"),
                                        mon("DP-2", 1920, 1080, 0, 1080, desc="LG Electronics 27GL850")], "origin": (960, 1080)},
        {"name": "mini", "monitors": [mon("DP-3", 1080, 1920, desc="Dell Inc. DELL P2419H", transform=1)], "origin": (3840, -420)}]},
    # tower is placed here, but another peer paired it: it has no link to this machine
    "untrusted": {"me": DESK, "me_origin": (0, 0), "rev": 15, "peers": [
        {"name": "lap", "monitors": LAP, "origin": (-1800, 0)},
        {"name": "tower", "monitors": TOWER, "origin": (3840, 0), "trusted": False}]},
    # lap is paired, but the layout has not placed it
    "unplaced": {"me": DESK, "me_origin": (0, 0), "rev": 3, "rev_by": "desk", "peers": [
        {"name": "lap", "monitors": LAP, "origin": None},
        {"name": "tower", "monitors": TOWER, "origin": (3840, 0)}]},
    # lap touches nothing: the pointer could never reach it
    "lonely": {"me": DESK, "me_origin": (0, 0), "rev": 6, "peers": [
        {"name": "lap", "monitors": LAP, "origin": (-4200, 300)},
        {"name": "tower", "monitors": TOWER, "origin": (3840, 0)}]},
    # lap's group was moved by a hot-plug: origin and plane position differ by the anchor shift
    "shifted": {"me": DESK, "me_origin": (0, 0), "rev": 11, "peers": [
        {"name": "lap", "monitors": LAP, "origin": (-2800, 0), "shift": (1000, 0)}]},
}


# ------------------------------------------------------------------------------------ world

class World:
    """Everything the daemon knows. The scenes only set fields here; the protocol reads them."""

    def __init__(self, name, legacy):
        self.legacy = legacy
        self.me = {"name": name, "fingerprint": fingerprint(name)}
        self.port = 4243
        self.rev = {"counter": 14, "peer": fingerprint("lap")}
        # `config` is the saved file; `running` is what this ferryd runs with. They differ for a
        # `restart` key that was changed since the last start: that key is `pending`.
        self.config = {c["key"]: c["default"] for c in SCHEMA}
        self.running = dict(self.config)
        self.config_readonly = False
        self.locked = False
        self.role = "idle"
        self.active = None
        self.attention = None  # None: derive; a list: forced (a scene sets it)
        self.pairing_status = lambda: None  # the Server's: {"role","stage","expires_ms"} while a pairing runs
        self.discovered = []  # what discover.list answers (--discover)
        self.last_release = None
        self.escape_chord = "Super+Ctrl+Escape"
        self.capture = {"backend": "portal", "gate": "armed", "barriers": 2, "failed_barriers": []}
        # this machine's monitors (local coordinates) come with the fixture, see load_fixture
        self.self_monitors = []
        # the layout: the revisioned origin of every placed peer, in the order the daemon keeps them
        # (this machine first), each peer's anchor shift (M3 design 4.1), and the peers that are placed
        # in the layout without being paired here (a third peer another peer placed)
        self.origins = {}
        self.shifts = {}
        self.placed_only = {}
        # trusted peers, in pairing order
        self.peers = {}
        self.fixture_name = ""
        self.load_fixture("three")

    # ---- peers
    def add_peer(self, name, addr, monitors, origin=None, state="connected", paired="sas", shift=None, fp=None):
        fp = fp or fingerprint(name)
        self.peers[fp] = {
            "name": name, "fingerprint": fp, "addr": addr, "state": state, "rtt_us": 450, "proto": "1.0",
            "detail": None, "last_seen": now_iso(30), "suspect": False, "locked": False, "paired": paired,
            "monitors": monitors,
        }
        if origin is not None:
            self.origins[fp] = tuple(origin)
        if shift and tuple(shift) != (0, 0):
            self.shifts[fp] = tuple(shift)
        return fp

    def add_placed_only(self, name, monitors, origin, shift=None, fp=None):
        """A peer placed in the layout that is not paired here: the daemon lists it after the paired
        ones and names it by the first 8 hex digits of its fingerprint."""
        fp = fp or fingerprint(name)
        self.placed_only[fp] = {"name": fp[:8], "monitors": monitors, "label": name}
        if origin is not None:
            self.origins[fp] = tuple(origin)
        if shift and tuple(shift) != (0, 0):
            self.shifts[fp] = tuple(shift)
        return fp

    def load_fixture(self, name):
        """Replace the layout, the peers and this machine's monitors by one of FIXTURES."""
        if name not in FIXTURES:
            raise ApiError("bad_params", f"unknown fixture {name!r}", "one of " + ", ".join(FIXTURES))
        fx = FIXTURES[name]
        me = self.me["fingerprint"]
        self.self_monitors = [dict(m) for m in fx["me"]]
        self.peers.clear()
        self.placed_only.clear()
        self.origins = {}
        self.shifts = {}
        if fx.get("me_origin") is not None:
            self.origins[me] = tuple(fx["me_origin"])
        if fx.get("me_shift"):
            self.shifts[me] = tuple(fx["me_shift"])
        for peer in fx["peers"]:
            monitors = [dict(m) for m in peer["monitors"]]
            if peer.get("trusted", True):
                self.add_peer(peer["name"], peer.get("addr", "198.51.100.%d:4243" % (51 + len(self.peers))), monitors,
                              origin=peer.get("origin"), state=peer.get("state", "connected"), shift=peer.get("shift"))
            else:
                self.add_placed_only(peer["name"], monitors, peer.get("origin"), shift=peer.get("shift"))
        by = fx.get("rev_by", "lap")
        self.rev = {"counter": fx.get("rev", 14), "peer": me if by == "@me" else fingerprint(by)}
        self.fixture_name = name

    def peer_by(self, ref):
        ref = str(ref)
        for p in self.peers.values():
            if p["name"] == ref:
                return p
        hits = [p for p in self.peers.values() if p["fingerprint"].startswith(ref.lower())]
        if len(ref) >= 8 and len(hits) == 1:
            return hits[0]
        if len(hits) > 1:
            raise ApiError("ambiguous_peer", f"{ref!r} matches more than one peer")
        raise ApiError("unknown_peer", f"no trusted peer called {ref!r}", "ferry peers lists them")

    def name_of(self, fp):
        if fp == self.me["fingerprint"]:
            return self.me["name"]
        p = self.peers.get(fp)
        return p["name"] if p else fp[:8]

    def resolve_layout_peer(self, what):
        """ferryd's `resolve_layout_peer`: this machine or a paired peer by exact name, else a peer that is
        paired or only placed by at least 8 hex digits of its fingerprint."""
        what = str(what)
        me = self.me
        if what == me["name"]:
            return me["fingerprint"]
        for p in self.peers.values():
            if p["name"] == what:
                return p["fingerprint"]
        prefix = what.lower()
        if len(prefix) >= 8 and all(c in "0123456789abcdef" for c in prefix):
            pool = [me["fingerprint"]] + list(self.peers) + [fp for fp in self.placed_only if fp not in self.peers]
            hits = [fp for fp in pool if fp.startswith(prefix)]
            if len(hits) == 1:
                return hits[0]
            if len(hits) > 1:
                raise ApiError("ambiguous_peer", f"{json.dumps(what)} matches {len(hits)} peers", "use more digits of the fingerprint, or the name")
        if len(prefix) >= 8 and all(c in "0123456789abcdef" for c in prefix):
            raise ApiError("unknown_peer", f"no peer matches {json.dumps(what)}", "ferry status lists the peers")
        raise ApiError("unknown_peer", f"no peer is named {json.dumps(what)}",
                       "ferry status lists the peers; a fingerprint prefix needs at least 8 hex digits")

    # ---- scenes
    def scene(self, name):
        if name not in SCENES:
            raise ApiError("bad_params", f"unknown scene {name!r}", "one of " + ", ".join(SCENES))
        self.locked = False
        self.role = "idle"
        self.active = None
        self.attention = None
        self.capture = {"backend": "portal", "gate": "armed", "barriers": 2, "failed_barriers": []}
        for p in self.peers.values():
            p.update(state="connected", suspect=False, detail=None, locked=False, last_seen=now_iso(5),
                     rtt_us=p["rtt_us"] or 450)
        if name == "idle-first":
            self.load_fixture("empty")
            self.capture["gate"] = "idle"
        elif not self.peers:
            self.load_fixture("three")
        lap = self.peers.get(fingerprint("lap"))
        tower = self.peers.get(fingerprint("tower"))
        if name == "idle":
            for p in self.peers.values():
                p.update(state="offline", rtt_us=None, last_seen=now_iso(1500))
        elif name == "away" and lap:
            self.role, self.active = "source", lap["fingerprint"]
            lap["rtt_us"] = 410
        elif name == "controlled" and lap:
            self.role, self.active = "target", lap["fingerprint"]
            self.capture.update(gate="idle", off_reason="controlled")
            lap["rtt_us"] = 410
        elif name == "locked":
            self.locked = True
        elif name == "attention" and lap:
            lap.update(state="refused", rtt_us=None, detail="fingerprint changed")
            self.attention = [
                {"code": "peer_refused", "message": "lap cannot connect: fingerprint changed",
                 "hint": "run `ferry doctor`; after a key change, `ferry unpair lap` and pair again", "peer": "lap"},
                {"code": "capture_denied", "message": "Hyprland does not let ferry capture input",
                 "hint": "run `ferry setup` to allow xdph in ~/.config/hypr/ferry.lua, then restart Hyprland, "
                         "which reads the rule only when it starts"},
            ]
        elif name == "lost" and tower:
            tower.update(state="offline", rtt_us=None, last_seen=now_iso(14 * 60))
        self.scene_name = name

    # ---- derived UI state (contract 2.4), the same priority the daemon uses
    def attention_list(self):
        if self.attention is not None:
            return self.attention
        out = []
        for p in self.peers.values():
            if p["state"] == "refused":
                out.append({"code": "peer_refused", "message": f"{p['name']} cannot connect: {p['detail'] or 'refused'}",
                            "hint": f"run `ferry doctor`; after a key change, `ferry unpair {p['name']}` and pair again",
                            "peer": p["name"]})
        if self.capture.get("gate") == "barriers_failed":
            out.append({"code": "barriers_failed",
                        "message": "1 of 2 pointer barriers failed to set, so crossings do not start at those edges",
                        "hint": "`ferry release`, then check the monitors with `hyprctl monitors`"})
        # a pinned peer whose monitors are known and that has no place in the layout
        for fp, p in self.peers.items():
            if p["monitors"] and fp not in self.origins:
                out.append({"code": "layout_unplaced", "message": f"{p['name']} is not placed in the layout",
                            "hint": f"ferry layout place {p['name']} --right-of {self.me['name']}", "peer": p["name"]})
        return out

    def summary(self):
        if self.attention_list():
            return {"state": "attention"}
        if self.role == "source" and self.active:
            return {"state": "away", "peer": self.name_of(self.active)}
        if self.role == "target" and self.active:
            return {"state": "controlled", "peer": self.name_of(self.active)}
        if self.locked:
            return {"state": "locked"}
        if any(p["state"] == "connected" for p in self.peers.values()):
            return {"state": "ready"}
        return {"state": "idle"}

    # ---- snapshots
    def peer_status(self, p):
        out = {"name": p["name"], "fingerprint": p["fingerprint"], "state": p["state"]}
        if p["addr"]:
            out["addr"] = p["addr"]
        if p["rtt_us"] is not None and p["state"] == "connected":
            out["rtt_us"] = p["rtt_us"]
        if p["state"] != "offline" or p["proto"]:
            out["proto"] = p["proto"]
        if p["detail"]:
            out["detail"] = p["detail"]
        if p["last_seen"]:
            out["last_seen"] = p["last_seen"]
        out["suspect"] = p["suspect"]
        out["locked"] = p["locked"]
        return out

    def status(self):
        s = {"api": API, "me": dict(self.me), "port": self.port, "role": self.role}
        if self.active:
            s["active_peer"] = {"name": self.name_of(self.active), "fingerprint": self.active}
        s["capture"] = dict(self.capture)
        s["screen_locked"] = False
        s["locked"] = self.locked
        s["peers"] = [self.peer_status(p) for p in self.peers.values()]
        if self.last_release:
            s["last_release"] = {**self.last_release, "ago_ms": int((time.time() - self.last_release["at"]) * 1000)}
            s["last_release"].pop("at")
        s["counters"] = {"bounces": 0, "enters": 3, "takeovers": 0, "watchdog_releases": 0,
                         "toasts_shown": 2, "toasts_undelivered": 0, "toast_queue_full": 0, "notification_actions": 0}
        pairing = self.pairing_status()
        if pairing:
            s["pairing"] = pairing
        if not self.legacy:
            s["summary"] = self.summary()
            s["attention"] = self.attention_list()
            s["escape_chord"] = self.escape_chord
        return s

    def peers_result(self):
        out = []
        for p in self.peers.values():
            e = {"name": p["name"], "fingerprint": p["fingerprint"]}
            if p["addr"]:
                e["addr"] = p["addr"]
            if p["last_seen"]:
                e["last_seen"] = p["last_seen"]
            e.update(paired=p["paired"], state=p["state"], locked=p["locked"])
            out.append(e)
        return {"peers": out}

    # ---- layout geometry (ferry_core::geom and ferryd::state::layout_view)
    def known_monitors(self, fp):
        if fp == self.me["fingerprint"]:
            return self.self_monitors
        if fp in self.peers:
            return self.peers[fp]["monitors"]
        if fp in self.placed_only:
            return self.placed_only[fp]["monitors"]
        return []

    def shift_of(self, fp):
        return self.shifts.get(fp, (0, 0))

    def group(self, fp, origins=None):
        """Monitors of one peer in plane coordinates (local + shift + origin), or local while unplaced."""
        origins = self.origins if origins is None else origins
        o = origins.get(fp)
        dx, dy = (o[0] + self.shift_of(fp)[0], o[1] + self.shift_of(fp)[1]) if o is not None else (0, 0)
        return [{**m, "x": m["x"] + dx, "y": m["y"] + dy} for m in self.known_monitors(fp)]

    def bounds(self, fp):
        mons = self.group(fp)
        if not mons:
            return None
        x0 = min(m["x"] for m in mons)
        y0 = min(m["y"] for m in mons)
        x1 = max(m["x"] + m["w"] for m in mons)
        y1 = max(m["y"] + m["h"] for m in mons)
        return {"x": x0, "y": y0, "w": x1 - x0, "h": y1 - y0}

    def all_fps(self):
        """This machine, the peers paired here, then the peers that are only placed: the daemon's order."""
        return [self.me["fingerprint"]] + list(self.peers.keys()) + [fp for fp in self.placed_only if fp not in self.peers]

    def is_up(self, fp):
        return fp == self.me["fingerprint"] or (fp in self.peers and self.peers[fp]["state"] == "connected")

    @staticmethod
    def _overlaps(a, b):
        return (a["w"] > 0 and a["h"] > 0 and b["w"] > 0 and b["h"] > 0
                and a["x"] < b["x"] + b["w"] and b["x"] < a["x"] + a["w"]
                and a["y"] < b["y"] + b["h"] and b["y"] < a["y"] + a["h"])

    def first_overlap(self, origins, moved):
        """ferry_core::geom::first_overlap: among the pairs with a moved peer, (the moved peer, the other),
        the moved peers in turn and the others in the order of the layout's origins."""
        for a in moved:
            if a not in origins or not self.known_monitors(a):
                continue
            mine = self.group(a, origins)
            for other in origins:
                if other == a or not self.known_monitors(other):
                    continue
                theirs = self.group(other, origins)
                if any(self._overlaps(r, t) for r in mine for t in theirs):
                    return a, other
        return None

    def links(self):
        """derive_links over this machine and the peers paired here, as LinkView: both directions of every
        touching segment between monitors of two different peers, sides in Left, Right, Top, Bottom order."""
        fps = [fp for fp in [self.me["fingerprint"]] + list(self.peers)
               if fp in self.origins and self.known_monitors(fp)]
        if len(fps) < 2:
            return []
        mons = [(fp, m) for fp in fps for m in self.group(fp)]
        opposite = {"left": "right", "right": "left", "top": "bottom", "bottom": "top"}

        def line(m, side):
            return {"left": m["x"], "right": m["x"] + m["w"], "top": m["y"], "bottom": m["y"] + m["h"]}[side]

        def span(m, side):
            return (m["y"], m["y"] + m["h"]) if side in ("left", "right") else (m["x"], m["x"] + m["w"])

        out = []
        for i, (fa, a) in enumerate(mons):
            for fb, b in mons[i + 1:]:
                if fa == fb:
                    continue
                for side in ("left", "right", "top", "bottom"):
                    if line(a, side) != line(b, opposite[side]):
                        continue
                    sa, sb = span(a, side), span(b, side)
                    lo, hi = max(sa[0], sb[0]), min(sa[1], sb[1])
                    if lo >= hi:
                        continue
                    live = self.is_up(fa) and self.is_up(fb)
                    out.append({"from": f"{self.name_of(fa)}:{a['name']}", "side": side,
                                "to": f"{self.name_of(fb)}:{b['name']}", "span": [lo, hi], "live": live})
                    out.append({"from": f"{self.name_of(fb)}:{b['name']}", "side": opposite[side],
                                "to": f"{self.name_of(fa)}:{a['name']}", "span": [lo, hi], "live": live})
        return out

    def layout(self):
        peers = []
        for fp in self.all_fps():
            is_self = fp == self.me["fingerprint"]
            trusted = is_self or fp in self.peers
            mons = self.group(fp)
            entry = {
                "name": self.name_of(fp), "fingerprint": fp,
                "origin": list(self.origins[fp]) if fp in self.origins else None,
                "monitors": [{"name": m["name"], "x": m["x"], "y": m["y"], "w": m["w"], "h": m["h"], "scale": m["scale"]}
                             if self.legacy else
                             {"name": m["name"], "description": m["description"], "x": m["x"], "y": m["y"],
                              "w": m["w"], "h": m["h"], "scale": m["scale"], "transform": m["transform"]}
                             for m in mons],
                "online": True if is_self else (fp in self.peers and self.peers[fp]["state"] == "connected"),
            }
            if not self.legacy:
                entry["trusted"] = trusted
                entry["self"] = is_self
                entry["bounds"] = self.bounds(fp)
                if self.shift_of(fp) != (0, 0):
                    entry["shift"] = list(self.shift_of(fp))
            peers.append(entry)
        return {"revision": dict(self.rev), "peers": peers, "links": self.links()}

    def config_values(self):
        """The `config.get` result and the `config` event: the saved values, and the keys that wait."""
        out = {"values": dict(self.config)}
        pending = [c["key"] for c in SCHEMA if self.config[c["key"]] != self.running[c["key"]]]
        if pending:
            out["pending"] = pending
        return out


# ------------------------------------------------------------------------------ the server

class Conn:
    _ids = 0

    def __init__(self, writer):
        Conn._ids += 1
        self.id = Conn._ids
        self.writer = writer
        self.topics = None  # set once subscribed
        self.last = {}

    def send(self, obj):
        try:
            self.writer.write((json.dumps(obj, separators=(",", ":")) + "\n").encode())
        except Exception:
            pass



# ----------------------------------------------------------------------------------- pairing

PAIR_WINDOW_S = 120.0
PAIR_MAX_FAILURES = 3
DEFAULT_SCRIPT = {"other": "confirm", "connect_ms": 700, "other_ms": 1500, "code": "418207",
                  "name": "workshop", "addr": None, "fingerprint": None}
LIVE = ("listening", "connecting", "code", "waiting_peer")
FAILURE_MESSAGES = {  # ferry-core pair.rs PairFailure::message
    "rejected": "cancelled: codes differ",
    "cancelled": "cancelled",
    "timeout": "timed out",
    "bad_reveal": "cancelled: the peer broke the protocol (its reveal does not match its commitment)",
    "protocol": "cancelled: the peer broke the protocol",
    "version": "version mismatch: 1.0 vs 2.0",
    "lost": "connection lost",
}


def capitalized(text):
    return text[:1].upper() + text[1:]


class Pairing:
    """One pairing on this machine, as ferryd's CtlPairing and a pairing attempt do it together, on timers.

    A dialer starts as `connecting` and dials; an acceptor starts as `listening` and waits for an attempt. The
    other machine is a script (Server.script). Every stage is published by Server.pair_publish."""

    def __init__(self, srv, role, owner, window_s):
        self.srv = srv
        self.role = role
        self.owner = owner
        self.loop = asyncio.get_event_loop()
        self.stage = "listening" if role == "acceptor" else "connecting"
        self.deadline = self.loop.time() + window_s
        self.window_s = window_s
        self.peer = None  # {"name","fingerprint","addr"}
        self.code = None
        self.local_yes = False
        self.remote_yes = False
        self.answered = False
        self.attempt = False
        self.failures = 0
        self.cancelled = False
        self.dial_name = None  # dialer: --name
        self.dial_addr = None
        self.script = dict(srv.script)
        self.timers = []
        self.timers.append(self.loop.call_later(window_s, self.on_deadline))

    # ---- helpers
    def later(self, seconds, fn, *args):
        self.timers.append(self.loop.call_later(seconds, fn, *args))

    def expires_ms(self):
        return max(0, int((self.deadline - self.loop.time()) * 1000))

    def live(self):
        return self.stage in LIVE

    def event(self, stage, peer=True, code=None, reason=None, message=None, cancelled_by=None):
        ev = {"stage": stage}
        if not self.srv.world.legacy:
            ev["role"] = self.role
        if peer and self.peer:
            ev["peer"] = dict(self.peer)
        if code:
            ev["code"] = code
        ev["expires_ms"] = self.expires_ms()
        if reason:
            ev["reason"] = reason
        if message:
            ev["message"] = message
        if cancelled_by and not self.srv.world.legacy:
            ev["cancelled_by"] = cancelled_by
        return ev

    def current_event(self):
        """What a connection that subscribes in the middle of this pairing is told (stream B: with the code)."""
        return self.event(self.stage, code=self.code if self.stage == "code" else None)

    def emit(self, stage, **kw):
        self.stage = stage
        self.srv.pair_publish(self, self.event(stage, **kw))

    def end(self):
        for h in self.timers:
            h.cancel()
        self.timers.clear()
        if self.srv.pairing is self:
            self.srv.pairing = None
        self.srv.touch("state")

    # ---- the dialer
    def dial(self):
        """The dial succeeded: the exchange before the code starts (`connecting` was published at pair.start)."""
        self.attempt = True
        self.later(self.script["connect_ms"] / 1000.0, self.on_exchanged)

    # ---- the acceptor
    def arrive(self, overrides):
        if self.role != "acceptor" or self.stage != "listening" or self.attempt:
            raise ApiError("no_pairing", "no acceptor window is waiting: call pair.accept first")
        self.script = dict(self.srv.script)
        self.script.update(overrides)
        self.attempt = True
        # no `connecting` event for an acceptor: the next thing the window hears is the code
        self.later(self.script["connect_ms"] / 1000.0, self.on_exchanged)

    # ---- both
    def on_exchanged(self):
        if not self.attempt:
            return
        s = self.script
        name = s["name"] or "workshop"
        addr = s["addr"] or ("198.51.100.88:4243" if self.role == "acceptor" else (self.dial_addr or "198.51.100.88:4243"))
        if ":" not in addr:
            addr += ":4243"
        self.peer = {"name": name, "fingerprint": s["fingerprint"] or fingerprint(name), "addr": addr}
        if s["other"] == "version":
            return self.fail("version")
        if s["other"] in ("bad_reveal", "protocol"):
            return self.fail(s["other"])
        self.code = s["code"]
        self.local_yes = self.remote_yes = self.answered = False
        self.emit("code", code=self.code)
        if self.role == "acceptor" and not self.srv.world.legacy:
            digits = self.code
            self.srv.notice("pair_code", f"Pairing code {digits[:3]} {digits[3:]}",
                            f"{name} is asking to pair. Confirm only if it shows the same code.", name, "critical")
        if s["other"] != "silent":
            self.later(s["other_ms"] / 1000.0, self.on_remote)
        if self.srv.args.auto_pair:
            self.later(2.0, self.auto_answer)

    def auto_answer(self):
        try:
            self.answer(True)
        except ApiError:
            pass

    def on_remote(self):
        if self.stage not in ("code", "waiting_peer"):
            return
        other = self.script["other"]
        if other == "confirm":
            self.remote_yes = True
            self.maybe_agree()
        elif other == "mismatch":
            self.fail("rejected")
        elif other == "cancel":
            self.fail("cancelled", by_peer=True)
        elif other == "drop":
            self.fail("lost")

    def answer(self, match):
        """pair.answer, from any connection (M3-D8). The first answer wins."""
        if self.loop.time() >= self.deadline:
            raise ApiError("pairing_expired", "the pairing has timed out")
        if self.stage != "code":
            raise ApiError("no_pairing", "no code to answer yet")
        if self.answered:
            raise ApiError("no_pairing", "the code was already answered",
                           "the first answer counts; the pairing events say how it went")
        self.answered = True
        if not match:
            self.fail("rejected", by_us=True)
            return
        self.local_yes = True
        if self.remote_yes:
            self.maybe_agree()
        else:
            self.emit("waiting_peer")

    def maybe_agree(self):
        if self.local_yes and self.remote_yes:
            self.later(0.3, self.finish)  # the peer's acknowledgement of our Confirm (core 2.6 condition 3)

    def finish(self):
        if not self.live():
            return
        w = self.srv.world
        wanted = self.peer["name"]
        stored, message = wanted, None
        if self.dial_name and self.role == "dialer":
            stored = wanted = self.dial_name
        taken = {p["name"] for p in w.peers.values()} | {w.me["name"]}
        if stored in taken:
            n = 2
            while f"{stored}-{n}" in taken:
                n += 1
            stored = f"{wanted}-{n}"
            message = f"pinned as {stored}: the name {wanted} belongs to another peer"
        fp = self.peer["fingerprint"]
        # pinned and not placed: ferryd places nobody (the layout_unplaced attention follows)
        w.add_peer(stored, self.peer["addr"], [
            {"name": "DP-2", "description": "Fake Monitor", "x": 0, "y": 0, "w": 1920, "h": 1080, "scale": 1.0,
             "transform": 0}], origin=None, state="connecting", fp=fp)
        self.peer = {"name": stored, "fingerprint": fp, "addr": self.peer["addr"]}
        self.emit("paired", message=message)
        self.srv.notice("paired", f"Paired with {stored}", capitalized(message or ""), stored)
        self.srv.touch("state", "peers", "layout")
        self.end()
        # the link comes up a moment later, as a dial after the pin does
        self.srv.loop.call_later(0.6, self.srv.link_up, fp)

    def fail(self, reason, by_peer=False, by_us=False):
        """This attempt ended without a pin. A dialer is over; an acceptor's window may stay open."""
        if not self.live():
            return
        self.attempt = False
        message = FAILURE_MESSAGES.get(reason, "cancelled")
        by = ("peer" if by_peer else "local") if reason == "cancelled" else None
        self.emit("failed", reason=reason, message=message, cancelled_by=by)
        detail = message[len("cancelled"):].lstrip(": ") if message.startswith("cancelled") else message
        cancelled_kind = reason in ("rejected", "cancelled", "timeout", "other")
        self.srv.notice("pair_failed", "Pairing cancelled" if cancelled_kind else "Pairing failed",
                        capitalized(detail), None)
        if self.role == "dialer":
            return self.end()
        self.failures += 1
        self.peer, self.code = None, None
        if self.cancelled:
            self.close("cancelled", "pairing window closed: cancelled")
        elif self.failures >= PAIR_MAX_FAILURES:
            self.close("failures", f"pairing window closed after {PAIR_MAX_FAILURES} failed attempts")
        elif self.loop.time() >= self.deadline:
            self.close("timeout", "pairing window closed: timed out")
        else:
            self.emit("listening")

    def close(self, reason, message):
        """The acceptor's window ended without a pin."""
        self.stage = "closed"
        by = "local" if reason == "cancelled" else None
        self.srv.pair_publish(self, self.event("closed", peer=False, reason=reason, message=message, cancelled_by=by))
        self.end()

    def cancel(self):
        """pair.cancel, or the opener's connection closed (FR-PAIR-04)."""
        if not self.live():
            return
        self.cancelled = True
        if self.role == "acceptor" and not self.attempt:
            return self.close("cancelled", "pairing window closed: cancelled")
        if self.attempt or self.role == "dialer":
            self.fail("cancelled")

    def on_deadline(self):
        if not self.live():
            return
        if self.role == "dialer" or self.attempt:
            self.cancelled = False
            self.fail("timeout")
            if self.role == "dialer":
                return
        if self.srv.pairing is self and self.live():
            self.close("timeout", "pairing window closed: timed out")


class Server:
    def __init__(self, args):
        self.args = args
        self.world = World(args.name, args.legacy)
        self.conns = []
        self.dirty = set()
        self.flush_handle = None
        self.listener = None
        self.dark_until = 0.0
        self.log = (lambda *a: print("fake-ferryd:", *a, file=sys.stderr, flush=True)) if args.log else (lambda *a: None)
        self.loop = asyncio.get_event_loop()
        self.pairing = None  # the Pairing that runs, or None
        self.script = dict(DEFAULT_SCRIPT)  # how the other machine behaves (fake.pair_next)
        self.window_s = args.pair_window_s
        self.dial_delay_s = 0.4
        self.world.pairing_status = self.pairing_status
        self.world.scene_name = "ready"
        self.last_peer_notice = {}  # peer name -> monotonic time of its last peer_lost / peer_back toast
        self.seen_attention = set()  # (code, peer) in the list at the last look
        self.attention_toast_at = {}  # (code, peer) -> monotonic time of its last toast

    # ---- lifecycle
    async def start(self):
        path = self.args.socket
        os.makedirs(os.path.dirname(path), mode=0o700, exist_ok=True)
        if os.path.exists(path):
            os.unlink(path)
        self.listener = await asyncio.start_unix_server(self.on_connect, path=path)
        os.chmod(path, 0o600)
        self.log("listening on", path)

    async def stop_listening(self):
        if self.listener:
            self.listener.close()
        # close the clients first: since Python 3.12 wait_closed() waits for them to go
        for c in list(self.conns):
            try:
                c.writer.close()
            except Exception:
                pass
        self.conns.clear()
        if self.listener:
            try:
                await asyncio.wait_for(self.listener.wait_closed(), 1.0)
            except asyncio.TimeoutError:
                pass
            self.listener = None
        try:
            os.unlink(self.args.socket)
        except FileNotFoundError:
            pass

    async def go_dark(self, seconds):
        self.log("dark for", seconds, "s")
        await self.stop_listening()
        await asyncio.sleep(seconds)
        await self.start()
        self.touch("state", "peers", "layout")

    # ---- connections
    async def on_connect(self, reader, writer):
        conn = Conn(writer)
        self.conns.append(conn)
        hello = {"api": API, "version": VERSION, "peer": self.world.me["name"], "fingerprint": self.world.me["fingerprint"]}
        if not self.world.legacy:
            hello["methods"] = NEW_METHODS + (["discover.list"] if self.args.discover else [])
            hello["topics"] = TOPICS
        conn.send({"event": "hello", "data": hello})
        try:
            while True:
                line = await reader.readline()
                if not line:
                    break
                # one task per request: a call that never answers (fake.silent) must not hold
                # the connection, as a real daemon does not let one slow call hold it either
                asyncio.ensure_future(self.on_line(conn, line.decode(errors="replace").strip()))
        except (ConnectionResetError, asyncio.IncompleteReadError):
            pass
        finally:
            if conn in self.conns:
                self.conns.remove(conn)
            if self.pairing is not None and self.pairing.owner is conn:
                self.pairing.cancel()
            try:
                writer.close()
            except Exception:
                pass

    async def on_line(self, conn, line):
        if not line:
            return
        try:
            req = json.loads(line)
            rid = req["id"]
            method = req["method"]
        except Exception:
            conn.send({"id": 0, "error": {"code": "bad_params", "message": "a request is a JSON object with id and method"}})
            return
        params = req.get("params") or {}
        self.log("<-", method, json.dumps(params)[:120])
        try:
            result = await self.dispatch(conn, method, params)
            conn.send({"id": rid, "result": result})
        except ApiError as e:
            conn.send({"id": rid, "error": e.body()})
        except Exception as e:  # a bug in the fake must not kill it
            conn.send({"id": rid, "error": {"code": "internal", "message": f"{type(e).__name__}: {e}"}})
        self.flush_soon()

    # ---- events
    def touch(self, *topics):
        self.dirty.update(topics)
        self.flush_soon()

    def flush_soon(self):
        if self.flush_handle is None and self.dirty:
            self.flush_handle = asyncio.get_event_loop().call_later(0.05, self.flush)

    def snapshot(self, topic):
        w = self.world
        if topic == "state":
            return w.status()
        if topic == "peers":
            return w.peers_result()
        if topic == "layout":
            return w.layout()
        if topic == "config":
            return w.config_values()
        if topic == "pairing" and self.pairing is not None and self.pairing.live():
            return self.pairing.current_event()
        return None

    def look_at_attention(self):
        """Stream C: an `attention` toast is edge-triggered per (code, peer), and one that goes and comes back
        inside 5 min is not another. Critical, the entry's message as the title and its hint as the body."""
        if self.world.legacy:
            return
        now = self.loop.time()
        keys = set()
        for a in self.world.attention_list():
            key = (a["code"], a.get("peer", ""))
            keys.add(key)
            if key in self.seen_attention:
                continue
            last = self.attention_toast_at.get(key)
            if last is not None and now - last < 300:
                continue
            self.attention_toast_at[key] = now
            self.notice("attention", a["message"], a.get("hint", ""), a.get("peer"), "critical")
        self.seen_attention = keys

    def flush(self):
        self.flush_handle = None
        dirty, self.dirty = self.dirty, set()
        if "state" in dirty:
            self.look_at_attention()
        for conn in self.conns:
            if conn.topics is None:
                continue
            for topic in sorted(dirty & conn.topics):
                if topic in ("notice", "ui", "pairing", "edge"):
                    continue  # sent as they happen, never as a diff
                data = self.snapshot(topic)
                text = json.dumps(data, sort_keys=True)
                if conn.last.get(topic) != text:
                    conn.last[topic] = text
                    conn.send({"event": topic, "data": data})

    def emit(self, topic, data):
        for conn in self.conns:
            if conn.topics is not None and topic in conn.topics:
                conn.send({"event": topic, "data": data})

    def notice(self, kind, title, body="", peer=None, urgency="normal"):
        data = {"kind": kind, "title": title, "body": body, "urgency": urgency}
        if peer:
            data["peer"] = peer
        self.emit("notice", data)

    # ---- pairing
    def pairing_status(self):
        """`status.pairing`: never with the code."""
        p = self.pairing
        if p is None or not p.live():
            return None
        return {"role": p.role, "stage": p.stage, "expires_ms": p.expires_ms()}

    def pair_publish(self, p, ev):
        """Every stage is an event to every subscriber of `pairing`, and to the opener when it did not
        subscribe, as in M1 (stream B, section 9 item 4). A daemon from before M3 tells the opener only."""
        line = {"event": "pairing", "data": ev}
        for conn in self.conns:
            subscribed = conn.topics is not None and "pairing" in conn.topics
            if self.world.legacy:
                if conn is p.owner:
                    conn.send(line)
            elif subscribed or conn is p.owner:
                conn.send(line)
        self.touch("state")

    def link_up(self, fp):
        peer = self.world.peers.get(fp)
        if peer and peer["state"] == "connecting":
            peer.update(state="connected", rtt_us=520, last_seen=now_iso())
            self.touch("state", "peers", "layout")

    # ---- dispatch
    async def dispatch(self, conn, method, params):
        w = self.world
        if w.legacy and method not in LEGACY_METHODS and not method.startswith("fake."):
            raise ApiError("unknown_method", f"unknown method {method!r}")
        handler = getattr(self, "m_" + method.replace(".", "_"), None)
        if handler is None:
            raise ApiError("unknown_method", f"unknown method {method!r}")
        r = handler(conn, params)
        if asyncio.iscoroutine(r):
            r = await r
        return r

    def m_status(self, conn, p):
        return self.world.status()

    def m_peers(self, conn, p):
        return self.world.peers_result()

    def m_layout_get(self, conn, p):
        return self.world.layout()

    def m_id(self, conn, p):
        return {"name": self.world.me["name"], "fingerprint": self.world.me["fingerprint"], "port": self.world.port,
                "addrs": ["192.0.2.111"]}

    def m_release(self, conn, p):
        w = self.world
        if w.role in ("source", "target"):
            w.last_release = {"reason": "force", "at": time.time()}
        w.role, w.active = "idle", None
        w.capture.pop("off_reason", None)
        w.capture["gate"] = "armed"
        self.touch("state")
        return {}

    def m_lock(self, conn, p):
        w = self.world
        on = p.get("on")
        w.locked = (not w.locked) if on is None else bool(on)
        self.touch("state", "peers")
        return {"locked": w.locked}

    def m_trust(self, conn, p):
        w = self.world
        fp = str(p.get("fingerprint", "")).replace(":", "").replace(" ", "").lower()
        if len(fp) != 64 or any(c not in "0123456789abcdef" for c in fp):
            raise ApiError("bad_fingerprint", "a fingerprint is 64 hex digits")
        if fp == w.me["fingerprint"]:
            raise ApiError("self_fingerprint", "that is this machine's own fingerprint")
        name = p.get("name") or fp[:8]
        w.peers[fp] = {"name": name, "fingerprint": fp, "addr": p.get("addr"), "state": "offline", "rtt_us": None,
                       "proto": None, "detail": None, "last_seen": None, "suspect": False, "locked": False,
                       "paired": "manual",
                       "monitors": [{"name": "DP-1", "description": "", "x": 0, "y": 0, "w": 1920, "h": 1080,
                                     "scale": 1.0, "transform": 0}]}
        self.touch("state", "peers", "layout")
        return {"peer": {"name": name, "fingerprint": fp}}

    def m_unpair(self, conn, p):
        w = self.world
        peer = w.peer_by(p.get("peer", ""))
        # a pairing in flight with this peer is not affected: ferryd closes only the link
        del w.peers[peer["fingerprint"]]
        w.origins.pop(peer["fingerprint"], None)
        w.shifts.pop(peer["fingerprint"], None)
        if w.active == peer["fingerprint"]:
            w.role, w.active = "idle", None
        w.rev = {"counter": w.rev["counter"] + 1, "peer": w.me["fingerprint"]}
        self.touch("state", "peers", "layout")
        return {"peer": {"name": peer["name"], "fingerprint": peer["fingerprint"]}}

    def m_peer_rename(self, conn, p):
        """ferryd api.rs peer_rename: the rule of peer.name, then not this machine's name, then not another peer's."""
        w = self.world
        name = str(p.get("name", ""))
        problem = peer_name_problem(name)
        if problem:
            raise ApiError("bad_params", problem)
        peer = w.peer_by(p.get("peer", ""))
        if name == w.me["name"]:
            raise ApiError("bad_params", f"{name!r} is this machine's own name", "pick another name")
        for other in w.peers.values():
            if other is not peer and other["name"] == name:
                raise ApiError("bad_params", f"the name {name!r} already belongs to peer {other['fingerprint']}",
                               "pick another name")
        peer["name"] = name
        self.touch("state", "peers", "layout")
        return {"peer": {"name": name, "fingerprint": peer["fingerprint"]}}

    def m_doctor(self, conn, p):
        return {"checks": [{"name": "ferryd", "status": "pass", "message": "running (fake)"}]}

    # ---- layout
    def m_layout_place(self, conn, p):
        w = self.world
        peer = w.peer_by(p["peer"])
        anchor_ref = str(p["anchor"]).split(":")[0]
        anchor_fp = w.me["fingerprint"] if anchor_ref in (w.me["name"], "") else w.peer_by(anchor_ref)["fingerprint"]
        ab = w.bounds(anchor_fp)
        pb = w.bounds(peer["fingerprint"])
        ox, oy = w.origins.get(peer["fingerprint"], (0, 0))
        rel = p["rel"]
        if rel == "right_of":
            nx, ny = ab["x"] + ab["w"], ab["y"]
        elif rel == "left_of":
            nx, ny = ab["x"] - pb["w"], ab["y"]
        elif rel == "below":
            nx, ny = ab["x"], ab["y"] + ab["h"]
        else:
            nx, ny = ab["x"], ab["y"] - pb["h"]
        return self._set_origins([(peer["fingerprint"], (nx - (pb["x"] - ox), ny - (pb["y"] - oy)))])

    def m_layout_set(self, conn, p):
        """`layout.set` as ferryd answers it (m3 design 2.5 and 4.2). The checks run in ferryd's order and
        the first to fail answers: the params, then base_rev (stale_layout), then the geometry (overlap)."""
        w = self.world
        origins = p.get("origins")
        if not isinstance(origins, list) or not origins:
            raise ApiError("bad_params", "origins is empty", "name at least one peer with its x and y")
        moves = []
        for o in origins:
            if not isinstance(o, dict) or not isinstance(o.get("peer"), str):
                raise ApiError("bad_params", "each origin is {peer, x, y}")
            fp = w.resolve_layout_peer(o["peer"])
            name = w.name_of(fp)
            if any(m[0] == fp for m in moves):
                raise ApiError("bad_params", f"{name} is named twice", "name each peer once")
            x, y = o.get("x"), o.get("y")
            if isinstance(x, bool) or isinstance(y, bool) or not isinstance(x, int) or not isinstance(y, int):
                raise ApiError("bad_params", f"{name}: x and y are whole numbers")
            if abs(x) > COORD_CAP or abs(y) > COORD_CAP:
                raise ApiError("bad_params", f"{name}: x and y are at most {COORD_CAP} px")
            if not w.known_monitors(fp):
                if fp == w.me["fingerprint"]:
                    raise ApiError("peer_monitors_unknown", "this machine reports no monitors", "is Hyprland running? ferry doctor checks")
                raise ApiError("peer_monitors_unknown", f"the monitors of {name} were never received",
                               "start ferryd on that machine and check ferry status")
            moves.append((fp, (x, y)))

        base = p.get("base_rev")
        if base is not None:
            peer = base.get("peer") if isinstance(base, dict) else None
            if not (isinstance(peer, str) and len(peer) == 64 and all(c in "0123456789abcdef" for c in peer.lower())):
                raise ApiError("bad_params", f"base_rev.peer {json.dumps(peer)} is not a fingerprint",
                               "send the revision that layout.get answered")
            if base.get("counter") != w.rev["counter"] or peer.lower() != w.rev["peer"]:
                raise ApiError("stale_layout", f"the layout changed (rev {w.rev['counter']} by {w.name_of(w.rev['peer'])})",
                               "read the layout again with layout.get, then apply your change on top of it")
        return self._set_origins(moves)

    def _set_origins(self, moves):
        """geom::set_origins plus the commit: one revision for any number of moved groups, none when
        every named origin already holds, the layout untouched when anything is refused."""
        w = self.world
        me = w.me["fingerprint"]
        unchanged = me in w.origins and all(w.origins.get(fp) == tuple(o) for fp, o in moves)
        if unchanged:
            return w.layout()
        # Layout::with_origins: this machine gets (0, 0) the first time anything is placed, the moved
        # peers go to the end of the list, the others keep their place
        moved = [fp for fp, _ in moves]
        origins = {fp: o for fp, o in w.origins.items() if fp not in moved}
        for fp, o in moves:
            origins[fp] = tuple(o)
        if me not in origins:
            origins = {me: (0, 0), **origins}
        if len(origins) > LAYOUT_PEERS:
            raise ApiError("bad_params", f"a layout holds at most {LAYOUT_PEERS} peers", "unpair a peer you no longer use")
        clash = w.first_overlap(origins, moved)
        if clash:
            raise ApiError("overlap", f"{w.name_of(clash[0])} overlaps {w.name_of(clash[1])}",
                           "ferry layout show lists where each peer's monitors are")
        w.origins = origins
        w.rev = {"counter": w.rev["counter"] + 1, "peer": me}
        self.touch("layout", "state")
        return w.layout()

    # ---- pairing methods (ferryd pairing.rs)
    def _busy(self):
        return ApiError("pairing_busy", "a pairing already runs on this machine",
                        "finish or cancel it first (Ctrl+C in the terminal that runs ferry pair)")

    def m_pair_accept(self, conn, p):
        if self.pairing is not None:
            raise self._busy()
        pairing = Pairing(self, "acceptor", conn, self.window_s)
        self.pairing = pairing
        pairing.emit("listening")
        return {"expires_ms": int(self.window_s * 1000)}

    async def m_pair_start(self, conn, p):
        w = self.world
        addr = str(p.get("addr") or "").strip()
        name = p.get("name")
        if name is not None:
            problem = peer_name_problem(name)
            if problem:
                raise ApiError("bad_params", problem)
        if not addr_ok(addr):
            raise ApiError("bad_params", f"addr {addr!r}: bad host", "write host, host:port or ip:port")
        if self.pairing is not None:
            raise self._busy()
        pairing = Pairing(self, "dialer", conn, self.window_s)
        pairing.dial_name, pairing.dial_addr = name, addr if ":" in addr else addr + ":4243"
        self.pairing = pairing
        # the `connecting` event goes out at once, and the answer to pair.start only when the dial is over:
        # a dial that fails leaves a `connecting` event that nothing follows, as ferryd does
        pairing.emit("connecting")
        await asyncio.sleep(self.dial_delay_s)
        host = addr.split(":")[0] if addr.count(":") <= 1 else addr
        failure, reason = None, None
        if addr.startswith("10.255.") or "unreachable" in addr:
            await asyncio.sleep(self.dial_delay_s * 2)  # a dial that gets no answer takes longer than one that does
            failure = ApiError("peer_unreachable", f"{addr} did not answer: no answer within 5 s",
                               "check the address, that ferryd runs there, and its firewall (UDP 4243)")
            reason = "unreachable"
        elif addr.startswith("10.254.") or "closed" in addr:
            failure = ApiError("pairing_not_accepting", f"{addr} is not accepting pairing",
                               "run ferry pair --accept there, or wait: it may be busy with another request")
            reason = "not_accepting"
        elif host in (w.me["name"], "localhost", "127.0.0.1", "192.0.2.111"):
            failure = ApiError("bad_params", f"{addr} is this machine", "give the other machine's address")
            reason = "own_key"
        if self.pairing is not pairing:
            return {}  # cancelled while dialling: the dial's end sees it, and the answer is Ok
        if failure is not None:
            self.pairing = None
            pairing.end()
            if not self.world.legacy:
                # the subscribers hear the end; an opener that did not subscribe has the error alone (M1)
                ev = pairing.event("failed", peer=False, reason=reason, message=failure.message)
                for c in self.conns:
                    if c.topics is not None and "pairing" in c.topics:
                        c.send({"event": "pairing", "data": ev})
            self.touch("state")
            raise failure
        pairing.dial()
        return {}

    def m_pair_answer(self, conn, p):
        pairing = self.pairing
        if self.world.legacy:
            # M1: only the connection that started the pairing may answer
            if pairing is None or pairing.owner is not conn:
                raise ApiError("no_pairing", "no pairing runs on this connection",
                               "pair.answer and pair.cancel go on the connection that sent pair.start or pair.accept")
        elif pairing is None:
            raise ApiError("no_pairing", "no pairing runs on this machine", "start one with ferry pair, or open the Pair panel")
        pairing.answer(bool(p.get("match")))
        return {}

    def m_pair_cancel(self, conn, p):
        pairing = self.pairing
        if pairing is not None and self.world.legacy and pairing.owner is not conn:
            raise ApiError("no_pairing", "no pairing runs on this connection",
                           "pair.answer and pair.cancel go on the connection that sent pair.start or pair.accept")
        if pairing is None:
            raise ApiError("no_pairing", "no pairing runs on this machine",
                           "start one with ferry pair, or open the Pair panel")
        pairing.cancel()
        return {}

    # ---- subscribe, config
    def m_subscribe(self, conn, p):
        topics = p.get("topics")
        if topics is None:
            topics = list(TOPICS)
        bad = [t for t in topics if t not in TOPICS]
        if bad:
            raise ApiError("bad_params", f"unknown topic {bad[0]!r}", "topics: " + ", ".join(TOPICS))
        conn.topics = set(topics)
        conn.last = {}
        # the result goes out first (the caller sends it), then one snapshot per topic, at once
        asyncio.get_event_loop().call_soon(self._snapshots, conn)
        return {"topics": topics}

    def _snapshots(self, conn):
        for topic in TOPICS:
            if conn.topics is None or topic not in conn.topics or topic in ("notice", "ui"):
                continue
            data = self.snapshot(topic)
            if data is None:
                continue
            conn.last[topic] = json.dumps(data, sort_keys=True)
            conn.send({"event": topic, "data": data})

    def m_unsubscribe(self, conn, p):
        conn.topics = None
        return {}

    def m_config_schema(self, conn, p):
        return {"keys": SCHEMA}

    def m_config_get(self, conn, p):
        w = self.world
        key = p.get("key")
        out = w.config_values()
        if key is None:
            return out
        if not any(c["key"] == key for c in SCHEMA):
            raise config_unknown_key(key)
        out["values"] = {key: out["values"][key]}
        out["pending"] = [k for k in out.get("pending", []) if k == key]
        if not out["pending"]:
            del out["pending"]
        return out

    def m_config_set(self, conn, p):
        """ferryd::config_api::set: check, refuse a file it cannot write, save, apply what applies at once."""
        w = self.world
        key = p.get("key")
        spec = next((c for c in SCHEMA if c["key"] == key), None)
        if spec is None:
            raise config_unknown_key(str(key))
        if "value" not in p:
            raise ApiError("bad_params", "config.set: missing field `value`")
        value = config_apply(spec, p["value"])
        if w.config_readonly:
            raise ApiError("read_only", f"{CONFIG_PATH} cannot be written: Permission denied (os error 13)",
                           "make the file and its directory writable, or edit it by hand; nothing was changed")
        w.config[key] = value
        if spec["applies"] != "restart":
            w.running[key] = value
        if key == "crossing.escape_chord":
            # ferryd matches the new chord at once; only the Hyprland bind waits for `ferry setup`
            w.escape_chord = value
        self.touch("config", "state")
        return {"key": key, "value": value, "applies": spec["applies"]}

    # ---- fake.* : steer the daemon from a test or a screenshot run
    def m_fake_scene(self, conn, p):
        name = p.get("name", "ready")
        if name == "off":
            asyncio.ensure_future(self.go_dark(float(p.get("seconds", 3))))
            return {"scene": "off"}
        self.world.scene(name)
        self.touch("state", "peers", "layout")
        return {"scene": name}

    def m_fake_notice(self, conn, p):
        self.notice(p.get("kind", "peer_back"), p.get("title", "ferry"), p.get("body", ""), p.get("peer"),
                    p.get("urgency", "normal"))
        return {}

    def m_fake_ui(self, conn, p):
        self.emit("ui", {"open": p.get("open", "popover")})
        return {}

    def m_fake_edge(self, conn, p):
        self.emit("edge", {"role": p.get("role", "source"), "home": bool(p.get("home", False)),
                           "peer": p.get("peer", "lap"), "monitor": p.get("monitor", "HDMI-A-1"),
                           "side": p.get("side", "bottom"), "start": int(p.get("start", 285)),
                           "end": int(p.get("end", 1635))})
        return {}

    def m_fake_dark(self, conn, p):
        asyncio.ensure_future(self.go_dark(float(p.get("seconds", 3))))
        return {}

    def m_fake_peer(self, conn, p):
        peer = self.world.peer_by(p["name"])
        before = peer["state"]
        for k in ("state", "rtt_us", "suspect", "locked", "detail", "last_seen", "addr"):
            if k in p:
                peer[k] = p[k]
        now = self.loop.time()
        if p.get("notice", True) and before != peer["state"] and "connected" in (before, peer["state"]):
            if now - self.last_peer_notice.get(peer["name"], -1e9) >= NOTICE_RATE_S:
                self.last_peer_notice[peer["name"]] = now
                if peer["state"] == "connected":
                    self.notice("peer_back", f"{peer['name']} is back", "", peer["name"])
                else:
                    self.notice("peer_lost", f"Lost {peer['name']}", "", peer["name"])
        self.touch("state", "peers", "layout")
        return {}

    def m_fake_unpaired_by(self, conn, p):
        """The other machine revoked this one (FR-PAIR-08): the pin goes and a toast says so."""
        w = self.world
        peer = w.peer_by(p["peer"])
        del w.peers[peer["fingerprint"]]
        w.origins.pop(peer["fingerprint"], None)
        w.shifts.pop(peer["fingerprint"], None)
        if w.active == peer["fingerprint"]:
            w.role, w.active = "idle", None
        self.notice("unpaired", f"{peer['name']} unpaired this machine", "Pair again to reconnect", peer["name"])
        self.touch("state", "peers", "layout")
        return {}

    def m_fake_untrusted(self, conn, p):
        """A peer that is placed in the layout but not paired here (a third peer another peer placed, J8)."""
        w = self.world
        if p.get("clear"):
            for fp in list(w.placed_only):
                w.origins.pop(fp, None)
                w.shifts.pop(fp, None)
            w.placed_only.clear()
            self.touch("layout")
            return {}
        fp = w.add_placed_only(p.get("name", "tower2"), [mon("DP-3", 1920, 1080, 0, 0, 1.0, "LG Electronics 24MK")],
                               tuple(p.get("origin", (0, 1080))), fp=p.get("fingerprint"))
        self.touch("layout")
        return {"fingerprint": fp, "name": fp[:8]}

    def m_fake_discover(self, conn, p):
        self.world.discovered = list(p.get("peers", []))
        return {}

    def m_discover_list(self, conn, p):
        """An M4 method: the machines advertising _ferry._udp on this link. Only with --discover."""
        if not self.args.discover:
            raise ApiError("unknown_method", "unknown method 'discover.list'")
        trusted = {x["fingerprint"] for x in self.world.peers.values()}
        out = []
        for d in self.world.discovered:
            fp = d.get("fingerprint") or fingerprint(d["name"])
            out.append({"name": d["name"], "fingerprint": fp, "addr": d.get("addr", ""), "trusted": fp in trusted})
        return {"peers": out}

    def m_fake_layout_by(self, conn, p):
        """Another peer applies a layout: the revision moves on without us (FR-UI-17). `peer` is the group
        that moves, `by` the peer that made the edit (default: the same one)."""
        w = self.world
        peer = w.peer_by(p["peer"])
        author = w.peer_by(p["by"]) if p.get("by") else peer
        origins = {**w.origins, peer["fingerprint"]: (int(p["x"]), int(p["y"]))}
        clash = w.first_overlap(origins, [peer["fingerprint"]])
        if clash:
            raise ApiError("overlap", f"{w.name_of(clash[0])} overlaps {w.name_of(clash[1])}", "fake.layout_by refuses an overlap too")
        w.origins = origins
        w.rev = {"counter": w.rev["counter"] + 1, "peer": author["fingerprint"]}
        self.touch("layout", "state")
        self.notice("layout_replaced", f"Layout replaced by {author['name']}", peer=author["name"])
        return {"revision": dict(w.rev)}

    def m_fake_fixture(self, conn, p):
        """Replace the layout and the peers by one of FIXTURES (two three four hidpi stack ...)."""
        self.world.load_fixture(p.get("name", "three"))
        self.touch("state", "peers", "layout")
        return {"fixture": self.world.fixture_name, "revision": dict(self.world.rev)}

    def m_fake_monitors(self, conn, p):
        """Replace the monitors a machine reports (a hot-plug, or a layout to mirror a real one):
        `peer` is a name, or this machine's; `monitors` are local, {name, x, y, w, h, scale, transform, description}."""
        w = self.world
        mons = [mon(m["name"], m["w"], m["h"], m.get("x", 0), m.get("y", 0), m.get("scale", 1.0), m.get("description"), m.get("transform", 0))
                for m in p["monitors"]]
        if p["peer"] == w.me["name"]:
            w.self_monitors = mons
        else:
            w.peer_by(p["peer"])["monitors"] = mons
        self.touch("state", "peers", "layout")
        return {}

    def m_fake_untrust(self, conn, p):
        """Another peer unpairs: it stays placed in the layout but is no longer paired here."""
        w = self.world
        peer = w.peer_by(p["peer"])
        del w.peers[peer["fingerprint"]]
        w.placed_only[peer["fingerprint"]] = {"name": peer["fingerprint"][:8], "monitors": peer["monitors"], "label": peer["name"]}
        self.touch("state", "peers", "layout")
        return {}

    def m_fake_pair_arrive(self, conn, p):
        """A dialer reaches the open window. Any `fake.pair_next` key may be given here for this attempt only."""
        if self.pairing is None:
            raise ApiError("no_pairing", "no acceptor window is open: call pair.accept first")
        overrides = {k: v for k, v in p.items() if k in DEFAULT_SCRIPT}
        self.pairing.arrive(overrides)
        return {}

    def m_fake_pair_next(self, conn, p):
        """How the other machine behaves from the next attempt on."""
        if p.get("reset"):
            self.script = dict(DEFAULT_SCRIPT)
        for k, v in p.items():
            if k in DEFAULT_SCRIPT:
                self.script[k] = v
        if "window_s" in p:
            self.window_s = float(p["window_s"])
        if "dial_delay_ms" in p:
            self.dial_delay_s = float(p["dial_delay_ms"]) / 1000.0
        return {"script": dict(self.script), "window_s": self.window_s}

    def m_fake_reset(self, conn, p):
        """Everything back to how a fresh fake starts (in a scene): the world, the pairing, the script. The start of a test."""
        if self.pairing is not None:
            # a pairing that runs is cancelled with its events, as ferryd would: a client that is counting
            # down must not be left with a stage that nothing will ever end
            self.pairing.cancel()
            if self.pairing is not None:
                self.pairing.end()
            self.pairing = None
        self.script = dict(DEFAULT_SCRIPT)
        self.window_s = self.args.pair_window_s
        self.dial_delay_s = 0.4
        self.world = World(self.args.name, self.args.legacy)
        self.world.pairing_status = self.pairing_status
        self.world.scene(p.get("scene", "ready"))
        self.world.scene_name = p.get("scene", "ready")
        self.last_peer_notice.clear()
        self.seen_attention.clear()
        self.attention_toast_at.clear()
        self.touch("state", "peers", "layout", "config")
        return {}

    def m_fake_pair_answer(self, conn, p):
        """The answer comes from another connection, as a terminal's `ferry pair` or a notification button would send it."""
        if self.pairing is None:
            raise ApiError("no_pairing", "no pairing runs on this machine")
        self.pairing.answer(bool(p.get("match", True)))
        return {}

    def m_fake_pair_elsewhere(self, conn, p):
        """A pairing that some other connection opened (`ferry pair --accept` in a terminal): no connection here owns it,
        and every subscriber hears it; any connection may cancel it."""
        if self.pairing is not None:
            raise self._busy()
        role = p.get("role", "acceptor")
        pairing = Pairing(self, role, None, self.window_s)
        self.pairing = pairing
        pairing.emit("listening" if role == "acceptor" else "connecting")
        if role == "dialer":
            pairing.dial_addr = p.get("addr", "198.51.100.88:4243")
            pairing.dial()
        return {}

    def m_fake_pair_reset(self, conn, p):
        """Drop any pairing without a word, and the script: the start of a test."""
        if self.pairing is not None:
            self.pairing.end()
            self.pairing = None
        self.script = dict(DEFAULT_SCRIPT)
        self.window_s = self.args.pair_window_s
        self.dial_delay_s = 0.4
        self.touch("state")
        return {}

    def m_fake_config_external(self, conn, p):
        """Somebody else changes a value, as `ferry config set` does from a terminal."""
        w = self.world
        spec = next((c for c in SCHEMA if c["key"] == p.get("key")), None)
        if spec is None:
            raise config_unknown_key(str(p.get("key")))
        value = config_apply(spec, p.get("value"))
        w.config[spec["key"]] = value
        if spec["applies"] != "restart":
            w.running[spec["key"]] = value
        if spec["key"] == "crossing.escape_chord":
            w.escape_chord = value
        self.touch("config", "state")
        return {"key": spec["key"], "value": value}

    def m_fake_config_reset(self, conn, p):
        """Every key back to its default, saved and in force, and config.toml writable again."""
        w = self.world
        w.config = {c["key"]: c["default"] for c in SCHEMA}
        w.running = dict(w.config)
        w.config_readonly = False
        w.escape_chord = w.config["crossing.escape_chord"]
        self.touch("config", "state")
        return {}

    def m_fake_config_readonly(self, conn, p):
        """config.toml (or its directory) cannot be written: every config.set answers read_only."""
        self.world.config_readonly = bool(p.get("on", True))
        return {}

    def m_fake_restart(self, conn, p):
        """`systemctl --user restart ferryd`: the saved config is now the one in force, and the
        socket is gone for a moment."""
        w = self.world
        w.running = dict(w.config)
        asyncio.ensure_future(self.go_dark(float(p.get("seconds", 1.2))))
        return {}

    async def m_fake_silent(self, conn, p):
        """Never answers: for a client's call timeout."""
        await asyncio.sleep(3600)
        return {}

    def m_fake_world(self, conn, p):
        w = self.world
        return {"scene": w.scene_name, "fixture": w.fixture_name, "origins": {w.name_of(k): v for k, v in w.origins.items()},
                "rev": w.rev, "config": w.config, "conns": len(self.conns)}

    # ---- background
    async def cycle(self, seconds):
        scenes = [s for s in SCENES]
        i = 0
        while True:
            name = scenes[i % len(scenes)]
            i += 1
            if name == "off":
                await self.go_dark(min(seconds, 3.0))
                continue
            self.world.scene(name)
            self.log("scene", name)
            self.touch("state", "peers", "layout")
            await asyncio.sleep(seconds)

    async def jitter(self):
        import random
        while True:
            await asyncio.sleep(2.0)
            for p in self.world.peers.values():
                if p["state"] == "connected":
                    p["rtt_us"] = max(150, int(p["rtt_us"] + random.randint(-120, 160)))
            self.touch("state", "peers")


async def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--socket", help="where to listen (never the real ferryd socket)")
    ap.add_argument("--name", default="desk", help="this machine's name (default: desk)")
    ap.add_argument("--scene", default="ready", choices=SCENES)
    ap.add_argument("--cycle", type=float, metavar="SECONDS", help="walk through every scene, this long each")
    ap.add_argument("--legacy", action="store_true", help="speak the pre-M3 protocol")
    ap.add_argument("--jitter", action="store_true", help="move the latencies every 2 s")
    ap.add_argument("--auto-pair", action="store_true", help="the local user answers a code by itself, 2 s after it shows")
    ap.add_argument("--pair-window-s", type=float, default=PAIR_WINDOW_S, metavar="N",
                    help="the pairing window and attempt, in seconds (default 120): short, to watch a timeout")
    ap.add_argument("--discover", action="store_true", help="speak the M4 method discover.list (M3's ferryd has no such method)")
    ap.add_argument("--log", action="store_true", help="print every request to stderr")
    ap.add_argument("--fixture", choices=sorted(FIXTURES), help="start with this layout (default: three)")
    ap.add_argument("--dump-layout", metavar="FIXTURE", choices=sorted(FIXTURES),
                    help="print the `layout.get` result of a fixture as JSON and exit: no socket is opened")
    ap.add_argument("--allow-real-socket", action="store_true",
                    help="for a lab VM only, where the default socket path is the VM's own: skip the guard")
    args = ap.parse_args()

    if args.dump_layout:
        world = World(args.name, args.legacy)
        world.load_fixture(args.dump_layout)
        print(json.dumps(world.layout(), sort_keys=True))
        return 0
    if not args.socket:
        ap.error("--socket is required (or --dump-layout FIXTURE)")

    real = os.path.join(os.environ.get("XDG_RUNTIME_DIR") or f"/run/user/{os.getuid()}", "ferry", "ferryd.sock")
    if os.path.realpath(args.socket) == os.path.realpath(real) and not args.allow_real_socket:
        print(f"fake-ferryd: refusing to listen on the real ferryd socket {real}", file=sys.stderr)
        return 2

    server = Server(args)
    if args.fixture:
        server.world.load_fixture(args.fixture)
    if args.scene != "off":
        server.world.scene(args.scene)
    await server.start()
    tasks = []
    if args.cycle:
        tasks.append(asyncio.ensure_future(server.cycle(args.cycle)))
    if args.jitter:
        tasks.append(asyncio.ensure_future(server.jitter()))

    stop = asyncio.Event()
    loop = asyncio.get_event_loop()
    for sig in (signal.SIGINT, signal.SIGTERM):
        loop.add_signal_handler(sig, stop.set)
    await stop.wait()
    for t in tasks:
        t.cancel()
    await server.stop_listening()
    return 0


if __name__ == "__main__":
    sys.exit(asyncio.run(main()))
