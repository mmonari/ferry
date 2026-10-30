import { test } from "node:test";
import assert from "node:assert/strict";
import { load } from "./harness.mjs";

// What the Pair panel shows for each thing ferryd can say about a pairing (stream F): the screen,
// the clock, the words for each way it can end, and the address the user types.

const Model = load("Model.js");

const FP_LAP = "a3f1c2d4".padEnd(56, "7") + "5be80d4e";
const NOW = 1_000_000;

function ev(stage, extra = {}) {
  return { stage, role: "dialer", expires_ms: 100000, ...extra };
}
const LAP = { name: "lap", fingerprint: FP_LAP, addr: "198.51.100.51:4243" };

function flow(event, extra = {}) {
  return Model.pairFlow({ event, at: NOW, now: NOW, ...extra });
}

// ---------------------------------------------------------------------------------- screens

test("with no pairing event the panel is at its start, and a null event is not an error", () => {
  const f = Model.pairFlow({ event: null, now: NOW });
  assert.equal(f.screen, "start");
  assert.equal(f.active, false);
  assert.equal(f.terminal, false);
  assert.equal(f.hero.title, "Pair a machine");
  assert.equal(f.steps[0].state, "current");
  assert.equal(Model.pairFlow({}).screen, "start", "no arguments at all");
  assert.equal(Model.pairFlow({ event: { foo: 1 }, now: NOW }).screen, "start", "an object that is not an event");
});

test("each live stage is one screen, and the dialer and the acceptor read differently", () => {
  assert.equal(flow(ev("listening", { role: "acceptor" })).screen, "listening");
  assert.equal(flow(ev("connecting")).screen, "connecting");
  assert.equal(flow(ev("code", { code: "418207", peer: LAP })).screen, "code");
  assert.equal(flow(ev("waiting_peer", { peer: LAP })).screen, "waiting");

  const dial = flow(ev("code", { code: "418207", peer: LAP }));
  assert.equal(dial.hero.title, "Pairing with lap");
  assert.equal(dial.role, "dialer");
  const accept = flow(ev("code", { role: "acceptor", code: "418207", peer: LAP }));
  assert.equal(accept.hero.title, "Pairing request from lap");
  assert.equal(accept.role, "acceptor");
  assert.equal(accept.steps[0].label, "Wait for a request");
  assert.equal(dial.steps[0].label, "Pick a machine");
});

test("the code is grouped for reading aloud and only exists at the code stage", () => {
  const f = flow(ev("code", { code: "418207", peer: LAP }));
  assert.equal(f.code, "418207");
  assert.equal(f.codeText, "418 207");
  assert.equal(f.codeSpoken, "4 1 8 2 0 7");
  assert.equal(f.canAnswer, true);
  const w = flow(ev("waiting_peer", { code: "418207", peer: LAP }));
  assert.equal(w.code, "", "a code left in a later event is not shown");
  assert.equal(w.canAnswer, false);
  assert.equal(Model.groupCode("12345"), "12345", "anything but six digits is left as it is");
  assert.equal(Model.groupCode("000012"), "000 012", "leading zeros are digits");
  assert.equal(Model.spokenCode("000012"), "0 0 0 0 1 2");
});

test("the clock counts down from when the event arrived, and never below zero", () => {
  const f = Model.pairFlow({ event: ev("code", { code: "418207", peer: LAP, expires_ms: 112000 }), at: NOW, now: NOW + 2000 });
  assert.equal(f.remainingText, "1:50");
  assert.equal(f.hero.detail, "", "the code card shows the clock, so the hero pill does not (#32)");
  const w = Model.pairFlow({ event: ev("waiting_peer", { peer: LAP, expires_ms: 112000 }), at: NOW, now: NOW + 2000 });
  assert.equal(w.hero.detail, "1:50", "while waiting, with no card clock, the pill carries it");
  assert.ok(Math.abs(f.fraction - 110000 / 120000) < 1e-9);
  assert.equal(f.expiring, false);

  const late = Model.pairFlow({ event: ev("code", { code: "418207", peer: LAP, expires_ms: 1000 }), at: NOW, now: NOW + 5000 });
  assert.equal(late.remainingText, "0:00");
  assert.equal(late.fraction, 0);
  assert.equal(late.expiring, true);
  assert.equal(late.screen, "code", "ferryd ends it with an event, so the screen waits for that");
  assert.equal(Model.clockText(112000), "1:52");
  assert.equal(Model.clockText(111001), "1:52", "rounds up, so 0:00 means the time is up");
  assert.equal(Model.clockText(59000), "0:59");
  assert.equal(Model.clockText(61000), "1:01");
  assert.equal(Model.clockText(-5), "0:00");
});

test("an event that arrived a moment after the clock last ticked does not make the window longer than it is", () => {
  const f = Model.pairFlow({ event: ev("listening", { role: "acceptor", expires_ms: 120000 }), at: NOW + 400, now: NOW });
  assert.equal(f.remainingText, "2:00");
  assert.equal(f.fraction, 1);
});

test("a live pairing that ferryd never closed is dropped well after its deadline", () => {
  const f = Model.pairFlow({ event: ev("code", { code: "418207", peer: LAP, expires_ms: 1000 }), at: NOW, now: NOW + 1000 + 16000 });
  assert.equal(f.screen, "start");
});

test("a result is shown while the panel is open, and a stale one is not shown to a panel that has just opened", () => {
  const paired = ev("paired", { peer: LAP, expires_ms: 0 });
  // arrived after the panel opened: shown
  assert.equal(Model.pairFlow({ event: paired, at: NOW, now: NOW + 60000, openedAt: NOW - 1000 }).screen, "paired", "it stays for as long as the user looks at it");
  // arrived 5 s before the panel opened (a tab switch): still shown
  assert.equal(Model.pairFlow({ event: paired, at: NOW, now: NOW + 1000, openedAt: NOW + 5000 }).screen, "paired");
  // arrived a minute before: not news
  assert.equal(Model.pairFlow({ event: paired, at: NOW, now: NOW + 61000, openedAt: NOW + 60000 }).screen, "start");
  // the user pressed Done
  assert.equal(Model.pairFlow({ event: paired, at: NOW, now: NOW + 2000, openedAt: NOW - 1000, dismissedAt: NOW + 1000 }).screen, "start");
  // a new pairing after the dismissal is shown again
  assert.equal(Model.pairFlow({ event: ev("connecting"), at: NOW + 3000, now: NOW + 3000, openedAt: NOW - 1000, dismissedAt: NOW + 1000 }).screen, "connecting");
});

test("a dial that failed leaves a connecting event that nothing follows, and the panel does not wait for it", () => {
  const event = ev("connecting");
  // the panel knows its own pair.start failed: everything that arrived before that is dead
  assert.equal(Model.pairFlow({ event, at: NOW, now: NOW + 500, ignoreBefore: NOW + 400 }).screen, "start");
  assert.equal(Model.pairFlow({ event, at: NOW, now: NOW + 500, ignoreBefore: NOW - 1 }).screen, "connecting", "an older failure is not this event's");
  // a pairing somebody else started that failed the same way: status lists none, and a moment later it is over
  const status = { pairing: undefined };
  assert.equal(Model.pairFlow({ event, at: NOW, now: NOW + 500, status }).screen, "connecting", "status lags the event by a few ms: not yet");
  assert.equal(Model.pairFlow({ event, at: NOW, now: NOW + 2500, status }).screen, "start");
  assert.equal(Model.pairFlow({ event, at: NOW, now: NOW + 2500, status: { pairing: { role: "dialer", stage: "connecting", expires_ms: 100000 } } }).screen, "connecting", "while status lists it, it is live");
  assert.equal(Model.pairFlow({ event, at: NOW, now: NOW + 2500, status: null }).screen, "connecting", "no status at all is not evidence");
});

test("a pair.start in flight is a connecting screen at once, before ferryd says anything", () => {
  const f = Model.pairFlow({ event: null, now: NOW, starting: { role: "dialer", addr: "198.51.100.51" } });
  assert.equal(f.screen, "connecting");
  assert.equal(f.role, "dialer");
  assert.equal(f.hero.title, "Pairing with 198.51.100.51");
  assert.equal(f.canCancel, true);
  // the typed address names the other machine until ferryd knows its name
  const g = Model.pairFlow({ event: ev("connecting"), at: NOW, now: NOW, intent: "dial", addr: "198.51.100.51" });
  assert.equal(g.hero.title, "Pairing with 198.51.100.51");
});

test("the role comes from the event, and from what the user asked for when the daemon predates it", () => {
  assert.equal(flow(ev("connecting", { role: "acceptor" })).role, "acceptor");
  const old = { stage: "code", code: "418207", expires_ms: 100000, peer: LAP };
  assert.equal(Model.pairFlow({ event: old, at: NOW, now: NOW, intent: "accept" }).role, "acceptor");
  assert.equal(Model.pairFlow({ event: old, at: NOW, now: NOW, intent: "dial" }).role, "dialer");
  assert.equal(Model.pairFlow({ event: { stage: "listening", expires_ms: 1000 }, at: NOW, now: NOW }).role, "acceptor", "only an acceptor listens");
  assert.equal(Model.pairFlow({ event: old, at: NOW, now: NOW }).role, "", "unknown, and the copy copes");
});

test("a result the user dismissed stays dismissed, even when the click is stamped the very millisecond it arrived", () => {
  const failed = ev("failed", { reason: "rejected", peer: LAP });
  assert.equal(Model.pairFlow({ event: failed, at: NOW, now: NOW, dismissedAt: NOW - 1 }).screen, "failed");
  assert.equal(Model.pairFlow({ event: failed, at: NOW, now: NOW, dismissedAt: NOW }).screen, "start");
  assert.equal(Model.pairFlow({ event: failed, at: NOW, now: NOW, dismissedAt: NOW + 5 }).screen, "start");
});

test("a pairing seen only in status has a stage and a clock but no code, and shows nothing once over", () => {
  const status = { pairing: { role: "acceptor", stage: "code", expires_ms: 90000 } };
  const f = Model.pairFlow({ event: null, status, now: NOW });
  // the stage is "code", but with no code to show the screen is the waiting one (next test)
  assert.equal(f.screen, "waiting");
  assert.equal(f.elsewhere, true);
  assert.equal(f.code, "");
  assert.equal(f.role, "acceptor");
  assert.equal(Model.pairFlow({ event: null, status: { pairing: { role: "dialer", stage: "paired", expires_ms: 0 } }, now: NOW }).screen, "start");
});

test("a code that only status can see is not a code screen: there is nothing to compare, and the panel says so", () => {
  const status = { pairing: { role: "acceptor", stage: "code", expires_ms: 90000 } };
  const f = Model.pairFlow({ event: null, status, now: NOW });
  assert.equal(f.screen, "waiting");
  assert.equal(f.noCode, true);
  assert.equal(f.code, "");
  // the hero does not say the user has something to confirm
  assert.equal(f.hero.title, "A pairing is in progress");
  assert.equal(f.hero.meta, "Started somewhere else");
  // and Cancel is not offered: pair.cancel is for the connection that started it, so it could only fail
  assert.equal(f.canCancel, false);
  assert.equal(f.canAnswer, false);
  // an event with the code is the code screen, whatever status says
  assert.equal(Model.pairFlow({ event: ev("code", { role: "acceptor", code: "418207", peer: LAP }), at: NOW, now: NOW, status }).screen, "code");
  assert.equal(Model.pairFlow({ event: ev("code", { code: "418207", peer: LAP }), at: NOW, now: NOW }).noCode, false);
});

test("the other machine is called something even before ferryd has said who it is", () => {
  assert.equal(Model.pairFlow({ event: ev("connecting"), at: NOW, now: NOW }).label, "the other machine");
  assert.equal(Model.pairFlow({ event: ev("connecting", { role: "acceptor" }), at: NOW, now: NOW }).labelCap, "A machine");
  assert.equal(Model.pairFlow({ event: ev("code", { code: "418207", peer: LAP }), at: NOW, now: NOW }).label, "lap");
});

// -------------------------------------------------------------------------------- the steps

test("the steps follow the stage: pick, compare, done, and a failure marks the step it stopped at", () => {
  const states = (f) => f.steps.map((s) => s.state);
  assert.deepEqual(states(flow(ev("connecting"))), ["current", "todo", "todo"]);
  assert.deepEqual(states(flow(ev("code", { code: "418207", peer: LAP }))), ["done", "current", "todo"]);
  assert.deepEqual(states(flow(ev("waiting_peer", { peer: LAP }))), ["done", "current", "todo"]);
  assert.deepEqual(states(flow(ev("paired", { peer: LAP }))), ["done", "done", "done"]);
  assert.deepEqual(states(flow(ev("failed", { peer: LAP, reason: "rejected", message: "cancelled: codes differ" }))), ["done", "failed", "todo"]);
});

// ------------------------------------------------------------------------ how it can end

test("a pairing that ended says why in the words for that reason", () => {
  const f = flow(ev("failed", { peer: LAP, reason: "rejected", message: "cancelled: codes differ" }));
  assert.equal(f.screen, "failed");
  assert.equal(f.failure.title, "The codes did not match");
  assert.match(f.failure.body, /Nothing was saved on either machine\. A different code means you reached another machine than lap/);
  assert.match(f.failure.hint, /Check the address/);
  assert.equal(f.hero.title, "Pairing stopped");
  assert.equal(f.hero.meta, "The codes did not match", "the hero line is the verdict, said once (#34)");
  assert.equal(f.canCancel, false);
});

test("every reason ferryd names has a title, and an unknown reason falls back without showing a key", () => {
  for (const reason of ["rejected", "cancelled", "timeout", "bad_reveal", "protocol", "version", "lost", "failures", "save_failed", "other"]) {
    const x = Model.pairFailure(reason, "some message", { peer: "lap", stage: "failed" });
    assert.ok(x.title.length > 0 && !x.title.startsWith("pair."), `${reason}: ${x.title}`);
    assert.ok(x.body.length > 0 || x.hint.length > 0, `${reason} says something under the title`);
  }
  const unknown = Model.pairFailure("a_reason_from_the_future", "the thing that went wrong", { peer: "lap", stage: "failed" });
  assert.equal(unknown.title, "Pairing failed");
  assert.equal(unknown.body, "The thing that went wrong", "ferryd's own message says what the table cannot");
});

test("a version mismatch and a save failure keep ferryd's own message, which holds the facts", () => {
  const v = Model.pairFailure("version", "version mismatch: 1.0 vs 2.0", { peer: "lap", stage: "failed" });
  assert.equal(v.title, "The Ferry versions do not match");
  assert.equal(v.body, "Version mismatch: 1.0 vs 2.0");
  assert.match(v.hint, /Update Ferry on both machines/);
  const s = Model.pairFailure("save_failed", "could not save the pin: disk full", { stage: "failed" });
  assert.equal(s.body, "Could not save the pin: disk full");
});

test("a cancel by the other machine is named, and the user's own cancel is no news", () => {
  const theirs = flow(ev("failed", { peer: LAP, reason: "cancelled", message: "cancelled" }));
  assert.equal(theirs.screen, "failed");
  assert.equal(theirs.failure.title, "lap cancelled the pairing");
  const mine = Model.pairFlow({ event: ev("failed", { peer: LAP, reason: "cancelled", message: "cancelled" }), at: NOW, now: NOW, cancelRequested: true });
  assert.equal(mine.screen, "start", "back to the start with nothing to read");
  const closed = Model.pairFlow({ event: ev("closed", { role: "acceptor", reason: "cancelled", message: "pairing window closed: cancelled" }), at: NOW, now: NOW, cancelRequested: true });
  assert.equal(closed.screen, "start");
});

test("a window that closed by itself says so, with the reason", () => {
  const timeout = flow(ev("closed", { role: "acceptor", reason: "timeout", message: "pairing window closed: timed out" }));
  assert.equal(timeout.screen, "closed");
  assert.equal(timeout.failure.title, "The pairing window closed");
  assert.equal(timeout.failure.body, "Nobody asked to pair within two minutes.");
  const failures = flow(ev("closed", { role: "acceptor", reason: "failures", message: "pairing window closed after 3 failed attempts" }));
  assert.equal(failures.failure.title, "Too many attempts failed");
  assert.equal(timeout.hero.title, "Pairing window closed");
});

test("a failure that leaves the window open is still told, on the screen that follows", () => {
  const failed = ev("failed", { role: "acceptor", peer: LAP, reason: "rejected", message: "cancelled: codes differ" });
  const memory = Model.pairRemember(null, failed, NOW, false);
  assert.equal(memory.reason, "rejected");
  assert.equal(memory.peer, "lap");
  const f = Model.pairFlow({ event: ev("listening", { role: "acceptor" }), at: NOW + 50, now: NOW + 100, failure: memory });
  assert.equal(f.screen, "listening");
  assert.equal(f.lastFailure.title, "The codes did not match");
  // 31 s on it is no longer news
  assert.equal(Model.pairFlow({ event: ev("listening", { role: "acceptor" }), at: NOW + 50, now: NOW + 31000, failure: memory }).lastFailure, null);
});

test("what the panel remembers is cleared by a new attempt, a pin and the end of the window", () => {
  const failure = { reason: "rejected", message: "x", peer: "lap", at: NOW, byUs: false };
  assert.equal(Model.pairRemember(failure, ev("listening"), NOW, false), failure, "listening keeps it");
  assert.equal(Model.pairRemember(failure, ev("waiting_peer"), NOW, false), failure);
  assert.equal(Model.pairRemember(failure, ev("connecting"), NOW, false), null);
  assert.equal(Model.pairRemember(failure, ev("code"), NOW, false), null);
  assert.equal(Model.pairRemember(failure, ev("paired"), NOW, false), null);
  assert.equal(Model.pairRemember(failure, ev("closed"), NOW, false), null);
  assert.equal(Model.pairRemember(null, ev("failed", { reason: "cancelled" }), NOW, true), null, "the user's own cancel is not remembered");
  assert.equal(Model.pairRemember(failure, null, NOW, false), failure);
});

// ------------------------------------------------------------------------------- success

test("a success names the machine, keeps ferryd's note about the name, and says whether it is placed", () => {
  const layout = { peers: [{ name: "lap", fingerprint: FP_LAP, origin: null }] };
  const f = Model.pairFlow({ event: ev("paired", { peer: LAP, expires_ms: 0, message: "pinned as lap-2: the name lap belongs to another peer" }), at: NOW, now: NOW, layout });
  assert.equal(f.screen, "paired");
  assert.equal(f.hero.title, "Paired with lap");
  assert.equal(f.note, "pinned as lap-2: the name lap belongs to another peer");
  assert.equal(f.placed, false);
  assert.equal(f.peer.short, "a3f1c2d4");
  const placed = Model.pairFlow({ event: ev("paired", { peer: LAP, expires_ms: 0 }), at: NOW, now: NOW, layout: { peers: [{ name: "lap", fingerprint: FP_LAP, origin: [0, 0] }] } });
  assert.equal(placed.placed, true);
  const unknown = Model.pairFlow({ event: ev("paired", { peer: LAP, expires_ms: 0 }), at: NOW, now: NOW, layout: null });
  assert.equal(unknown.placed, false, "a peer the layout has not heard of is not placed");
});

test("fingerprints are shown at both ends so they can be compared", () => {
  assert.equal(Model.shortFingerprint(FP_LAP), "a3f1c2d4");
  assert.equal(Model.shortFingerprint("A3F1:C2D4"), "a3f1c2d4");
  assert.equal(Model.shortFingerprint(""), "");
  assert.equal(Model.groupedFingerprint(FP_LAP), "A3F1 C2D4 … 5BE8 0D4E");
  assert.equal(Model.groupedFingerprint("abcdef"), "ABCD EF");
});

// ------------------------------------------------------------------------------- the banner

test("the popover's strip appears for a live pairing and asks for the user only at the code", () => {
  assert.equal(Model.pairBanner(Model.pairFlow({ event: null, now: NOW })), null);
  assert.equal(Model.pairBanner(flow(ev("paired", { peer: LAP }))), null, "a result is not a strip");
  assert.equal(Model.pairBanner(null), null);
  const code = Model.pairBanner(flow(ev("code", { role: "acceptor", code: "418207", peer: LAP })));
  assert.equal(code.text, "lap is asking to pair");
  assert.equal(code.urgent, true);
  assert.equal(code.clock, "1:40");
  const dial = Model.pairBanner(flow(ev("code", { code: "418207", peer: LAP })));
  assert.equal(dial.text, "Compare the code with lap");
  const listening = Model.pairBanner(flow(ev("listening", { role: "acceptor" })));
  assert.equal(listening.text, "Waiting for a pairing request");
  assert.equal(listening.urgent, false);
  assert.equal(Model.pairBanner(flow(ev("waiting_peer", { peer: LAP }))).text, "Waiting for lap to confirm");
  assert.equal(JSON.stringify(code).includes("418"), false, "the strip never carries the code");
});

// ------------------------------------------------------------------------------ addresses

test("an address is checked the way ferryd reads it, before the call", () => {
  const ok = (s) => Model.addrCheck(s).ok;
  for (const good of ["192.0.2.124", "192.0.2.124:4243", "workshop.lan", "workshop.lan:4243", "lap", "::1", "[::1]", "[::1]:4243", "fe80::1", "my_host-1.example.com:5000"]) {
    assert.equal(ok(good), true, good);
  }
  for (const bad of ["", "   ", "two words", "host:", "host:abc", "host:70000", "[::1", "[::1]x", "[::1]:", "http://host", "host/path", "a@b"]) {
    assert.equal(ok(bad), false, JSON.stringify(bad));
  }
  assert.equal(Model.addrCheck("").error, "Type an address first.");
  assert.match(Model.addrCheck("two words").error, /does not look like an address/);
  assert.equal(Model.addrCheck("  192.0.2.124  ").addr, "192.0.2.124", "spaces at the ends are dropped");
});

test("the address to give the other machine comes from `id`, IPv4 first, with the port when it is not the default", () => {
  const a = Model.selfAddress({ name: "desk", port: 4243, addrs: ["192.0.2.111", "fd00::1"] });
  assert.equal(a.addr, "192.0.2.111");
  assert.equal(a.command, "ferry pair 192.0.2.111");
  assert.deepEqual(a.all, ["192.0.2.111", "fd00::1"]);
  assert.equal(Model.selfAddress({ port: 4251, addrs: ["10.0.0.5"] }).addr, "10.0.0.5:4251");
  assert.equal(Model.selfAddress({ port: 4251, addrs: ["fd00::1"] }).addr, "[fd00::1]:4251");
  assert.equal(Model.selfAddress({ port: 4243, addrs: [] }), null);
  assert.equal(Model.selfAddress(null), null);
});

test("peers that are in the layout but not paired here are listed, this machine never is", () => {
  const layout = {
    peers: [
      { name: "desk", fingerprint: "d".repeat(64), self: true, trusted: true },
      { name: "lap", fingerprint: FP_LAP, trusted: true },
      { name: "9bce07aa", fingerprint: "9bce07aa".padEnd(64, "1"), trusted: false }
    ]
  };
  const list = Model.untrustedPeers(layout);
  assert.deepEqual(list.map((p) => p.name), ["9bce07aa"]);
  assert.equal(list[0].short, "9bce07aa");
  assert.deepEqual(Model.untrustedPeers({ peers: [{ name: "lap", fingerprint: FP_LAP }] }), [], "a daemon that does not say is taken to mean trusted");
  assert.deepEqual(Model.untrustedPeers(null), []);
});

test("the nearby list marks the machines already paired, which cannot be chosen again", () => {
  const rows = Model.nearbyRows({ peers: [
    { name: "workshop", addr: "192.0.2.124:4243", fingerprint: "1".repeat(64) },
    { name: "travel", addr: "192.0.2.57:4243", fingerprint: FP_LAP }
  ] }, [{ name: "travel", fingerprint: FP_LAP }]);
  assert.deepEqual(rows.map((r) => [r.name, r.trusted, r.tag]), [["workshop", false, "Ready to pair"], ["travel", true, "Trusted"]]);
  assert.deepEqual(Model.nearbyRows(null, []), []);
});
