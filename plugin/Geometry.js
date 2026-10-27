.pragma library

// The maths of the Arrange panel, with no QML and no I/O, so `node --test` runs the exact file the
// shell loads (scripts/m3/test/geometry.test.mjs) and the views only draw what comes out.
//
// Everything is in logical pixels, which is what the daemon's monitors are in: the
// *plane* is the shared layout, the *canvas* is the panel's picture of it. A view turns one into the
// other (toCanvas, toPlane). Rectangles are `{ x, y, w, h }`, half-open like `ferry_core::geom::Rect`:
// they cover [x, x+w) and [y, y+h), so two monitors that share an edge do not overlap.
//
// What mirrors the daemon (crates/ferry-core/src/geom.rs), so the seams the panel draws are the links
// ferryd will compute and the drop it refuses is the drop `layout.set` refuses:
//   overlaps / groupsOverlap / firstOverlap   Rect::overlaps and first_overlap
//   deriveLinks                               derive_links, same loops and same order
// What is the panel's own: touches (a link seen from both ends, plus the touches that are not links),
// wallSegments, snapping, nudging, the view. A *group* is one peer's monitors, moved as one rigid
// piece:
//   { fp, name, self, trusted, online, placed, monitors: [{ name, x, y, w, h, ... }] }

var SIDES = ["left", "right", "top", "bottom"];

// ------------------------------------------------------------------ sides and rectangles

function opposite(side) {
  switch (side) {
    case "left": return "right";
    case "right": return "left";
    case "top": return "bottom";
    default: return "top";
  }
}

// Left and right edges run along y; top and bottom along x.
function isVertical(side) {
  return side === "left" || side === "right";
}

function rect(x, y, w, h) {
  return { x: x, y: y, w: w, h: h };
}

function translate(r, dx, dy) {
  return { x: r.x + dx, y: r.y + dy, w: r.w, h: r.h };
}

function sameRect(a, b) {
  return !!a && !!b && a.x === b.x && a.y === b.y && a.w === b.w && a.h === b.h;
}

// The plane coordinate of the edge line of `r` on `side`.
function sideLine(r, side) {
  switch (side) {
    case "left": return r.x;
    case "right": return r.x + r.w;
    case "top": return r.y;
    default: return r.y + r.h;
  }
}

// `r`'s extent along `side`, [lo, hi).
function sideSpan(r, side) {
  return isVertical(side) ? [r.y, r.y + r.h] : [r.x, r.x + r.w];
}

function overlaps(a, b) {
  return a.w > 0 && a.h > 0 && b.w > 0 && b.h > 0
    && a.x < b.x + b.w && b.x < a.x + a.w
    && a.y < b.y + b.h && b.y < a.y + a.h;
}

// The rectangle two overlapping rectangles share, or null.
function intersection(a, b) {
  if (!overlaps(a, b)) return null;
  var x0 = Math.max(a.x, b.x), y0 = Math.max(a.y, b.y);
  var x1 = Math.min(a.x + a.w, b.x + b.w), y1 = Math.min(a.y + a.h, b.y + b.h);
  return { x: x0, y: y0, w: x1 - x0, h: y1 - y0 };
}

// The bounding box of `rects`; null when there are none.
function bounding(rects) {
  if (!rects || rects.length === 0) return null;
  var x0 = Infinity, y0 = Infinity, x1 = -Infinity, y1 = -Infinity;
  for (var i = 0; i < rects.length; i++) {
    var r = rects[i];
    x0 = Math.min(x0, r.x); y0 = Math.min(y0, r.y);
    x1 = Math.max(x1, r.x + r.w); y1 = Math.max(y1, r.y + r.h);
  }
  return { x: x0, y: y0, w: x1 - x0, h: y1 - y0 };
}

function union(a, b) {
  return bounding([a, b]);
}

function contains(outer, inner) {
  return outer.x <= inner.x && outer.y <= inner.y
    && outer.x + outer.w >= inner.x + inner.w && outer.y + outer.h >= inner.y + inner.h;
}

function clamp(v, lo, hi) {
  return Math.max(lo, Math.min(hi, v));
}

// ------------------------------------------------------------------------------- groups

function groupRects(g) {
  var out = [];
  var mons = g && g.monitors ? g.monitors : [];
  for (var i = 0; i < mons.length; i++) out.push({ x: mons[i].x, y: mons[i].y, w: mons[i].w, h: mons[i].h });
  return out;
}

function groupBounds(g) {
  return bounding(groupRects(g));
}

// The same group, `dx`, `dy` further along the plane. Nothing else about it changes.
function moveGroup(g, dx, dy) {
  var out = {};
  for (var k in g) out[k] = g[k];
  out.monitors = (g.monitors || []).map(function (m) {
    var c = {};
    for (var key in m) c[key] = m[key];
    c.x = m.x + dx;
    c.y = m.y + dy;
    return c;
  });
  if (g.rect) out.rect = translate(g.rect, dx, dy);
  return out;
}

// ------------------------------------------------------------------------------ overlap

// Two groups overlap when a monitor of one overlaps a monitor of the other, not when their bounding
// boxes do: a group is not a box (`first_overlap`).
function groupsOverlap(a, b) {
  var ra = groupRects(a), rb = groupRects(b);
  for (var i = 0; i < ra.length; i++)
    for (var j = 0; j < rb.length; j++)
      if (overlaps(ra[i], rb[j])) return true;
  return false;
}

// The rectangles where two groups cover each other, for drawing the tint.
function overlapAreas(a, b) {
  var ra = groupRects(a), rb = groupRects(b);
  var out = [];
  for (var i = 0; i < ra.length; i++)
    for (var j = 0; j < rb.length; j++) {
      var x = intersection(ra[i], rb[j]);
      if (x) out.push(x);
    }
  return out;
}

// The first pair of placed groups that overlap among the pairs with a peer in `moved`, as
// `{ moved, other }` (fingerprints), or null. Same order as the daemon: `moved` in turn, then the
// others in list order, so the message names the same two peers in the same order. Groups that are
// not placed overlap nothing.
function firstOverlap(groups, moved) {
  for (var i = 0; i < moved.length; i++) {
    var a = byFp(groups, moved[i]);
    if (!a || a.placed === false) continue;
    for (var j = 0; j < groups.length; j++) {
      var b = groups[j];
      if (b.fp === a.fp || b.placed === false) continue;
      if (groupsOverlap(a, b)) return { moved: a.fp, other: b.fp };
    }
  }
  return null;
}

// Every overlapping pair among placed groups, once each: [{ a, b, areas }].
function allOverlaps(groups) {
  var out = [];
  for (var i = 0; i < groups.length; i++) {
    if (groups[i].placed === false) continue;
    for (var j = i + 1; j < groups.length; j++) {
      if (groups[j].placed === false) continue;
      if (groupsOverlap(groups[i], groups[j])) out.push({ a: groups[i].fp, b: groups[j].fp, areas: overlapAreas(groups[i], groups[j]) });
    }
  }
  return out;
}

function byFp(groups, fp) {
  for (var i = 0; i < groups.length; i++) if (groups[i].fp === fp) return groups[i];
  return null;
}

// -------------------------------------------------------------------- links, seams, walls

function flatMonitors(groups) {
  var out = [];
  for (var i = 0; i < groups.length; i++) {
    var mons = groups[i].monitors || [];
    for (var j = 0; j < mons.length; j++)
      out.push({ fp: groups[i].fp, index: j, name: mons[j].name, rect: { x: mons[j].x, y: mons[j].y, w: mons[j].w, h: mons[j].h } });
  }
  return out;
}

// `ferry_core::geom::derive_links`: every touching segment between monitors of two different peers,
// in both directions, as { from, side, to, line, span: [lo, hi) }. The caller passes the groups that
// take part (this machine and the peers paired here, placed). Same loops, same order as the Rust:
// pairs (i, j > i) of monitors, the sides in Left, Right, Top, Bottom order, a -> b and then b -> a.
function deriveLinks(groups) {
  var mons = flatMonitors(groups);
  var links = [];
  for (var i = 0; i < mons.length; i++) {
    for (var j = i + 1; j < mons.length; j++) {
      var a = mons[i], b = mons[j];
      if (a.fp === b.fp) continue;
      for (var s = 0; s < SIDES.length; s++) {
        var side = SIDES[s];
        if (sideLine(a.rect, side) !== sideLine(b.rect, opposite(side))) continue;
        var sa = sideSpan(a.rect, side), sb = sideSpan(b.rect, side);
        var span = [Math.max(sa[0], sb[0]), Math.min(sa[1], sb[1])];
        if (span[0] >= span[1]) continue;
        var line = sideLine(a.rect, side);
        links.push({ from: { fp: a.fp, index: a.index, name: a.name }, side: side, to: { fp: b.fp, index: b.index, name: b.name }, line: line, span: span });
        links.push({ from: { fp: b.fp, index: b.index, name: b.name }, side: opposite(side), to: { fp: a.fp, index: a.index, name: a.name }, line: line, span: span });
      }
    }
  }
  return links;
}

// Every touching segment between two placed groups, once, with what it amounts to:
//   link     both machines are this one or paired here, and both are up: the pointer crosses here
//   dormant  the same, but one of them is offline: it is a wall until that peer is back
//   blocked  one of them is not paired with this machine: crossings there are off
// { a: { fp, index, name, side }, b: { ... }, line, span, vertical, kind }, `a` the first in list order.
function touches(groups) {
  var placed = groups.filter(function (g) { return g.placed !== false; });
  var out = [];
  var byFpKind = {};
  placed.forEach(function (g) { byFpKind[g.fp] = g; });
  var all = deriveLinks(placed);
  // deriveLinks pushes the two directions of a segment one after the other; the first of each pair
  // is the one from the earlier monitor.
  for (var k = 0; k + 1 < all.length; k += 2) {
    var l = all[k];
    var ga = byFpKind[l.from.fp], gb = byFpKind[l.to.fp];
    var kind = "link";
    if (ga.trusted === false || gb.trusted === false) kind = "blocked";
    else if (ga.online === false || gb.online === false) kind = "dormant";
    out.push({
      a: { fp: l.from.fp, index: l.from.index, name: l.from.name, side: l.side },
      b: { fp: l.to.fp, index: l.to.index, name: l.to.name, side: opposite(l.side) },
      line: l.line,
      span: l.span,
      vertical: isVertical(l.side),
      kind: kind
    });
  }
  return out;
}

// Subtract [lo, hi) from a list of disjoint intervals.
function cutInterval(list, lo, hi) {
  var out = [];
  for (var i = 0; i < list.length; i++) {
    var s = list[i];
    if (hi <= s[0] || lo >= s[1]) { out.push(s); continue; }
    if (lo > s[0]) out.push([s[0], lo]);
    if (hi < s[1]) out.push([hi, s[1]]);
  }
  return out;
}

// The parts of monitor edges that are walls: not shared with another monitor of the same peer (the
// pointer moves freely there) and not touching any other peer (`touches`, whatever it amounts to:
// a seam is drawn there instead). { fp, index, side, line, span: [lo, hi), vertical }.
function wallSegments(groups, touchList) {
  var out = [];
  var placed = groups.filter(function (g) { return g.placed !== false; });
  for (var gi = 0; gi < placed.length; gi++) {
    var g = placed[gi];
    var rects = groupRects(g);
    for (var mi = 0; mi < rects.length; mi++) {
      var r = rects[mi];
      for (var s = 0; s < SIDES.length; s++) {
        var side = SIDES[s];
        var parts = [sideSpan(r, side)];
        var line = sideLine(r, side);
        // inside the group
        for (var ni = 0; ni < rects.length; ni++) {
          if (ni === mi) continue;
          if (sideLine(rects[ni], opposite(side)) !== line) continue;
          var ns = sideSpan(rects[ni], opposite(side));
          parts = cutInterval(parts, ns[0], ns[1]);
        }
        // against another peer
        for (var ti = 0; ti < touchList.length; ti++) {
          var t = touchList[ti];
          var end = t.a.fp === g.fp && t.a.index === mi && t.a.side === side ? t.a
            : (t.b.fp === g.fp && t.b.index === mi && t.b.side === side ? t.b : null);
          if (end) parts = cutInterval(parts, t.span[0], t.span[1]);
        }
        for (var pi = 0; pi < parts.length; pi++)
          if (parts[pi][1] > parts[pi][0])
            out.push({ fp: g.fp, index: mi, side: side, line: line, span: parts[pi], vertical: isVertical(side) });
      }
    }
  }
  return out;
}

// ------------------------------------------------------------------------------ snapping

// The offsets (plane px, to add to the position of `moving`) that make an edge of one of its
// rectangles coincide with an edge of a rectangle in `others`, on one axis ("x" or "y"). `touch`
// puts the two rectangles side by side on that axis (a link, when the other axis overlaps), `align`
// lines the edges up. Each: { d, kind, at (the plane coordinate of the shared edge), o (index of the
// other group), m (index of the moving rectangle), q (index of its rectangle) }.
function edgeOffsets(moving, others, axis) {
  var out = [];
  var x = axis === "x";
  for (var m = 0; m < moving.length; m++) {
    var r = moving[m];
    var r0 = x ? r.x : r.y, r1 = x ? r.x + r.w : r.y + r.h;
    for (var o = 0; o < others.length; o++) {
      for (var q = 0; q < others[o].length; q++) {
        var t = others[o][q];
        var t0 = x ? t.x : t.y, t1 = x ? t.x + t.w : t.y + t.h;
        out.push({ d: t1 - r0, kind: "touch", at: t1, o: o, m: m, q: q });
        out.push({ d: t0 - r1, kind: "touch", at: t0, o: o, m: m, q: q });
        out.push({ d: t0 - r0, kind: "align", at: t0, o: o, m: m, q: q });
        out.push({ d: t1 - r1, kind: "align", at: t1, o: o, m: m, q: q });
      }
    }
  }
  return out;
}

function byDistance(a, b) {
  var d = Math.abs(a.d) - Math.abs(b.d);
  if (d !== 0) return d;
  if (a.kind !== b.kind) return a.kind === "touch" ? -1 : 1;
  return 0;
}

// The indices of the groups in `others` (each an array of rectangles) that `moving` overlaps.
function overlapping(moving, others) {
  var out = [];
  for (var o = 0; o < others.length; o++) {
    var hit = false;
    for (var i = 0; i < moving.length && !hit; i++)
      for (var j = 0; j < others[o].length && !hit; j++)
        if (overlaps(moving[i], others[o][j])) hit = true;
    if (hit) out.push(o);
  }
  return out;
}

// A whole number of pixels, and never -0.
function whole(v) {
  return Math.round(v) + 0;
}

function shiftedRects(rects, dx, dy) {
  return rects.map(function (r) { return translate(r, dx, dy); });
}

// Where a dragged group lands. `start` is its rectangles when the drag began, `origin` its layout
// origin then, `delta` how far the pointer has taken it (plane px, fractional), `others` the
// rectangles of every other placed group (one array per group). Within `threshold` of an edge or
// an alignment line it snaps; otherwise it moves by the pointer's travel, in whole pixels, and
// `free` (the modifier that turns snapping off) skips the snapping altogether. A drag that comes back
// to where it began therefore changes nothing. A snap is only taken when it does not make the
// group overlap another, and when every candidate would, the drop is reported as invalid.
//
// Returns { delta: [dx, dy] whole pixels from `start`, origin: [x, y], snapX, snapY (the candidate
// used or null), guides: [{ axis, at, from, to }], valid, overlapWith: [index into others] }.
function snapMove(args) {
  var start = args.start, others = args.others;
  var origin = args.origin || [0, 0];
  var delta = args.delta;
  var threshold = args.threshold;
  var free = args.free === true;
  var px = delta[0], py = delta[1];

  var proposed = shiftedRects(start, px, py);
  var candX = [], candY = [];
  if (!free) {
    candX = edgeOffsets(proposed, others, "x").filter(function (c) { return Math.abs(c.d) <= threshold; }).sort(byDistance);
    candY = edgeOffsets(proposed, others, "y").filter(function (c) { return Math.abs(c.d) <= threshold; }).sort(byDistance);
  }
  var gx = whole(px);
  var gy = whole(py);

  var xs = candX.map(function (c) { return { c: c, dx: whole(px + c.d) }; });
  var ys = candY.map(function (c) { return { c: c, dy: whole(py + c.d) }; });
  xs.push({ c: null, dx: gx });
  ys.push({ c: null, dy: gy });

  // Valid positions only. Snapping wins over not snapping whatever the distance (inside the
  // threshold that is the magnet), snapping both axes over one, and then the nearest to the pointer;
  // a tie keeps the first, and the candidates are sorted touch before align.
  var pick = null;
  for (var i = 0; i < xs.length; i++) {
    for (var j = 0; j < ys.length; j++) {
      var dx = xs[i].dx, dy = ys[j].dy;
      if (overlapping(shiftedRects(start, dx, dy), others).length > 0) continue;
      var entry = { dx: dx, dy: dy, cost: Math.abs(dx - px) + Math.abs(dy - py), snaps: (xs[i].c ? 1 : 0) + (ys[j].c ? 1 : 0), cx: xs[i].c, cy: ys[j].c, hit: [] };
      if (!pick || entry.snaps > pick.snaps || (entry.snaps === pick.snaps && entry.cost < pick.cost - 1e-9)) pick = entry;
    }
  }
  // Nothing valid within reach: the position the pointer asks for, flagged, with whom it overlaps.
  if (!pick) pick = { dx: gx, dy: gy, cx: null, cy: null, hit: overlapping(shiftedRects(start, gx, gy), others) };

  var final = shiftedRects(start, pick.dx, pick.dy);
  var guides = [];
  var fb = bounding(final);
  [[pick.cx, "x"], [pick.cy, "y"]].forEach(function (pair) {
    var c = pair[0];
    if (!c) return;
    var ob = bounding(others[c.o]);
    var x = pair[1] === "x";
    var lo = Math.min(x ? fb.y : fb.x, x ? ob.y : ob.x);
    var hi = Math.max(x ? fb.y + fb.h : fb.x + fb.w, x ? ob.y + ob.h : ob.x + ob.w);
    guides.push({ axis: pair[1], at: c.at, from: lo, to: hi, kind: c.kind });
  });

  return {
    delta: [pick.dx, pick.dy],
    origin: [origin[0] + pick.dx, origin[1] + pick.dy],
    snapX: pick.cx,
    snapY: pick.cy,
    guides: guides,
    valid: pick.hit.length === 0,
    overlapWith: pick.hit
  };
}

// The snap distance in plane px for `screenPx` canvas pixels at this view's scale.
function snapThreshold(scale, screenPx) {
  return scale > 0 ? (screenPx || 14) / scale : 0;
}

// ------------------------------------------------------------------------------- nudging

var NUDGE = { step: 10, far: 100 };

// One keyboard move of a group along one axis: `dir` is "left", "right", "up" or "down", `mode` is
//   step  10 px
//   far   100 px (Shift)
//   edge  to the next edge or alignment line, however far; 100 px when there is none (Ctrl)
// Step and far stop at an edge or an alignment line they would otherwise jump over, so a gap that is
// not a multiple of 10 px can still be closed with the arrow keys. An edge is only a stop when the
// group can stand there without overlapping another. A move that would overlap is refused, and says
// with whom: { delta, moved, overlapWith, snap }.
function nudgeMove(args) {
  var start = args.start, others = args.others;
  var x = args.dir === "left" || args.dir === "right";
  var sign = (args.dir === "left" || args.dir === "up") ? -1 : 1;
  var mode = args.mode || "step";
  var reach = mode === "step" ? NUDGE.step : (mode === "far" ? NUDGE.far : Infinity);

  var d = sign * (mode === "step" ? NUDGE.step : NUDGE.far);
  var snap = null;
  var best = null;
  var cands = edgeOffsets(start, others, x ? "x" : "y");
  for (var i = 0; i < cands.length; i++) {
    var c = cands[i];
    var along = c.d * sign;
    if (along <= 0 || along > reach) continue;
    if (best && along >= best.along) continue;
    if (overlapping(shiftedRects(start, x ? c.d : 0, x ? 0 : c.d), others).length > 0) continue;
    best = { along: along, c: c };
  }
  if (best) { d = sign * best.along; snap = best.c; }
  var dx = x ? d : 0, dy = x ? 0 : d;
  var hit = overlapping(shiftedRects(start, dx, dy), others);
  if (hit.length > 0) return { delta: [0, 0], moved: false, overlapWith: hit, snap: null };
  return { delta: [dx, dy], moved: true, overlapWith: [], snap: snap };
}

// ---------------------------------------------------------------------------------- view

// The part of the plane the canvas shows first: everything placed, with room around it to put a
// group on any side of the rest. `content` is the bounding box of every group, `biggest` the largest
// width and height of one group. A group is grabbed anywhere and goes where the pointer takes it, so
// room for a quarter of the widest one to the sides and a fifth of the tallest above and below is
// enough: the picture stays as large as it can, and what is dropped past the edge makes the region
// grow afterwards.
function fitRegion(content, biggest) {
  var padX = Math.max(500, Math.round(biggest.w * 0.25));
  var padY = Math.max(350, Math.round(biggest.h * 0.2));
  return { x: content.x - padX, y: content.y - padY, w: content.w + padX * 2, h: content.h + padY * 2 };
}

// The view that shows `region` whole, centred, in a canvas of `cw` x `ch`: canvas = (plane - x) *
// scale. `x` and `y` are the plane coordinates of the canvas corner.
function viewFor(region, cw, ch) {
  var scale = Math.min(cw / region.w, ch / region.h);
  if (!isFinite(scale) || scale <= 0) scale = 1;
  var visibleW = cw / scale, visibleH = ch / scale;
  return {
    scale: scale,
    x: region.x - (visibleW - region.w) / 2,
    y: region.y - (visibleH - region.h) / 2,
    w: visibleW,
    h: visibleH
  };
}

// A region only ever grows while the panel is open, so a drop never makes the whole picture jump:
// it is the union of what it was and what is needed now. `Fit` starts over.
function growRegion(prev, needed) {
  if (!prev) return needed;
  return contains(prev, needed) ? prev : union(prev, needed);
}

function toCanvas(view, px, py) {
  return { x: (px - view.x) * view.scale, y: (py - view.y) * view.scale };
}

function toPlane(view, cx, cy) {
  return { x: cx / view.scale + view.x, y: cy / view.scale + view.y };
}

// Zoom by `factor` about the canvas point (cx, cy), which stays where it is. `fitScale` is the scale
// of the whole-region view: the zoom cannot go below it or above `maxFactor` times it.
function zoomAt(view, factor, cx, cy, fitScale, maxFactor) {
  var scale = clamp(view.scale * factor, fitScale, fitScale * (maxFactor || 8));
  var p = toPlane(view, cx, cy);
  return { scale: scale, x: p.x - cx / scale, y: p.y - cy / scale, w: view.w * view.scale / scale, h: view.h * view.scale / scale };
}

function panBy(view, dxCanvas, dyCanvas) {
  return { scale: view.scale, x: view.x - dxCanvas / view.scale, y: view.y - dyCanvas / view.scale, w: view.w, h: view.h };
}

// Keep a dragged rectangle's centre inside what the canvas shows, so a group cannot be lost off its
// edge: returns the delta to add.
function keepVisible(bounds, view, delta) {
  var cx = bounds.x + bounds.w / 2 + delta[0], cy = bounds.y + bounds.h / 2 + delta[1];
  var nx = clamp(cx, view.x, view.x + view.w), ny = clamp(cy, view.y, view.y + view.h);
  return [delta[0] + (nx - cx), delta[1] + (ny - cy)];
}

// Whether anything lies in the strip `w` x `h` (plane px) just above a group's box: the room its name
// needs when it is written above it. `others` are the rectangles of the other groups' monitors.
function stripBlocked(box, others, w, h) {
  var strip = { x: box.x, y: box.y - h, w: w, h: h };
  for (var i = 0; i < others.length; i++) if (overlaps(strip, others[i])) return true;
  return false;
}

// A rectangle on the canvas grown about its centre to at least `minW` x `minH`, so a small group is
// still easy to hit with a pointer. Returns the rectangle and where the original sits inside it.
function hitRect(r, minW, minH) {
  var w = Math.max(r.w, minW), h = Math.max(r.h, minH);
  var x = r.x - (w - r.w) / 2, y = r.y - (h - r.h) / 2;
  return { x: x, y: y, w: w, h: h, innerX: r.x - x, innerY: r.y - y };
}

// How much a monitor tile of w x h canvas pixels can say: 3 name, size and description, 2 name and
// size, 1 the name, 0 nothing.
function labelTier(w, h) {
  if (w >= 150 && h >= 66) return 3;
  if (w >= 92 && h >= 44) return 2;
  if (w >= 40 && h >= 22) return 1;
  return 0;
}

// ---------------------------------------------------------------------- unplaced groups

// Where a group that has no place yet is shown until the user drags it: right of everything placed,
// its top level with the first placed group, and moved down clear of anything it would
// cover. `placed` are the placed groups, `local` the unplaced group's rectangles at their own
// position (local coordinates, shift included). Returns the plane offset to add to `local`.
function provisionalOffset(placed, local) {
  var lb = bounding(local);
  if (!lb) return [0, 0];
  var pb = bounding([].concat.apply([], placed.map(groupRects)));
  if (!pb) return [-lb.x, -lb.y];
  var anchor = placed[0] ? groupBounds(placed[0]) : pb;
  var others = placed.map(groupRects);
  var dx = pb.x + pb.w - lb.x, dy = anchor.y - lb.y;
  for (var guard = 0; guard < 8; guard++) {
    var hit = overlapping(shiftedRects(local, dx, dy), others);
    if (hit.length === 0) break;
    var below = 0;
    hit.forEach(function (o) { below = Math.max(below, bounding(others[o]).y + bounding(others[o]).h); });
    dy = below - lb.y;
  }
  return [dx, dy];
}
