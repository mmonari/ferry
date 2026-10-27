import { test } from "node:test";
import assert from "node:assert/strict";
import { load } from "./harness.mjs";

const G = load("Geometry.js");

// -------------------------------------------------------------------------------- fixtures

const mon = (name, x, y, w, h) => ({ name, x, y, w, h });
const group = (fp, name, monitors, extra = {}) => ({ fp, name, self: false, trusted: true, online: true, placed: true, monitors, ...extra });

// desk: HDMI-A-1 and DVI-D-1 side by side, 3840x1080, at the plane origin
const desk = (dx = 0, dy = 0) => group("desk", "desk", [mon("HDMI-A-1", dx, dy, 1920, 1080), mon("DVI-D-1", dx + 1920, dy, 1920, 1080)], { self: true });
// lap: one 1800x1125 monitor (2880x1800 at scale 1.6)
const lap = (x, y) => group("lap", "lap", [mon("eDP-1", x, y, 1800, 1125)]);
const tower = (x, y) => group("tower", "tower", [mon("DP-1", x, y, 2560, 1440)]);

// one link in the shape of ferry_core::geom::EdgeLink, with names for the monitors
const link = (from, side, to, line, span) => ({ from: from.split(":"), side, to: to.split(":"), line, span });
const shape = (links) => links.map((l) => ({ from: [l.from.fp, l.from.name], side: l.side, to: [l.to.fp, l.to.name], line: l.line, span: l.span }));
const asLinks = (links) => shape(links).map((l) => link(l.from.join(":"), l.side, l.to.join(":"), l.line, l.span));

// ---------------------------------------------------------------------------- rectangles

test("rectangles are half-open: a shared edge is not an overlap", () => {
  const a = G.rect(0, 0, 100, 100);
  assert.equal(G.overlaps(a, G.rect(100, 0, 50, 50)), false, "touching on the right");
  assert.equal(G.overlaps(a, G.rect(0, 100, 50, 50)), false, "touching below");
  assert.equal(G.overlaps(a, G.rect(99, 99, 50, 50)), true, "one pixel in");
  assert.equal(G.overlaps(a, G.rect(-50, -50, 50, 50)), false, "corner contact");
  assert.equal(G.overlaps(a, G.rect(10, 10, 0, 50)), false, "an empty rectangle overlaps nothing");
});

test("intersection, bounding box, union and containment", () => {
  assert.deepEqual(G.intersection(G.rect(0, 0, 100, 100), G.rect(60, 80, 100, 100)), G.rect(60, 80, 40, 20));
  assert.equal(G.intersection(G.rect(0, 0, 10, 10), G.rect(10, 0, 10, 10)), null);
  assert.deepEqual(G.bounding([G.rect(0, 0, 10, 10), G.rect(20, -5, 10, 10)]), G.rect(0, -5, 30, 15));
  assert.equal(G.bounding([]), null);
  assert.deepEqual(G.union(G.rect(0, 0, 10, 10), G.rect(5, 5, 20, 20)), G.rect(0, 0, 25, 25));
  assert.equal(G.contains(G.rect(0, 0, 100, 100), G.rect(10, 10, 20, 20)), true);
  assert.equal(G.contains(G.rect(0, 0, 100, 100), G.rect(90, 10, 20, 20)), false);
});

test("sides: lines, spans and opposites follow ferry_core::geom::Side", () => {
  const r = G.rect(10, 20, 100, 50);
  assert.deepEqual(["left", "right", "top", "bottom"].map((s) => G.sideLine(r, s)), [10, 110, 20, 70]);
  assert.deepEqual(G.sideSpan(r, "left"), [20, 70]);
  assert.deepEqual(G.sideSpan(r, "bottom"), [10, 110]);
  assert.deepEqual(G.SIDES.map(G.opposite), ["right", "left", "bottom", "top"]);
  assert.equal(G.isVertical("right"), true);
  assert.equal(G.isVertical("top"), false);
});

// -------------------------------------------------------------------------- group overlap

test("two groups overlap when a monitor of one covers a monitor of the other, not when their boxes do", () => {
  // an L-shaped group: the box of {A, B} is 2000x2000, but the corner (1000..2000, 1000..2000) is empty
  const l = group("l", "l", [mon("A", 0, 0, 2000, 1000), mon("B", 0, 1000, 1000, 1000)]);
  const inCorner = group("c", "c", [mon("C", 1200, 1200, 500, 500)]);
  assert.equal(G.groupsOverlap(l, inCorner), false);
  const onB = group("c", "c", [mon("C", 900, 1200, 500, 500)]);
  assert.equal(G.groupsOverlap(l, onB), true);
  assert.deepEqual(G.overlapAreas(l, onB), [G.rect(900, 1200, 100, 500)]);
});

test("firstOverlap names the moved peer first, in the daemon's order, and ignores unplaced groups", () => {
  const groups = [desk(), lap(100, 0), tower(10000, 0)];
  assert.deepEqual(G.firstOverlap(groups, ["lap"]), { moved: "lap", other: "desk" });
  assert.deepEqual(G.firstOverlap(groups, ["desk"]), { moved: "desk", other: "lap" });
  assert.equal(G.firstOverlap(groups, ["tower"]), null);
  const unplaced = [desk(), { ...lap(100, 0), placed: false }];
  assert.equal(G.firstOverlap(unplaced, ["lap"]), null);
  // a layout that already overlaps, with nobody moved there, is not reported
  assert.equal(G.firstOverlap(groups, ["tower"]), null);
});

test("allOverlaps lists every overlapping pair once, with the area to tint", () => {
  const groups = [desk(), lap(-900, 0), tower(1000, 500)];
  const o = G.allOverlaps(groups);
  assert.deepEqual(o.map((p) => [p.a, p.b]), [["desk", "lap"], ["desk", "tower"]]);
  assert.deepEqual(o[0].areas, [G.rect(0, 0, 900, 1080)]);
});

// ------------------------------------------------------------------------------- links

test("derive_links parity: lap left of desk, tops aligned (journey J3)", () => {
  const groups = [desk(), lap(-1800, 0)];
  const links = G.deriveLinks(groups);
  assert.deepEqual(asLinks(links), [
    link("desk:HDMI-A-1", "left", "lap:eDP-1", 0, [0, 1080]),
    link("lap:eDP-1", "right", "desk:HDMI-A-1", 0, [0, 1080])
  ]);
});

test("derive_links parity: first light, the netbook below the desk (Rust `first_light`)", () => {
  const netbook = group("nb", "nb", [mon("eDP-1", 277, 1080, 1366, 768)]);
  const links = G.deriveLinks([desk(), netbook]);
  assert.deepEqual(asLinks(links), [
    link("desk:HDMI-A-1", "bottom", "nb:eDP-1", 1080, [277, 1643]),
    link("nb:eDP-1", "top", "desk:HDMI-A-1", 1080, [277, 1643])
  ]);
});

test("derive_links parity: each side of the desk (Rust `links_each_side`)", () => {
  // the netbook, 1366x768, centred on the anchor monitor's edge: 277 along x, 156 along y
  const cases = [
    ["below", [277, 1080], "HDMI-A-1", "bottom", 1080, [277, 1643]],
    ["above", [277, -768], "HDMI-A-1", "top", 0, [277, 1643]],
    ["left-of", [-1366, 156], "HDMI-A-1", "left", 0, [156, 924]],
    ["right-of", [3840, 156], "DVI-D-1", "right", 3840, [156, 924]]
  ];
  for (const [rel, at, anchor, side, line, span] of cases) {
    const nb = group("nb", "nb", [mon("eDP-1", at[0], at[1], 1366, 768)]);
    assert.deepEqual(asLinks(G.deriveLinks([desk(), nb])), [
      link(`desk:${anchor}`, side, "nb:eDP-1", line, span),
      link("nb:eDP-1", G.opposite(side), `desk:${anchor}`, line, span)
    ], rel);
  }
});

test("a corner contact is not a link, and monitors of one peer never link to each other", () => {
  assert.deepEqual(G.deriveLinks([desk(), lap(-1800, 1080)]), [], "lap's top-right corner touches desk's bottom-left");
  // desk's own two monitors share an edge and make no link
  assert.deepEqual(G.deriveLinks([desk()]), []);
  // one pixel of overlap along the edge is a link of one pixel
  const one = G.deriveLinks([desk(), lap(-1800, 1079)]);
  assert.deepEqual(one[0].span, [1079, 1080]);
});

test("a monitor can link to two monitors of another peer at once", () => {
  // tower, 2560 wide, sits on top of both desk monitors: it links to HDMI-A-1 and to DVI-D-1
  const links = G.deriveLinks([desk(), tower(640, -1440)]);
  assert.deepEqual(asLinks(links), [
    link("desk:HDMI-A-1", "top", "tower:DP-1", 0, [640, 1920]),
    link("tower:DP-1", "bottom", "desk:HDMI-A-1", 0, [640, 1920]),
    link("desk:DVI-D-1", "top", "tower:DP-1", 0, [1920, 3200]),
    link("tower:DP-1", "bottom", "desk:DVI-D-1", 0, [1920, 3200])
  ]);
});

test("touches: one entry per segment, and what it amounts to", () => {
  const live = G.touches([desk(), lap(-1800, 0)]);
  assert.equal(live.length, 1);
  assert.deepEqual(live[0], {
    a: { fp: "desk", index: 0, name: "HDMI-A-1", side: "left" },
    b: { fp: "lap", index: 0, name: "eDP-1", side: "right" },
    line: 0, span: [0, 1080], vertical: true, kind: "link"
  });

  const offline = G.touches([desk(), { ...lap(-1800, 0), online: false }]);
  assert.equal(offline[0].kind, "dormant", "an offline peer's links act as walls until it is back");

  const strange = G.touches([desk(), { ...lap(-1800, 0), trusted: false }]);
  assert.equal(strange[0].kind, "blocked", "a peer not paired here has no link");

  const unplaced = G.touches([desk(), { ...lap(-1800, 0), placed: false }]);
  assert.deepEqual(unplaced, [], "a group that is not placed touches nothing");
});

// -------------------------------------------------------------------------------- walls

const edgesOf = (segs, fp) => segs.filter((s) => s.fp === fp).map((s) => `${s.index}:${s.side}@${s.line}[${s.span[0]},${s.span[1]}]`).sort();

test("journey J3: lap's right edge below y = 1080 is shown as a wall", () => {
  const groups = [desk(), lap(-1800, 0)];
  const walls = G.wallSegments(groups, G.touches(groups));
  assert.ok(edgesOf(walls, "lap").includes("0:right@0[1080,1125]"), "the 45 px that touch nothing");
  assert.ok(!edgesOf(walls, "lap").some((e) => e.startsWith("0:right@0[0,")), "and not the 1080 px that are a link");
  assert.ok(!edgesOf(walls, "desk").some((e) => e.startsWith("0:left")), "desk's left edge is all link");
});

test("the edge two monitors of one peer share is not a wall; every other outer edge is", () => {
  const walls = G.wallSegments([desk()], []);
  assert.deepEqual(edgesOf(walls, "desk"), [
    "0:bottom@1080[0,1920]", "0:left@0[0,1080]", "0:top@0[0,1920]",
    "1:bottom@1080[1920,3840]", "1:right@3840[0,1080]", "1:top@0[1920,3840]"
  ]);
});

test("a link takes only the part of the edge it covers, and the rest stays a wall", () => {
  const groups = [desk(), tower(640, -1440)];
  const walls = G.wallSegments(groups, G.touches(groups));
  const top = edgesOf(walls, "desk").filter((e) => /:top@/.test(e));
  // HDMI-A-1's top is covered from 640 on; DVI-D-1's top until 3200
  assert.deepEqual(top, ["0:top@0[0,640]", "1:top@0[3200,3840]"]);
});

test("a blocked or dormant touch is not a wall either: a seam of its own is drawn there", () => {
  const groups = [desk(), { ...lap(-1800, 0), trusted: false }];
  const walls = G.wallSegments(groups, G.touches(groups));
  assert.ok(!edgesOf(walls, "lap").some((e) => e.startsWith("0:right@0[0,1080]")));
});

// ----------------------------------------------------------------------------- snapping

// the rectangles of every other group, as snapMove takes them
const rectsOf = (...groups) => groups.map(G.groupRects);
const snap = (moving, others, delta, extra = {}) => G.snapMove({
  start: G.groupRects(moving), others: rectsOf(...others), origin: extra.origin || [moving.monitors[0].x, moving.monitors[0].y],
  delta, threshold: extra.threshold === undefined ? 40 : extra.threshold, grid: extra.grid, free: extra.free
});

test("a drag that comes within the threshold of an edge snaps to it and shows the guide", () => {
  // lap at x = -2000 dragged +185 to the right: its right edge (-15) is 15 px from desk's left edge
  const r = snap(lap(-2000, 0), [desk()], [185, 0]);
  assert.deepEqual(r.delta, [200, 0], "right edge to desk's left edge: x = -1800");
  assert.deepEqual(r.origin, [-1800, 0]);
  assert.equal(r.snapX.kind, "touch");
  assert.equal(r.snapX.at, 0);
  assert.equal(r.valid, true);
  assert.deepEqual(r.guides.find((g) => g.axis === "x"), { axis: "x", at: 0, from: 0, to: 1125, kind: "touch" });
});

test("snapping a corner: touch on one axis and the tops aligned on the other", () => {
  const r = snap(lap(-2000, 30), [desk()], [190, -22]);
  assert.deepEqual(r.origin, [-1800, 0]);
  assert.equal(r.snapX.kind, "touch");
  assert.equal(r.snapY.kind, "align");
  assert.equal(r.guides.length, 2);
});

test("outside the threshold nothing snaps, and the group moves by the pointer's travel in whole pixels", () => {
  const r = snap(lap(-2000, 0), [desk()], [118.4, 46.6], { threshold: 20 });
  assert.equal(r.snapX, null);
  assert.equal(r.snapY, null);
  assert.deepEqual(r.delta, [118, 47]);
  assert.deepEqual(r.origin, [-1882, 47]);
  assert.deepEqual(r.guides, []);
  assert.equal(r.valid, true);
});

test("a drag that comes back to where it began changes nothing, whatever the origin is", () => {
  // 277 is not a multiple of 10: the netbook below the desk, as the first-light drill places it
  const mini = group("m", "m", [mon("eDP-1", 277, 1080, 1366, 768)]);
  const r = snap(mini, [desk()], [0, 0], { origin: [277, 1080] });
  assert.deepEqual(r.delta, [0, 0]);
  assert.deepEqual(r.origin, [277, 1080]);
  assert.equal(r.valid, true);
  const small = snap(mini, [desk()], [0.4, -0.4], { origin: [277, 1080], threshold: 0 });
  assert.deepEqual(small.delta, [0, 0]);
});

test("holding the modifier turns snapping off and moves by whole pixels", () => {
  const r = snap(lap(-2000, 0), [desk()], [185, 3], { free: true });
  assert.deepEqual(r.origin, [-1815, 3]);
  assert.equal(r.snapX, null);
  assert.deepEqual(r.guides, []);
});

test("the magnet lifts a group out of a small overlap: it snaps to the touching position", () => {
  // lap's right edge is 12 px inside desk
  const r = snap(lap(-1812, 0), [desk()], [0, 0]);
  assert.deepEqual(r.origin, [-1800, 0]);
  assert.equal(r.valid, true);
});

test("a drop deep inside another group is invalid, says whom it covers, and does not snap", () => {
  const r = snap(lap(-2000, 0), [desk()], [1500, 0]);
  assert.equal(r.valid, false);
  assert.deepEqual(r.overlapWith, [0]);
  assert.deepEqual(r.origin, [-500, 0], "where the pointer asks for it, on the grid");
});

test("a snap that would overlap is passed over for the next one that does not", () => {
  // desk's inner boundary (x = 1920) is a candidate for lap's left edge; lap is wide enough to
  // cover desk's monitors there, so the snap to desk's right edge (3840) must win instead
  const wide = group("w", "w", [mon("W", 3700, 500, 1000, 800)]);
  const r = snap(wide, [desk()], [-20, 0], { threshold: 200 });
  assert.equal(r.valid, true);
  assert.deepEqual(r.origin, [3840, 500], "left edge on desk's right edge");
});

test("a multi-monitor group snaps by any of its monitors", () => {
  // tower's left edge to desk's right edge, but it is desk that moves: its DVI-D-1 right edge is the one
  const r = snap(desk(), [tower(5000, 0)], [1130, 0], { origin: [0, 0], threshold: 60 });
  assert.equal(r.snapX.kind, "touch");
  assert.deepEqual(r.origin, [1160, 0], "DVI-D-1's right edge (3840 + 1160) on tower's left edge");
});

test("the snap distance is 14 screen pixels converted to the plane", () => {
  assert.equal(G.snapThreshold(0.07), 14 / 0.07);
  assert.equal(G.snapThreshold(0.5, 20), 40);
  assert.equal(G.snapThreshold(0), 0);
});

// ------------------------------------------------------------------------------- nudging

const nudge = (moving, others, dir, mode) => G.nudgeMove({ start: G.groupRects(moving), others: rectsOf(...others), dir, mode });

test("an arrow key moves 10 px; Shift moves 100 px", () => {
  const l = lap(-3000, 400);
  assert.deepEqual(nudge(l, [desk()], "right", "step").delta, [10, 0]);
  assert.deepEqual(nudge(l, [desk()], "left", "step").delta, [-10, 0]);
  assert.deepEqual(nudge(l, [desk()], "down", "step").delta, [0, 10]);
  assert.deepEqual(nudge(l, [desk()], "up", "step").delta, [0, -10]);
  assert.deepEqual(nudge(l, [desk()], "right", "far").delta, [100, 0]);
  assert.deepEqual(nudge(l, [desk()], "up", "far").delta, [0, -100]);
});

test("a step stops at an edge it would jump over, so a gap that is not a multiple of 10 can be closed", () => {
  // lap's right edge is 4 px from desk's left edge
  const l = lap(-1804, 0);
  const r = nudge(l, [desk()], "right", "step");
  assert.deepEqual(r.delta, [4, 0]);
  assert.equal(r.snap.kind, "touch");
  // and from there the same key is refused: it would cover desk
  const there = lap(-1800, 0);
  const again = nudge(there, [desk()], "right", "step");
  assert.equal(again.moved, false);
  assert.deepEqual(again.overlapWith, [0]);
});

test("Shift stops at the first edge within 100 px, Ctrl goes to the next edge however far", () => {
  const l = lap(-2200, 0);
  assert.deepEqual(nudge(l, [desk()], "right", "far").delta, [100, 0], "the edge is 400 px away");
  assert.deepEqual(nudge(l, [desk()], "right", "edge").delta, [400, 0], "Ctrl: right edge on desk's left edge");
  const near = lap(-1850, 0);
  assert.deepEqual(nudge(near, [desk()], "right", "far").delta, [50, 0], "Shift stops at the edge 50 px away");
});

test("Ctrl moves 100 px when there is no edge in that direction; an edge that would overlap is not a stop", () => {
  assert.deepEqual(nudge(lap(5000, 0), [desk()], "right", "edge").delta, [100, 0]);
  // moving lap left from the touching position: the next edge on the left is lap's left on desk's... none
  assert.deepEqual(nudge(lap(-1800, 0), [desk()], "left", "edge").delta, [-100, 0]);
  // aligning lap's top with the bottom of desk's monitors would be fine; aligning lap's left edge with
  // desk's right edge (3840) means standing right of desk, which is a valid stop for lap at x = 3840
  const r = nudge(lap(3000, 0), [desk()], "right", "edge");
  assert.equal(r.moved, true);
  assert.equal(r.delta[0], 840, "the first valid edge to the right is lap's left on desk's right: 3840");
});

test("an arrow into another group is refused and names it", () => {
  const r = nudge(lap(-1800, 0), [desk()], "right", "far");
  assert.equal(r.moved, false);
  assert.deepEqual(r.delta, [0, 0]);
  assert.deepEqual(r.overlapWith, [0]);
});

// ---------------------------------------------------------------------------------- view

test("fitRegion leaves room to drag a group to either side, and viewFor centres it", () => {
  const content = G.rect(-1800, 0, 8200, 1440);
  const region = G.fitRegion(content, { w: 3840, h: 1440 });
  assert.ok(region.x < content.x && region.y < content.y);
  assert.ok(region.x + region.w > content.x + content.w && region.y + region.h > content.y + content.h);
  assert.ok(region.w - content.w >= 2 * 3840 * 0.25, "a quarter of the widest group on each side");
  assert.ok(region.h - content.h >= 2 * 1440 * 0.2, "and a fifth of the tallest above and below");
  const v = G.viewFor(region, 800, 300);
  assert.ok(Math.abs(v.scale - Math.min(800 / region.w, 300 / region.h)) < 1e-12);
  // the region is centred: the canvas centre is the region's centre
  const c = G.toPlane(v, 400, 150);
  assert.ok(Math.abs(c.x - (region.x + region.w / 2)) < 1e-6);
  assert.ok(Math.abs(c.y - (region.y + region.h / 2)) < 1e-6);
});

test("toCanvas and toPlane are inverses", () => {
  const v = G.viewFor(G.rect(-3000, -1000, 12000, 4000), 800, 300);
  for (const [x, y] of [[0, 0], [-1800, 55], [5000, 1999]]) {
    const c = G.toCanvas(v, x, y);
    const p = G.toPlane(v, c.x, c.y);
    assert.ok(Math.abs(p.x - x) < 1e-9 && Math.abs(p.y - y) < 1e-9);
  }
});

test("the region only grows, so a drop never makes the picture jump", () => {
  const a = G.rect(0, 0, 100, 100);
  assert.equal(G.growRegion(null, a), a);
  assert.equal(G.growRegion(a, G.rect(10, 10, 50, 50)), a, "inside: unchanged");
  assert.deepEqual(G.growRegion(a, G.rect(50, 50, 100, 100)), G.rect(0, 0, 150, 150));
});

test("zoom keeps the point under the pointer where it is, and stays between Fit and 8x", () => {
  const v = G.viewFor(G.rect(0, 0, 10000, 3000), 800, 300);
  const fit = v.scale;
  const z = G.zoomAt(v, 2, 400, 100, fit, 8);
  const before = G.toPlane(v, 400, 100), after = G.toPlane(z, 400, 100);
  assert.ok(Math.abs(before.x - after.x) < 1e-6 && Math.abs(before.y - after.y) < 1e-6);
  assert.ok(Math.abs(z.scale - 2 * fit) < 1e-12);
  assert.ok(Math.abs(G.zoomAt(v, 0.1, 0, 0, fit, 8).scale - fit) < 1e-12, "no further out than Fit");
  assert.ok(Math.abs(G.zoomAt(v, 100, 0, 0, fit, 8).scale - 8 * fit) < 1e-12, "no further in than 8x");
});

test("panning moves the view the other way by the pointer's travel", () => {
  const v = G.viewFor(G.rect(0, 0, 4000, 2000), 800, 400);
  const p = G.panBy(v, 100, -50);
  assert.ok(Math.abs(p.x - (v.x - 100 / v.scale)) < 1e-9);
  assert.ok(Math.abs(p.y - (v.y + 50 / v.scale)) < 1e-9);
  assert.equal(p.scale, v.scale);
});

test("a dragged group's centre is kept inside what the canvas shows", () => {
  const v = { scale: 0.1, x: 0, y: 0, w: 1000, h: 500 };
  const b = G.rect(100, 100, 200, 100);
  assert.deepEqual(G.keepVisible(b, v, [50, 0]), [50, 0]);
  assert.deepEqual(G.keepVisible(b, v, [2000, 0]), [1000 - 200, 0], "centre at x = 1000, the edge");
  assert.deepEqual(G.keepVisible(b, v, [0, -400]), [0, -150], "the centre stops at y = 0");
});

test("a small group gets a hit area of at least the minimum size, about its centre", () => {
  const h = G.hitRect(G.rect(100, 100, 20, 12), 44, 36);
  assert.deepEqual([h.w, h.h], [44, 36]);
  assert.deepEqual([h.x + h.w / 2, h.y + h.h / 2], [110, 106]);
  assert.deepEqual([h.innerX, h.innerY], [12, 12]);
  const big = G.hitRect(G.rect(0, 0, 300, 200), 44, 36);
  assert.deepEqual([big.x, big.y, big.w, big.h, big.innerX, big.innerY], [0, 0, 300, 200, 0, 0]);
});

test("a group's name goes above it unless another group is in the way", () => {
  const box = G.rect(0, 1080, 1920, 1080);
  assert.equal(G.stripBlocked(box, [G.rect(0, 0, 3840, 1080)], 600, 200), true, "desk's monitors are right above");
  assert.equal(G.stripBlocked(box, [G.rect(0, 0, 3840, 800)], 600, 200), false, "a gap of 280 px is enough for 200");
  assert.equal(G.stripBlocked(box, [G.rect(900, 0, 3840, 1080)], 600, 200), false, "beside the strip, not in it");
  assert.equal(G.stripBlocked(box, [], 600, 200), false);
});

test("a tile says as much as it has room for", () => {
  assert.equal(G.labelTier(200, 100), 3);
  assert.equal(G.labelTier(120, 60), 2);
  assert.equal(G.labelTier(60, 30), 1);
  assert.equal(G.labelTier(30, 18), 0);
});

// -------------------------------------------------------------------- unplaced groups

test("a group that has no place yet is shown right of everything, level with the first group", () => {
  const placed = [desk(), lap(-1800, 0)];
  const local = G.groupRects(tower(0, 0));
  assert.deepEqual(G.provisionalOffset(placed, local), [3840, 0]);
});

test("the provisional place moves down clear of what it would cover", () => {
  const placed = [group("a", "a", [mon("A", 0, 0, 1000, 1000), mon("B", 1000, 500, 1000, 1000)])];
  // right of the box is x = 2000, which is free: level with the first group's top
  assert.deepEqual(G.provisionalOffset(placed, [G.rect(0, 0, 500, 500)]), [2000, 0]);
  // nothing placed: the group goes where its own coordinates put it, at the origin
  assert.deepEqual(G.provisionalOffset([], [G.rect(40, 60, 500, 500)]), [-40, -60]);
});

// ------------------------------------------------------------------ random layouts
//
// "Links equal the touching segments for random placements": checked against a second, dumber way
// to say the same thing, one pixel at a time, so the loops in deriveLinks are not testing themselves.

function prng(seed) {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

// 2-4 peers of 1-3 monitors on a 100 px grid (small, so edges meet often), monitors of one peer
// side by side or stacked. Each group after the first is put against a side of an earlier one, at a
// random offset along it, or (one in four) anywhere; layouts with an overlap are thrown away.
function randomLayout(rand) {
  const peers = 2 + Math.floor(rand() * 3);
  const sizes = [[3, 2], [4, 3], [2, 2], [5, 3], [3, 3]];
  const groups = [];
  for (let p = 0; p < peers; p++) {
    const n = 1 + Math.floor(rand() * 3);
    const [w, h] = sizes[Math.floor(rand() * sizes.length)];
    const stacked = rand() < 0.4;
    const build = (ox, oy) => {
      const monitors = [];
      for (let m = 0; m < n; m++) {
        monitors.push(stacked ? mon(`M${m}`, ox, oy + m * h * 100, w * 100, h * 100) : mon(`M${m}`, ox + m * w * 100, oy, w * 100, h * 100));
      }
      return group(`p${p}`, `p${p}`, monitors);
    };
    let g = build(0, 0);
    const b = G.groupBounds(g);
    if (groups.length === 0 || rand() < 0.25) {
      g = build(Math.floor(rand() * 16) * 100 - 400, Math.floor(rand() * 10) * 100 - 200);
    } else {
      const anchor = G.groupBounds(groups[Math.floor(rand() * groups.length)]);
      const side = G.SIDES[Math.floor(rand() * 4)];
      const along = (lo, len, mine) => lo - mine + 100 * Math.floor((rand() * (len + mine)) / 100);
      let x, y;
      if (side === "left") { x = anchor.x - b.w; y = along(anchor.y, anchor.h, b.h); }
      else if (side === "right") { x = anchor.x + anchor.w; y = along(anchor.y, anchor.h, b.h); }
      else if (side === "top") { y = anchor.y - b.h; x = along(anchor.x, anchor.w, b.w); }
      else { y = anchor.y + anchor.h; x = along(anchor.x, anchor.w, b.w); }
      g = build(x, y);
    }
    groups.push(g);
  }
  return groups;
}

const noOverlap = (groups) => G.allOverlaps(groups).length === 0;

// the touching pixels between two rectangles along one side, as the run [lo, hi) of them, or null
function pixelRun(a, b, side) {
  const vertical = G.isVertical(side);
  const covers = (r, px, py) => px >= r.x && px < r.x + r.w && py >= r.y && py < r.y + r.h;
  const line = G.sideLine(a, side);
  if (line !== G.sideLine(b, G.opposite(side))) return null;
  const [lo, hi] = G.sideSpan(a, side);
  let first = null, last = null;
  for (let t = lo - 100; t < hi + 100; t++) {
    // the pixel just inside a on `side`, and the pixel just inside b across the line
    const inside = side === "left" ? [line, t] : side === "right" ? [line - 1, t] : side === "top" ? [t, line] : [t, line - 1];
    const across = side === "left" ? [line - 1, t] : side === "right" ? [line, t] : side === "top" ? [t, line - 1] : [t, line];
    const [ix, iy] = vertical ? inside : [inside[0], inside[1]];
    const [cx, cy] = vertical ? across : [across[0], across[1]];
    if (covers(a, ix, iy) && covers(b, cx, cy)) {
      if (first === null) first = t;
      last = t;
    }
  }
  return first === null ? null : [first, last + 1];
}

test("derive_links equals the touching pixels, for 400 random layouts", () => {
  const rand = prng(20261021);
  let checked = 0, withLinks = 0;
  for (let i = 0; i < 400; i++) {
    const groups = randomLayout(rand);
    if (!noOverlap(groups)) continue;
    checked++;
    const expected = [];
    const mons = groups.flatMap((g) => g.monitors.map((m, index) => ({ fp: g.fp, index, name: m.name, rect: G.rect(m.x, m.y, m.w, m.h) })));
    for (let a = 0; a < mons.length; a++) for (let b = a + 1; b < mons.length; b++) {
      if (mons[a].fp === mons[b].fp) continue;
      for (const side of G.SIDES) {
        const run = pixelRun(mons[a].rect, mons[b].rect, side);
        if (!run) continue;
        expected.push([mons[a].fp, mons[a].index, side, mons[b].fp, mons[b].index, run]);
        expected.push([mons[b].fp, mons[b].index, G.opposite(side), mons[a].fp, mons[a].index, run]);
      }
    }
    const got = G.deriveLinks(groups).map((l) => [l.from.fp, l.from.index, l.side, l.to.fp, l.to.index, l.span]);
    assert.deepEqual(got, expected, `layout ${i}`);
    if (got.length) withLinks++;
  }
  assert.ok(checked > 150, `only ${checked} layouts were valid`);
  assert.ok(withLinks > 60, `only ${withLinks} layouts had a link: the generator is too sparse to test anything`);
});

test("every monitor edge is exactly a wall, a seam or the inside of its own group, for random layouts", () => {
  const rand = prng(777);
  let checked = 0;
  for (let i = 0; i < 300; i++) {
    const groups = randomLayout(rand);
    if (!noOverlap(groups)) continue;
    checked++;
    const touches = G.touches(groups);
    const walls = G.wallSegments(groups, touches);
    groups.forEach((g) => g.monitors.forEach((m, index) => {
      const r = G.rect(m.x, m.y, m.w, m.h);
      for (const side of G.SIDES) {
        const [lo, hi] = G.sideSpan(r, side);
        const covered = new Array(hi - lo).fill(0);
        const paint = (a, b) => { for (let t = Math.max(a, lo); t < Math.min(b, hi); t++) covered[t - lo]++; };
        walls.filter((w) => w.fp === g.fp && w.index === index && w.side === side).forEach((w) => paint(w.span[0], w.span[1]));
        touches.forEach((t) => {
          for (const end of [t.a, t.b]) if (end.fp === g.fp && end.index === index && end.side === side) paint(t.span[0], t.span[1]);
        });
        g.monitors.forEach((n, ni) => {
          if (ni === index) return;
          const rn = G.rect(n.x, n.y, n.w, n.h);
          if (G.sideLine(rn, G.opposite(side)) === G.sideLine(r, side)) { const s = G.sideSpan(rn, G.opposite(side)); paint(s[0], s[1]); }
        });
        assert.ok(covered.every((c) => c === 1), `layout ${i}, ${g.fp}:${index} ${side}: ${covered.join("")}`);
      }
    }));
  }
  assert.ok(checked > 100);
});

test("snapping never lands on an overlap when it says valid, and settles within the threshold when repeated", () => {
  const rand = prng(42);
  let snapped = 0, valid = 0;
  for (let i = 0; i < 300; i++) {
    const groups = randomLayout(rand);
    if (!noOverlap(groups)) continue;
    const mover = groups[0], others = groups.slice(1);
    const delta = [Math.round((rand() - 0.5) * 2400), Math.round((rand() - 0.5) * 1600)];
    const args = { start: G.groupRects(mover), others: rectsOf(...others), origin: [mover.monitors[0].x, mover.monitors[0].y], delta, threshold: 60 };
    const r = G.snapMove(args);
    assert.ok(Number.isInteger(r.delta[0]) && Number.isInteger(r.delta[1]), "whole pixels");
    if (r.snapX || r.snapY) snapped++;
    if (!r.valid) { assert.ok(r.overlapWith.length > 0); continue; }
    valid++;
    assert.equal(G.firstOverlap([G.moveGroup(mover, r.delta[0], r.delta[1]), ...others], [mover.fp]), null);
    // dropped there and picked up again with no motion: it may settle on a second edge that is
    // within reach, but it stays valid and never travels further than the threshold
    const again = G.snapMove({ ...args, start: G.groupRects(G.moveGroup(mover, r.delta[0], r.delta[1])), origin: r.origin, delta: [0, 0] });
    assert.equal(again.valid, true);
    assert.ok(Math.abs(again.delta[0]) <= 60 && Math.abs(again.delta[1]) <= 60, `layout ${i} moved again by ${again.delta}`);
  }
  assert.ok(snapped > 40 && valid > 100, `snapped ${snapped}, valid ${valid}`);
});
