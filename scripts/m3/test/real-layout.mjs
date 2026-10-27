// The fake daemon against the real one, and plugin/Geometry.js against the real one, for `layout.set`.
//
//   node real-layout.mjs REAL_SOCKET FAKE_SOCKET
//
// REAL_SOCKET is a `ferryd --no-input` of a fresh pair (scripts/m3/dev-ferryd.sh start --pair --monitors
// host), FAKE_SOCKET a fake-ferryd started with `--name dev-a --fixture pair`. Both are asked the same
// things in the same order, and their answers must be the same once fingerprints are names:
//   * a scripted run of every case the contract lists (first placement, overlap, the same origins again,
//     two peers at once, a stale revision, a bad fingerprint, a peer that does not exist...)
//   * 150 random placements, which also check that Geometry.firstOverlap predicts what ferryd refuses and
//     that Geometry.deriveLinks gives the links ferryd lists
//   * the `layout` event of a change, and none for a call that changes nothing
// scripts/m3/test/real-layout.sh runs it. It needs a built ferryd and the host's Hyprland (read-only), so it
// is not part of the gate: it is what makes the gate's fake worth trusting.

import { createConnection } from "node:net";
import assert from "node:assert/strict";
import { load } from "./harness.mjs";

const G = load("Geometry.js");
const A = load("Arrange.js");

const [realPath, fakePath] = process.argv.slice(2);
if (!realPath || !fakePath) {
  console.error("usage: real-layout.mjs REAL_SOCKET FAKE_SOCKET");
  process.exit(2);
}

function client(path) {
  const c = createConnection(path);
  let buf = "";
  let nextId = 1;
  const pending = new Map();
  const events = [];
  let hello = null;
  const waiters = [];
  c.setEncoding("utf8");
  c.on("data", (d) => {
    buf += d;
    let i;
    while ((i = buf.indexOf("\n")) >= 0) {
      const msg = JSON.parse(buf.slice(0, i));
      buf = buf.slice(i + 1);
      if (msg.event === "hello") hello = msg.data;
      else if (msg.event !== undefined) { events.push(msg); waiters.splice(0).forEach((w) => w()); }
      else if (pending.has(msg.id)) { pending.get(msg.id)(msg); pending.delete(msg.id); }
    }
  });
  return {
    get hello() { return hello; },
    events,
    call: (method, params) => new Promise((resolve) => {
      const id = nextId++;
      pending.set(id, resolve);
      c.write(JSON.stringify({ id, method, params }) + "\n");
    }),
    event: (name, from, ms = 1200) => new Promise((resolve) => {
      const find = () => events.slice(from).find((e) => e.event === name) || null;
      if (find()) return resolve(find());
      const t = setTimeout(() => resolve(find()), ms);
      const again = () => { if (find()) { clearTimeout(t); resolve(find()); } else waiters.push(again); };
      waiters.push(again);
    }),
    close: () => c.destroy()
  };
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const real = client(realPath), fake = client(fakePath);
await sleep(300);

let failures = 0, checks = 0;
function ok(name, cond, detail) {
  checks++;
  if (!cond) failures++;
  console.log(`REAL ${cond ? "ok   " : "FAIL "}${name}${cond ? "" : "  <- " + detail}`);
}

// ------------------------------------------------------------------------------ normalising

const names = {};   // per side: fingerprint -> name
async function learn(side, c) {
  const l = (await c.call("layout.get")).result;
  names[side] = Object.fromEntries(l.peers.map((p) => [p.fingerprint, `<${p.name}>`]));
  return l;
}
function norm(side, v) {
  const walk = (x) => {
    if (typeof x === "string") return names[side][x] || x;
    if (Array.isArray(x)) return x.map(walk);
    if (x && typeof x === "object") {
      const o = {};
      for (const k of Object.keys(x).sort()) if (k !== "description") o[k] = walk(x[k]);
      return o;
    }
    return x;
  };
  return walk(v);
}
const answer = (r) => (r.error ? { error: r.error } : { result: r.result });

// the same call on both, with `@me` standing for this side's own fingerprint and `@rev` for its current revision
async function both(label, method, build) {
  const out = {};
  for (const [side, c] of [["real", real], ["fake", fake]]) {
    const cur = (await c.call("layout.get")).result;
    const me = cur.peers.find((p) => p.self).fingerprint;
    out[side] = answer(await c.call(method, build({ me, rev: cur.revision, cur })));
  }
  const a = JSON.stringify(norm("real", out.real)), b = JSON.stringify(norm("fake", out.fake));
  ok(label, a === b, `\n    real ${a}\n    fake ${b}`);
  return out;
}

// ------------------------------------------------------------------------------- the run

console.log("REAL hello: " + real.hello.version + " (real), " + fake.hello.version + " (fake)");
ok("the real daemon advertises layout.set and subscribe", real.hello.methods.includes("layout.set") && real.hello.methods.includes("subscribe"), JSON.stringify(real.hello.methods));

// make the fake's machines look like the real ones: the same monitors, from the real daemon's own answer
const first = await learn("real", real);
const realSelf = first.peers.find((p) => p.self);
const monitorsOf = (p) => p.monitors.map((m) => ({ name: m.name, x: m.x - p.bounds.x, y: m.y - p.bounds.y, w: m.w, h: m.h, scale: m.scale, transform: m.transform, description: m.description }));
await fake.call("fake.fixture", { name: "pair" });
for (const p of first.peers) await fake.call("fake.monitors", { peer: p.name, monitors: monitorsOf(p) });
await sleep(100);
await learn("fake", fake);

const same = JSON.stringify(norm("real", first)) === JSON.stringify(norm("fake", (await fake.call("layout.get")).result));
ok("a fresh pair looks the same on both: this machine unplaced, rev 0, monitors known", same, JSON.stringify(norm("real", first)) + "\n    " + JSON.stringify(norm("fake", (await fake.call("layout.get")).result)));

const width = realSelf.bounds.w, height = realSelf.bounds.h;
console.log(`REAL monitors: ${first.peers[0].monitors.map((m) => `${m.name} ${m.w}x${m.h}`).join(", ")} (group ${width}x${height})`);

// ---- the scripted cases, in the order a user meets them
await both("first placement: this machine at 0,0 (it was unplaced)", "layout.set", () => ({ origins: [{ peer: "dev-a", x: 0, y: 0 }] }));
await both("placing the other peer next to it", "layout.set", () => ({ origins: [{ peer: "dev-b", x: width, y: 0 }] }));
await both("overlap: the moved peer is named first", "layout.set", () => ({ origins: [{ peer: "dev-b", x: 100, y: 0 }] }));
await both("overlap when this machine is the one that moves", "layout.set", () => ({ origins: [{ peer: "dev-a", x: 100, y: 0 }] }));
await both("the same origins again change nothing, spend no revision", "layout.set", () => ({ origins: [{ peer: "dev-b", x: width, y: 0 }] }));
await both("two peers in one revision", "layout.set", () => ({ origins: [{ peer: "dev-a", x: 0, y: 100 }, { peer: "dev-b", x: width, y: 100 }] }));
await both("by fingerprint, with a base_rev that is current", "layout.set", ({ rev, cur }) => ({ origins: [{ peer: cur.peers[1].fingerprint, x: width, y: 160 }], base_rev: rev }));
await both("by a prefix of 8 hex digits", "layout.set", ({ cur }) => ({ origins: [{ peer: cur.peers[1].fingerprint.slice(0, 8), x: width, y: 120 }] }));
await both("a stale base_rev is refused", "layout.set", ({ me }) => ({ origins: [{ peer: "dev-b", x: width, y: 0 }], base_rev: { counter: 1, peer: me } }));
await both("a base_rev that is not a fingerprint", "layout.set", () => ({ origins: [{ peer: "dev-b", x: width, y: 0 }], base_rev: { counter: 1, peer: "x" } }));
await both("an empty origins", "layout.set", () => ({ origins: [] }));
await both("a peer named twice", "layout.set", () => ({ origins: [{ peer: "dev-b", x: 1, y: 1 }, { peer: "dev-b", x: 2, y: 2 }] }));
await both("a peer named by name and by fingerprint is named twice", "layout.set", ({ cur }) => ({ origins: [{ peer: "dev-b", x: 1, y: 1 }, { peer: cur.peers[1].fingerprint, x: 2, y: 2 }] }));
await both("a peer that does not exist", "layout.set", () => ({ origins: [{ peer: "nobody", x: 1, y: 1 }] }));
await both("a fingerprint prefix that matches nobody", "layout.set", () => ({ origins: [{ peer: "abcdef0123", x: 1, y: 1 }] }));
await both("an origin past the cap", "layout.set", () => ({ origins: [{ peer: "dev-b", x: 20000000, y: 0 }] }));
await both("an origin past the cap, negative", "layout.set", () => ({ origins: [{ peer: "dev-b", x: 0, y: -20000000 }] }));
await both("a check order: bad params beat a stale revision", "layout.set", ({ me }) => ({ origins: [], base_rev: { counter: 1, peer: me } }));
await both("a check order: a stale revision beats an overlap", "layout.set", ({ me }) => ({ origins: [{ peer: "dev-b", x: 100, y: 0 }], base_rev: { counter: 1, peer: me } }));

// ---- 150 random placements: both daemons, and the plugin's own maths against the real one
function rand(seed) {
  let a = seed >>> 0;
  return () => { a = (a + 0x6d2b79f5) >>> 0; let t = a; t = Math.imul(t ^ (t >>> 15), t | 1); t ^= t + Math.imul(t ^ (t >>> 7), t | 61); return ((t ^ (t >>> 14)) >>> 0) / 4294967296; };
}
const R = rand(20261021);
const pick = (xs) => xs[Math.floor(R() * xs.length)];
const coords = (extent) => [0, extent, -extent, Math.floor(extent / 2), -Math.floor(extent / 2), extent + 37, 100, -100, 1, 2 * extent, -2 * extent, 17];

let overlapsSeen = 0, appliedSeen = 0, linksSeen = 0, predicted = 0;
for (let i = 0; i < 150; i++) {
  const kind = R();
  const origins = [];
  if (kind < 0.55) origins.push({ peer: "dev-b", x: pick(coords(width)), y: pick(coords(height)) });
  else if (kind < 0.7) origins.push({ peer: "dev-a", x: pick(coords(width)), y: pick(coords(height)) });
  else {
    origins.push({ peer: "dev-a", x: pick(coords(width)), y: pick(coords(height)) });
    origins.push({ peer: "dev-b", x: pick(coords(width)), y: pick(coords(height)) });
  }

  // what Geometry predicts, from the real daemon's own snapshot, before asking it
  const before = (await real.call("layout.get")).result;
  const meFp = before.peers.find((p) => p.self).fingerprint;
  const { groups } = A.groupsOf(before, meFp);
  const fpOf = (n) => before.peers.find((p) => p.name === n).fingerprint;
  const draft = origins.reduce((d, o) => ({ ...d, origins: { ...d.origins, [fpOf(o.peer)]: [o.x, o.y] } }), A.emptyDraft());
  // this machine gets 0,0 the first time anything is placed (Layout::with_origins)
  if (!groups.find((g) => g.self).placed && !draft.origins[meFp]) draft.origins[meFp] = [0, 0];
  const eff = A.effectiveGroups(groups, draft, null).map((g) => (draft.origins[g.fp] ? { ...g, placed: true } : g));
  const moved = origins.map((o) => fpOf(o.peer));
  const clash = G.firstOverlap(eff.filter((g) => g.placed), moved);
  const unchanged = origins.every((o) => {
    const g = groups.find((x) => x.fp === fpOf(o.peer));
    return g.placed && g.origin[0] === o.x && g.origin[1] === o.y;
  }) && groups.find((g) => g.self).placed;

  const out = await both(`random ${i}: ${origins.map((o) => `${o.peer}@${o.x},${o.y}`).join(" ")}`, "layout.set", () => ({ origins }));
  const r = out.real;
  const expected = clash ? `${before.peers.find((p) => p.fingerprint === clash.moved).name} overlaps ${before.peers.find((p) => p.fingerprint === clash.other).name}` : null;
  if (clash) {
    overlapsSeen++;
    ok(`  Geometry predicted the refusal: ${expected}`, r.error && r.error.code === "overlap" && r.error.message === expected, JSON.stringify(r));
  } else if (r.result) {
    predicted++;
    ok("  Geometry predicted that it applies", !!r.result, JSON.stringify(r));
    if (!unchanged) appliedSeen++;
    ok("  the revision moved by exactly one, or not at all when nothing changed", r.result.revision.counter === before.revision.counter + (unchanged ? 0 : 1), `${before.revision.counter} -> ${r.result.revision.counter}`);
    // the links the real daemon lists are the ones Geometry derives
    const after = A.groupsOf(r.result, meFp).groups.filter((g) => g.trusted && g.placed);
    const nameOf = (fp) => r.result.peers.find((p) => p.fingerprint === fp).name;
    const ours = G.deriveLinks(after).map((l) => `${nameOf(l.from.fp)}:${l.from.name}|${l.side}|${nameOf(l.to.fp)}:${l.to.name}|${l.span}`).sort();
    const theirs = r.result.links.map((l) => `${l.from}|${l.side}|${l.to}|${l.span}`).sort();
    if (theirs.length) linksSeen++;
    ok("  derive_links equals ferryd's links", JSON.stringify(ours) === JSON.stringify(theirs), `\n    ours   ${ours.join("  ")}\n    theirs ${theirs.join("  ")}`);
  } else {
    ok("  Geometry predicted no refusal, ferryd refused", false, JSON.stringify(r));
  }
}
ok(`the random run covered refusals (${overlapsSeen}), edits (${appliedSeen}) and layouts with links (${linksSeen})`, overlapsSeen > 15 && appliedSeen > 30 && linksSeen > 8, `${overlapsSeen}/${appliedSeen}/${linksSeen}`);

// ---- the layout event
const watcher = client(realPath);
await sleep(200);
await watcher.call("subscribe", { topics: ["layout"] });
const snap = await watcher.event("layout", 0);
ok("subscribing sends the layout snapshot at once", !!snap && snap.data.revision.counter >= 1, JSON.stringify(snap && snap.data.revision));
await real.call("layout.set", { origins: [{ peer: "dev-a", x: 0, y: 0 }, { peer: "dev-b", x: width, y: 0 }] });
await sleep(200);
const mark = watcher.events.length;
const result = (await real.call("layout.set", { origins: [{ peer: "dev-b", x: width, y: 7 }] })).result;
ok("a change that applies", !!result && result.revision.counter > snap.data.revision.counter, JSON.stringify(result));
const ev = await watcher.event("layout", mark);
ok("a change is a layout event, and the event is the snapshot the call answered", !!ev && JSON.stringify(norm("real", ev.data)) === JSON.stringify(norm("real", result)), JSON.stringify(ev && ev.data.revision));
const mark2 = watcher.events.length;
await real.call("layout.set", { origins: [{ peer: "dev-b", x: width, y: 7 }] });
ok("a call that changes nothing sends no event", (await watcher.event("layout", mark2, 500)) === null, "");
watcher.close();

console.log(`REAL ${failures === 0 ? "PASS" : "FAIL"} ${checks - failures}/${checks}`);
real.close();
fake.close();
process.exit(failures === 0 ? 0 : 1);
