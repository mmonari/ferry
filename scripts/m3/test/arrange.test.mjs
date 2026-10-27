import { test } from "node:test";
import assert from "node:assert/strict";
import { load } from "./harness.mjs";

const A = load("Arrange.js");
const G = load("Geometry.js");
const Strings = load("Strings.js");

// ------------------------------------------------------------------------------ fixtures
//
// A `layout` snapshot as ferryd writes it (crates/ferryd/tests/snapshots/api__layout_*.snap): the
// monitors are on the plane when the peer is placed (local position + shift + origin) and local when
// it is not, `origin` is [x, y] or null, `shift` is left out when zero.

const FP = { desk: "d".repeat(64), lap: "a".repeat(64), tower: "b".repeat(64), deck: "c".repeat(64) };
const mview = (name, x, y, w, h, extra = {}) => ({ name, description: `${name} (test)`, x, y, w, h, scale: 1, transform: 0, ...extra });

function peer(name, monitors, origin, extra = {}) {
  const shift = extra.shift || [0, 0];
  const dx = origin ? origin[0] + shift[0] : 0, dy = origin ? origin[1] + shift[1] : 0;
  const views = monitors.map(([n, x, y, w, h, scale]) => mview(n, x + dx, y + dy, w, h, scale ? { scale } : {}));
  const xs = views.map((m) => m.x), ys = views.map((m) => m.y);
  const x0 = Math.min(...xs), y0 = Math.min(...ys);
  const x1 = Math.max(...views.map((m) => m.x + m.w)), y1 = Math.max(...views.map((m) => m.y + m.h));
  const out = {
    name, fingerprint: FP[name] || extra.fp, origin, monitors: views,
    online: extra.online !== false, trusted: extra.trusted !== false, self: name === "desk",
    bounds: views.length ? { x: x0, y: y0, w: x1 - x0, h: y1 - y0 } : null
  };
  if (extra.shift && (extra.shift[0] || extra.shift[1])) out.shift = extra.shift;
  return out;
}

const DESK = [["HDMI-A-1", 0, 0, 1920, 1080], ["DVI-D-1", 1920, 0, 1920, 1080]];
const LAP = [["eDP-1", 0, 0, 1800, 1125, 1.6]];
const TOWER = [["DP-1", 0, 0, 2560, 1440]];

const rev = (counter, by = "desk") => ({ counter, peer: FP[by] });
const layout = (peers, counter = 14, by = "lap") => ({ revision: rev(counter, by), peers, links: [] });

// desk at the origin, lap left of it tops aligned, tower right of it, tops 180 px higher
const three = () => layout([peer("desk", DESK, [0, 0]), peer("lap", LAP, [-1800, 0]), peer("tower", TOWER, [3840, -180])]);

const named = (groups, name) => groups.find((g) => g.name === name);

// ----------------------------------------------------------------------------- the groups

test("groups come from the snapshot with plane coordinates, trust, presence and the bounds the daemon gave", () => {
  const { groups, waiting } = A.groupsOf(three(), FP.desk);
  assert.deepEqual(groups.map((g) => g.name), ["desk", "lap", "tower"]);
  assert.deepEqual(waiting, []);
  const desk = named(groups, "desk");
  assert.equal(desk.self, true);
  assert.equal(desk.placed, true);
  assert.deepEqual(desk.origin, [0, 0]);
  assert.deepEqual(desk.rect, { x: 0, y: 0, w: 3840, h: 1080 });
  const lap = named(groups, "lap");
  assert.equal(lap.self, false);
  assert.deepEqual(lap.rect, { x: -1800, y: 0, w: 1800, h: 1125 });
  assert.equal(lap.monitors[0].scale, 1.6);
  assert.equal(lap.monitors[0].description, "eDP-1 (test)");
});

test("an older ferryd that sends no trusted, self, bounds, shift or description still draws", () => {
  const old = layout([
    { name: "desk", fingerprint: FP.desk, origin: [0, 0], online: true, monitors: [{ name: "HDMI-A-1", x: 0, y: 0, w: 1920, h: 1080, scale: 1 }] },
    { name: "lap", fingerprint: FP.lap, origin: [-1800, 0], online: false, monitors: [{ name: "eDP-1", x: -1800, y: 0, w: 1800, h: 1125, scale: 1.6 }] }
  ]);
  const { groups } = A.groupsOf(old, FP.desk);
  assert.equal(named(groups, "desk").self, true, "by the fingerprint of this machine");
  assert.equal(named(groups, "lap").trusted, true, "absent means true");
  assert.equal(named(groups, "lap").online, false);
  assert.deepEqual(named(groups, "lap").rect, { x: -1800, y: 0, w: 1800, h: 1125 }, "computed from the monitors");
  assert.equal(named(groups, "lap").monitors[0].description, "");
  assert.deepEqual(named(groups, "lap").shift, [0, 0]);
});

test("a peer that has not reported its monitors cannot be drawn and is listed apart", () => {
  const l = layout([peer("desk", DESK, [0, 0]), { ...peer("lap", [], [-1800, 0]), monitors: [], bounds: null }]);
  const { groups, waiting } = A.groupsOf(l, FP.desk);
  assert.deepEqual(groups.map((g) => g.name), ["desk"]);
  assert.deepEqual(waiting.map((g) => g.name), ["lap"]);
});

test("a peer that is only placed, not paired here, is untrusted and gets its fingerprint as a name", () => {
  const l = layout([peer("desk", DESK, [0, 0]), peer("lap", LAP, [-1800, 0]), { ...peer("tower", TOWER, [3840, 0], { trusted: false }), name: "bbbbbbbb" }]);
  const { groups } = A.groupsOf(l, FP.desk);
  assert.equal(named(groups, "bbbbbbbb").trusted, false);
});

test("an anchor shift is already in the monitors and is kept apart from the origin", () => {
  // a hot-plug moved lap's group by (1000, 0): the origin stays -2800, the monitors sit at -1800
  const l = layout([peer("desk", DESK, [0, 0]), peer("lap", LAP, [-2800, 0], { shift: [1000, 0] })]);
  const { groups } = A.groupsOf(l, FP.desk);
  const lap = named(groups, "lap");
  assert.deepEqual(lap.origin, [-2800, 0]);
  assert.deepEqual(lap.shift, [1000, 0]);
  assert.deepEqual(lap.rect, { x: -1800, y: 0, w: 1800, h: 1125 });
  // dragged 100 px to the right on the canvas, the new ORIGIN is -2700, not the plane position
  const draft = A.withOrigin(A.emptyDraft(), groups, l.revision, FP.lap, [-2700, 0]);
  const eff = A.effectiveGroups(groups, draft, null);
  assert.deepEqual(named(eff, "lap").rect, { x: -1700, y: 0, w: 1800, h: 1125 });
  assert.deepEqual(A.payload(draft, groups).origins, [{ peer: FP.lap, x: -2700, y: 0 }]);
});

// ------------------------------------------------------------------------- the draft

test("a clean draft shows the daemon's layout as it is", () => {
  const { groups } = A.groupsOf(three(), FP.desk);
  const eff = A.effectiveGroups(groups, A.emptyDraft(), null);
  assert.deepEqual(eff.map((g) => g.rect), groups.map((g) => g.rect));
  assert.ok(eff.every((g) => g.moved === false && g.provisional === false));
  assert.equal(A.isDirty(A.emptyDraft(), groups), false);
});

test("a move edits the draft, not the layout; the first edit fixes the revision it is based on", () => {
  const l = three();
  const { groups } = A.groupsOf(l, FP.desk);
  const d1 = A.withOrigin(A.emptyDraft(), groups, l.revision, FP.lap, [-1800, 300]);
  assert.deepEqual(d1.base, l.revision);
  assert.deepEqual(d1.origins, { [FP.lap]: [-1800, 300] });
  assert.equal(A.isDirty(d1, groups), true);
  // a later edit, when the layout has moved on, keeps the first revision
  const d2 = A.withOrigin(d1, groups, rev(15), FP.tower, [3840, 0]);
  assert.deepEqual(d2.base, l.revision);
  const eff = A.effectiveGroups(groups, d2, null);
  assert.deepEqual(named(eff, "lap").rect, { x: -1800, y: 300, w: 1800, h: 1125 });
  assert.equal(named(eff, "lap").moved, true);
  assert.deepEqual(named(eff, "lap").baseOrigin, [-1800, 0]);
  assert.equal(named(eff, "desk").moved, false);
});

test("moving a group back to where the daemon has it leaves a clean draft", () => {
  const l = three();
  const { groups } = A.groupsOf(l, FP.desk);
  const d1 = A.withOrigin(A.emptyDraft(), groups, l.revision, FP.lap, [-1800, 300]);
  const d2 = A.withOrigin(d1, groups, l.revision, FP.lap, [-1800, 0]);
  assert.equal(A.isDirty(d2, groups), false);
  assert.equal(d2.base, null);
  assert.deepEqual(d2.origins, {});
  assert.equal(A.canUndo(d2), true, "and the moves can still be undone");
});

test("undo steps back one edit at a time; reset throws the draft away and can itself be undone", () => {
  const l = three();
  const { groups } = A.groupsOf(l, FP.desk);
  let d = A.withOrigin(A.emptyDraft(), groups, l.revision, FP.lap, [-1800, 300]);
  d = A.withOrigin(d, groups, l.revision, FP.tower, [3840, 0]);
  d = A.undo(d);
  assert.deepEqual(d.origins, { [FP.lap]: [-1800, 300] });
  d = A.reset(d, groups);
  assert.equal(A.isDirty(d, groups), false);
  d = A.undo(d);
  assert.deepEqual(d.origins, { [FP.lap]: [-1800, 300] }, "the reset was undone");
  d = A.undo(A.undo(d));
  assert.equal(A.isDirty(d, groups), false);
  assert.equal(A.canUndo(d), false);
  assert.equal(A.undo(d), d, "nothing to undo");
});

test("the undo history is bounded", () => {
  const l = three();
  const { groups } = A.groupsOf(l, FP.desk);
  let d = A.emptyDraft();
  for (let i = 1; i <= 80; i++) d = A.withOrigin(d, groups, l.revision, FP.lap, [-1800, i * 10]);
  assert.equal(d.undo.length, 50);
});

test("Apply sends only the groups that moved, by fingerprint, in one call, with the base revision", () => {
  const l = three();
  const { groups } = A.groupsOf(l, FP.desk);
  let d = A.withOrigin(A.emptyDraft(), groups, l.revision, FP.tower, [3840, 0]);
  d = A.withOrigin(d, groups, l.revision, FP.lap, [-1800, 300]);
  assert.deepEqual(A.payload(d, groups), {
    origins: [{ peer: FP.lap, x: -1800, y: 300 }, { peer: FP.tower, x: 3840, y: 0 }],
    base_rev: { counter: 14, peer: FP.lap }
  });
  // the daemon's `base_rev` is the revision value, never a name
  assert.match(A.payload(d, groups).base_rev.peer, /^[0-9a-f]{64}$/);
});

test("a draft that names a peer the daemon no longer lists is pruned", () => {
  const l = three();
  const { groups } = A.groupsOf(l, FP.desk);
  const d = A.withOrigin(A.emptyDraft(), groups, l.revision, FP.lap, [-1800, 300]);
  const fewer = A.groupsOf(layout([peer("desk", DESK, [0, 0]), peer("tower", TOWER, [3840, -180])], 15), FP.desk).groups;
  const pruned = A.forgetMissing(d, fewer);
  assert.deepEqual(pruned.origins, {});
  assert.equal(pruned.base, null);
  assert.equal(A.forgetMissing(d, groups), d, "nothing to prune: the same object");
});

// ----------------------------------------------------------------------- unplaced groups

test("a group the daemon has not placed is shown right of the rest until it is dragged, and takes part in nothing", () => {
  const l = layout([peer("desk", DESK, [0, 0]), peer("lap", LAP, null)], 3, "desk");
  const { groups } = A.groupsOf(l, FP.desk);
  assert.equal(named(groups, "lap").placed, false);
  assert.deepEqual(named(groups, "lap").rect, { x: 0, y: 0, w: 1800, h: 1125 }, "local, as the daemon sends it");
  const eff = A.effectiveGroups(groups, A.emptyDraft(), null);
  const lap = named(eff, "lap");
  assert.equal(lap.provisional, true);
  assert.equal(lap.placed, false);
  assert.deepEqual(lap.rect, { x: 3840, y: 0, w: 1800, h: 1125 });
  assert.deepEqual(lap.origin, [3840, 0]);
  const a = A.analyse(eff);
  assert.deepEqual(a.touches, [], "not placed: no seam");
  assert.deepEqual(a.unplaced, [FP.lap]);
  assert.equal(A.isDirty(A.emptyDraft(), groups), false, "showing it is not an edit");
});

test("dragging an unplaced group places it: the draft names its origin, and its monitors are local + shift + origin", () => {
  const l = layout([peer("desk", DESK, [0, 0]), peer("lap", LAP, null, { shift: [40, 0] })], 3, "desk");
  const { groups } = A.groupsOf(l, FP.desk);
  const d = A.withOrigin(A.emptyDraft(), groups, l.revision, FP.lap, [-1840, 0]);
  assert.equal(A.isDirty(d, groups), true);
  const lap = named(A.effectiveGroups(groups, d, null), "lap");
  assert.equal(lap.placed, true);
  assert.deepEqual(lap.rect, { x: -1800, y: 0, w: 1800, h: 1125 });
  assert.deepEqual(A.payload(d, groups).origins, [{ peer: FP.lap, x: -1840, y: 0 }]);
  const a = A.analyse(A.effectiveGroups(groups, d, null));
  assert.equal(a.touches.length, 1, "now it links");
});

test("a fresh daemon lists this machine unplaced, and it is shown where its own screens are", () => {
  const fresh = { revision: rev(0), peers: [peer("desk", DESK, null)], links: [] };
  const { groups } = A.groupsOf(fresh, FP.desk);
  const eff = A.effectiveGroups(groups, A.emptyDraft(), null);
  assert.deepEqual(eff[0].rect, { x: 0, y: 0, w: 3840, h: 1080 });
  assert.equal(eff[0].provisional, true);
});

// ------------------------------------------------------------------- the group in flight

test("the group being dragged is where the pointer has it, whatever the draft says", () => {
  const l = three();
  const { groups } = A.groupsOf(l, FP.desk);
  const d = A.withOrigin(A.emptyDraft(), groups, l.revision, FP.lap, [-1800, 300]);
  const eff = A.effectiveGroups(groups, d, { fp: FP.lap, origin: [-2500, 40] });
  assert.deepEqual(named(eff, "lap").rect, { x: -2500, y: 40, w: 1800, h: 1125 });
  assert.deepEqual(named(eff, "tower").rect, { x: 3840, y: -180, w: 2560, h: 1440 });
});

// -------------------------------------------------------------------------- analysis

test("journey J3: the seam is desk HDMI-A-1's left edge, y 0 to 1080, and lap's lower 45 px are a wall", () => {
  const { groups } = A.groupsOf(three(), FP.desk);
  const eff = A.effectiveGroups(groups, A.emptyDraft(), null);
  const a = A.analyse(eff);
  const rows = A.linkRows(a, eff);
  assert.deepEqual(rows.map((r) => r.primary), [
    "desk ↔ lap",
    "desk ↔ tower"
  ]);
  assert.equal(rows[0].secondary, "HDMI-A-1 left ↔ eDP-1 right · y 0–1080 · 1080 px");
  assert.equal(rows[1].secondary, "DVI-D-1 right ↔ DP-1 left · y 0–1080 · 1080 px", "tower is 180 px higher, so the link covers desk's height");
  const lapWalls = a.walls.filter((w) => w.fp === FP.lap && w.side === "right");
  assert.deepEqual(lapWalls.map((w) => w.span), [[1080, 1125]]);
  assert.deepEqual(a.overlaps, []);
  assert.deepEqual(a.lonely, []);
});

test("a machine nothing touches is flagged, because the pointer could never reach it", () => {
  const l = layout([peer("desk", DESK, [0, 0]), peer("lap", LAP, [-3000, 0]), peer("tower", TOWER, [3840, 0])]);
  const eff = A.effectiveGroups(A.groupsOf(l, FP.desk).groups, A.emptyDraft(), null);
  assert.deepEqual(A.analyse(eff).lonely, [FP.lap]);
});

test("alone, or with nobody else placed, no machine is called lonely", () => {
  const eff = A.effectiveGroups(A.groupsOf(layout([peer("desk", DESK, [0, 0])]), FP.desk).groups, A.emptyDraft(), null);
  assert.deepEqual(A.analyse(eff).lonely, []);
});

test("an offline peer next to this machine is a dormant link, an unpaired one a blocked touch", () => {
  const off = layout([peer("desk", DESK, [0, 0]), peer("lap", LAP, [-1800, 0], { online: false })]);
  const a1 = A.analyse(A.effectiveGroups(A.groupsOf(off, FP.desk).groups, A.emptyDraft(), null));
  assert.equal(a1.touches[0].kind, "dormant");
  assert.deepEqual(a1.lonely, [], "it waits for the peer: not lonely");
  const strange = layout([peer("desk", DESK, [0, 0]), peer("lap", LAP, [-1800, 0], { trusted: false })]);
  const a2 = A.analyse(A.effectiveGroups(A.groupsOf(strange, FP.desk).groups, A.emptyDraft(), null));
  assert.equal(a2.touches[0].kind, "blocked");
  assert.equal(a2.blocked.length, 1);
  assert.deepEqual(A.linkRows(a2, A.effectiveGroups(A.groupsOf(strange, FP.desk).groups, A.emptyDraft(), null)), [], "a blocked touch is not an edge link");
});

test("an overlap in the draft is found and named", () => {
  const l = three();
  const { groups } = A.groupsOf(l, FP.desk);
  const d = A.withOrigin(A.emptyDraft(), groups, l.revision, FP.lap, [-900, 0]);
  const a = A.analyse(A.effectiveGroups(groups, d, null));
  assert.deepEqual(a.overlaps.map((o) => [o.a, o.b]), [[FP.desk, FP.lap]]);
});

// ---------------------------------------------------------------------------- conflict

test("A newer revision over a dirty draft is a conflict that names the peer and the revision", () => {
  const l = three();
  const { groups } = A.groupsOf(l, FP.desk);
  const d = A.withOrigin(A.emptyDraft(), groups, l.revision, FP.lap, [-1800, 300]);
  assert.equal(A.conflictOf(d, l, groups, FP.desk), null, "same revision: no conflict");
  const newer = layout([peer("desk", DESK, [0, 0]), peer("lap", LAP, [-1800, 0]), peer("tower", TOWER, [3840, -180])], 15, "tower");
  const c = A.conflictOf(d, newer, A.groupsOf(newer, FP.desk).groups, FP.desk);
  assert.deepEqual([c.counter, c.by, c.byFp, c.mine], [15, "tower", FP.tower, false]);
  assert.equal(A.bannerTitle(c), "tower changed the layout while you were editing");
});

test("a clean draft never conflicts: the newer revision simply shows (U6)", () => {
  const l = three();
  const { groups } = A.groupsOf(l, FP.desk);
  const newer = layout([peer("desk", DESK, [0, 0])], 15, "tower");
  assert.equal(A.conflictOf(A.emptyDraft(), newer, groups, FP.desk), null);
});

test("the same counter from another peer is a different revision (concurrent edits)", () => {
  const l = three();
  const { groups } = A.groupsOf(l, FP.desk);
  const d = A.withOrigin(A.emptyDraft(), groups, l.revision, FP.lap, [-1800, 300]);
  const sibling = layout(three().peers, 14, "tower");
  assert.ok(A.conflictOf(d, sibling, groups, FP.desk));
});

test("a change made on this very machine is worded as such", () => {
  const l = three();
  const { groups } = A.groupsOf(l, FP.desk);
  const d = A.withOrigin(A.emptyDraft(), groups, l.revision, FP.lap, [-1800, 300]);
  const byMe = layout(three().peers, 15, "desk");
  const c = A.conflictOf(d, byMe, groups, FP.desk);
  assert.equal(c.mine, true);
  assert.equal(A.bannerTitle(c), "The layout changed while you were editing");
});

test("Keep mine rebases the draft on the new revision and keeps every edit", () => {
  const l = three();
  const { groups } = A.groupsOf(l, FP.desk);
  const d = A.withOrigin(A.emptyDraft(), groups, l.revision, FP.lap, [-1800, 300]);
  const kept = A.rebase(d, rev(15, "tower"));
  assert.deepEqual(kept.origins, d.origins);
  assert.deepEqual(A.payload(kept, groups).base_rev, { counter: 15, peer: FP.tower });
  assert.equal(A.revKey(kept.base), "15:" + FP.tower);
});

// ------------------------------------------------------------------------------ words

function ctx(extra = {}) {
  const { groups } = A.groupsOf(three(), FP.desk);
  const eff = A.effectiveGroups(groups, A.emptyDraft(), null);
  return { groups: eff, analysis: A.analyse(eff), drag: null, dirty: false, conflict: null, error: null, flash: "", rev: 14, canSet: true, meName: "desk", ...extra };
}

test("status, quiet: which revision is live and where", () => {
  const s = A.statusOf(ctx());
  assert.deepEqual([s.tone, s.glyph], ["normal", "info"]);
  assert.equal(s.text, "This layout is live on desk, lap and tower.");
});

test("status names the peer that is offline", () => {
  const l = layout([peer("desk", DESK, [0, 0]), peer("lap", LAP, [-1800, 0]), peer("tower", TOWER, [3840, -180], { online: false })]);
  const eff = A.effectiveGroups(A.groupsOf(l, FP.desk).groups, A.emptyDraft(), null);
  const s = A.statusOf(ctx({ groups: eff, analysis: A.analyse(eff) }));
  assert.equal(s.text, "This layout is live on desk and lap. tower is offline and gets it when it is back.");
});

test("status, edited: changed, not applied", () => {
  assert.equal(A.statusOf(ctx({ dirty: true })).text, "Changed, not applied yet.");
});

test("status, conflict: both ways out are named", () => {
  const s = A.statusOf(ctx({ dirty: true, conflict: { by: "tower", counter: 15, mine: false } }));
  // the banner names the choice; the status box only says where the draft stands
  assert.equal(s.tone, "normal");
  assert.equal(s.glyph, "info");
  assert.equal(s.text, "Your changes are not applied yet. Choose below.");
});

test("status, overlap: it names both machines and says Apply is off", () => {
  const l = three();
  const { groups } = A.groupsOf(l, FP.desk);
  const d = A.withOrigin(A.emptyDraft(), groups, l.revision, FP.lap, [-900, 0]);
  const eff = A.effectiveGroups(groups, d, null);
  const s = A.statusOf(ctx({ groups: eff, analysis: A.analyse(eff), dirty: true }));
  assert.equal(s.tone, "err");
  assert.equal(s.glyph, "alert");
  assert.equal(s.text, "desk covers part of lap. Move it until the edges touch without overlapping. Apply is off until then.");
});

test("status, a drag that would overlap: let go and it goes back", () => {
  const { groups } = A.groupsOf(three(), FP.desk);
  const eff = A.effectiveGroups(groups, A.emptyDraft(), null);
  const s = A.statusOf(ctx({ drag: { fp: FP.lap, valid: false, overlapWith: [FP.desk] } }));
  assert.equal(s.text, "lap would cover desk. Let go and it goes back.");
  assert.equal(eff.length, 3);
});

test("status, a daemon error shows its message and its hint; the codes the panel knows get plain words", () => {
  assert.equal(A.statusOf(ctx({ error: { code: "overlap", message: "lap overlaps desk", hint: "ferry layout show lists where each peer's monitors are" } })).text,
    "lap overlaps desk. ferry layout show lists where each peer's monitors are");
  assert.match(A.statusOf(ctx({ error: { code: "unknown_method", message: "unknown method" } })).text, /does not know layout\.set/);
  assert.match(A.statusOf(ctx({ error: { code: "timeout", message: "layout.set did not answer" } })).text, /did not answer/);
  assert.match(A.statusOf(ctx({ error: { code: "not_connected", message: "ferryd is not running" } })).text, /not running/);
  assert.equal(A.statusOf(ctx({ error: { code: "x", message: "m" } })).tone, "err");
});

test("status, lonely and unpaired and unplaced each say what to do", () => {
  const l = layout([peer("desk", DESK, [0, 0]), peer("lap", LAP, [-3000, 0]), peer("tower", TOWER, [3840, 0])]);
  const eff = A.effectiveGroups(A.groupsOf(l, FP.desk).groups, A.emptyDraft(), null);
  assert.equal(A.statusOf(ctx({ groups: eff, analysis: A.analyse(eff) })).text, "lap touches no other machine, so the pointer can never reach it. Drag it against an edge.");

  const strange = layout([peer("desk", DESK, [0, 0]), peer("lap", LAP, [-1800, 0], { trusted: false })]);
  const e2 = A.effectiveGroups(A.groupsOf(strange, FP.desk).groups, A.emptyDraft(), null);
  assert.equal(A.statusOf(ctx({ groups: e2, analysis: A.analyse(e2) })).text, "lap is not paired with desk: crossings to it are off.");

  const unplaced = layout([peer("desk", DESK, [0, 0]), peer("lap", LAP, null)], 3, "desk");
  const e3 = A.effectiveGroups(A.groupsOf(unplaced, FP.desk).groups, A.emptyDraft(), null);
  assert.match(A.statusOf(ctx({ groups: e3, analysis: A.analyse(e3) })).text, /^lap is not placed yet\. Drag it against an edge, or press Enter/);
});

test("status: two lonely machines are named in the plural", () => {
  const l = layout([peer("desk", DESK, [0, 0]), peer("lap", LAP, [-1800, 0]), peer("tower", TOWER, [9000, 0]), peer("deck", [["DP-2", 0, 0, 1920, 1080]], [-9000, 0], { fp: FP.deck })]);
  const eff = A.effectiveGroups(A.groupsOf(l, FP.desk).groups, A.emptyDraft(), null);
  assert.match(A.statusOf(ctx({ groups: eff, analysis: A.analyse(eff) })).text, /^tower and deck touch no other machine, so the pointer can never reach them\./);
});

test("status: a paired peer that has not reported its monitors is named, not silently missing", () => {
  const l = layout([peer("desk", DESK, [0, 0]), peer("lap", LAP, [-1800, 0]), { ...peer("tower", [], [3840, 0]), monitors: [], bounds: null }]);
  const parsed = A.groupsOf(l, FP.desk);
  const eff = A.effectiveGroups(parsed.groups, A.emptyDraft(), null);
  const s = A.statusOf(ctx({ groups: eff, analysis: A.analyse(eff), waiting: parsed.waiting }));
  assert.equal(s.tone, "warn");
  assert.equal(s.text, "tower has not reported its screens yet, so it cannot be placed.");
  // a problem the user can fix comes first, and a draft is still a draft when nothing is waiting
  assert.equal(A.statusOf(ctx({ dirty: true, waiting: [] })).text, "Changed, not applied yet.");
});

test("status: a daemon that cannot arrange says so before anything else; a flash comes after the problems", () => {
  assert.match(A.statusOf(ctx({ canSet: false, conflict: { by: "x", counter: 1 } })).text, /cannot move machines from the panel/);
  const flash = A.statusOf(ctx({ flash: "Applied. Sent to lap.", flashTone: "ok" }));
  assert.deepEqual([flash.tone, flash.glyph, flash.text], ["ok", "check", "Applied. Sent to lap."]);
  const withError = A.statusOf(ctx({ flash: "x", error: { code: "overlap", message: "m" } }));
  assert.equal(withError.tone, "err", "a problem outranks the flash");
});

test("the Applied line says who got it and who will", () => {
  const l = layout([peer("desk", DESK, [0, 0]), peer("lap", LAP, [-1800, 0]), peer("tower", TOWER, [3840, -180], { online: false })], 15, "desk");
  const { groups } = A.groupsOf(l, FP.desk);
  assert.equal(A.appliedText(l, groups), "Applied. Sent to lap. tower is offline and gets it when it is back.");
  const alone = layout([peer("desk", DESK, [0, 0]), peer("lap", LAP, [-1800, 0], { online: false })], 15, "desk");
  assert.equal(A.appliedText(alone, A.groupsOf(alone, FP.desk).groups), "Applied. No other machine is online yet. lap is offline and gets it when it is back.");
});

test("a group is announced with its name, tags, monitors and place", () => {
  const strange = layout([peer("desk", DESK, [0, 0]), peer("lap", LAP, [-1800, 0], { trusted: false }), peer("tower", TOWER, [3840, 0], { online: false })]);
  const eff = A.effectiveGroups(A.groupsOf(strange, FP.desk).groups, A.emptyDraft(), null);
  assert.equal(A.groupName(named(eff, "desk"), "desk"), "desk (this machine), HDMI-A-1 and DVI-D-1, at x 0, y 0. Arrow keys move it.");
  assert.equal(A.groupName(named(eff, "lap"), "desk"), "lap (not paired with desk), eDP-1, at x -1800, y 0. Arrow keys move it.");
  assert.equal(A.groupName(named(eff, "tower"), "desk"), "tower (offline), DP-1, at x 3840, y 0. Arrow keys move it.");
});

test("Apply is available only for a valid, unconflicted, idle draft on a connected daemon", () => {
  const ok = { connected: true, dirty: true, canSet: true, overlap: false, conflict: false, applying: false, dragging: false };
  assert.equal(A.canApply(ok), true);
  for (const key of ["dirty", "connected"]) assert.equal(A.canApply({ ...ok, [key]: false }), false, key);
  for (const key of ["overlap", "conflict", "applying", "dragging"]) assert.equal(A.canApply({ ...ok, [key]: true }), false, key);
  assert.equal(A.canApply({ ...ok, canSet: false }), false);
});

test("a daemon that lists its methods is believed about layout.set; one that does not is tried", () => {
  assert.equal(A.canSet({ methods: ["status", "layout.set"] }), true);
  assert.equal(A.canSet({ methods: ["status", "layout.get"] }), false);
  assert.equal(A.canSet({ api: 1 }), true, "a hello from before M3 has no methods");
  assert.equal(A.canSet(null), true);
});

test("a stale_layout answer is read for the revision and the peer", () => {
  assert.deepEqual(A.staleInfo({ code: "stale_layout", message: "the layout changed (rev 15 by lap)" }), { counter: 15, by: "lap" });
  assert.equal(A.staleInfo({ code: "stale_layout", message: "something else" }), null);
  assert.equal(A.staleInfo({ code: "overlap", message: "the layout changed (rev 15 by lap)" }), null);
  assert.equal(A.staleInfo(null), null);
});

test("every Arrange string the module asks for exists", () => {
  // the sentences above are built from keys; a missing key would print the key itself
  for (const key of ["arrange.status.live", "arrange.status.dirty", "arrange.status.conflict", "arrange.status.overlap", "arrange.status.lonely.many",
    "arrange.status.untrusted.many", "arrange.status.unplaced.many", "arrange.status.legacy", "arrange.error.timeout", "arrange.conflict.body", "arrange.keep",
    "arrange.take", "arrange.apply", "arrange.reset", "arrange.undo", "arrange.empty.title", "arrange.hint", "arrange.zoom.in", "arrange.fit"]) {
    assert.notEqual(Strings.t(key), key, key);
  }
});
