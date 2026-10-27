import { test, before, after } from "node:test";
import assert from "node:assert/strict";
import { startFake, sleep } from "./fakeclient.mjs";

// The fake daemon is what every UI picture and every QML test stands on, so what it says about
// pairing, renaming and notices is checked here against what ferryd says (stream B's snapshots in
// crates/ferry-core/src/api/snapshots, and `crates/ferryd/tests/pair_events.rs`).

let fake;
before(async () => { fake = await startFake(["--pair-window-s", "3"]); });
after(() => fake.stop());

const FAST = { connect_ms: 50, other_ms: 120, dial_delay_ms: 30 };

// a client that has subscribed, and a clean daemon
async function session(extra = {}) {
  const c = await fake.connect();
  await c.until((e) => e.event === "hello");
  await c.call("fake.reset");
  await c.call("fake.pair_next", { ...FAST, ...extra });
  const sub = await c.call("subscribe");
  assert.ok(sub.result, JSON.stringify(sub));
  await sleep(120);
  return c;
}

const pairing = (c, from) => c.events.slice(from).filter((e) => e.event === "pairing").map((e) => e.data);

// ------------------------------------------------------------------------------ the dialer

test("a dialer goes connecting, code, waiting_peer, paired, and the peer is pinned but not placed", async () => {
  const c = await session({ other_ms: 600 });
  const at = c.mark();
  const started = await c.call("pair.start", { addr: "198.51.100.88" });
  assert.deepEqual(started.result, {});
  const code = await c.stage("code", 3000, at);
  assert.equal(code.data.role, "dialer");
  assert.match(code.data.code, /^\d{6}$/);
  assert.equal(code.data.peer.name, "workshop");
  assert.equal(code.data.peer.addr, "198.51.100.88:4243");
  assert.ok(code.data.expires_ms > 2000 && code.data.expires_ms <= 3000, "the window is what --pair-window-s said");

  assert.deepEqual((await c.call("pair.answer", { match: true })).result, {});
  const paired = await c.stage("paired", 3000, at);
  assert.equal(paired.data.peer.name, "workshop");
  assert.equal(paired.data.code, undefined, "the code is never in an event after the code stage");
  assert.deepEqual(c.stages(at), ["connecting", "code", "waiting_peer", "paired"]);

  const peers = (await c.call("peers")).result.peers;
  assert.ok(peers.some((p) => p.name === "workshop" && p.paired === "sas"));
  const layout = (await c.call("layout.get")).result;
  assert.equal(layout.peers.find((p) => p.name === "workshop").origin, null, "ferryd places nobody");
  const status = (await c.call("status")).result;
  assert.equal(status.pairing, undefined, "a finished pairing leaves status");
  assert.ok(status.attention.some((a) => a.code === "layout_unplaced" && a.peer === "workshop"));
  assert.equal(status.summary.state, "attention");
  const notices = c.notices(at);
  assert.deepEqual(notices.map((n) => n.kind), ["paired"]);
  assert.equal(notices[0].title, "Paired with workshop");
  c.close();
});

test("when the other person confirms first there is no waiting_peer, as in ferryd", async () => {
  const c = await session({ other_ms: 50, connect_ms: 50 });
  const at = c.mark();
  await c.call("pair.start", { addr: "198.51.100.89", name: "studio" });
  await c.stage("code", 3000, at);
  await sleep(300);  // the other side has confirmed by now
  await c.call("pair.answer", { match: true });
  const paired = await c.stage("paired", 3000, at);
  assert.equal(paired.data.peer.name, "studio", "--name pins the peer under that name");
  assert.deepEqual(c.stages(at), ["connecting", "code", "paired"]);
  c.close();
});

test("the other machine says the codes differ: failed, rejected, nothing pinned, a cancelled toast", async () => {
  const c = await session({ other: "mismatch" });
  const at = c.mark();
  await c.call("pair.start", { addr: "198.51.100.90" });
  const failed = await c.stage("failed", 3000, at);
  assert.equal(failed.data.reason, "rejected");
  assert.equal(failed.data.message, "cancelled: codes differ");
  assert.equal(failed.data.peer.name, "workshop");
  assert.equal(failed.data.role, "dialer");
  assert.ok(!(await c.call("peers")).result.peers.some((p) => p.name === "workshop"));
  assert.equal((await c.call("status")).result.pairing, undefined, "a dialer is over after a failure");
  const [n] = c.notices(at);
  assert.equal(n.kind, "pair_failed");
  assert.equal(n.title, "Pairing cancelled");
  assert.equal(n.body, "Codes differ");
  c.close();
});

test("answering that the codes differ fails the pairing with the same reason", async () => {
  const c = await session({ other_ms: 5000 });
  const at = c.mark();
  await c.call("pair.start", { addr: "198.51.100.91" });
  await c.stage("code", 3000, at);
  await c.call("pair.answer", { match: false });
  const failed = await c.stage("failed", 3000, at);
  assert.equal(failed.data.reason, "rejected");
  c.close();
});

test("a silent other machine lets the window run out: failed, timeout, 'timed out'", async () => {
  const c = await session({ other: "silent", window_s: 0.8 });
  const at = c.mark();
  await c.call("pair.start", { addr: "198.51.100.92" });
  await c.stage("code", 3000, at);
  await c.call("pair.answer", { match: true });
  const failed = await c.stage("failed", 3000, at);
  assert.equal(failed.data.reason, "timeout");
  assert.equal(failed.data.message, "timed out");
  assert.deepEqual(c.stages(at), ["connecting", "code", "waiting_peer", "failed"]);
  c.close();
});

test("each way the other side can end it has ferryd's reason and words", async () => {
  const expected = {
    cancel: ["cancelled", "cancelled"],
    drop: ["lost", "connection lost"],
    version: ["version", "version mismatch: 1.0 vs 2.0"],
    bad_reveal: ["bad_reveal", "cancelled: the peer broke the protocol (its reveal does not match its commitment)"],
    protocol: ["protocol", "cancelled: the peer broke the protocol"]
  };
  for (const [other, [reason, message]] of Object.entries(expected)) {
    const c = await session({ other });
    const at = c.mark();
    await c.call("pair.start", { addr: "198.51.100.93" });
    const failed = await c.stage("failed", 3000, at);
    assert.equal(failed.data.reason, reason, other);
    assert.equal(failed.data.message, message, other);
    c.close();
  }
});

test("pair.cancel from another connection ends a dialer, and the opener hears a local cancel, as in ferryd", async () => {
  const c = await session({ other_ms: 5000 });
  const other = await fake.connect();
  await other.until((e) => e.event === "hello");
  const at = c.mark();
  await c.call("pair.start", { addr: "198.51.100.94" });
  await c.stage("code", 3000, at);
  assert.deepEqual((await other.call("pair.cancel")).result, {});
  const failed = await c.stage("failed", 3000, at);
  assert.equal(failed.data.reason, "cancelled");
  assert.equal(failed.data.cancelled_by, "local");
  assert.equal((await c.call("pair.cancel")).error.message, "no pairing runs on this machine");
  assert.equal((await other.call("pair.cancel")).error.code, "no_pairing", "and there is nothing left to cancel");
  other.close();
  c.close();
});

test("a dial that fails answers with an error, and the subscribers hear a failed event that says why", async () => {
  const c = await session();
  const at = c.mark();
  const r = await c.call("pair.start", { addr: "10.255.0.9" });
  assert.equal(r.error.code, "peer_unreachable");
  assert.match(r.error.message, /10\.255\.0\.9 did not answer/);
  assert.match(r.error.hint, /firewall/);
  const end = await c.stage("failed", 3000, at);
  assert.equal(end.data.reason, "unreachable");
  assert.deepEqual(c.stages(at), ["connecting", "failed"], "connecting, then one last event");
  assert.equal((await c.call("status")).result.pairing, undefined, "but status has no pairing");

  const at2 = c.mark();
  const closed = await c.call("pair.start", { addr: "10.254.0.9" });
  assert.equal(closed.error.code, "pairing_not_accepting");
  assert.match(closed.error.message, /is not accepting pairing/);
  assert.equal((await c.stage("failed", 3000, at2)).data.reason, "not_accepting");

  const own = await c.call("pair.start", { addr: "192.0.2.111" });
  assert.equal(own.error.code, "bad_params");
  assert.match(own.error.message, /is this machine/);
  c.close();
});

test("pair.start refuses what ferryd refuses before it dials", async () => {
  const c = await session();
  assert.equal((await c.call("pair.start", { addr: "two words" })).error.code, "bad_params");
  assert.equal((await c.call("pair.start", { addr: "" })).error.code, "bad_params");
  assert.equal((await c.call("pair.start", { addr: "host:70000" })).error.code, "bad_params");
  const name = await c.call("pair.start", { addr: "198.51.100.95", name: "has space" });
  assert.equal(name.error.code, "bad_params");
  assert.match(name.error.message, /cannot contain/);
  assert.equal((await c.call("status")).result.pairing, undefined);
  c.close();
});

test("a second pairing is refused while one runs, with ferryd's busy error", async () => {
  const c = await session({ other_ms: 5000 });
  await c.call("pair.start", { addr: "198.51.100.96" });
  const again = await c.call("pair.start", { addr: "198.51.100.97" });
  assert.equal(again.error.code, "pairing_busy");
  assert.equal((await c.call("pair.accept")).error.code, "pairing_busy");
  assert.equal(again.error.message, "a pairing already runs on this machine");
  await c.call("pair.cancel");
  c.close();
});

test("the opener closing its connection cancels the pairing", async () => {
  const opener = await session({ other_ms: 5000 });
  const watcher = await session({ other_ms: 5000 });
  const at = watcher.mark();
  await opener.call("pair.start", { addr: "198.51.100.98" });
  await watcher.stage("code", 3000, at);
  opener.close();
  const failed = await watcher.stage("failed", 3000, at);
  assert.equal(failed.data.reason, "cancelled");
  watcher.close();
});

// ----------------------------------------------------------------------------- the acceptor

test("an acceptor listens, hears the code with a critical toast, and any connection may answer: the first wins", async () => {
  const panel = await session();
  const terminal = await fake.connect();
  await terminal.until((e) => e.event === "hello");
  await terminal.call("subscribe", { topics: ["pairing", "notice"] });
  const at = panel.mark();
  const tat = terminal.mark();

  const opened = await panel.call("pair.accept");
  assert.equal(opened.result.expires_ms, 3000);
  const listening = await panel.stage("listening", 2000, at);
  assert.equal(listening.data.role, "acceptor");
  assert.deepEqual(c(panel.stages(at)), ["listening"], "the opener hears it once, through its subscription");

  await panel.call("fake.pair_arrive", { name: "tower2", addr: "198.51.100.60:4243" });
  const code = await panel.stage("code", 3000, at);
  assert.equal(code.data.role, "acceptor");
  assert.equal(code.data.peer.name, "tower2");
  const tcode = await terminal.stage("code", 3000, tat);
  assert.equal(tcode.data.code, code.data.code, "the same code reaches every subscriber");
  const toast = (await panel.until((e) => e.event === "notice" && e.data.kind === "pair_code", 2000, at)).data;
  assert.equal(toast.kind, "pair_code");
  assert.equal(toast.urgency, "critical");
  assert.equal(toast.title, "Pairing code 418 207");
  assert.match(toast.body, /tower2 is asking to pair\. Confirm only if it shows the same code\./);

  // the terminal answers first; the panel's answer comes too late
  assert.deepEqual((await terminal.call("pair.answer", { match: true })).result, {});
  const late = await panel.call("pair.answer", { match: true });
  assert.equal(late.error.code, "no_pairing");
  // ferryd: "the code was already answered" while its net thread has not moved the stage, "no code to
  // answer yet" once it has; the fake moves it at once
  assert.match(late.error.message, /^(the code was already answered|no code to answer yet)$/);
  const paired = await panel.stage("paired", 3000, at);
  assert.equal(paired.data.peer.name, "tower2");
  assert.deepEqual(panel.stages(at), ["listening", "code", "waiting_peer", "paired"], "no connecting event for an acceptor");
  assert.equal((await panel.call("pair.answer", { match: true })).error.code, "no_pairing", "nothing left to answer");
  terminal.close();
  panel.close();
});

function c(x) { return x; }

test("a failed attempt leaves the window open: failed, then listening; the third failure closes it", async () => {
  const p = await session({ other: "mismatch" });
  const at = p.mark();
  await p.call("pair.accept");
  for (let i = 1; i <= 3; i++) {
    const from = p.mark();
    await p.call("fake.pair_arrive", { name: "tower" + i });
    const failed = await p.stage("failed", 3000, from);
    assert.equal(failed.data.reason, "rejected");
    assert.equal(failed.data.peer.name, "tower" + i);
    if (i < 3) {
      const back = await p.stage("listening", 3000, from);
      assert.equal(back.data.peer, undefined, "a window that waits again has no peer");
    }
  }
  const closed = await p.stage("closed", 3000, at);
  assert.equal(closed.data.reason, "failures");
  assert.equal(closed.data.message, "pairing window closed after 3 failed attempts");
  assert.equal(closed.data.peer, undefined);
  assert.equal((await p.call("status")).result.pairing, undefined);
  p.close();
});

test("a window nobody uses closes by itself: closed, timeout", async () => {
  const p = await session({ window_s: 0.8 });
  const at = p.mark();
  await p.call("pair.accept");
  const closed = await p.stage("closed", 5000, at);
  assert.equal(closed.data.reason, "timeout");
  assert.equal(closed.data.message, "pairing window closed: timed out");
  p.close();
});

test("cancelling a window that listens closes it; cancelling an attempt fails it and closes it", async () => {
  const p = await session({ other_ms: 5000 });
  let at = p.mark();
  await p.call("pair.accept");
  await p.call("pair.cancel");
  const closed = await p.stage("closed", 2000, at);
  assert.equal(closed.data.reason, "cancelled");
  assert.equal(closed.data.message, "pairing window closed: cancelled");

  at = p.mark();
  await p.call("pair.accept");
  await p.call("fake.pair_arrive", { name: "tower" });
  await p.stage("code", 3000, at);
  await p.call("pair.cancel");
  assert.deepEqual(c(p.stages(at)).slice(-2), ["failed", "closed"]);
  p.close();
});

test("a connection that subscribes in the middle of a pairing is told, with the code; an idle one is told nothing", async () => {
  const p = await session({ other_ms: 5000 });
  const idle = await fake.connect();
  await idle.until((e) => e.event === "hello");
  await idle.call("subscribe");
  await sleep(200);
  assert.deepEqual(idle.stages(), [], "nothing is sent while none runs");
  idle.close();

  await p.call("pair.accept");
  await p.call("fake.pair_arrive", { name: "tower" });
  await p.stage("code", 3000);
  const late = await fake.connect();
  await late.until((e) => e.event === "hello");
  await late.call("subscribe");
  const snapshot = await late.stage("code", 2000);
  assert.match(snapshot.data.code, /^\d{6}$/);
  assert.equal(snapshot.data.role, "acceptor");
  assert.equal(snapshot.data.peer.name, "tower");
  late.close();
  await p.call("pair.cancel");
  p.close();
});

test("an opener that did not subscribe keeps M1's stream; another connection that did not subscribe hears nothing", async () => {
  const opener = await fake.connect();
  await opener.until((e) => e.event === "hello");
  const bystander = await fake.connect();
  await bystander.until((e) => e.event === "hello");
  await opener.call("fake.reset");
  await opener.call("fake.pair_next", { ...FAST, other_ms: 5000 });
  await opener.call("pair.accept");
  await sleep(100);
  assert.deepEqual(opener.stages(), ["listening"]);
  assert.deepEqual(bystander.stages(), []);
  await opener.call("pair.cancel");
  await sleep(100);
  assert.deepEqual(opener.stages(), ["listening", "closed"]);
  opener.close();
  bystander.close();
});

// ------------------------------------------------------------------------ rename, unpair, notices

test("peer.rename applies ferryd's rules and says why when it refuses", async () => {
  const c = await session();
  const at = c.mark();
  const ok = await c.call("peer.rename", { peer: "lap", name: "laptop" });
  assert.equal(ok.result.peer.name, "laptop");
  assert.equal(ok.result.peer.fingerprint.length, 64);
  const peers = await c.until((e) => e.event === "peers" && e.data.peers.some((p) => p.name === "laptop"), 2000, at);
  assert.ok(peers, "the peers topic follows");
  const state = await c.until((e) => e.event === "state" && e.data.peers.some((p) => p.name === "laptop"), 2000, at);
  assert.ok(state);

  const spaced = await c.call("peer.rename", { peer: "laptop", name: "my laptop" });
  assert.equal(spaced.error.code, "bad_params");
  assert.match(spaced.error.message, /a peer name cannot contain ' ' \(no whitespace, control characters or ':'\)/);
  assert.match((await c.call("peer.rename", { peer: "laptop", name: "a:b" })).error.message, /cannot contain ':'/);
  assert.equal((await c.call("peer.rename", { peer: "laptop", name: "" })).error.message, "a peer name cannot be empty");
  assert.equal((await c.call("peer.rename", { peer: "laptop", name: "x".repeat(65) })).error.message, "a peer name is at most 64 characters");
  const own = await c.call("peer.rename", { peer: "laptop", name: "desk" });
  assert.equal(own.error.code, "bad_params");
  assert.match(own.error.message, /is this machine's own name/);
  assert.equal(own.error.hint, "pick another name");
  const taken = await c.call("peer.rename", { peer: "laptop", name: "tower" });
  assert.match(taken.error.message, /already belongs to peer/);
  assert.equal((await c.call("peer.rename", { peer: "nobody", name: "x" })).error.code, "unknown_peer");
  assert.equal((await c.call("peer.rename", { peer: "laptop", name: "laptop" })).result.peer.name, "laptop", "the same name is not an error");
  c.close();
});

test("unpair removes the pin, the placement and the link, and answers with the peer", async () => {
  const c = await session();
  const at = c.mark();
  const r = await c.call("unpair", { peer: "tower" });
  assert.equal(r.result.peer.name, "tower");
  const gone = await c.until((e) => e.event === "peers" && !e.data.peers.some((p) => p.name === "tower"), 2000, at);
  assert.ok(gone);
  const layout = (await c.call("layout.get")).result;
  assert.ok(!layout.peers.some((p) => p.name === "tower"));
  assert.equal((await c.call("unpair", { peer: "tower" })).error.code, "unknown_peer");
  c.close();
});

test("a peer revoking this machine is a toast and a pin gone; a link that flaps toasts once per 30 s", async () => {
  const c = await session();
  const at = c.mark();
  await c.call("fake.unpaired_by", { peer: "lap" });
  const [n] = c.notices(at);
  assert.deepEqual([n.kind, n.title, n.body, n.peer], ["unpaired", "lap unpaired this machine", "Pair again to reconnect", "lap"]);
  assert.ok(!(await c.call("peers")).result.peers.some((p) => p.name === "lap"));

  const from = c.mark();
  await c.call("fake.peer", { name: "tower", state: "offline" });
  await c.call("fake.peer", { name: "tower", state: "connected" });
  await c.call("fake.peer", { name: "tower", state: "offline" });
  const toasts = c.notices(from);
  assert.deepEqual(toasts.map((t) => t.kind), ["peer_lost"], "the rest are inside the 30 s");
  assert.equal(toasts[0].title, "Lost tower");
  c.close();
});

test("a peer that is in the layout but not paired here is listed untrusted, with its short fingerprint as its name", async () => {
  const c = await session();
  const r = await c.call("fake.untrusted", { name: "stranger" });
  const layout = (await c.call("layout.get")).result;
  const entry = layout.peers.find((p) => p.fingerprint === r.result.fingerprint);
  assert.equal(entry.trusted, false);
  assert.equal(entry.self, false);
  assert.equal(entry.name, r.result.fingerprint.slice(0, 8));
  assert.ok(!(await c.call("peers")).result.peers.some((p) => p.fingerprint === r.result.fingerprint));
  await c.call("fake.untrusted", { clear: true });
  c.close();
});

test("the attention scene speaks ferryd's words and codes, and there is no escape_bind_missing", async () => {
  const c = await session();
  await c.call("fake.scene", { name: "attention" });
  const items = (await c.call("status")).result.attention;
  assert.deepEqual(items.map((a) => a.code), ["peer_refused", "capture_denied"]);
  assert.equal(items[0].message, "lap cannot connect: fingerprint changed");
  assert.match(items[0].hint, /`ferry unpair lap`/);
  await c.call("fake.scene", { name: "ready" });
  c.close();
});

test("an attention entry is a critical toast once, and not again for the same code and peer inside 5 minutes", async () => {
  const c = await session();
  const at = c.mark();
  await c.call("fake.scene", { name: "attention" });
  const toast = (await c.until((e) => e.event === "notice" && e.data.kind === "attention", 2000, at)).data;
  assert.equal(toast.urgency, "critical");
  assert.equal(toast.title, "lap cannot connect: fingerprint changed");
  assert.match(toast.body, /`ferry unpair lap`/);
  assert.equal(toast.peer, "lap");
  // the scene has two entries, and so two toasts
  await c.until((e) => e.event === "notice" && e.data.kind === "attention" && e.data.title === "Hyprland does not let ferry capture input", 2000, at);
  const again = c.mark();
  await c.call("fake.scene", { name: "ready" });
  await c.call("fake.scene", { name: "attention" });
  await sleep(300);
  assert.equal(c.notices(again).filter((n) => n.kind === "attention").length, 0, "the same problem coming back inside 5 min is not news");
  c.close();
});

test("a pairing another connection opened is heard by every subscriber and can be cancelled from here", async () => {
  const c = await session();
  const at = c.mark();
  await c.call("fake.pair_elsewhere", { role: "acceptor" });
  assert.equal((await c.stage("listening", 2000, at)).data.role, "acceptor");
  await c.call("fake.pair_arrive", { name: "tower2" });
  await c.stage("code", 3000, at);
  assert.deepEqual((await c.call("pair.cancel")).result, {});
  const failed = await c.stage("failed", 3000, at);
  assert.deepEqual([failed.data.reason, failed.data.cancelled_by], ["cancelled", "local"]);
  assert.equal((await c.stage("closed", 3000, at)).data.cancelled_by, "local");
  c.close();
});

// ------------------------------------------------------------------------------- the old daemon

test("an M1 daemon: no role, no subscribe, the opener alone is told, and only the opener may answer", async () => {
  const legacy = await startFake(["--legacy", "--pair-window-s", "3"]);
  try {
    const a = await legacy.connect();
    await a.until((e) => e.event === "hello");
    assert.equal(a.hello.methods, undefined);
    const b = await legacy.connect();
    await b.until((e) => e.event === "hello");
    await a.call("fake.pair_next", FAST);
    const at = a.mark();
    await a.call("pair.start", { addr: "198.51.100.88" });
    const code = await a.stage("code", 3000, at);
    assert.equal(code.data.role, undefined);
    assert.equal(b.stages().length, 0, "the other connection hears nothing");
    const refused = await b.call("pair.answer", { match: true });
    assert.equal(refused.error.code, "no_pairing");
    assert.equal(refused.error.message, "no pairing runs on this connection");
    assert.deepEqual((await a.call("pair.answer", { match: true })).result, {});
    await a.stage("paired", 3000, at);
    assert.equal((await a.call("peer.rename", { peer: "workshop", name: "x" })).error.code, "unknown_method");
    a.close();
    b.close();
  } finally {
    legacy.stop();
  }
});

test("discover.list is the M4 method: absent unless the fake is asked for it", async () => {
  const c = await session();
  assert.equal(c.hello.methods.includes("discover.list"), false);
  assert.equal((await c.call("discover.list")).error.code, "unknown_method");
  c.close();
  const d = await startFake(["--discover"]);
  try {
    const x = await d.connect();
    await x.until((e) => e.event === "hello");
    assert.equal(x.hello.methods.includes("discover.list"), true);
    await x.call("fake.discover", { peers: [{ name: "workshop", addr: "192.0.2.124:4243" }, { name: "lap", addr: "198.51.100.51:4243" }] });
    const r = (await x.call("discover.list")).result.peers;
    assert.deepEqual(r.map((p) => [p.name, p.trusted]), [["workshop", false], ["lap", true]]);
    x.close();
  } finally {
    d.stop();
  }
});
