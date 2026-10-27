.pragma library
.import "Geometry.js" as Geometry
.import "Strings.js" as Strings

// What the Arrange panel decides, with no QML and no I/O (node-tested, scripts/m3/test/arrange.test.mjs):
// the groups a `layout` snapshot describes, the local draft that edits them, the conflict banner of
// FR-UI-17, the words the status box says, and the `layout.set` call Apply makes. The maths of
// plane, snapping and links is in Geometry.js; the views only bind to what comes out.
//
// A *group* is one peer's monitors as one rigid piece:
//   { fp, name, self, trusted, online, placed, origin: [x, y] | null, shift: [dx, dy],
//     monitors: [{ name, description, x, y, w, h, scale, transform }], rect }
// with plane coordinates when `placed`. The *draft* holds origins (the revisioned value, not the plane
// position, so a peer's anchor shift is never sent back) for the groups the user moved, and the
// revision it was started on:
//   { base: { counter, peer } | null, origins: { fp: [x, y] }, undo: [...] }

var UNDO_DEPTH = 50;

function list(v) {
  return Array.isArray(v) ? v : [];
}

function isPair(v) {
  return Array.isArray(v) && v.length === 2 && isFinite(Number(v[0])) && isFinite(Number(v[1]));
}

// An origin as the daemon writes it ([x, y], a tuple) or as an object, as [x, y]; null when it has none.
function originOf(v) {
  if (isPair(v)) return [Math.round(Number(v[0])), Math.round(Number(v[1]))];
  if (v && typeof v === "object" && isFinite(Number(v.x)) && isFinite(Number(v.y))) return [Math.round(Number(v.x)), Math.round(Number(v.y))];
  return null;
}

function shortFp(fp) {
  return String(fp || "").slice(0, 8);
}

// ---------------------------------------------------------------------------- the groups

// Groups from a `layout` snapshot, in the daemon's order (this machine, the peers paired here, then
// the peers that are only placed). Peers that have not reported their monitors cannot be drawn:
// they are listed apart, as `waiting`. Fields an older ferryd does not send are taken as true
// (`trusted`, `online`) or absent (`self`, `bounds`, `shift`, `description`).
function groupsOf(layout, meFp) {
  var groups = [], waiting = [];
  list(layout && layout.peers).forEach(function (p) {
    if (!p || !p.fingerprint) return;
    var self = p.self === true || (!!meFp && p.fingerprint === meFp);
    var base = {
      fp: String(p.fingerprint),
      name: p.name ? String(p.name) : shortFp(p.fingerprint),
      self: self,
      trusted: self ? true : p.trusted !== false,
      online: self ? true : p.online !== false
    };
    var monitors = list(p.monitors).map(function (m) {
      return {
        name: String(m.name || ""),
        description: String(m.description || ""),
        x: Math.round(Number(m.x) || 0), y: Math.round(Number(m.y) || 0),
        w: Math.round(Number(m.w) || 0), h: Math.round(Number(m.h) || 0),
        scale: Number(m.scale) > 0 ? Number(m.scale) : 1,
        transform: Number(m.transform) || 0
      };
    }).filter(function (m) { return m.w > 0 && m.h > 0; });
    if (monitors.length === 0) { waiting.push(base); return; }

    var origin = originOf(p.origin);
    var shift = isPair(p.shift) ? [Math.round(Number(p.shift[0])), Math.round(Number(p.shift[1]))] : [0, 0];
    var g = base;
    g.placed = origin !== null;
    g.origin = origin;
    g.shift = shift;
    g.monitors = monitors;
    var b = p.bounds && isFinite(Number(p.bounds.w)) ? { x: Number(p.bounds.x), y: Number(p.bounds.y), w: Number(p.bounds.w), h: Number(p.bounds.h) } : null;
    g.rect = b || Geometry.groupBounds(g);
    groups.push(g);
  });
  return { groups: groups, waiting: waiting };
}

function copy(o) {
  var out = {};
  for (var k in o) out[k] = o[k];
  return out;
}

// The groups as the panel shows them: the daemon's snapshot with the draft's origins applied, and
// the group being dragged (`drag`: { fp, origin }) where the pointer has it. A group the daemon has
// not placed is shown, until the user drags it, right of everything placed (`provisional`); it takes
// part in no link and no overlap until then. Each result also carries `baseOrigin` (the daemon's)
// and `moved`.
function effectiveGroups(groups, draft, drag) {
  var origins = (draft && draft.origins) || {};
  var out = [], pending = [];
  groups.forEach(function (g) {
    var o = drag && drag.fp === g.fp ? drag.origin : origins[g.fp];
    if (g.placed) {
      var m = o ? Geometry.moveGroup(g, o[0] - g.origin[0], o[1] - g.origin[1]) : copy(g);
      m.origin = o ? [o[0], o[1]] : [g.origin[0], g.origin[1]];
      m.baseOrigin = g.origin;
      m.moved = !!o && (o[0] !== g.origin[0] || o[1] !== g.origin[1]);
      m.provisional = false;
      out.push(m);
    } else if (o) {
      // monitors are local until placed: the plane position is local + shift + origin
      var p = Geometry.moveGroup(g, o[0] + g.shift[0], o[1] + g.shift[1]);
      p.placed = true;
      p.origin = [o[0], o[1]];
      p.baseOrigin = null;
      p.moved = true;
      p.provisional = false;
      out.push(p);
    } else {
      pending.push(g);
    }
  });
  pending.forEach(function (g) {
    var local = g.monitors.map(function (m) { return { x: m.x + g.shift[0], y: m.y + g.shift[1], w: m.w, h: m.h }; });
    var placed = out.filter(function (x) { return x.placed; });
    var off = Geometry.provisionalOffset(placed, local);
    var p = Geometry.moveGroup(g, g.shift[0] + off[0], g.shift[1] + off[1]);
    p.placed = false;
    p.origin = [off[0], off[1]];
    p.baseOrigin = null;
    p.moved = false;
    p.provisional = true;
    out.push(p);
  });
  // keep the daemon's order, whatever the order of placement above
  var order = {};
  groups.forEach(function (g, i) { order[g.fp] = i; });
  out.sort(function (a, b) { return order[a.fp] - order[b.fp]; });
  return out;
}

function groupBy(groups, fp) {
  return Geometry.byFp(groups, fp);
}

// Everything the picture draws besides the tiles: the seams (a link seen from both ends, with what it
// amounts to), the walls, the overlaps, who touches nobody, and who is not paired here.
function analyse(groups) {
  var touches = Geometry.touches(groups);
  var walls = Geometry.wallSegments(groups, touches);
  var overlaps = Geometry.allOverlaps(groups);
  var linked = {};
  touches.forEach(function (t) {
    if (t.kind === "blocked") return;
    linked[t.a.fp] = true;
    linked[t.b.fp] = true;
  });
  var trustedPlaced = groups.filter(function (g) { return g.placed && g.trusted; });
  var lonely = trustedPlaced.length >= 2 ? trustedPlaced.filter(function (g) { return !linked[g.fp]; }) : [];
  return {
    touches: touches,
    walls: walls,
    overlaps: overlaps,
    lonely: lonely.map(function (g) { return g.fp; }),
    blocked: touches.filter(function (t) { return t.kind === "blocked"; }),
    unplaced: groups.filter(function (g) { return g.provisional; }).map(function (g) { return g.fp; })
  };
}

function nameOf(groups, fp) {
  var g = groupBy(groups, fp);
  return g ? g.name : shortFp(fp);
}

var SIDE_WORD = { left: "left", right: "right", top: "top", bottom: "bottom" };

// "desk ↔ lap" and, dimmed after it, which monitors and edges touch, and where along the edge and how long.
function linkRows(analysis, groups) {
  var rows = [];
  analysis.touches.forEach(function (t) {
    if (t.kind === "blocked") return;
    var len = t.span[1] - t.span[0];
    var axis = t.vertical ? "y" : "x";
    rows.push({
      kind: t.kind,
      primary: nameOf(groups, t.a.fp) + " ↔ " + nameOf(groups, t.b.fp),
      secondary: t.a.name + " " + SIDE_WORD[t.a.side] + " ↔ " + t.b.name + " " + SIDE_WORD[t.b.side] + " · "
        + Strings.t("arrange.link.extent", [axis, t.span[0], t.span[1], len]),
      a: t.a.fp,
      b: t.b.fp
    });
  });
  return rows;
}

// ---------------------------------------------------------------------------- the draft

function emptyDraft() {
  return { base: null, origins: {}, undo: [] };
}

function revKey(rev) {
  return rev ? String(rev.counter) + ":" + String(rev.peer) : "";
}

function snapshot(draft) {
  var o = {};
  for (var fp in draft.origins) o[fp] = draft.origins[fp].slice();
  return { base: draft.base, origins: o };
}

// A draft entry that equals what the daemon has is not an edit.
function pruned(origins, groups) {
  var out = {};
  groups.forEach(function (g) {
    var o = origins[g.fp];
    if (!o) return;
    if (g.placed && g.origin[0] === o[0] && g.origin[1] === o[1]) return;
    out[g.fp] = o;
  });
  return out;
}

function isDirty(draft, groups) {
  if (!draft) return false;
  return Object.keys(pruned(draft.origins, groups)).length > 0;
}

// The same draft with `fp` at `origin`. The first edit fixes the revision the draft is based on (`rev`,
// the layout's at that moment), the previous state goes on the undo stack, and putting every group
// back where the daemon has it leaves a clean draft. `groups` are the daemon's groups.
function withOrigin(draft, groups, rev, fp, origin) {
  var current = draft.origins[fp] || null;
  var g = groupBy(groups, fp);
  if (!g) return draft;
  var was = current || (g.placed ? g.origin : null);
  if (was && was[0] === origin[0] && was[1] === origin[1]) return draft;
  var origins = snapshot(draft).origins;
  origins[fp] = [origin[0], origin[1]];
  origins = pruned(origins, groups);
  var undo = draft.undo.concat([snapshot(draft)]).slice(-UNDO_DEPTH);
  var clean = Object.keys(origins).length === 0;
  return { base: clean ? null : (draft.base || rev || null), origins: origins, undo: undo };
}

function canUndo(draft) {
  return !!draft && draft.undo.length > 0;
}

function undo(draft) {
  if (!canUndo(draft)) return draft;
  var prev = draft.undo[draft.undo.length - 1];
  return { base: prev.base, origins: prev.origins, undo: draft.undo.slice(0, -1) };
}

// Reset to the live snapshot. It can be undone.
function reset(draft, groups) {
  if (!isDirty(draft, groups)) return draft;
  return { base: null, origins: {}, undo: draft.undo.concat([snapshot(draft)]).slice(-UNDO_DEPTH) };
}

// Keep the draft and base it on a newer revision (Keep mine).
function rebase(draft, rev) {
  return { base: rev, origins: snapshot(draft).origins, undo: draft.undo };
}

// The `layout.set` params for Apply: the groups that moved, by fingerprint (a name can repeat, a
// fingerprint cannot), in the daemon's order, and the revision the draft started on. Nothing else
// is named, so the other peers keep their origins (contract 2.5).
function payload(draft, groups) {
  var moved = pruned(draft.origins, groups);
  var origins = [];
  groups.forEach(function (g) {
    if (moved[g.fp]) origins.push({ peer: g.fp, x: moved[g.fp][0], y: moved[g.fp][1] });
  });
  var out = { origins: origins };
  if (draft.base) out.base_rev = { counter: draft.base.counter, peer: draft.base.peer };
  return out;
}

// The draft's origins for groups the daemon no longer lists are dropped (an unpaired peer).
function forgetMissing(draft, groups) {
  var known = {};
  groups.forEach(function (g) { known[g.fp] = true; });
  var origins = {}, changed = false;
  for (var fp in draft.origins) {
    if (known[fp]) origins[fp] = draft.origins[fp];
    else changed = true;
  }
  if (!changed) return draft;
  return { base: Object.keys(origins).length ? draft.base : null, origins: origins, undo: draft.undo };
}

// ----------------------------------------------------------------------------- conflict

// A newer revision arrived over a dirty draft. `layout` is the live snapshot. null when
// there is no draft, or the revision is still the one the draft started on.
function conflictOf(draft, layout, groups, meFp) {
  if (!isDirty(draft, groups) || !draft.base || !layout || !layout.revision) return null;
  if (revKey(layout.revision) === revKey(draft.base)) return null;
  var by = layout.revision.peer;
  return {
    counter: layout.revision.counter,
    byFp: by,
    by: nameOf(groups, by),
    mine: !!meFp && by === meFp
  };
}

// ---------------------------------------------------------------------- the words

function joinNames(names) {
  if (names.length <= 1) return names.join("");
  if (names.length === 2) return names[0] + " and " + names[1];
  return names.slice(0, -1).join(", ") + " and " + names[names.length - 1];
}

function bannerTitle(conflict) {
  return Strings.t(conflict.mine ? "arrange.conflict.title.self" : "arrange.conflict.title", [conflict.by, conflict.counter]);
}

// What the two ways out do, in the words of the banner: the safe one first, the one that overwrites second.
function bannerBody(conflict) {
  return Strings.t(conflict.mine ? "arrange.conflict.body.self" : "arrange.conflict.body", [conflict.by, conflict.counter]);
}

// What an `overlap` answer or a refused drop says, naming both. `moved` and `other` are names.
function overlapText(moved, other) {
  return Strings.t("arrange.status.overlap", [moved, other]);
}

// A daemon error as one line, and its hint.
function errorText(error) {
  if (!error) return "";
  var msg = String(error.message || error.code || "");
  if (error.code === "unknown_method") return Strings.t("arrange.error.unknown_method");
  if (error.code === "not_connected") return Strings.t("arrange.error.not_connected");
  if (error.code === "timeout") return Strings.t("arrange.error.timeout");
  return error.hint ? msg + ". " + error.hint : msg;
}

// The answer to a successful Apply: which revision, and who has it.
function appliedText(result, groups) {
  var rev = result && result.revision ? result.revision.counter : "";
  var others = groups.filter(function (g) { return !g.self && g.trusted; });
  var up = others.filter(function (g) { return g.online; }).map(function (g) { return g.name; });
  var down = others.filter(function (g) { return !g.online; }).map(function (g) { return g.name; });
  var text = up.length
    ? Strings.t("arrange.status.applied", [rev, joinNames(up)])
    : Strings.t("arrange.status.applied.alone", [rev]);
  if (down.length) text += " " + Strings.t(down.length === 1 ? "arrange.status.applied.offline" : "arrange.status.applied.offline.many", [joinNames(down)]);
  return text;
}

// The status box (the mockup's "Status"): one sentence, how serious it is, and a glyph key, so the
// problem is never colour alone. First match wins:
//   conflict, a refused Apply, an overlap, a drag that would overlap, "applied", a machine nothing
//   touches, a machine that is not paired here, a machine not placed, a draft, and then the quiet one.
// ctx: { groups, analysis, drag, dirty, conflict, error, flash, rev, canSet, meName, waiting (the peers with no monitors) }
function statusOf(ctx) {
  var groups = ctx.groups;
  if (ctx.canSet === false) return { tone: "warn", glyph: "alert", text: Strings.t("arrange.status.legacy") };
  if (ctx.conflict) {
    // the banner above says what happened and what to choose; this box says only where the draft stands
    return { tone: "normal", glyph: "info", text: Strings.t(ctx.conflict.mine ? "arrange.status.conflict.self" : "arrange.status.conflict", [ctx.conflict.by, ctx.conflict.counter]) };
  }
  if (ctx.error) return { tone: "err", glyph: "alert", text: errorText(ctx.error) };
  if (ctx.drag && ctx.drag.valid === false) {
    var dragged = nameOf(groups, ctx.drag.fp);
    var withWhom = ctx.drag.overlapWith.length ? joinNames(ctx.drag.overlapWith.map(function (fp) { return nameOf(groups, fp); })) : "";
    return { tone: "err", glyph: "alert", text: Strings.t("arrange.status.overlap.drag", [dragged, withWhom]) };
  }
  if (ctx.analysis.overlaps.length) {
    var pair = ctx.analysis.overlaps[0];
    return { tone: "err", glyph: "alert", text: overlapText(nameOf(groups, pair.a), nameOf(groups, pair.b)) };
  }
  if (ctx.flash) return { tone: ctx.flashTone || "ok", glyph: ctx.flashTone === "warn" ? "alert" : "check", text: ctx.flash };
  function many(key, fps) {
    return Strings.t(key + (fps.length > 1 ? ".many" : ""), [joinNames(fps.map(function (fp) { return nameOf(groups, fp); })), ctx.meName || Strings.t("me.fallback")]);
  }
  if (ctx.analysis.unplaced.length) return { tone: "warn", glyph: "alert", text: many("arrange.status.unplaced", ctx.analysis.unplaced) };
  if (ctx.analysis.lonely.length) return { tone: "warn", glyph: "alert", text: many("arrange.status.lonely", ctx.analysis.lonely) };
  var untrusted = groups.filter(function (g) { return g.placed && !g.trusted; }).map(function (g) { return g.fp; });
  if (untrusted.length) return { tone: "warn", glyph: "alert", text: many("arrange.status.untrusted", untrusted) };
  var others = groups.filter(function (g) { return !g.self && g.trusted; });
  // a paired peer that has not told ferryd its monitors yet cannot be drawn: it would otherwise be missing without a word
  if (ctx.waiting && ctx.waiting.length) {
    var names = ctx.waiting.map(function (g) { return g.name; });
    return { tone: "warn", glyph: "alert", text: Strings.t(names.length > 1 ? "arrange.waiting.many" : "arrange.waiting", [joinNames(names)]) };
  }
  if (ctx.dirty) return { tone: "normal", glyph: "info", text: Strings.t("arrange.status.dirty", [ctx.rev]) };
  var live = groups.filter(function (g) { return g.trusted && g.online; }).map(function (g) { return g.name; });
  var offline = others.filter(function (g) { return !g.online; }).map(function (g) { return g.name; });
  var text = Strings.t("arrange.status.live", [ctx.rev, joinNames(live)]);
  if (offline.length) text += " " + Strings.t(offline.length === 1 ? "arrange.status.live.offline" : "arrange.status.live.offline.many", [joinNames(offline)]);
  return { tone: "normal", glyph: "info", text: text };
}

// What a screen reader says for a group: who, what monitors, where, and how to move it.
function groupName(g, meName) {
  var mons = g.monitors.map(function (m) { return m.name; });
  var tags = [];
  if (g.self) tags.push(Strings.t("arrange.tag.self"));
  if (!g.trusted) tags.push(Strings.t("arrange.tag.untrusted", [meName || Strings.t("me.fallback")]));
  else if (!g.online) tags.push(Strings.t("arrange.tag.offline"));
  if (g.provisional) tags.push(Strings.t("arrange.tag.unplaced"));
  var rect = g.rect || Geometry.groupBounds(g);
  return g.name + (tags.length ? " (" + tags.join(", ") + ")" : "") + ", " + mons.join(" and ")
    + ", " + Strings.t("arrange.aria.at", [Math.round(rect.x), Math.round(rect.y)]);
}

// A daemon that lists its methods is believed: `layout.set` has to be there. One that does not (from
// before M3) is tried anyway, and its `unknown_method` says what to do (errorText).
function canSet(hello) {
  var methods = hello && Array.isArray(hello.methods) ? hello.methods : null;
  return methods ? methods.indexOf("layout.set") !== -1 : true;
}

// What a `stale_layout` answer says: "the layout changed (rev 15 by lap)", as { counter, by }. Null when
// the text is not that, and the banner then says it without the names.
function staleInfo(error) {
  var m = error && error.code === "stale_layout" ? /\(rev (\d+) by ([^)]+)\)/.exec(String(error.message || "")) : null;
  return m ? { counter: Number(m[1]), by: m[2] } : null;
}

// Whether the Apply button may be pressed.
function canApply(args) {
  return args.connected === true && args.dirty === true && args.canSet !== false
    && !args.overlap && !args.conflict && !args.applying && !args.dragging;
}
