import { test } from "node:test";
import assert from "node:assert/strict";
import { load } from "./harness.mjs";

const Model = load("Model.js");
const Glyphs = load("Glyphs.js");
const Strings = load("Strings.js");

const FP_LAP = "a".repeat(64);
const FP_TOWER = "b".repeat(64);
const FP_ME = "d".repeat(64);

function peer(name, fp, extra = {}) {
  return { name, fingerprint: fp, state: "connected", rtt_us: 850, suspect: false, locked: false, ...extra };
}

function status(extra = {}) {
  return {
    api: 1,
    me: { name: "desk", fingerprint: FP_ME },
    port: 4243,
    role: "idle",
    capture: { backend: "portal", gate: "armed", barriers: 1, failed_barriers: [] },
    screen_locked: false,
    locked: false,
    peers: [peer("lap", FP_LAP)],
    counters: {},
    ...extra
  };
}

// ------------------------------------------------------------------------- capabilities

test("a hello without methods is an M1 daemon: no subscribe, the old methods", () => {
  const hello = { api: 1, version: "0.1.0", peer: "desk", fingerprint: FP_ME };
  assert.equal(Model.supportsEvents(hello), false);
  assert.equal(Model.hasMethod(hello, "layout.get"), true);
  assert.equal(Model.hasMethod(hello, "config.set"), false);
  assert.equal(Model.hasMethod(null, "status"), false);
});

test("a hello with methods is believed, subscribe included", () => {
  const hello = { api: 1, methods: ["status", "subscribe", "layout.set"], topics: ["state"] };
  assert.equal(Model.supportsEvents(hello), true);
  assert.equal(Model.hasMethod(hello, "layout.set"), true);
  assert.equal(Model.hasMethod(hello, "release"), false);
});

// ---------------------------------------------------------------------------- summary

test("ferryd's summary is used when it sends one", () => {
  const s = status({ summary: { state: "away", peer: "lap" } });
  assert.deepEqual(Model.summaryOf(s, null), { state: "away", peer: "lap" });
});

test("`off` never comes from ferryd; a summary that says so is ignored", () => {
  const s = status({ summary: { state: "off" } });
  assert.equal(Model.summaryOf(s, null).state, "ready");
});

test("without a summary the same priority applies: attention, away, controlled, locked, ready, idle", () => {
  const away = status({ role: "source", active_peer: { name: "lap", fingerprint: FP_LAP } });
  assert.deepEqual(Model.summaryOf(away, null), { state: "away", peer: "lap" });

  const controlled = status({ role: "target", active_peer: { name: "lap", fingerprint: FP_LAP } });
  assert.deepEqual(Model.summaryOf(controlled, null), { state: "controlled", peer: "lap" });

  // locked beats ready, and away beats locked
  assert.equal(Model.summaryOf(status({ locked: true }), null).state, "locked");
  assert.equal(Model.summaryOf({ ...away, locked: true }, null).state, "away");

  assert.equal(Model.summaryOf(status(), null).state, "ready");
  assert.equal(Model.summaryOf(status({ peers: [peer("lap", FP_LAP, { state: "offline" })] }), null).state, "idle");
  assert.equal(Model.summaryOf(status({ peers: [] }), null).state, "idle");

  const refused = status({ peers: [peer("lap", FP_LAP, { state: "refused", detail: "fingerprint changed" })], role: "source", active_peer: { name: "tower", fingerprint: FP_TOWER } });
  assert.equal(Model.summaryOf(refused, null).state, "attention");
});

test("a pending or arming source is not away yet", () => {
  assert.equal(Model.summaryOf(status({ role: "pending" }), null).state, "ready");
  assert.equal(Model.summaryOf(status({ role: "arming" }), null).state, "ready");
});

test("attention is derived from the raw facts, with a hint that tells the user what to do", () => {
  const refused = status({ peers: [peer("lap", FP_LAP, { state: "refused", detail: "version mismatch: 1.0 vs 2.0" })] });
  const items = Model.attentionOf(refused, null);
  assert.equal(items.length, 1);
  assert.equal(items[0].code, "peer_refused");
  assert.equal(items[0].peer, "lap");
  assert.match(items[0].hint, /Update Ferry on lap/);

  const changed = status({ peers: [peer("lap", FP_LAP, { state: "refused", detail: "fingerprint changed" })] });
  assert.match(Model.attentionOf(changed, null)[0].hint, /ferry unpair lap, then pair again/);

  const barriers = status({ capture: { backend: "portal", gate: "barriers_failed", barriers: 0, failed_barriers: [1] } });
  assert.equal(Model.attentionOf(barriers, null)[0].code, "barriers_failed");

  const denied = status({ capture: { backend: "portal", gate: "armed", barriers: 1, failed_barriers: [], notice: "input capture may be denied" } });
  assert.equal(Model.attentionOf(denied, null)[0].code, "capture_denied");
});

test("a trusted peer with no place in the layout is a problem; this machine's own missing origin is not", () => {
  const layout = {
    revision: { counter: 0, peer: FP_ME },
    peers: [
      { name: "desk", fingerprint: FP_ME, origin: null, monitors: [], online: true },
      { name: "lap", fingerprint: FP_LAP, origin: null, monitors: [], online: true }
    ],
    links: []
  };
  const items = Model.attentionOf(status(), layout);
  assert.deepEqual(items.map((i) => i.code), ["layout_unplaced"]);
  assert.equal(items[0].peer, "lap");
});

test("ferryd's own attention list wins, even when it is empty", () => {
  const s = status({ attention: [], peers: [peer("lap", FP_LAP, { state: "refused" })] });
  assert.deepEqual(Model.attentionOf(s, null), []);
});

// -------------------------------------------------------------------------------- widget

test("every state has a glyph of its own and the glyphs are real code points", () => {
  const seen = new Set();
  for (const state of Model.STATES) {
    const g = Glyphs.state(state);
    assert.ok(g.length >= 1, state);
    assert.ok(g.codePointAt(0) > 0xe000, `${state} is not a private-use glyph`);
    assert.ok(!seen.has(g), `${state} repeats a glyph`);
    seen.add(g);
  }
  // above U+FFFF: a surrogate pair, never the broken "3" that spells U+F021 and a "3"
  assert.equal(Glyphs.state("ready").length, 2);
  assert.equal(Glyphs.state("ready").codePointAt(0), 0xf0213);
});

test("the tooltip of an away widget reads as FR-UI-16 words it", () => {
  const s = status({ role: "source", active_peer: { name: "travel", fingerprint: FP_LAP }, escape_chord: "Super+Ctrl+Escape", summary: { state: "away", peer: "travel" } });
  const w = Model.widget({ connected: true, status: s });
  assert.equal(w.state, "away");
  assert.equal(w.label, "travel");
  assert.equal(w.tone, "accent");
  assert.equal(w.tooltip, "Steering travel · Super+Ctrl+Esc brings it back");
});

test("showPeerName=false drops the label and keeps the name in the tooltip", () => {
  const s = status({ role: "source", active_peer: { name: "travel", fingerprint: FP_LAP }, summary: { state: "away", peer: "travel" } });
  const w = Model.widget({ connected: true, status: s, showPeerName: false });
  assert.equal(w.label, "");
  assert.match(w.tooltip, /Steering travel · Super\+Ctrl\+Esc brings it back/);
});

test("controlled shows the other machine too", () => {
  const s = status({ role: "target", active_peer: { name: "lap", fingerprint: FP_LAP }, summary: { state: "controlled", peer: "lap" } });
  const w = Model.widget({ connected: true, status: s });
  assert.equal(w.state, "controlled");
  assert.equal(w.label, "lap");
  assert.match(w.tooltip, /^lap has the helm/);
});

test("off is the frontend's own state: dim, no label, and it says how to start ferryd", () => {
  const w = Model.widget({ connected: false, status: null });
  assert.equal(w.state, "off");
  assert.equal(w.dim, true);
  assert.match(w.tooltip, /systemctl --user start ferryd/);
});

test("idle tells a first run from a quiet network", () => {
  const first = Model.widget({ connected: true, status: status({ peers: [] }) });
  assert.equal(first.state, "idle");
  assert.match(first.tooltip, /No machine paired yet/);

  const quiet = Model.widget({ connected: true, status: status({ peers: [peer("lap", FP_LAP, { state: "offline" })] }) });
  assert.equal(quiet.state, "idle");
  assert.match(quiet.tooltip, /Waiting for lap/);
});

test("ready names who is connected", () => {
  const s = status({ peers: [peer("lap", FP_LAP), peer("tower", FP_TOWER)] });
  const w = Model.widget({ connected: true, status: s });
  assert.equal(w.state, "ready");
  assert.equal(w.tooltip, "Ready\nPointer on desk · connected to lap and tower");
});

test("attention is urgent, has no extra dot, its own words, and never relies on colour alone", () => {
  const s = status({
    summary: { state: "attention", peer: "" },
    attention: [
      { code: "peer_refused", message: "lap's fingerprint changed", hint: "ferry unpair lap, then pair again", peer: "lap" },
      { code: "barriers_failed", message: "Edges failed" }
    ]
  });
  const w = Model.widget({ connected: true, status: s, attention: s.attention });
  assert.equal(w.state, "attention");
  assert.equal(w.tone, "urgent");
  assert.equal(w.dot, false);
  assert.notEqual(Glyphs.state("attention"), Glyphs.state("ready"));
  assert.match(w.tooltip, /^lap's key changed\nUnpair it and pair again to trust the new key\.\n1 more to look at$/);
});

test("the widget uses the escape chord ferryd reports, and falls back to the default", () => {
  assert.equal(Model.chordLabel("Super+Ctrl+Escape"), "Super+Ctrl+Esc");
  assert.equal(Model.chordLabel("Ctrl+Alt+Shift+F12"), "Ctrl+Alt+Shift+F12");
  assert.equal(Model.chordOf(status()), "Super+Ctrl+Escape");
  assert.equal(Model.chordOf(status({ escape_chord: "Ctrl+Alt+Escape" })), "Ctrl+Alt+Escape");
});

// ----------------------------------------------------------------------------- peer rows

test("rows merge the live link, the pin and the layout, and put the pointer holder first", () => {
  const s = status({
    peers: [
      peer("tower", FP_TOWER, { rtt_us: 12000 }),
      peer("lap", FP_LAP, { rtt_us: 400 })
    ],
    role: "source",
    active_peer: { name: "lap", fingerprint: FP_LAP }
  });
  const pins = {
    peers: [
      { name: "lap", fingerprint: FP_LAP, addr: "198.51.100.5:4243", last_seen: "2026-10-21T10:00:00Z", paired: "sas", state: "connected", locked: false },
      { name: "tower", fingerprint: FP_TOWER, addr: "198.51.100.9:4243", last_seen: "2026-10-21T10:00:00Z", paired: "sas", state: "connected", locked: false }
    ]
  };
  const layout = {
    revision: { counter: 14, peer: FP_ME },
    peers: [{ name: "lap", fingerprint: FP_LAP, origin: [-1800, 0], online: true, monitors: [{ name: "eDP-1", x: -1800, y: 0, w: 1800, h: 1125, scale: 1.6 }] }],
    links: []
  };
  const rows = Model.peerRows(s, pins, layout, { state: "away", peer: "lap" }, Date.parse("2026-10-21T10:05:00Z"));
  assert.deepEqual(rows.map((r) => r.name), ["lap", "tower"]);
  assert.equal(rows[0].tone, "active");
  assert.equal(rows[0].st, "You're steering");
  assert.equal(rows[0].lat, "0.4 ms");
  assert.equal(rows[0].kind, "laptop");
  assert.equal(rows[0].sub, "eDP-1 · 1800×1125 @ ×1.6");
  assert.equal(rows[1].tone, "ok");
  assert.equal(rows[1].lat, "12 ms");
  assert.equal(rows[1].sub, "198.51.100.9:4243");
  assert.equal(rows[1].kind, "desktop");
});

test("an offline peer says when it was last seen; one never seen says so", () => {
  const s = status({ peers: [peer("lap", FP_LAP, { state: "offline", rtt_us: undefined }), peer("tower", FP_TOWER, { state: "offline" })] });
  const pins = {
    peers: [
      { name: "lap", fingerprint: FP_LAP, last_seen: "2026-10-21T09:40:00Z", paired: "sas", state: "offline", locked: false },
      { name: "tower", fingerprint: FP_TOWER, paired: "manual", state: "offline", locked: false }
    ]
  };
  const rows = Model.peerRows(s, pins, null, { state: "idle" }, Date.parse("2026-10-21T10:00:00Z"));
  const lap = rows.find((r) => r.name === "lap");
  const tower = rows.find((r) => r.name === "tower");
  assert.equal(lap.lat, "Offline");
  assert.equal(lap.st, "Seen 20 min ago");
  assert.equal(lap.tone, "lost");
  assert.equal(tower.lat, "Offline");
  assert.equal(tower.st, "Not seen yet");
  assert.equal(tower.tone, "wait");
});

test("a refused peer says why in our words, for a reason we know", () => {
  const s = status({ peers: [peer("lap", FP_LAP, { state: "refused", detail: "version mismatch: 1.0 vs 2.0" })] });
  const rows = Model.peerRows(s, null, null, { state: "attention" }, 0);
  assert.equal(rows[0].tone, "lost");
  assert.equal(rows[0].lat, "Refused");
  assert.equal(rows[0].st, "Another version of Ferry");
});

test("a suspect peer is flagged, and a peer with two monitors is counted", () => {
  const s = status({ peers: [peer("lap", FP_LAP, { suspect: true })] });
  const layout = { peers: [{ name: "lap", fingerprint: FP_LAP, origin: [0, 0], online: true, monitors: [{ name: "A", w: 1, h: 1, scale: 1 }, { name: "B", w: 1, h: 1, scale: 1 }] }] };
  const rows = Model.peerRows(s, null, layout, { state: "ready" }, 0);
  assert.equal(rows[0].tone, "warn");
  assert.equal(rows[0].st, "Not answering");
  assert.equal(rows[0].sub, "2 monitors");
});

test("the first-run card shows only when connected and nothing is paired", () => {
  assert.equal(Model.isFirstRun(true, status({ peers: [] }), { peers: [] }), true);
  assert.equal(Model.isFirstRun(true, status(), { peers: [] }), false);
  assert.equal(Model.isFirstRun(true, status({ peers: [] }), { peers: [{ name: "lap", fingerprint: FP_LAP }] }), false);
  assert.equal(Model.isFirstRun(false, null, null), false);
  // until both snapshots are in, nothing is known about what is paired: no card that flashes on every connect
  assert.equal(Model.isFirstRun(true, null, null), false);
  assert.equal(Model.isFirstRun(true, status({ peers: [] }), null), false);
  assert.equal(Model.isFirstRun(true, null, { peers: [] }), false);
});

// ------------------------------------------------------------------------------ the hero

test("the hero names the machine and the peer, and carries the latency while input travels", () => {
  const s = status({ role: "source", active_peer: { name: "lap", fingerprint: FP_LAP }, peers: [peer("lap", FP_LAP, { rtt_us: 400 })], summary: { state: "away", peer: "lap" } });
  const h = Model.hero({ connected: true, status: s });
  assert.equal(h.title, "Steering lap");
  assert.equal(h.detail, "0.4 ms");
  assert.equal(h.tone, "accent");
  // the second line is the key that brings input back, one entry per keycap
  assert.deepEqual(h.chord, ["Super", "Ctrl", "Esc"]);
  assert.equal(h.chordTail, "brings it back");

  const idle = Model.hero({ connected: true, status: status({ peers: [] }) });
  assert.equal(idle.title, "Pair your first machine");

  const off = Model.hero({ connected: false, status: null });
  assert.equal(off.title, "Ferry is not running");
});

test("the attention hero says what is wrong in the cards below, and what the machine does underneath", () => {
  const s = status({
    role: "source", active_peer: { name: "lap", fingerprint: FP_LAP },
    summary: { state: "attention" },
    attention: [{ code: "escape_bind_missing", message: "The escape bind is not installed", hint: "Run ferry setup" }, { code: "x", message: "y" }]
  });
  const h = Model.hero({ connected: true, status: s, attention: s.attention });
  assert.equal(h.state, "attention");
  assert.equal(h.title, "Needs your attention");
  assert.equal(h.meta, "Steering lap");
  assert.deepEqual(h.chord, [], "the attention hero names the problem, the keycaps stay in the footer");
  // under a problem a machine that is simply ready says where the pointer is, and not just "Ready"
  const ready = Model.hero({ connected: true, status: status({ summary: { state: "attention" }, attention: [{ code: "x", message: "y" }] }), attention: [{ code: "x", message: "y" }] });
  assert.equal(ready.meta, "Pointer on desk · 1 machine connected");
  assert.equal(h.detail, "2 to fix");
  assert.equal(h.tone, "urgent");
});

// ------------------------------------------------------------------------------ helpers

test("latency reads like the mockup: one decimal under ten milliseconds, whole above", () => {
  assert.equal(Model.rttText(400), "0.4 ms");
  assert.equal(Model.rttText(850), "0.9 ms");
  assert.equal(Model.rttText(12000), "12 ms");
  assert.equal(Model.rttText(null), "—");
  assert.equal(Model.rttText(undefined), "—");
});

test("ages are short and need no clock", () => {
  const now = Date.parse("2026-10-21T12:00:00Z");
  assert.equal(Model.agoText("2026-10-21T11:59:40Z", now), "just now");
  assert.equal(Model.agoText("2026-10-21T11:15:00Z", now), "45 min ago");
  assert.equal(Model.agoText("2026-10-21T07:00:00Z", now), "5 h ago");
  assert.equal(Model.agoText("2026-10-18T09:12:03Z", now), "18 Oct");
  assert.equal(Model.agoText("not a time", now), "");
  assert.equal(Model.agoText("", now), "");
});

test("a missing string shows its key, never nothing", () => {
  assert.equal(Strings.t("no.such.key"), "no.such.key");
  assert.equal(Strings.t("tip.away", ["lap", "Super+Ctrl+Esc"]), "Steering lap · Super+Ctrl+Esc brings it back");
});

// ------------------------------------------------------------------------- the travel arrow

// desk with two heads, lap under the left one, air to the left (a three-machine layout as it is used)
const PLANE = {
  peers: [
    { name: "desk", self: true, origin: [0, 0], monitors: [{ name: "HDMI-A-1", x: 0, y: 0, w: 1920, h: 1080 }, { name: "DVI-D-1", x: 1920, y: 0, w: 1920, h: 1080 }] },
    { name: "lap", origin: [554, 1080], monitors: [{ name: "eDP-1", x: 554, y: 1080, w: 1366, h: 768 }] },
    { name: "air", origin: [-1440, 428], monitors: [{ name: "eDP-1", x: -1440, y: 428, w: 1440, h: 932 }] },
    { name: "far", origin: [0, -3000], monitors: [{ name: "eDP-1", x: 100, y: -3000, w: 800, h: 600 }] },
    { name: "loose", origin: null, monitors: [{ name: "eDP-1", x: 0, y: 0, w: 800, h: 600 }] }
  ]
};
const fromAir = { peers: PLANE.peers.map((p) => ({ ...p, self: p.name === "air" })) };

test("the arrow points at the other machine's side of the layout", () => {
  assert.equal(Model.sideOf(PLANE, "air"), "left");
  assert.equal(Model.sideOf(PLANE, "lap"), "down");
  assert.equal(Model.sideOf(fromAir, "desk"), "right", "the controlled machine points back at the one that steers it");
  // not touching: where its middle is
  assert.equal(Model.sideOf(PLANE, "far"), "up");
  // nothing to go by
  assert.equal(Model.sideOf(PLANE, "loose"), "");
  assert.equal(Model.sideOf(null, "air"), "");
  assert.equal(Model.sideOf(PLANE, "nobody"), "");
  // without a side the arrows are the old ones: out to the right, in from the left
  assert.equal(Model.travelSide("away", null, "lap"), "right");
  assert.equal(Model.travelSide("controlled", null, "lap"), "left");
  assert.equal(Model.travelSide("ready", PLANE, "air"), "");
});

test("the widget, the hero and the row carry the side, and the glyphs follow it", () => {
  const s = status({ role: "target", active_peer: { name: "desk", fingerprint: FP_LAP }, peers: [peer("desk", FP_LAP)], summary: { state: "controlled", peer: "desk" } });
  const w = Model.widget({ connected: true, status: s, layout: fromAir });
  assert.equal(w.side, "right");
  assert.equal(Glyphs.travel(w.state, w.side).codePointAt(0), 0xf0734);
  assert.equal(Model.hero({ connected: true, status: s, layout: fromAir }).side, "right");
  const rows = Model.peerRows(s, { peers: [{ name: "desk", fingerprint: FP_LAP, state: "connected" }] }, fromAir, s.summary, 0);
  assert.equal(rows[0].side, "right");
  assert.equal(Glyphs.pointing(rows[0].side), Glyphs.ui("arrowRight"));
  // a state that does not travel keeps its own glyph
  assert.equal(Glyphs.travel("ready", "left"), Glyphs.state("ready"));
  assert.equal(Glyphs.travel("away", ""), Glyphs.state("away"));
});
