.pragma library
.import "Strings.js" as Strings

// What ferryd said, turned into what the UI shows. No QML in this file and no I/O, so
// `node --test` runs the exact file the shell loads (scripts/m3/test), and the thin QML views
// only bind to what comes out. If a rule is worth getting right, it lives here.
//
// ferryd computes the derived UI state itself since M3 (`status.summary`, `status.attention`,
// contract section 2.4), and this file believes it when it is there. When it is not (a daemon
// from before M3, which has no `subscribe` either), summaryOf() and attentionOf() derive the
// same answer from the facts every daemon sends, with the same priority.

var STATES = ["off", "idle", "ready", "away", "controlled", "locked", "attention"];
var DEFAULT_CHORD = "Super+Ctrl+Escape";

// What every ferryd answers, from M1 on. A `hello` without `methods` is one of those.
var LEGACY_METHODS = [
  "status", "release", "id", "trust", "layout.get", "layout.place", "lock",
  "pair.accept", "pair.start", "pair.answer", "pair.cancel", "peers", "unpair", "doctor"
];

// ----------------------------------------------------------------- hello, capabilities

function methodsOf(hello) {
  return hello && Array.isArray(hello.methods) ? hello.methods : null;
}

function hasMethod(hello, name) {
  var methods = methodsOf(hello);
  if (methods) return methods.indexOf(name) !== -1;
  return hello ? LEGACY_METHODS.indexOf(name) !== -1 : false;
}

// A daemon that has `subscribe` pushes snapshots; one that has not is polled.
function supportsEvents(hello) {
  return hasMethod(hello, "subscribe");
}

// ------------------------------------------------------------------------ small helpers

// True when two lists of keys (strings) are the same, in the same order. A Repeater rebuilds every
// delegate when its model is a new array, even a new array with the same content, and ferryd sends the
// same facts again every 2 s: the views keep a list of keys that changes only when this says it did, and
// so keep the focus and the half-typed name of the row they hold.
function sameList(a, b) {
  if (!Array.isArray(a) || !Array.isArray(b) || a.length !== b.length) return false;
  for (var i = 0; i < a.length; i++) if (a[i] !== b[i]) return false;
  return true;
}

function isObject(v) {
  return v !== null && typeof v === "object" && !Array.isArray(v);
}

function list(v) {
  if (Array.isArray(v)) return v;
  // A list inside an object that went through a model (a Repeater's modelData) comes back from QML
  // as a sequence wrapper: array-like, and Array.isArray says false. The settings view reads
  // `choices` that way.
  if (v !== null && typeof v === "object" && typeof v.length === "number") return Array.prototype.slice.call(v);
  return [];
}

function meName(status, hello) {
  if (status && status.me && status.me.name) return String(status.me.name);
  if (hello && hello.peer) return String(hello.peer);
  return "";
}

function chordOf(status) {
  return status && status.escape_chord ? String(status.escape_chord) : DEFAULT_CHORD;
}

// "Super+Ctrl+Escape" is what the config says; the tooltip says "Super+Ctrl+Esc".
function chordLabel(chord) {
  return String(chord || DEFAULT_CHORD).replace(/\bEscape\b/g, "Esc");
}

// The keys of a chord one by one, for the keycaps: "Super+Ctrl+Escape" -> ["Super", "Ctrl", "Esc"].
function chordKeys(chord) {
  return chordLabel(chord).split("+").map(function (k) { return k.trim(); }).filter(function (k) { return k !== ""; });
}

// 850 -> "0.9 ms", 12000 -> "12 ms", missing -> "—".
function rttText(us) {
  var n = Number(us);
  if (us === null || us === undefined || !isFinite(n) || n < 0) return "—";
  // Round on integers: 0.85.toFixed(1) is "0.8", and 850 us is 0.9 ms to anyone reading it.
  var tenths = Math.round(n / 100);
  return (tenths < 100 ? (tenths / 10).toFixed(1) : String(Math.round(n / 1000))) + " ms";
}

var MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

// A time as a short age: "just now", "5 min ago", "3 h ago", "18 Oct". `when` is an RFC 3339 string
// (ferryd's `last_seen`) or epoch milliseconds (a notice the plugin stamped when it arrived);
// `now` is a Date or milliseconds, injected so a test needs no clock. Anything unreadable answers "".
function agoText(when, now) {
  if (when === null || when === undefined || when === "") return "";
  var then = typeof when === "number" ? when : Date.parse(String(when));
  if (!isFinite(then)) return "";
  var nowMs = now instanceof Date ? now.getTime() : (typeof now === "number" ? now : Date.now());
  var s = Math.max(0, Math.round((nowMs - then) / 1000));
  if (s < 60) return "just now";
  if (s < 3600) return Math.floor(s / 60) + " min ago";
  if (s < 86400) return Math.floor(s / 3600) + " h ago";
  var d = new Date(then);
  return d.getUTCDate() + " " + MONTHS[d.getUTCMonth()];
}

function joinNames(names) {
  if (names.length <= 1) return names.join("");
  if (names.length === 2) return names[0] + " and " + names[1];
  return names.slice(0, -1).join(", ") + " and " + names[names.length - 1];
}

// ----------------------------------------------------------------------- summary, attention

function connectedPeers(status) {
  return list(status && status.peers).filter(function (p) { return p && p.state === "connected"; });
}

// Problems the user must act on. ferryd's own list wins; this builds the same kind of list from
// the raw facts for a daemon that does not send one. Codes match contract section 2.4.
function attentionOf(status, layout) {
  if (!status) return [];
  if (Array.isArray(status.attention)) return status.attention;

  var out = [];
  list(status.peers).forEach(function (p) {
    if (!p || p.state !== "refused") return;
    var detail = p.detail ? String(p.detail) : "it was refused";
    var version = /version/i.test(detail);
    out.push({
      code: "peer_refused",
      message: p.name + ": " + detail,
      hint: version ? "Update ferry on " + p.name : "ferry unpair " + p.name + ", then pair again",
      peer: p.name
    });
  });

  var capture = status.capture || {};
  if (capture.gate === "barriers_failed") {
    out.push({
      code: "barriers_failed",
      message: "The screen edges could not be set up",
      hint: "Run ferry doctor"
    });
  }
  if (capture.notice) {
    out.push({
      code: "capture_denied",
      message: String(capture.notice),
      hint: "Run ferry doctor"
    });
  }

  var meFp = status.me ? status.me.fingerprint : "";
  list(layout && layout.peers).forEach(function (p) {
    if (!p || p.fingerprint === meFp || p.self === true) return;
    if (p.origin === null || p.origin === undefined) {
      out.push({
        code: "layout_unplaced",
        message: p.name + " is not placed in the layout",
        hint: "Open Arrange and drop " + p.name + " next to this machine",
        peer: p.name
      });
    }
  });
  return out;
}

// The state without the attention override: what the machine is doing underneath.
function baseState(status) {
  if (!status) return { state: "idle" };
  var active = status.active_peer && status.active_peer.name ? String(status.active_peer.name) : "";
  if (status.role === "source" && active) return { state: "away", peer: active };
  if (status.role === "target" && active) return { state: "controlled", peer: active };
  if (status.locked === true) return { state: "locked" };
  if (connectedPeers(status).length > 0) return { state: "ready" };
  return { state: "idle" };
}

// `off` is not a ferryd state: the caller says it when there is no connection.
function summaryOf(status, layout) {
  if (!status) return { state: "idle" };
  var given = status.summary;
  if (isObject(given) && STATES.indexOf(given.state) > 0) {
    return { state: given.state, peer: given.peer ? String(given.peer) : "" };
  }
  if (attentionOf(status, layout).length > 0) return { state: "attention", peer: "" };
  return baseState(status);
}

// -------------------------------------------------------------------------------- peer rows

function monitorsOf(layout, fingerprint) {
  var peers = list(layout && layout.peers);
  for (var i = 0; i < peers.length; i++) {
    if (peers[i] && peers[i].fingerprint === fingerprint) return list(peers[i].monitors);
  }
  return [];
}

function monitorSummary(monitors, fallback) {
  if (monitors.length === 0) return fallback || "";
  if (monitors.length > 1) return Strings.t("peer.monitors.many", [monitors.length]);
  var m = monitors[0];
  var text = m.name + " · " + m.w + "×" + m.h;
  // 1.6 -> "1.6", 2 -> "2", 1.25 -> "1.25"; a scale of 1 is not worth a word.
  var scale = Math.round(Number(m.scale) * 100) / 100;
  if (isFinite(scale) && scale > 0 && scale !== 1) text += " @ ×" + scale;
  return text;
}

// Why a link was refused, from the words ferryd puts in the link's `detail` or the attention `message`:
// "key" (the peer's key is not the one pinned), "untrusted" (the peer no longer trusts this machine),
// "version" (another protocol version) or "other". The words are ferryd's and not a contract, so an
// unknown one is "other", and the row then shows ferryd's own text.
function refusalKind(text) {
  var t = String(text || "");
  if (/version/i.test(t)) return "version";
  if (/does not trust|refused this machine|not trusted by/i.test(t)) return "untrusted";
  if (/fingerprint|certificate|unknown peer|\bkey\b/i.test(t)) return "key";
  return "other";
}

// The short reason under a refused peer's name, or ferryd's own text when it is not one we know.
function refusalText(text) {
  var kind = refusalKind(text);
  return kind === "other" ? String(text || "") : Strings.t("peer.refused." + kind);
}

function peerKind(monitors) {
  for (var i = 0; i < monitors.length; i++) {
    if (/^(eDP|LVDS)/i.test(String(monitors[i].name || ""))) return "laptop";
  }
  return "desktop";
}

var TONE_ORDER = { active: 0, ok: 1, wait: 2, warn: 3, lost: 4 };

// One row per trusted peer. `status.peers` carries the live link (state, rtt); `peers` adds how
// it was paired and when it was last seen; `layout` adds the monitors. Any of them may be missing.
function peerRows(status, peersResult, layout, summary, now) {
  var live = list(status && status.peers);
  var pinned = list(peersResult && peersResult.peers);
  var byFp = {};
  var order = [];
  function row(fp) {
    if (!byFp[fp]) { byFp[fp] = { fingerprint: fp }; order.push(fp); }
    return byFp[fp];
  }
  pinned.forEach(function (p) {
    if (!p) return;
    var r = row(p.fingerprint);
    r.name = p.name;
    r.addr = p.addr || "";
    r.lastSeen = p.last_seen || "";
    r.paired = p.paired || "";
    r.state = p.state || "offline";
    r.locked = p.locked === true;
  });
  live.forEach(function (p) {
    if (!p) return;
    var r = row(p.fingerprint);
    r.name = p.name;
    r.addr = p.addr || r.addr || "";
    r.lastSeen = p.last_seen || r.lastSeen || "";
    r.state = p.state || r.state || "offline";
    r.rttUs = p.rtt_us === undefined ? null : p.rtt_us;
    r.suspect = p.suspect === true;
    r.locked = p.locked === true || r.locked === true;
    r.detail = p.detail || "";
  });

  var sum = summary || { state: "idle" };
  var rows = order.map(function (fp) {
    var r = byFp[fp];
    var monitors = monitorsOf(layout, fp);
    var out = {
      name: r.name || fp.slice(0, 8),
      fingerprint: fp,
      shortFp: shortFingerprint(fp),
      kind: peerKind(monitors),
      sub: monitorSummary(monitors, r.addr),
      state: r.state || "offline",
      online: (r.state || "offline") === "connected",
      rttUs: r.rttUs === undefined ? null : r.rttUs,
      lat: "",
      st: "",
      tone: "ok",
      // "away": this machine drives it; "controlled": it drives this machine; "": neither
      arrow: "",
      locked: r.locked === true,
      suspect: r.suspect === true,
      paired: r.paired || "",
      addr: r.addr || "",
      seen: agoText(r.lastSeen, now),
      detail: r.detail || ""
    };

    // Top right: the round trip, or the one word that says why there is none. Under it: what the
    // peer is doing, or since when it is gone.
    var drives = (sum.state === "away" || sum.state === "controlled") && sum.peer === out.name;
    if (drives && out.state === "connected") out.arrow = sum.state;
    if (out.state === "connected") {
      out.lat = rttText(out.rttUs);
      out.tone = "ok";
      out.st = Strings.t("peer.connected");
      if (out.suspect) { out.tone = "warn"; out.st = Strings.t("peer.suspect"); }
      if (drives) {
        out.tone = "active";
        out.st = Strings.t(sum.state === "away" ? "peer.active.away" : "peer.active.controlled");
      }
    } else if (out.state === "connecting") {
      out.tone = "wait";
      out.lat = "—";
      out.st = Strings.t("peer.connecting");
    } else if (out.state === "refused") {
      out.tone = "lost";
      out.lat = Strings.t("peer.refused");
      out.st = refusalText(out.detail);
    } else {
      var ago = agoText(r.lastSeen, now);
      out.lat = Strings.t("peer.offline");
      out.tone = r.lastSeen ? "lost" : "wait";
      out.st = ago ? Strings.t("peer.offline.seen", [ago]) : Strings.t("peer.offline.never");
    }
    return out;
  });

  rows.sort(function (a, b) {
    var d = TONE_ORDER[a.tone] - TONE_ORDER[b.tone];
    return d !== 0 ? d : (a.name < b.name ? -1 : (a.name > b.name ? 1 : 0));
  });
  return rows;
}

// ---------------------------------------------------------------------------------- widget

// A machine's name on the bar: a long one is cut, so it cannot widen the bar (the tooltip has all of it).
function labelOf(name) {
  var n = String(name || "");
  return n.length > 12 ? n.slice(0, 11) + "…" : n;
}

// The bar widget: one glyph per state, an optional label, a tooltip, and how it is painted.
//   tone  "normal" | "accent" | "urgent"      colour is never the only signal:
//   dim   the state is quiet                  every state has its own glyph, and attention
//   dot   a badge on the glyph                also carries a dot badge
function widget(args) {
  var connected = !!args.connected;
  var status = args.status || null;
  var summary = connected ? (args.summary || summaryOf(status, args.layout)) : { state: "off" };
  var state = STATES.indexOf(summary.state) >= 0 ? summary.state : "idle";
  var attention = connected ? list(args.attention) : [];
  var chord = chordLabel(args.chord || chordOf(status));
  var me = meName(status, args.hello) || Strings.t("me.fallback");
  var peers = list(status && status.peers);
  var connectedNames = connectedPeers(status).map(function (p) { return p.name; });
  var peer = summary.peer || "";
  var showName = args.showPeerName !== false;

  var out = {
    state: state,
    label: "",
    tone: "normal",
    dim: false,
    dot: false,
    headline: "",
    hint: "",
    tooltip: ""
  };

  switch (state) {
    case "off":
      out.dim = true;
      out.headline = Strings.t("tip.off");
      out.hint = Strings.t("tip.off.hint");
      break;
    case "idle":
      out.dim = true;
      if (peers.length === 0) {
        out.headline = Strings.t("tip.idle.first");
        out.hint = Strings.t("tip.idle.first.hint");
      } else {
        out.headline = Strings.t("tip.idle");
        var names = peers.map(function (p) { return p.name; });
        out.hint = names.length <= 3
          ? Strings.t("tip.idle.hint", [joinNames(names)])
          : Strings.t("tip.idle.hint.any");
      }
      break;
    case "ready":
      out.headline = Strings.t("tip.ready");
      out.hint = connectedNames.length
        ? Strings.t("tip.ready.hint", [me, joinNames(connectedNames)])
        : Strings.t("tip.ready.hint.none", [me]);
      break;
    case "away":
      out.tone = "accent";
      out.label = showName ? labelOf(peer) : "";
      out.headline = Strings.t("tip.away", [peer, chord]);
      break;
    case "controlled":
      out.tone = "accent";
      out.label = showName ? labelOf(peer) : "";
      out.headline = Strings.t("tip.controlled", [peer, chord]);
      break;
    case "locked":
      out.headline = Strings.t("tip.locked");
      out.hint = Strings.t("tip.locked.hint");
      break;
    case "attention":
      out.tone = "urgent";
      // the alert glyph and the urgent colour already say it: no dot on top
      if (attention.length > 0) {
        var first = attentionActions([attention[0]])[0];
        out.headline = String(first.title || "");
        out.hint = String(first.body || "").replace(/`/g, "");
        if (attention.length > 1) {
          out.hint += (out.hint ? "\n" : "") + Strings.t("tip.attention.more", [attention.length - 1]);
        }
      } else {
        out.headline = "ferry needs you";
      }
      break;
  }
  out.tooltip = out.hint ? out.headline + "\n" + out.hint : out.headline;
  return out;
}

// ----------------------------------------------------------------------------------- hero

// The popover's top block (PanelHero): glyph key, a one-line sentence, a second line, and a pill on
// the right. `tone` colours the glyph: "normal" | "accent" | "urgent". While input travels the second
// line is the escape chord (`chord`, one entry per key, and `chordTail`), because it is the one thing
// the user needs when something goes wrong (U1); `meta` still says the same in words, for a screen
// reader and for a view that has no keycaps.
function hero(args) {
  var connected = !!args.connected;
  var status = args.status || null;
  var summary = connected ? (args.summary || summaryOf(status, args.layout)) : { state: "off" };
  var state = STATES.indexOf(summary.state) >= 0 ? summary.state : "idle";
  var attention = connected ? list(args.attention) : [];
  var me = meName(status, args.hello) || Strings.t("me.fallback");
  var peers = list(status && status.peers);
  var online = connectedPeers(status);
  var peer = summary.peer || "";
  var keys = chordKeys(args.chord || chordOf(status));
  var rtt = "";
  for (var i = 0; i < peers.length; i++) {
    if (peers[i].name === peer && peers[i].state === "connected") rtt = rttText(peers[i].rtt_us);
  }

  var out = { state: state, title: "", meta: "", detail: "", tone: "normal", detailTone: "normal", chord: [], chordTail: "" };
  switch (state) {
    case "off":
      out.title = Strings.t("hero.off.title");
      out.meta = Strings.t("hero.off.meta");
      out.detail = Strings.t("hero.off.detail");
      break;
    case "idle":
      if (peers.length === 0) {
        out.title = Strings.t("hero.first.title");
        out.meta = Strings.t("hero.first.meta");
      } else {
        out.title = Strings.t("hero.idle.title");
        out.meta = Strings.t("hero.idle.meta", [joinNames(peers.map(function (p) { return p.name; }))]);
      }
      break;
    case "ready":
      out.title = Strings.t("hero.ready.title");
      out.meta = Strings.t(online.length === 1 ? "hero.ready.meta.one" : "hero.ready.meta.many", [me, online.length]);
      break;
    case "away":
      out.title = Strings.t("hero.away.title", [peer]);
      out.meta = Strings.t("hero.away.meta", [peer]);
      out.detail = rtt;
      out.tone = "accent";
      out.detailTone = "accent";
      out.chord = keys;
      out.chordTail = Strings.t("hero.chord.tail");
      break;
    case "controlled":
      out.title = Strings.t("hero.controlled.title", [peer]);
      out.meta = Strings.t("hero.controlled.meta", [peer]);
      out.detail = rtt;
      out.tone = "accent";
      out.detailTone = "accent";
      out.chord = keys;
      out.chordTail = Strings.t("hero.chord.tail");
      break;
    case "locked":
      out.title = Strings.t("hero.locked.title");
      out.meta = Strings.t("hero.locked.meta");
      out.detail = Strings.t("hero.locked.detail");
      break;
    case "attention":
      out.tone = "urgent";
      out.detailTone = "urgent";
      // The cards below say what is wrong; the hero keeps saying what the machine is doing
      // underneath, which the attention state would otherwise hide.
      out.title = Strings.t("hero.attention.title");
      var under = hero({ connected: true, status: status, layout: args.layout, hello: args.hello,
        chord: args.chord, summary: baseState(status), attention: [] });
      out.meta = under.state === "ready" ? under.meta : under.title;
      out.detail = Strings.t(attention.length === 1 ? "hero.attention.detail.one" : "hero.attention.detail.many", [attention.length]);
      break;
  }
  return out;
}

// True when the popover should show the first-run card: connected, and nothing is paired.
function isFirstRun(connected, status, peersResult) {
  if (!connected) return false;
  // "nothing is paired" is known once both snapshots are in: until then the card would flash on every connect
  if (!isObject(status) || !isObject(peersResult)) return false;
  var live = list(status && status.peers);
  var pinned = list(peersResult && peersResult.peers);
  return live.length === 0 && pinned.length === 0;
}

// ================================================================================ settings
//
// The Settings view is generic: it is driven by `config.schema`, so a key that
// ferryd adds later appears without a change in any QML file. This section is all of its thinking:
// how keys group, which control a key gets, how a value reads, how typed text becomes a value,
// and how a key press becomes an escape chord. The view only draws what these functions return.

// The groups in the order people reach for them. A prefix not listed comes after these, in the
// order the schema gives it.
var SETTINGS_GROUP_ORDER = ["crossing", "input", "clipboard", "discovery", "notify", "peer", "safety"];
// An int whose range fits in this many steps is a slider; a wider one (a port, a byte count) is
// a number field. The slider is for a value you steer by eye.
var SLIDER_MAX_RANGE = 1000;
var MAX_SAFE_INT = 9007199254740991;

function capitalize(text) {
  var s = String(text);
  return s.length ? s.charAt(0).toUpperCase() + s.slice(1) : s;
}

// `crossing.edge_resistance_px` -> "crossing"; a key with no dot is its own "other" group.
function keyGroup(key) {
  var s = String(key);
  var dot = s.indexOf(".");
  return dot > 0 ? s.slice(0, dot) : "other";
}

function groupTitle(id) {
  var key = "settings.group." + id;
  return Strings.has(key) ? Strings.t(key) : capitalize(String(id).replace(/[_-]+/g, " "));
}

// [{ id, title, keys: [spec] }]: the schema's keys by their dotted prefix. Keys stay in schema order
// inside a group. A spec without a usable key is dropped rather than drawn broken.
function settingsGroups(schema) {
  var byId = {};
  var seen = [];
  list(schema).forEach(function (spec) {
    if (!isObject(spec) || typeof spec.key !== "string" || spec.key === "") return;
    var id = keyGroup(spec.key);
    if (!byId[id]) {
      byId[id] = { id: id, title: groupTitle(id), keys: [] };
      seen.push(id);
    }
    byId[id].keys.push(spec);
  });
  var first = SETTINGS_GROUP_ORDER.filter(function (id) { return byId[id] !== undefined; });
  var rest = seen.filter(function (id) { return SETTINGS_GROUP_ORDER.indexOf(id) < 0; });
  return first.concat(rest).map(function (id) { return byId[id]; });
}

// The words for a key: polished in Strings.js for the keys that exist today, else derived from the
// schema ("edge_resistance_px" with unit "px" -> "Edge resistance").
function settingLabel(spec) {
  var polished = "settings.label." + spec.key;
  if (Strings.has(polished)) return Strings.t(polished);
  var name = String(spec.key).slice(String(spec.key).indexOf(".") + 1);
  var words = name.split(/[_-]+/).filter(function (w) { return w !== ""; });
  var unit = spec.unit ? String(spec.unit).toLowerCase() : "";
  if (unit && words.length > 1 && words[words.length - 1].toLowerCase() === unit) words.pop();
  return capitalize(words.join(" "));
}

// ferryd's one-line help, with the full stop its schema leaves off.
function settingHelp(spec) {
  var t = spec.doc ? String(spec.doc).trim() : "";
  return t === "" || /[.!?]$/.test(t) ? t : t + ".";
}

// False for a key whose feature this ferryd has not built (`available: false` in config.schema): the value is
// saved and nothing reads it, so a control for it is shown off and disabled. A key the schema does not
// list, or no schema at all (a ferryd from before M3), counts as available.
function keyAvailable(schema, key) {
  var keys = list(schema);
  for (var i = 0; i < keys.length; i++) {
    if (keys[i] && keys[i].key === key) return keys[i].available !== false;
  }
  return true;
}

function choiceLabel(spec, value) {
  var polished = "settings.choice." + spec.key + "." + value;
  if (Strings.has(polished)) return Strings.t(polished);
  return capitalize(String(value).replace(/[_-]+/g, " "));
}

// [{ value, label }] for a dropdown.
function choiceOptions(spec) {
  return list(spec.choices).map(function (c) { return { value: String(c), label: choiceLabel(spec, c) }; });
}

// Which control draws a key: toggle | slider | number | dropdown | static | text | chord.
// `static` is an enum with nothing to choose (one choice): it reads, it does not ask.
function controlFor(spec) {
  switch (spec.type) {
    case "bool": return "toggle";
    case "int": {
      var range = sliderMax(spec) - Number(spec.min);
      return isFinite(range) && range > 0 && range <= SLIDER_MAX_RANGE ? "slider" : "number";
    }
    case "enum": return list(spec.choices).length > 1 ? "dropdown" : "static";
    case "chord": return "chord";
    default: return "text";   // name, string, and a type this build does not know
  }
}

// The top of a slider: the schema's `slider_max` (the usual range of a key whose `max` is far
// above it, such as a delay in ms), else `max`. The field beside the slider still takes up to `max`.
function sliderMax(spec) {
  var max = Number(spec.max);
  var soft = Number(spec.slider_max);
  return spec.slider_max !== undefined && isFinite(soft) && soft > Number(spec.min) && soft < max ? soft : max;
}

// Whole steps of a slider: arrows move one, Page Up/Down a tenth of the range, at least five.
function sliderPageStep(spec) {
  var range = sliderMax(spec) - Number(spec.min);
  return Math.max(5, Math.round(range / 10));
}

// The largest value a field can hold exactly. A JS number is exact to 2^53; ferryd's int goes to
// 2^63 for a byte count, and a value above this is typed in bytes by nobody.
function fieldMax(spec) {
  var max = Number(spec.max);
  return isFinite(max) ? Math.min(max, MAX_SAFE_INT) : MAX_SAFE_INT;
}

var BYTE_UNITS = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];

// 33554432 -> "32 MiB", 1500000 -> "1.4 MiB". Exact multiples lose no digits.
function formatBytes(n) {
  var v = Number(n);
  if (!isFinite(v) || v < 0) return String(n);
  var i = 0;
  while (i < BYTE_UNITS.length - 1 && v >= 1024) { v /= 1024; i++; }
  var text = v === Math.floor(v) ? String(v) : (Math.round(v * 10) / 10).toFixed(1);
  return text + " " + BYTE_UNITS[i];
}

// A value as the user reads it. The wire value is never changed: this is only for the screen.
function formatValue(spec, value) {
  if (value === undefined || value === null) return "";
  switch (spec.type) {
    case "bool": return value === true ? Strings.t("settings.on") : Strings.t("settings.off");
    case "int":
      if (spec.unit === "bytes") return formatBytes(value);
      return spec.unit ? value + " " + spec.unit : String(value);
    case "enum": return choiceLabel(spec, value);
    case "chord": return chordLabel(value);
    default: return String(value);
  }
}

// The number a field shows while it is being edited: bytes read as a size, anything else as digits.
function editableNumber(spec, value) {
  if (value === undefined || value === null) return "";
  return spec.unit === "bytes" ? formatBytes(value) : String(value);
}

var BYTE_SUFFIX = { "": 1, "b": 1, "k": 1024, "kb": 1024, "kib": 1024, "m": 1048576, "mb": 1048576, "mib": 1048576,
  "g": 1073741824, "gb": 1073741824, "gib": 1073741824, "t": 1099511627776, "tb": 1099511627776, "tib": 1099511627776 };

// Typed text -> { ok: true, value } or { ok: false, message }. It checks the shape (a whole
// number, a size for bytes), never the range: ferryd owns the range, and its refusal, with the
// allowed values as the hint, is what the user sees.
function parseNumber(spec, text) {
  var raw = String(text === undefined || text === null ? "" : text).trim();
  var bad = {
    ok: false,
    message: spec.unit === "bytes" ? Strings.t("settings.error.number.bytes") : Strings.t("settings.error.number")
  };
  if (raw === "") return bad;
  var m = /^([+-]?\d+(?:\.\d+)?)\s*([A-Za-z]*)$/.exec(raw);
  if (!m) return bad;
  var suffix = m[2].toLowerCase();
  var n = Number(m[1]);
  if (spec.unit === "bytes") {
    if (BYTE_SUFFIX[suffix] === undefined) return bad;
    n = n * BYTE_SUFFIX[suffix];
  } else if (suffix !== "" && suffix !== String(spec.unit || "").toLowerCase()) {
    return bad;
  }
  if (!isFinite(n) || n !== Math.floor(n)) return bad;
  if (Math.abs(n) > MAX_SAFE_INT) return bad;
  return { ok: true, value: n };
}

// Text that goes on as text: the edges of a name are never meant.
function parseText(text) {
  return { ok: true, value: String(text === undefined || text === null ? "" : text).trim() };
}

// When a saved value is in force, as the badge words and the sentence that explains it.
function appliesOf(spec) {
  var id = spec.applies === "live" || spec.applies === "restart" || spec.applies === "setup" ? spec.applies : "";
  return {
    id: id,
    badge: id ? Strings.t("settings.applies." + id) : "",
    long: id ? Strings.t("settings.applies." + id + ".long") : ""
  };
}

function pendingKeys(pending) {
  return Array.isArray(pending) ? pending.filter(function (k) { return typeof k === "string"; }) : [];
}

function isPending(pending, key) {
  return pendingKeys(pending).indexOf(key) >= 0;
}

function isDefault(spec, value) {
  return value === spec.default;
}

// The default as the Reset button's tooltip says it. An empty text is "empty" in words, since the
// bare empty string reads as a bug.
function defaultText(spec) {
  var d = spec.default;
  if (d === "" || d === undefined || d === null) return Strings.t("settings.default.empty");
  return formatValue(spec, d);
}

// What the Hero's pill and the banner say about the keys that wait for a restart. Empty when none.
function pendingBanner(pending) {
  var n = pendingKeys(pending).length;
  if (n === 0) return { count: 0, pill: "", text: "", command: "" };
  return {
    count: n,
    pill: Strings.t(n === 1 ? "settings.pill.one" : "settings.pill.many", [n]),
    text: Strings.t(n === 1 ? "settings.banner.one" : "settings.banner.many", [n]),
    command: Strings.t("settings.banner.cmd")
  };
}

// An error from a call, shaped for the line under a control: `message` is ferryd's own words
// (it names the key and the value), `hint` says what is allowed. A transport failure gets words.
function settingError(error, key) {
  if (!isObject(error)) return null;
  var code = String(error.code || "");
  if (code === "timeout") return { code: code, message: Strings.t("settings.error.timeout"), hint: "" };
  if (code === "not_connected") return { code: code, message: Strings.t("settings.error.offline"), hint: "" };
  // A sentence of ours for the key, when there is one: ferryd's message starts with the key's path and the
  // value that was typed, and its hint says the same again.
  if (code === "bad_value" && key && Strings.has("settings.error." + key)) return { code: code, message: Strings.t("settings.error." + key), hint: "" };
  if (code !== "bad_value") {
    var known = "err." + code;
    if (Strings.has(known)) return { code: code, message: Strings.t(known), hint: "" };
  }
  var text = String(error.message || code);
  // "crossing.edge_resistance_px: 900 is out of range" -> "900 is out of range."
  text = text.replace(/^[a-z_]+\.[a-z_]+:\s*/, "").replace(/^"[^"]*":\s*/, "");
  var hint = error.hint ? String(error.hint) : "";
  return { code: code, message: sentence(text), hint: hint === "" ? "" : Strings.t("settings.error.allowed", [hint]) };
}

// "the escape chord needs at least 2 modifiers" -> "The escape chord needs at least 2 modifiers."
function sentence(text) {
  var t = String(text || "").replace(/^\s+|\s+$/g, "");
  if (t === "") return t;
  t = t.charAt(0).toUpperCase() + t.slice(1);
  return /[.!?]$/.test(t) ? t : t + ".";
}

// off | loading | unsupported | ready: what the view can do right now.
//   off          no connection (the widget's own state)
//   unsupported  a ferryd that has no config.schema (before M3)
//   loading      the schema or the values have not arrived yet
function settingsState(connected, hasSchemaMethod, schema, config) {
  if (!connected) return "off";
  if (!hasSchemaMethod) return "unsupported";
  if (!Array.isArray(schema) || !isObject(config)) return "loading";
  return "ready";
}

// A rough height of a key's row in lines, to split the groups over columns evenly.
var CONTROL_WEIGHT = { toggle: 1.2, slider: 1.9, number: 1.7, dropdown: 1.7, "static": 1.1, text: 1.7, chord: 2.2 };

function groupWeight(group) {
  var w = 0.6;
  group.keys.forEach(function (spec) { w += CONTROL_WEIGHT[controlFor(spec)] || 1.7; });
  return w;
}

// Whole groups, in order, split into `count` runs (1 or 2) that are as even as they can be. Order is
// kept so the Tab order reads down the first column, then down the second.
function settingsColumns(groups, count) {
  if (count < 2 || groups.length < 2) return [groups.slice()];
  var weights = groups.map(groupWeight);
  var total = weights.reduce(function (a, b) { return a + b; }, 0);
  var best = 1;
  var bestWorst = Infinity;
  var left = 0;
  for (var i = 1; i < groups.length; i++) {
    left += weights[i - 1];
    var worst = Math.max(left, total - left);
    if (worst < bestWorst) { bestWorst = worst; best = i; }
  }
  return [groups.slice(0, best), groups.slice(best)];
}

// ------------------------------------------------------------------------- the chord recorder
//
// A chord is `Super+Ctrl+Escape`: two to three modifiers and one key (ferry-core chord.rs). The
// key names ferryd accepts are a fixed table, so the recorder says which keys it can use before
// asking ferryd; ferryd still has the last word and its refusal is shown as it is.

// Qt key and modifier codes (Qt.Key_*, Qt.*Modifier). A .pragma library file has no Qt namespace.
var QT_MOD_SHIFT = 0x02000000;
var QT_MOD_CTRL = 0x04000000;
var QT_MOD_ALT = 0x08000000;
var QT_MOD_META = 0x10000000;
var QT_KEY_ESCAPE = 0x01000000;
var QT_MODIFIER_KEYS = [0x01000020, 0x01000021, 0x01000022, 0x01000023, 0x01001103, 0x01000024, 0x01000025, 0x01000026];

var CHORD_NAMED_KEYS = {
  0x01000000: "Escape", 0x20: "Space", 0x01000003: "Backspace", 0x01000007: "Delete",
  0x01000010: "Home", 0x01000011: "End", 0x01000008: "Pause"
};
// Names that are real keys but not in ferryd's table, so the recorder can name what it refused.
var CHORD_REFUSED_KEYS = {
  0x01000001: "Tab", 0x01000002: "Backtab", 0x01000004: "Return", 0x01000005: "Enter", 0x01000006: "Insert",
  0x01000012: "Left", 0x01000013: "Up", 0x01000014: "Right", 0x01000015: "Down",
  0x01000016: "PageUp", 0x01000017: "PageDown", 0x01000009: "Print"
};

// ["Super", "Ctrl"] from Qt modifier flags, in the order the default chord is spelt.
function chordModifiers(modifiers) {
  var m = Number(modifiers) || 0;
  var out = [];
  if (m & QT_MOD_META) out.push("Super");
  if (m & QT_MOD_CTRL) out.push("Ctrl");
  if (m & QT_MOD_ALT) out.push("Alt");
  if (m & QT_MOD_SHIFT) out.push("Shift");
  return out;
}

function chordTriggerName(key) {
  var k = Number(key);
  if (CHORD_NAMED_KEYS[k] !== undefined) return CHORD_NAMED_KEYS[k];
  if (k >= 0x41 && k <= 0x5a) return String.fromCharCode(k);          // Qt.Key_A .. Qt.Key_Z
  if (k >= 0x30 && k <= 0x39) return String.fromCharCode(k);          // Qt.Key_0 .. Qt.Key_9
  if (k >= 0x01000030 && k <= 0x0100003b) return "F" + (k - 0x01000030 + 1);   // Qt.Key_F1 .. F12
  return "";
}

function refusedKeyName(key, text) {
  var k = Number(key);
  if (CHORD_REFUSED_KEYS[k] !== undefined) return CHORD_REFUSED_KEYS[k];
  var t = String(text || "");
  if (t.length === 1 && t > " ") return t.toUpperCase();
  if (k >= 0x01000030 && k <= 0x01000047) return "F" + (k - 0x01000030 + 1);
  return "that key";
}

var QT_ARROW_KEYS = [0x01000012, 0x01000013, 0x01000014, 0x01000015];

// The go chord (`crossing.go_chord`) is modifiers only: an arrow key finishes it and is
// not part of it.
function chordIsModifiersOnly(key) {
  return String(key) === "crossing.go_chord";
}

// One key press while recording -> what the recorder does.
//   { state: "holding", held: ["Super","Ctrl"] }       modifiers only so far: keep listening
//   { state: "cancel" }                                Escape alone
//   { state: "done", chord: "Super+Ctrl+Escape", held, trigger }
//   { state: "refused", held, name, message }          a key ferryd has no name for
// With `modifiersOnly` (the go chord), an arrow key finishes the chord with the modifiers alone,
// and any other key is refused.
function chordFromKey(key, modifiers, text, modifiersOnly) {
  var held = chordModifiers(modifiers);
  if (QT_MODIFIER_KEYS.indexOf(Number(key)) >= 0) return { state: "holding", held: held };
  if (modifiersOnly) {
    if (Number(key) === QT_KEY_ESCAPE && held.length === 0) return { state: "cancel" };
    if (QT_ARROW_KEYS.indexOf(Number(key)) >= 0) {
      return { state: "done", chord: held.join("+"), held: held, trigger: "" };
    }
    var other = chordTriggerName(key) || refusedKeyName(key, text);
    return { state: "refused", held: held, name: other, message: Strings.t("settings.record.arrow", [other]) };
  }
  var trigger = chordTriggerName(key);
  if (Number(key) === QT_KEY_ESCAPE && held.length === 0) return { state: "cancel" };
  if (trigger === "") {
    var name = refusedKeyName(key, text);
    return { state: "refused", held: held, name: name, message: Strings.t("settings.record.badkey", [name]) };
  }
  return { state: "done", chord: held.concat([trigger]).join("+"), held: held, trigger: trigger };
}

// "Super+Ctrl+Escape" -> ["Super", "Ctrl", "Esc"]: the key caps of a chord, with the word the
// tooltip uses for Escape.
function chordCaps(chord) {
  return String(chord || "").split("+").filter(function (p) { return p !== ""; })
    .map(function (p) { return p === "Escape" ? "Esc" : p; });
}

// ----------------------------------------------------------------------------- fingerprints

// "a3f1c2d4": the first 8 hex digits, which is what `unpair` and `peer.rename` accept and what ferryd
// names a peer that has no valid name of its own. FR-PAIR-09's "short fingerprint".
function shortFingerprint(fp) {
  return String(fp || "").replace(/[^0-9a-fA-F]/g, "").slice(0, 8).toLowerCase();
}

// "A3F1 C2D4 … 5BE8 0D4E": enough of both ends to compare with what the other machine prints.
function groupedFingerprint(fp) {
  var hex = String(fp || "").replace(/[^0-9a-fA-F]/g, "").toUpperCase();
  if (hex.length < 16) return hex.replace(/(.{4})(?=.)/g, "$1 ");
  var head = hex.slice(0, 8).replace(/(.{4})(?=.)/g, "$1 ");
  var tail = hex.slice(-8).replace(/(.{4})(?=.)/g, "$1 ");
  return head + " … " + tail;
}

// ----------------------------------------------------------------------------------- rename

// The rule of `peer.name` (ferry-core `validate_peer_name`): not empty, at most 64 characters, and no
// whitespace, control character or ':'. ferryd stays the authority; this answers while the user types.
// ctx: { me: "desk", fingerprint: <the peer being renamed>, current: "lap",
//        others: [{ name, fingerprint }] }
// Leading and trailing spaces are dropped rather than refused: a pasted name should not fail on them.
function renameCheck(name, ctx) {
  ctx = ctx || {};
  var n = String(name === undefined || name === null ? "" : name).replace(/^\s+|\s+$/g, "");
  var fail = function (key, args) { return { ok: false, name: n, unchanged: false, error: Strings.t(key, args) }; };
  if (n === "") return fail("rename.empty");
  // code points, as ferryd counts characters: a surrogate pair is one
  if (n.replace(/[\uD800-\uDBFF][\uDC00-\uDFFF]/g, "_").length > 64) return fail("rename.long");
  if (/[\s\u0000-\u001F\u007F-\u009F:]/.test(n)) return fail("rename.chars");
  if (n === ctx.current) return { ok: true, name: n, unchanged: true, error: "" };
  if (ctx.me && n === ctx.me) return fail("rename.self");
  var others = list(ctx.others);
  for (var i = 0; i < others.length; i++) {
    if (others[i] && others[i].name === n && others[i].fingerprint !== ctx.fingerprint) return fail("rename.taken", [n]);
  }
  return { ok: true, name: n, unchanged: false, error: "" };
}

// ------------------------------------------------------------------------------ api errors

// What to say about an error a call answered with: ferryd's own message and hint when it sent
// them (they are written for the user), and a sentence of ours for the two failures that are the
// client's own, a daemon that is gone and one that did not answer.
function errorText(error) {
  var e = error || {};
  if (e.code === "not_connected") return { message: Strings.t("err.not_connected"), hint: "" };
  if (e.code === "timeout") return { message: Strings.t("err.timeout"), hint: "" };
  // A code with a sentence of ours says that and nothing more: ferryd's text for it names a flag, a key or a
  // fingerprint. The codes whose message names the value that was wrong (bad_params, bad_value) and the ones
  // we do not know keep ferryd's words.
  var known = e.code ? "err." + String(e.code) : "";
  if (known !== "" && Strings.has(known)) return { message: Strings.t(known), hint: "" };
  return { message: e.message ? String(e.message) : Strings.t("err.unknown"), hint: e.hint ? String(e.hint) : "" };
}

// ----------------------------------------------------------------------------- attention

function escapeHtml(text) {
  return String(text).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}

// ferryd writes a command in backticks (`ferry doctor`); Text.StyledText shows it bold. The
// text is escaped first: a peer name or a path can hold a "<".
function richText(text) {
  return escapeHtml(text).replace(/`([^`]+)`/g, "<b>$1</b>");
}

// The first command a hint puts in backticks, or "".
function commandIn(text) {
  var m = /`([^`]+)`/.exec(String(text || ""));
  return m ? m[1].trim() : "";
}

// What a button can do about each problem ferryd names (contract 2.4). Each entry is the item as
// ferryd sent it plus `action`, or null when there is nothing a button can fix:
//   { kind: "unpair", label, peer, again }   a changed key: unpair, then pair again
//   { kind: "panel", label, view }           Arrange or Settings has the control that fixes it
//   { kind: "copy", label, command }         a command to run in a terminal, one click to copy
function attentionActions(items) {
  return list(items).map(function (item) {
    var out = {};
    for (var k in item) out[k] = item[k];
    var code = item.code;
    var peer = item.peer ? String(item.peer) : "";
    var action = null;
    // The message says why the link was refused ("lap cannot connect: fingerprint changed"); the hint
    // is the same sentence for every refusal ("after a key change, `ferry unpair lap` and pair again"),
    // so it cannot tell a changed key from a version mismatch that unpairing would not fix.
    var refusal = code === "peer_refused" ? refusalKind(item.message) : "";
    if (code === "peer_refused" && peer && (refusal === "key" || refusal === "untrusted")) {
      action = { kind: "unpair", label: Strings.t("attn.unpair.again"), peer: peer, again: true };
    } else if (code === "peer_refused" || code === "barriers_failed") {
      action = copyAction("ferry doctor");
    } else if (code === "capture_denied" || code === "escape_bind_missing") {
      action = copyAction("ferry setup");
    } else if (code === "layout_unplaced") {
      action = { kind: "panel", label: Strings.t("attn.arrange"), view: "arrange" };
    } else if (code === "config_invalid") {
      action = { kind: "panel", label: Strings.t("attn.settings"), view: "settings" };
    } else if (commandIn(item.hint) !== "") {
      action = copyAction(commandIn(item.hint));
    }
    out.action = action;
    // What the card says: our sentence for a code we know, ferryd's own words for one we do not. `message`
    // and `hint` stay as ferryd sent them (the toast of the same problem is matched by its message).
    var words = attentionWords(code, refusal, item);
    out.title = words.title;
    out.body = words.body;
    return out;
  });
}

// { title, body } for one attention item: the entry of Strings.js for its code (and, for a refused link,
// why), else ferryd's `message` and `hint`.
function attentionWords(code, refusal, item) {
  var key = "attn." + code;
  if (code === "peer_refused") key += "." + refusal;
  else if (code === "capture_denied" && /ask/i.test(String(item.message || ""))) key = "attn.capture_ask";
  var peer = item.peer ? String(item.peer) : Strings.t("pair.who.a");
  if (Strings.has(key + ".title")) return { title: Strings.t(key + ".title", [peer]), body: Strings.has(key + ".body") ? Strings.t(key + ".body", [peer]) : "" };
  return { title: String(item.message || ""), body: String(item.hint || "") };
}

function copyAction(command) {
  return { kind: "copy", label: Strings.t("attn.copy", [command]), command: command };
}

// ---------------------------------------------------------------------------------- activity

// The last few `notice` events as a list for the popover. FerryClient stamps each with `at` (epoch
// milliseconds) when it arrives, since a notice carries no time. A `pair_code` is left out: it is
// a prompt that is closed when the pairing moves on, and its text holds the code. An `attention` toast
// whose problem is still in the card above is left out too: the list would say it twice.
// `current` is the attention list as it is now.
// ferryd's toasts say "fingerprint"; the panel says "key".
function plainWords(text) {
  return String(text).replace(/\bfingerprint\b/gi, function (w) { return w.charAt(0) === "F" ? "Key" : "key"; });
}

function activityItems(notices, now, max, current) {
  var out = [];
  var showing = {};
  list(current).forEach(function (a) { if (a && a.message) showing[String(a.message)] = true; });
  list(notices).forEach(function (n, i) {
    if (!isObject(n) || n.kind === "pair_code") return;
    var raw = String(n.title || "").replace(/^\s+|\s+$/g, "");
    if (raw === "") return;
    // matched on ferryd's words, which the card's `message` keeps
    if (n.kind === "attention" && showing[raw]) return;
    var title = plainWords(raw);
    out.push({
      kind: String(n.kind || ""),
      title: title,
      body: plainWords(String(n.body || "").replace(/^\s+|\s+$/g, "")),
      peer: n.peer ? String(n.peer) : "",
      urgent: n.urgency === "critical",
      at: typeof n.at === "number" ? n.at : 0,
      ago: agoText(typeof n.at === "number" ? n.at : "", now),
      order: i
    });
  });
  // newest first; notices without a time keep the order they came in, the last one first
  out.sort(function (a, b) { return a.at !== b.at ? b.at - a.at : b.order - a.order; });
  return out.slice(0, max || 5);
}

// ---------------------------------------------------------------------------------- pairing

// Where a pairing is, as the panel and the popover read it. ferryd sends `pairing` events (contract 2.3);
// each is a full description of the stage, and the receiver keeps when it arrived (`at`), because
// `expires_ms` is what was left then. Stages: listening, connecting, code, waiting_peer (live) and
// paired, failed, closed (the last word).

var PAIR_LIVE = ["listening", "connecting", "code", "waiting_peer"];
var PAIR_LAST = ["paired", "failed", "closed"];
// the pairing window and the attempt are 120 s; a bar's width is measured against it
var PAIR_WINDOW_MS = 120000;
// A result that is older than this when the panel opens belongs to a flow nobody is looking at.
var PAIR_STALE_MS = 15000;
// ferryd ends a pairing with an event; one that never came, this long after the deadline, is not waited for.
var PAIR_GRACE_MS = 15000;
// A live stage that status no longer lists, this long after it arrived, is over. (A dial that fails
// leaves a `connecting` event that nothing follows: ferryd clears the pairing and answers pair.start
// with the error, and a subscriber is told nothing more.) Longer than the 50 ms the state topic lags.
var PAIR_SETTLE_MS = 2000;

// the hero's glyph for each screen: a key in Glyphs.UI
var HERO_GLYPH = {
  start: "plus", listening: "bell", connecting: "link", code: "key", waiting: "timer",
  paired: "checkCircle", failed: "alertCircle", closed: "linkOff"
};

function isPairLive(stage) { return PAIR_LIVE.indexOf(stage) >= 0; }
function isPairLast(stage) { return PAIR_LAST.indexOf(stage) >= 0; }

// "418207" -> "418 207". Six digits are read aloud as two groups of three.
function groupCode(code) {
  var d = String(code || "").replace(/\D/g, "");
  return d.length === 6 ? d.slice(0, 3) + " " + d.slice(3) : d;
}

// "418207" -> "4 1 8 2 0 7", for a screen reader that would say "four hundred and eighteen thousand".
function spokenCode(code) {
  return String(code || "").replace(/\D/g, "").split("").join(" ");
}

// 112000 -> "1:52". Rounds up, so the clock reaches 0:00 when the time is up and not before.
function clockText(ms) {
  var s = Math.max(0, Math.ceil(Number(ms) / 1000));
  if (!isFinite(s)) return "";
  return Math.floor(s / 60) + ":" + (s % 60 < 10 ? "0" : "") + (s % 60);
}

// The event to show: the last `pairing` event, or (a daemon that only tells the connection that
// started the pairing) what `status.pairing` says, which has everything but the code.
function pairingOf(event, status) {
  if (isObject(event) && typeof event.stage === "string") return event;
  var sp = status && status.pairing;
  if (isObject(sp) && typeof sp.stage === "string") {
    return { stage: sp.stage, role: sp.role, expires_ms: sp.expires_ms, elsewhere: true };
  }
  return null;
}

function stageScreen(stage) {
  return stage === "waiting_peer" ? "waiting" : stage;
}

// The reason a pairing ended, in words: { title, body, hint }. `ctx`: { peer, stage, byPeer }. A
// reason ferryd adds later falls back to the general text and shows ferryd's own message.
function pairFailure(reason, message, ctx) {
  ctx = ctx || {};
  var r = String(reason || "other");
  var peer = ctx.peer || Strings.t("pair.who.other");
  var base = "pair.fail." + r;
  if (ctx.stage === "closed" && r === "timeout") base = "pair.closed.timeout";
  if (!Strings.has(base)) base = "pair.fail.other";
  var text = String(message || "").replace(/^\s+|\s+$/g, "");
  var out = { reason: r, title: "", body: "", hint: "" };
  if (r === "cancelled" && ctx.byPeer && ctx.peer) {
    out.title = Strings.t("pair.fail.cancelled.by", [peer]);
    out.body = Strings.t("pair.fail.cancelled.by.body");
    return out;
  }
  out.title = Strings.t(base, [peer]);
  if (Strings.has(base + ".body")) out.body = Strings.t(base + ".body", [peer]);
  if (Strings.has(base + ".hint")) out.hint = Strings.t(base + ".hint", [peer]);
  // ferryd's own words say what a table cannot: the two versions, the disk error, an unknown reason
  if (out.body === "" && text !== "") out.body = text.charAt(0).toUpperCase() + text.slice(1);
  return out;
}

// The panel's whole picture of a pairing, from what ferryd said and what the panel itself did.
//   event          the last `pairing` event (null: none yet)         at          when it arrived (ms)
//   status         `status`, for a pairing seen only there            now         the clock (ms)
//   openedAt       when this panel opened                             dismissedAt  when the user closed a result
//   ignoreBefore   events that arrived up to this time are dead (the dial that just failed)
//   starting       { role, addr } while a pair.start is in flight, else null
//   intent         "dial" | "accept" | "": what the user asked for here
//   cancelRequested  the user pressed Cancel: the failure that follows is not news
//   failure        the last failed attempt this panel saw: { reason, message, peer, at }
//   layout         `layout`, to say whether the new peer is placed
function pairFlow(a) {
  a = a || {};
  var now = typeof a.now === "number" ? a.now : Date.now();
  var at = typeof a.at === "number" && a.at > 0 ? a.at : now;
  var ev = pairingOf(a.event, a.status);
  var role = "";
  var screen = "start";
  var remaining = null;
  var peer = null;

  if (ev) {
    // `at` can be a moment after `now` (the panel's clock ticks once a second): never a negative age
    remaining = Number(ev.expires_ms) - Math.max(0, now - at);
    var live = isPairLive(ev.stage);
    var shown = !(Number(a.ignoreBefore) > 0 && at <= Number(a.ignoreBefore));
    if (live && shown) {
      shown = !(remaining < -PAIR_GRACE_MS);
      // status is known and lists no pairing, and the event is not new: it is over
      if (shown && !ev.elsewhere && isObject(a.status) && !isObject(a.status.pairing) && now - at > PAIR_SETTLE_MS) shown = false;
    } else if (isPairLast(ev.stage) && shown) {
      // What the user dismissed is the event they saw, which arrived before or in the very millisecond of
      // the click: so an event stamped the same millisecond is not new.
      shown = at > (Number(a.dismissedAt) || 0) && at >= (Number(a.openedAt) || 0) - PAIR_STALE_MS;
      // The user's own Cancel is not news: back to the start, with no failure on screen.
      if (a.cancelRequested && ev.reason === "cancelled") shown = false;
    }
    if (ev.elsewhere && !live) shown = false;
    if (shown) {
      screen = stageScreen(ev.stage);
      // status has the stage and not the code: there is nothing to compare here, and the screen says so
      if (ev.elsewhere && ev.stage === "code") screen = "waiting";
      if (isObject(ev.peer) && ev.peer.name) peer = {
        name: String(ev.peer.name),
        addr: ev.peer.addr ? String(ev.peer.addr) : "",
        fingerprint: ev.peer.fingerprint ? String(ev.peer.fingerprint) : "",
        short: shortFingerprint(ev.peer.fingerprint)
      };
      role = ev.role === "dialer" || ev.role === "acceptor" ? ev.role : "";
    } else {
      // an event that is not shown (over, or not this panel's) lends nothing to the start screen
      remaining = null;
    }
  }
  if (!peer && role === "dialer" && a.addr) peer = { name: "", addr: String(a.addr), fingerprint: "", short: "" };
  if (screen === "start" && a.starting) {
    screen = "connecting";
    role = "dialer";
    peer = a.starting.addr ? { name: "", addr: String(a.starting.addr), fingerprint: "", short: "" } : null;
    remaining = PAIR_WINDOW_MS;
  }
  if (role === "" && screen !== "start") role = a.intent === "dial" ? "dialer" : (a.intent === "accept" || screen === "listening" ? "acceptor" : "");

  var who = peer ? (peer.name || peer.addr) : "";
  // what to call the other machine in a sentence when ferryd has not said yet
  var other = who || Strings.t("pair.who.other");
  var flow = {
    screen: screen,
    role: role,
    active: isPairLive(screen === "waiting" ? "waiting_peer" : screen),
    terminal: isPairLast(screen),
    elsewhere: !!(ev && ev.elsewhere),
    peer: peer,
    who: who,
    label: other,
    labelCap: who || Strings.t("pair.who.someone"),
    noCode: !!(ev && ev.elsewhere && ev.stage === "code" && screen === "waiting"),
    code: "",
    codeText: "",
    codeSpoken: "",
    remainingMs: remaining === null ? 0 : Math.max(0, remaining),
    remainingText: "",
    fraction: 0,
    expiring: false,
    canAnswer: screen === "code",
    canCancel: false,
    hero: { glyph: "plus", tone: "normal", title: "", meta: "", detail: "", detailTone: "normal" },
    steps: [],
    failure: null,
    lastFailure: null,
    note: "",
    placed: null
  };
  // a pairing that only status shows was started by another connection, and only that one may cancel it
  flow.canCancel = flow.active && !flow.elsewhere;
  var timed = screen === "listening" || screen === "code" || screen === "waiting" || screen === "connecting";
  if (timed && remaining !== null) {
    flow.remainingText = clockText(remaining);
    flow.fraction = Math.max(0, Math.min(1, remaining / PAIR_WINDOW_MS));
    flow.expiring = remaining <= 0;
  }
  if (screen === "code" && ev && ev.code) {
    flow.code = String(ev.code);
    flow.codeText = groupCode(ev.code);
    flow.codeSpoken = spokenCode(ev.code);
  }

  // the last attempt that failed, for a window that stays open (listening follows failed)
  if (a.failure && (screen === "listening" || screen === "start") && now - (a.failure.at || 0) < 30000) {
    flow.lastFailure = pairFailure(a.failure.reason, a.failure.message, { peer: a.failure.peer, stage: "failed", byPeer: !a.failure.byUs });
  }
  if (screen === "failed" || screen === "closed") {
    flow.failure = pairFailure(ev.reason, ev.message, { peer: who && peer && peer.name ? peer.name : "", stage: screen, byPeer: !a.cancelRequested });
  }
  if (screen === "paired" && ev && ev.message) flow.note = String(ev.message);
  if (screen === "paired" && peer) {
    var lp = list(a.layout && a.layout.peers);
    flow.placed = false;
    for (var i = 0; i < lp.length; i++) {
      if (lp[i] && (lp[i].fingerprint === peer.fingerprint || lp[i].name === peer.name) && lp[i].origin) flow.placed = true;
    }
  }

  // the three steps of the mockup: pick, compare, done
  var pick = Strings.t(role === "acceptor" ? "pair.step.wait" : "pair.step.pick");
  var step = screen === "code" || screen === "waiting" ? 2 : (screen === "paired" ? 3 : (screen === "failed" || screen === "closed" ? 2 : 1));
  var failed = screen === "failed" || screen === "closed";
  var labels = [pick, Strings.t("pair.step.compare"), Strings.t("pair.step.done")];
  flow.steps = labels.map(function (label, i) {
    var n = i + 1;
    var st = n < step ? "done" : (n === step ? (failed ? "failed" : (screen === "paired" ? "done" : "current")) : "todo");
    return { n: n, label: label, state: st };
  });

  // the hero: a glyph, a title, a line under it, and the clock in the pill while one is running
  var h = flow.hero;
  h.glyph = HERO_GLYPH[screen] || "plus";
  h.tone = screen === "failed" ? "urgent" : (screen === "start" || screen === "closed" ? "normal" : "accent");
  switch (screen) {
    case "listening":
      h.title = Strings.t("pair.listening.title");
      h.meta = Strings.t("pair.meta.listening");
      break;
    case "connecting":
      h.title = role === "acceptor"
        ? (who ? Strings.t("pair.hero.accept", [who]) : Strings.t("pair.connecting.accept.unknown"))
        : Strings.t("pair.hero.dial", [other]);
      h.meta = Strings.t("pair.meta.connecting");
      break;
    case "code":
      h.title = role === "acceptor" ? Strings.t("pair.hero.accept", [who || Strings.t("pair.who.a")]) : Strings.t("pair.hero.dial", [other]);
      h.meta = Strings.t("pair.meta.code");
      break;
    case "waiting":
      // a code that only status sees: the user has nothing to confirm here, so the hero does not say they do
      h.title = flow.noCode ? Strings.t("pair.hero.elsewhere") : Strings.t("pair.hero.waiting", [other]);
      h.meta = flow.noCode ? Strings.t("pair.meta.elsewhere") : Strings.t("pair.meta.waiting");
      break;
    case "paired":
      h.title = Strings.t("pair.hero.done", [other]);
      h.meta = Strings.t("pair.meta.done");
      break;
    // the hero says what happened, once; the body under it says the rest (#34)
    case "failed":
      h.title = Strings.t("pair.hero.failed");
      h.meta = flow.failure && flow.failure.title ? flow.failure.title : Strings.t("pair.meta.failed");
      break;
    case "closed":
      h.title = Strings.t("pair.hero.closed");
      h.meta = flow.failure && flow.failure.title && flow.failure.title !== h.title ? flow.failure.title : Strings.t("pair.meta.failed");
      break;
    default:
      h.title = Strings.t("panel.pair.title");
      h.meta = Strings.t("panel.pair.meta");
  }
  // The listening and code cards show the clock themselves; the hero pill carries it only where no
  // card does (#32: never twice).
  if (flow.remainingText !== "" && screen === "waiting") {
    h.detail = flow.remainingText;
    h.detailTone = flow.expiring || flow.fraction < 0.15 ? "urgent" : "normal";
  }
  return flow;
}

// What the panel keeps between events: the last attempt that failed, so a window that stays open
// (`failed`, then `listening`) can still say what happened. A new attempt or a pin clears it, and
// so does the end of the window, which says it itself. The user's own Cancel is not a failure.
function pairRemember(failure, ev, at, cancelRequested) {
  if (!isObject(ev)) return failure;
  if (ev.stage === "failed") {
    if (cancelRequested && ev.reason === "cancelled") return null;
    return {
      reason: String(ev.reason || "other"),
      message: String(ev.message || ""),
      peer: isObject(ev.peer) && ev.peer.name ? String(ev.peer.name) : "",
      at: at,
      byUs: !!cancelRequested
    };
  }
  if (ev.stage === "connecting" || ev.stage === "code" || ev.stage === "paired" || ev.stage === "closed") return null;
  return failure;
}

// The popover's strip about a pairing that is running (so a request that arrives while the popover
// is open is seen), or null. `flow` is pairFlow() of the same event.
function pairBanner(flow) {
  if (!flow || !flow.active) return null;
  var who = flow.who || "";
  var text;
  switch (flow.screen) {
    case "listening": text = Strings.t("banner.listening"); break;
    case "connecting": text = flow.role === "acceptor" ? Strings.t("banner.connecting.accept") : Strings.t("banner.connecting.dial", [flow.label]); break;
    case "code": text = flow.role === "acceptor" ? Strings.t("banner.code.accept", [flow.labelCap]) : Strings.t("banner.code.dial", [flow.label]); break;
    case "waiting": text = Strings.t("banner.waiting", [flow.label]); break;
    default: text = Strings.t("banner.elsewhere");
  }
  // a code to compare is the one moment the user is being waited for
  return { text: text, urgent: flow.screen === "code", clock: flow.remainingText, label: Strings.t("banner.open") };
}

// ------------------------------------------------------------------------------ addresses

// The address typed into the Pair panel: `host`, `host:port`, an IPv4, `[v6]`, `[v6]:port` or a bare
// IPv6, which is what `pair.start` takes (ferryd `parse_host_port`). The daemon decides; this
// says so before the call, and in words.
function addrCheck(text) {
  var s = String(text === undefined || text === null ? "" : text).replace(/^\s+|\s+$/g, "");
  if (s === "") return { ok: false, addr: "", error: Strings.t("pair.dial.empty") };
  var bad = { ok: false, addr: s, error: Strings.t("pair.dial.bad") };
  var host = s;
  var port = null;
  if (s.charAt(0) === "[") {
    var close = s.indexOf("]");
    if (close < 0) return bad;
    host = s.slice(1, close);
    var tail = s.slice(close + 1);
    if (tail !== "") {
      if (tail.charAt(0) !== ":") return bad;
      port = tail.slice(1);
    }
    if (!/^[0-9a-fA-F:.]+$/.test(host) || host.indexOf(":") < 0) return bad;
  } else if ((s.match(/:/g) || []).length > 1) {
    if (!/^[0-9a-fA-F:.]+$/.test(s)) return bad;
  } else if (s.indexOf(":") >= 0) {
    host = s.slice(0, s.indexOf(":"));
    port = s.slice(s.indexOf(":") + 1);
  }
  if (port !== null && (!/^\d{1,5}$/.test(port) || Number(port) > 65535)) return bad;
  if (s.charAt(0) !== "[" && (s.match(/:/g) || []).length <= 1 && !/^[A-Za-z0-9._-]{1,253}$/.test(host)) return bad;
  return { ok: true, addr: s, error: "" };
}

// What the other machine has to be told to pair with this one: from the `id` result, the address
// to type (IPv4 first, as ferryd lists them) and the command to run.
function selfAddress(id) {
  var addrs = list(id && id.addrs).map(String);
  if (addrs.length === 0) return null;
  var port = id && id.port ? Number(id.port) : 4243;
  var first = addrs[0];
  var host = first.indexOf(":") >= 0 && first.charAt(0) !== "[" ? "[" + first + "]" : first;
  var display = port === 4243 ? first : host + ":" + port;
  return { addr: display, command: "ferry pair " + display, all: addrs };
}

// Peers that are in the layout but not paired with this machine (contract 2.4):
// crossings between this machine and them are off until they pair.
function untrustedPeers(layout) {
  return list(layout && layout.peers).filter(function (p) {
    return p && p.trusted === false && p.self !== true;
  }).map(function (p) {
    return { name: String(p.name || shortFingerprint(p.fingerprint)), fingerprint: String(p.fingerprint || ""), short: shortFingerprint(p.fingerprint) };
  });
}

// The machines `discover.list` reports (M4), as rows for the Nearby list: those already paired
// are marked and cannot be chosen. The shape is an assumption (contract notes of stream F).
function nearbyRows(result, trusted) {
  var have = {};
  list(trusted).forEach(function (t) { have[t.fingerprint] = true; have[t.name] = true; });
  return list(result && result.peers).filter(isObject).map(function (p) {
    var name = String(p.name || "");
    var known = p.trusted === true || have[p.fingerprint] === true || have[name] === true;
    return {
      name: name,
      addr: String(p.addr || ""),
      fingerprint: String(p.fingerprint || ""),
      trusted: known,
      tag: Strings.t(known ? "pair.nearby.trusted" : "pair.nearby.ready")
    };
  }).filter(function (r) { return r.name !== "" || r.addr !== ""; });
}
