import { test } from "node:test";
import assert from "node:assert/strict";
import { load } from "./harness.mjs";

// The popover's decisions (stream F): the hero sentence, a peer row's arrow and age, the rename
// rule, what a button can do about each problem ferryd names, and the recent-activity list.

const Model = load("Model.js");
const Glyphs = load("Glyphs.js");
const Strings = load("Strings.js");

const FP_LAP = "a".repeat(64);
const FP_TOWER = "b".repeat(64);
const FP_ME = "d".repeat(64);
const NOW = Date.parse("2026-10-21T12:00:00Z");

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

// ------------------------------------------------------------------------------- the hero

test("the hero says one sentence per state, as the brief words them", () => {
  const ready = Model.hero({ connected: true, status: status({ peers: [peer("lap", FP_LAP), peer("tower", FP_TOWER)] }) });
  assert.equal(ready.title, "Ready");
  assert.equal(ready.meta, "Pointer on desk · 2 machines connected");
  assert.equal(Model.hero({ connected: true, status: status() }).meta, "Pointer on desk · 1 machine connected");

  const away = Model.hero({ connected: true, status: status({ role: "source", active_peer: { name: "lap", fingerprint: FP_LAP } }) });
  assert.equal(away.title, "Steering lap");

  const controlled = Model.hero({ connected: true, status: status({ role: "target", active_peer: { name: "lap", fingerprint: FP_LAP } }) });
  assert.equal(controlled.title, "lap has the helm");

  const locked = Model.hero({ connected: true, status: status({ locked: true }) });
  assert.equal(locked.title, "Locked to this machine");
  assert.equal(locked.detail, "locked");
  assert.deepEqual(locked.chord, [], "no input travels, so no chord is taught in the hero");
});

test("while input travels the hero carries the escape chord, in the user's own chord", () => {
  const s = status({ role: "source", active_peer: { name: "lap", fingerprint: FP_LAP }, escape_chord: "Ctrl+Alt+Escape" });
  const h = Model.hero({ connected: true, status: s });
  assert.deepEqual(h.chord, ["Ctrl", "Alt", "Esc"]);
  assert.equal(h.chordTail, "brings it back");
  assert.deepEqual(Model.chordKeys("Super+Ctrl+Escape"), ["Super", "Ctrl", "Esc"]);
  assert.deepEqual(Model.chordKeys(""), ["Super", "Ctrl", "Esc"], "an empty chord falls back to the default");
  assert.deepEqual(Model.chordKeys("Ctrl+Alt+Shift+F12"), ["Ctrl", "Alt", "Shift", "F12"]);
});

test("peers online are counted in the hero, not listed, so the line stays one line: the list is below it", () => {
  const peers = ["a", "b", "c", "d"].map((n, i) => peer(n, String(i).repeat(64)));
  const h = Model.hero({ connected: true, status: status({ peers }) });
  assert.equal(h.meta, "Pointer on desk · 4 machines connected");
});

// -------------------------------------------------------------------------------- peer rows

test("a row says which way input travels, shows the short fingerprint and since when it is gone", () => {
  const s = status({ role: "source", active_peer: { name: "lap", fingerprint: FP_LAP }, peers: [peer("lap", FP_LAP), peer("tower", FP_TOWER, { state: "offline", rtt_us: undefined })] });
  const pins = {
    peers: [
      { name: "lap", fingerprint: FP_LAP, last_seen: "2026-10-21T11:59:50Z", paired: "sas", state: "connected", locked: false },
      { name: "tower", fingerprint: FP_TOWER, last_seen: "2026-10-21T11:46:00Z", paired: "sas", state: "offline", locked: false }
    ]
  };
  const rows = Model.peerRows(s, pins, null, { state: "away", peer: "lap" }, NOW);
  const lap = rows.find((r) => r.name === "lap");
  const tower = rows.find((r) => r.name === "tower");
  assert.equal(lap.arrow, "away");
  assert.equal(lap.online, true);
  assert.equal(lap.shortFp, "aaaaaaaa");
  assert.equal(tower.arrow, "");
  assert.equal(tower.online, false);
  assert.equal(tower.seen, "14 min ago");
  assert.equal(tower.st, "Seen 14 min ago");

  const controlled = Model.peerRows(s, pins, null, { state: "controlled", peer: "lap" }, NOW);
  assert.equal(controlled.find((r) => r.name === "lap").arrow, "controlled");
});

test("an offline peer never carries an arrow, even if the summary still names it", () => {
  const s = status({ peers: [peer("lap", FP_LAP, { state: "offline" })] });
  const rows = Model.peerRows(s, null, null, { state: "away", peer: "lap" }, NOW);
  assert.equal(rows[0].arrow, "");
});

// ----------------------------------------------------------------------------------- rename

test("a name follows the rule of peer.name before it is sent", () => {
  const ctx = { me: "desk", fingerprint: FP_LAP, current: "lap", others: [{ name: "tower", fingerprint: FP_TOWER }, { name: "lap", fingerprint: FP_LAP }] };
  assert.deepEqual(Model.renameCheck("studio", ctx), { ok: true, name: "studio", unchanged: false, error: "" });
  assert.equal(Model.renameCheck("  studio  ", ctx).name, "studio", "pasted spaces at the ends are dropped, not refused");
  assert.equal(Model.renameCheck("lap", ctx).unchanged, true, "the same name is no change, and not an error");
  assert.equal(Model.renameCheck("", ctx).error, "A name cannot be empty.");
  assert.equal(Model.renameCheck("   ", ctx).error, "A name cannot be empty.");
  assert.equal(Model.renameCheck("two words", ctx).error, "A name cannot contain spaces, control characters or a colon.");
  assert.equal(Model.renameCheck("a:b", ctx).ok, false);
  assert.equal(Model.renameCheck("tab\tname", ctx).ok, false);
  assert.equal(Model.renameCheck("x".repeat(64), ctx).ok, true);
  assert.equal(Model.renameCheck("x".repeat(65), ctx).error, "A name is at most 64 characters.");
  assert.equal(Model.renameCheck("desk", ctx).error, "That is this machine's own name.");
  assert.equal(Model.renameCheck("tower", ctx).error, "tower already has that name.");
});

test("a name counts characters, not UTF-16 units: 64 emoji are allowed, 65 are not", () => {
  const ctx = { me: "desk", fingerprint: FP_LAP, current: "lap", others: [] };
  assert.equal(Model.renameCheck("\u{1F680}".repeat(64), ctx).ok, true);
  assert.equal(Model.renameCheck("\u{1F680}".repeat(65), ctx).ok, false);
  assert.equal(Model.renameCheck("naïve", ctx).ok, true);
});

// ------------------------------------------------------------------------------- api errors

test("an error shows ferryd's own words, and ours for a daemon that is gone or slow", () => {
  assert.deepEqual(Model.errorText({ code: "bad_params", message: "the name \"x\" already belongs to peer abc", hint: "pick another name" }),
    { message: "the name \"x\" already belongs to peer abc", hint: "pick another name" });
  assert.equal(Model.errorText({ code: "not_connected", message: "ferryd is not running" }).message, "ferryd is not running.");
  assert.equal(Model.errorText({ code: "timeout", message: "peer.rename did not answer" }).message, "ferryd did not answer in time.");
  assert.equal(Model.errorText(null).message, "Something went wrong.");
});

// ----------------------------------------------------------------------- attention actions

test("a changed key offers unpair-then-pair, judged by the message and not by the generic hint", () => {
  const real = {
    code: "peer_refused",
    message: "lap cannot connect: fingerprint changed",
    hint: "run `ferry doctor`; after a key change, `ferry unpair lap` and pair again",
    peer: "lap"
  };
  const [changed] = Model.attentionActions([real]);
  assert.equal(changed.action.kind, "unpair");
  assert.equal(changed.action.peer, "lap");
  assert.equal(changed.action.again, true);
  assert.equal(changed.action.label, "Unpair and pair again…");

  // ferryd's hint is the same for a version mismatch, which unpairing would not fix
  const [version] = Model.attentionActions([{ ...real, message: "lap cannot connect: version mismatch: 1.0 vs 2.0" }]);
  assert.equal(version.action.kind, "copy");
  assert.equal(version.action.command, "ferry doctor");
});

test("each problem ferryd names gets the control that can fix it, or none", () => {
  const items = Model.attentionActions([
    { code: "layout_unplaced", message: "tower is not placed in the layout", hint: "ferry layout place tower --right-of desk", peer: "tower" },
    { code: "config_invalid", message: "config.toml: unknown key", hint: "`ferry config get` shows what is saved" },
    { code: "capture_denied", message: "Hyprland does not let Ferry capture input", hint: "run `ferry setup` to allow xdph, then restart Hyprland" },
    { code: "barriers_failed", message: "1 of 2 pointer barriers failed", hint: "`ferry release`, then check the monitors with `hyprctl monitors`" },
    { code: "escape_bind_missing", message: "The escape bind is not installed", hint: "Run ferry setup" },
    { code: "future_code", message: "something new", hint: "try `ferry frobnicate`" },
    { code: "other_future_code", message: "something else" }
  ]);
  assert.deepEqual(items[0].action, { kind: "panel", label: "Arrange screens…", view: "arrange" });
  assert.deepEqual(items[1].action, { kind: "panel", label: "Settings…", view: "settings" });
  assert.deepEqual(items[2].action, { kind: "copy", label: "Copy ferry setup", command: "ferry setup" });
  assert.deepEqual(items[3].action, { kind: "copy", label: "Copy ferry doctor", command: "ferry doctor" });
  assert.equal(items[4].action.command, "ferry setup", "kept for a daemon that can tell, which this one cannot yet");
  assert.equal(items[5].action.command, "ferry frobnicate", "an unknown code with a command in its hint still gets a copy button");
  assert.equal(items[6].action, null);
  // the original fields come through untouched
  assert.equal(items[0].message, "tower is not placed in the layout");
  assert.equal(items[0].peer, "tower");
});

test("a hint's backticks become bold text, and what is inside a hint is escaped first", () => {
  assert.equal(Model.richText("run `ferry doctor` first"), "run <b>ferry doctor</b> first");
  assert.equal(Model.richText("<script>& `x`"), "&lt;script&gt;&amp; <b>x</b>");
  assert.equal(Model.richText("no code here"), "no code here");
  assert.equal(Model.commandIn("run `ferry setup` then `ferry doctor`"), "ferry setup");
  assert.equal(Model.commandIn("nothing"), "");
});

// ------------------------------------------------------------------------------- activity

test("recent activity lists the last five notices, newest first, with their age", () => {
  const notices = [
    { kind: "peer_lost", title: "Lost lap", body: "", peer: "lap", urgency: "normal", at: NOW - 40 * 60000 },
    { kind: "peer_back", title: "lap is back", body: "", peer: "lap", urgency: "normal", at: NOW - 30 * 60000 },
    { kind: "paired", title: "Paired with tower", body: "", peer: "tower", urgency: "normal", at: NOW - 5 * 60000 },
    { kind: "attention", title: "lap cannot connect: fingerprint changed", body: "run `ferry doctor`", urgency: "critical", at: NOW - 20000 }
  ];
  const items = Model.activityItems(notices, NOW);
  assert.deepEqual(items.map((i) => i.title), ["lap cannot connect: key changed", "Paired with tower", "lap is back", "Lost lap"]);
  assert.equal(items[0].ago, "just now");
  assert.equal(items[1].ago, "5 min ago");
  assert.equal(items[0].urgent, true);
  assert.equal(items[1].urgent, false);

  const many = Array.from({ length: 8 }, (_, i) => ({ kind: "peer_back", title: `n${i}`, urgency: "normal", at: NOW - i * 1000 }));
  assert.equal(Model.activityItems(many, NOW).length, 5);
  assert.equal(Model.activityItems(many, NOW, 3).length, 3);
});

test("a pairing code never reaches the activity list, and a notice with no title is not a line", () => {
  const items = Model.activityItems([
    { kind: "pair_code", title: "Pairing code 418 207", body: "lap is asking to pair", urgency: "critical", at: NOW },
    { kind: "paired", title: "", at: NOW },
    { kind: "paired", title: "Paired with lap", at: NOW }
  ], NOW);
  assert.deepEqual(items.map((i) => i.title), ["Paired with lap"]);
  assert.equal(JSON.stringify(items).includes("418"), false);
  assert.deepEqual(Model.activityItems(null, NOW), []);
});

test("an attention toast is not repeated in the list while its card is on screen, and shows when the card is gone", () => {
  const toast = { kind: "attention", title: "lap cannot connect: fingerprint changed", body: "run `ferry doctor`", urgency: "critical", at: NOW };
  const other = { kind: "paired", title: "Paired with tower", at: NOW - 1000 };
  const card = [{ code: "peer_refused", message: "lap cannot connect: fingerprint changed", hint: "x", peer: "lap" }];
  assert.deepEqual(Model.activityItems([toast, other], NOW, 5, card).map((i) => i.title), ["Paired with tower"]);
  assert.deepEqual(Model.activityItems([toast, other], NOW, 5, []).map((i) => i.kind), ["attention", "paired"], "the problem is gone: its toast is history");
  assert.deepEqual(Model.activityItems([toast, other], NOW).map((i) => i.kind), ["attention", "paired"], "no list given: nothing is hidden");
});

test("notices of one moment keep the order they came in, the last one first", () => {
  const items = Model.activityItems([
    { kind: "peer_lost", title: "first", at: NOW },
    { kind: "peer_back", title: "second", at: NOW }
  ], NOW);
  assert.deepEqual(items.map((i) => i.title), ["second", "first"]);
});

test("every kind of notice has its own glyph, and an unknown kind falls back to the info glyph", () => {
  const kinds = ["paired", "pair_failed", "pair_code", "peer_lost", "peer_back", "unpaired", "layout_replaced", "attention"];
  for (const k of kinds) assert.ok(Glyphs.notice(k).codePointAt(0) > 0xe000, k);
  assert.equal(Glyphs.notice("something_new"), Glyphs.ui("info"));
  assert.notEqual(Glyphs.notice("paired"), Glyphs.notice("pair_failed"));
  assert.notEqual(Glyphs.notice("peer_lost"), Glyphs.notice("peer_back"));
});

// --------------------------------------------------------------------------------- strings

test("a name containing a percent sign cannot be read as the next hole of a string", () => {
  assert.equal(Strings.t("hero.away.title", ["100%2"]), "Steering 100%2");
  assert.equal(Strings.t("tip.away", ["a%2b", "Super+Ctrl+Esc"]), "Steering a%2b · Super+Ctrl+Esc brings it back");
  assert.equal(Strings.t("hero.idle.meta", [undefined]), "This machine · undefined offline");
});

test("every key the model builds from data has a text, so a reason ferryd adds later still reads", () => {
  assert.equal(Strings.has("pair.fail.rejected"), true);
  assert.equal(Strings.has("pair.fail.no_such_reason"), false);
});
