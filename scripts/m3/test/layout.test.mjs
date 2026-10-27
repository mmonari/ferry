import { test, before, after } from "node:test";
import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { mkdtempSync, rmSync, existsSync } from "node:fs";
import { createConnection } from "node:net";
import { tmpdir } from "node:os";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { load } from "./harness.mjs";

// The fake daemon is what every UI view is built and photographed against, so its layout has to behave
// like the real one: the answers of `layout.set` (crates/ferryd/tests/snapshots/api__layout_set_*), the
// order of the checks, the `layout` events, and links equal to what ferry_core::geom derives. This
// file holds the fake to that, and holds plugin/Geometry.js to the fake: the same links from two
// implementations in two languages. A run against the real daemon is scripts/m3/test/real-layout.sh.

const FAKE = join(dirname(fileURLToPath(import.meta.url)), "..", "fake-ferryd.py");
const G = load("Geometry.js");
const A = load("Arrange.js");

const FIXTURES = ["empty", "pair", "two", "three", "four", "hidpi", "stack", "untrusted", "unplaced", "lonely", "shifted"];
const dump = (name) => JSON.parse(spawnSync("python3", ["-I", FAKE, "--dump-layout", name], { encoding: "utf8" }).stdout);

// ------------------------------------------------------------------------ links, two languages

test("the fake's links and plugin/Geometry.js's derive_links agree on every fixture", () => {
  for (const name of FIXTURES) {
    const layout = dump(name);
    const { groups } = A.groupsOf(layout, layout.peers[0].fingerprint);
    // the daemon derives links over this machine and the peers paired here, placed
    const taking = groups.filter((g) => g.trusted && g.placed);
    const ours = G.deriveLinks(taking).map((l) => ({ from: `${taking.find((g) => g.fp === l.from.fp).name}:${l.from.name}`, side: l.side, to: `${taking.find((g) => g.fp === l.to.fp).name}:${l.to.name}`, span: l.span }));
    const theirs = layout.links.map((l) => ({ from: l.from, side: l.side, to: l.to, span: l.span }));
    assert.deepEqual(ours, theirs, name);
  }
});

test("every fixture is a valid layout: no overlap, bounds that match the monitors, and the cases it was made for", () => {
  for (const name of FIXTURES) {
    const layout = dump(name);
    const { groups } = A.groupsOf(layout, layout.peers[0].fingerprint);
    const eff = A.effectiveGroups(groups, A.emptyDraft(), null);
    assert.deepEqual(A.analyse(eff).overlaps, [], name);
    layout.peers.forEach((p) => {
      if (!p.monitors.length) return;
      const b = G.bounding(p.monitors.map((m) => G.rect(m.x, m.y, m.w, m.h)));
      assert.deepEqual(p.bounds, b, `${name}/${p.name}`);
    });
  }
  const count = (n) => dump(n).peers.length;
  assert.deepEqual(["empty", "two", "three", "four"].map(count), [1, 2, 3, 4]);
  assert.equal(dump("four").peers.length, 4, "the most a layout holds");
});

test("the fixtures cover what the panel must draw: HiDPI, a vertical stack, an unpaired peer, a peer not placed, a shift", () => {
  const hidpi = dump("hidpi");
  assert.deepEqual([...new Set(hidpi.peers.flatMap((p) => p.monitors.map((m) => m.scale)))].sort(), [1, 1.25, 1.6, 2]);
  const stack = A.groupsOf(dump("stack"), null).groups;
  const lap = stack.find((g) => g.name === "lap"), desk = stack.find((g) => g.name === "desk"), tower = stack.find((g) => g.name === "tower");
  assert.ok(lap.rect.y + lap.rect.h === desk.rect.y && tower.rect.y === desk.rect.y + desk.rect.h, "lap over desk over tower");
  assert.equal(tower.monitors.length, 2);
  assert.ok(tower.monitors[0].x === tower.monitors[1].x && tower.monitors[1].y === tower.monitors[0].y + 1080, "tower's own monitors are stacked");
  const un = dump("untrusted").peers.find((p) => p.trusted === false);
  assert.match(un.name, /^[0-9a-f]{8}$/, "a peer that is not paired here is named by its fingerprint");
  assert.equal(dump("unplaced").peers.find((p) => p.name === "lap").origin, null);
  const shifted = dump("shifted").peers.find((p) => p.name === "lap");
  assert.deepEqual([shifted.origin, shifted.shift, shifted.bounds.x], [[-2800, 0], [1000, 0], -1800]);
  assert.equal(dump("empty").peers[0].origin, null, "a fresh daemon lists this machine unplaced");
});

// ----------------------------------------------------------------------------- a live fake

let dir, proc, sock;

function client(path) {
  const c = createConnection(path);
  let buf = "";
  let nextId = 1;
  const pending = new Map();
  const events = [];
  const waiters = [];
  c.setEncoding("utf8");
  c.on("data", (d) => {
    buf += d;
    let i;
    while ((i = buf.indexOf("\n")) >= 0) {
      const line = buf.slice(0, i);
      buf = buf.slice(i + 1);
      const msg = JSON.parse(line);
      if (msg.event !== undefined) {
        events.push(msg);
        waiters.splice(0).forEach((w) => w());
      } else if (pending.has(msg.id)) {
        pending.get(msg.id)(msg);
        pending.delete(msg.id);
      }
    }
  });
  return {
    call: (method, params) => new Promise((resolve) => {
      const id = nextId++;
      pending.set(id, resolve);
      c.write(JSON.stringify({ id, method, params }) + "\n");
    }),
    events,
    // the next event of this name after `from` (an index into events), or null after `ms`. A wait for an event that
    // should come returns as soon as it does, so the default is long: a gate run next to cargo builds is slow.
    event: (name, from = 0, ms = 10000) => new Promise((resolve) => {
      const find = () => events.slice(from).find((e) => e.event === name) || null;
      if (find()) return resolve(find());
      const t = setTimeout(() => resolve(find()), ms);
      waiters.push(() => { if (find()) { clearTimeout(t); resolve(find()); } });
      // keep waiting on later events
      const again = () => { if (!find()) waiters.push(again); };
      waiters.push(again);
    }),
    close: () => c.destroy()
  };
}

before(async () => {
  dir = mkdtempSync(join(tmpdir(), "ferry-m3-layout-"));
  sock = join(dir, "run", "ferry", "ferryd.sock");
  proc = spawn("python3", ["-I", FAKE, "--socket", sock, "--fixture", "three", "--scene", "ready"], { stdio: "ignore" });
  for (let i = 0; i < 300 && !existsSync(sock); i++) await new Promise((r) => setTimeout(r, 50));
  assert.ok(existsSync(sock), "the fake did not start");
});

after(() => {
  if (proc) proc.kill();
  if (dir) rmSync(dir, { recursive: true, force: true });
});

const FP = (name) => dump("three").peers.find((p) => p.name === name).fingerprint;

async function fresh(fixture = "three") {
  const c = client(sock);
  await c.call("fake.fixture", { name: fixture });
  return c;
}

test("layout.set that overlaps is refused with ferryd's words, names the moved peer first, and changes nothing", async () => {
  const c = await fresh();
  const before = (await c.call("layout.get")).result;
  const r = await c.call("layout.set", { origins: [{ peer: "lap", x: 100, y: 0 }] });
  assert.deepEqual(r.error, { code: "overlap", message: "lap overlaps desk", hint: "ferry layout show lists where each peer's monitors are" });
  assert.deepEqual((await c.call("layout.get")).result, before);
  const rev = await c.call("layout.set", { origins: [{ peer: "desk", x: -1000, y: 0 }] });
  assert.equal(rev.error.message, "desk overlaps lap", "the moved peer is named first, whoever it is");
  c.close();
});

test("layout.set moves any number of groups in one revision, and the layout event carries it", async () => {
  const c = await fresh();
  await c.call("subscribe", { topics: ["layout"] });
  const first = await c.event("layout");
  assert.ok(first, "a snapshot at once on subscribe");
  const mark = c.events.length;
  const r = await c.call("layout.set", { origins: [{ peer: "lap", x: -1800, y: 300 }, { peer: "tower", x: 3840, y: 0 }], base_rev: first.data.revision });
  assert.equal(r.result.revision.counter, 15);
  assert.equal(r.result.revision.peer, FP("desk"), "stamped with this peer");
  const origin = (name) => r.result.peers.find((p) => p.name === name).origin;
  assert.deepEqual([origin("lap"), origin("tower"), origin("desk")], [[-1800, 300], [3840, 0], [0, 0]]);
  const ev = await c.event("layout", mark);
  assert.ok(ev, "a layout event follows");
  assert.deepEqual(ev.data, r.result, "the event is the same snapshot the call answered");
  c.close();
});

test("a layout.set that changes nothing spends no revision and sends no event", async () => {
  const c = await fresh();
  await c.call("subscribe", { topics: ["layout"] });
  await c.event("layout");
  const mark = c.events.length;
  const r = await c.call("layout.set", { origins: [{ peer: "lap", x: -1800, y: 0 }] });
  assert.equal(r.result.revision.counter, 14);
  assert.equal(await c.event("layout", mark, 300), null);
  c.close();
});

test("base_rev: a stale revision is refused and nothing changes; the current one, and none, are accepted", async () => {
  const c = await fresh();
  const rev = (await c.call("layout.get")).result.revision;
  await c.call("fake.layout_by", { peer: "tower", x: 3840, y: 100, by: "tower" });
  const stale = await c.call("layout.set", { origins: [{ peer: "lap", x: -1800, y: 300 }], base_rev: rev });
  assert.equal(stale.error.code, "stale_layout");
  assert.equal(stale.error.message, "the layout changed (rev 15 by tower)");
  assert.equal(stale.error.hint, "read the layout again with layout.get, then apply your change on top of it");
  const now = (await c.call("layout.get")).result;
  assert.equal(now.peers.find((p) => p.name === "lap").origin[1], 0, "the stale edit changed nothing");
  const kept = await c.call("layout.set", { origins: [{ peer: "lap", x: -1800, y: 300 }], base_rev: now.revision });
  assert.equal(kept.result.revision.counter, 16, "Keep mine: the same edit on the new revision");
  const blind = await c.call("layout.set", { origins: [{ peer: "lap", x: -1800, y: 400 }] });
  assert.equal(blind.result.revision.counter, 17, "no base_rev: applied on whatever is there");
  c.close();
});

test("the checks run in ferryd's order: the params first, then base_rev, then the overlap", async () => {
  const c = await fresh();
  const stale = { counter: 3, peer: FP("lap") };
  // a bad param beats a stale revision
  assert.equal((await c.call("layout.set", { origins: [], base_rev: stale })).error.code, "bad_params");
  // a stale revision beats an overlap
  assert.equal((await c.call("layout.set", { origins: [{ peer: "lap", x: 100, y: 0 }], base_rev: stale })).error.code, "stale_layout");
  c.close();
});

test("bad params are named as ferryd names them", async () => {
  const c = await fresh();
  const err = async (params) => (await c.call("layout.set", params)).error;
  assert.deepEqual(await err({ origins: [] }), { code: "bad_params", message: "origins is empty", hint: "name at least one peer with its x and y" });
  assert.deepEqual(await err({ origins: [{ peer: "lap", x: 1, y: 1 }, { peer: "lap", x: 2, y: 2 }] }), { code: "bad_params", message: "lap is named twice", hint: "name each peer once" });
  assert.equal((await err({ origins: [{ peer: "lap", x: 1, y: 1 }, { peer: FP("lap"), x: 2, y: 2 }] })).message, "lap is named twice", "a name and its fingerprint are the same peer");
  assert.equal((await err({ origins: [{ peer: "nobody", x: 1, y: 1 }] })).code, "unknown_peer");
  assert.equal((await err({ origins: [{ peer: "lap", x: 1.5, y: 1 }] })).code, "bad_params");
  assert.equal((await err({ origins: [{ peer: "lap", x: 20000000, y: 1 }] })).message, "lap: x and y are at most 16777216 px");
  assert.equal((await err({ origins: [{ peer: "lap", x: 0, y: 0 }], base_rev: { counter: 14, peer: "x" } })).code, "bad_params", "base_rev.peer is a fingerprint");
  c.close();
});

test("peers are named by name or by fingerprint, this machine too", async () => {
  const c = await fresh();
  const byFp = await c.call("layout.set", { origins: [{ peer: FP("lap"), x: -1800, y: 40 }] });
  assert.equal(byFp.result.peers.find((p) => p.name === "lap").origin[1], 40);
  const byPrefix = await c.call("layout.set", { origins: [{ peer: FP("tower").slice(0, 8), x: 3840, y: 40 }] });
  assert.equal(byPrefix.result.peers.find((p) => p.name === "tower").origin[1], 40);
  const me = await c.call("layout.set", { origins: [{ peer: FP("desk"), x: 0, y: 20 }, { peer: "lap", x: -1800, y: 40 }] });
  assert.ok(me.result, JSON.stringify(me));
  assert.deepEqual(me.result.peers[0].origin, [0, 20], "this machine's origin is editable too");
  c.close();
});

test("a peer that is only placed is listed after the paired ones, by fingerprint, and can be moved by it", async () => {
  const c = await fresh("untrusted");
  const l = (await c.call("layout.get")).result;
  assert.deepEqual(l.peers.map((p) => [p.trusted, p.self]), [[true, true], [true, false], [false, false]]);
  const third = l.peers[2];
  assert.match(third.name, /^[0-9a-f]{8}$/);
  assert.equal(third.online, false);
  assert.deepEqual(l.links.map((k) => k.to).concat(l.links.map((k) => k.from)).filter((n) => n.startsWith(third.name)), [], "no link to a peer that is not paired here");
  const moved = await c.call("layout.set", { origins: [{ peer: third.fingerprint.slice(0, 12), x: 3840, y: 200 }] });
  assert.deepEqual(moved.result.peers[2].origin, [3840, 200]);
  c.close();
});

test("a peer the layout has not placed is placed by layout.set; this machine is placed at 0,0 with it", async () => {
  const c = await fresh("unplaced");
  const l = (await c.call("layout.get")).result;
  assert.equal(l.peers.find((p) => p.name === "lap").origin, null);
  assert.deepEqual(l.links.length, 2 - 0, "only the desk-tower link: lap is not placed");
  const r = await c.call("layout.set", { origins: [{ peer: "lap", x: -1800, y: 0 }] });
  assert.deepEqual(r.result.peers.find((p) => p.name === "lap").origin, [-1800, 0]);
  assert.equal(r.result.revision.counter, 4);
  assert.equal(r.result.links.length, 4, "now it links to desk as well");
  c.close();
  const e = await fresh("empty");
  assert.equal((await e.call("layout.get")).result.peers[0].origin, null);
  const p = await e.call("layout.set", { origins: [{ peer: "desk", x: 0, y: 0 }] });
  assert.deepEqual(p.result.peers[0].origin, [0, 0]);
  assert.equal(p.result.revision.counter, 1);
  e.close();
});

test("a layout holds four peers at most", async () => {
  const c = await fresh("four");
  const fp = "e".repeat(64);
  const added = await c.call("trust", { fingerprint: fp, name: "fifth", addr: "198.51.100.99:4243" });
  assert.ok(added.result, JSON.stringify(added));
  const r = await c.call("layout.set", { origins: [{ peer: "fifth", x: 9000, y: 0 }] });
  assert.deepEqual(r.error, { code: "bad_params", message: "a layout holds at most 4 peers", hint: "unpair a peer you no longer use" });
  c.close();
});

test("an anchor shift stays out of the origin: the monitors sit at local + shift + origin", async () => {
  const c = await fresh("shifted");
  const got = await c.call("layout.get");
  assert.ok(got.result, JSON.stringify(got));
  const lap = got.result.peers.find((p) => p.name === "lap");
  assert.deepEqual([lap.origin, lap.shift, lap.monitors[0].x], [[-2800, 0], [1000, 0], -1800]);
  const r = await c.call("layout.set", { origins: [{ peer: "lap", x: -2900, y: 0 }] });
  assert.ok(r.result, JSON.stringify(r));
  const moved = r.result.peers.find((p) => p.name === "lap");
  assert.deepEqual([moved.origin, moved.shift, moved.monitors[0].x, moved.bounds.x], [[-2900, 0], [1000, 0], -1900, -1900]);
  // the same origin that the daemon lists puts lap's right edge on desk's left edge, so it is the plane position that links
  assert.equal((await c.call("layout.set", { origins: [{ peer: "lap", x: -2800, y: 0 }] })).result.links.length, 2);
  c.close();
});

test("an offline peer's links are listed, but not live", async () => {
  const c = await fresh();
  await c.call("fake.peer", { name: "lap", state: "offline" });
  const l = (await c.call("layout.get")).result;
  assert.equal(l.peers.find((p) => p.name === "lap").online, false);
  assert.deepEqual(l.links.map((k) => k.live), [false, false, true, true]);
  c.close();
});

test("fake.layout_by is another peer's edit: the revision moves on, stamped with its author", async () => {
  const c = await fresh();
  const r = await c.call("fake.layout_by", { peer: "lap", x: -1800, y: 200, by: "tower" });
  assert.deepEqual(r.result.revision, { counter: 15, peer: FP("tower") });
  const l = (await c.call("layout.get")).result;
  assert.deepEqual(l.revision, { counter: 15, peer: FP("tower") });
  assert.equal((await c.call("fake.layout_by", { peer: "lap", x: 100, y: 0 })).error.code, "overlap");
  c.close();
});
