import QtQuick
import Quickshell
import "ferry" as Ferry

// FerryClient against a daemon, end to end: snapshots, calls and their errors, pairing events,
// notices, the ui request, a call that times out, and a call with no daemon. Prints one line per
// check and a verdict; client.sh turns that into an exit status. Runs the same checks against an
// M3 daemon (events) and an M1 daemon (polling), which is the point of FerryClient's fallback.
ShellRoot {
  id: root

  readonly property var c: Ferry.FerryClient
  property int failures: 0
  property int checks: 0
  property var seen: ({ notice: null, ui: null, pairing: [] })
  readonly property bool legacy: Quickshell.env("FERRY_TEST_LEGACY") === "1"

  function check(name, ok, detail) {
    checks++;
    if (!ok) failures++;
    console.log("CLIENT " + (ok ? "ok   " : "FAIL ") + name + (ok ? "" : "  <- " + detail));
  }

  // steps run in order; each calls next() when it is done
  property int step: -1
  function next() { step++; Qt.callLater(steps[step] || finish); }

  property var steps: [
    function () { waitFor(function () { return c.connected && c.status && c.peers && c.layout; }, 4000, function (ok) {
      check("connects and has every snapshot", ok, "connected=" + c.connected);
      check("eventMode is " + (legacy ? "false" : "true"), c.eventMode === !legacy, "eventMode=" + c.eventMode);
      check("hello says who it is", c.hello && c.hello.peer === "desk", JSON.stringify(c.hello));
      check("status has a summary state", c.state === "ready", c.state);
      check("peers lists the trusted peers", c.peers.peers.length === 2, JSON.stringify(c.peers));
      check("layout has the revision", c.layout.revision.counter === 14, JSON.stringify(c.layout.revision));
      check(legacy ? "no config.schema on an M1 daemon" : "config.schema was fetched", legacy ? c.configSchema === null : (c.configSchema && c.configSchema.length >= 10), JSON.stringify(c.configSchema).slice(0, 60));
      next(); }); },

    function () { c.setLock(true, function (r, e) {
      check("lock on answers locked=true", r && r.locked === true, JSON.stringify(e));
      check("the toggle moved at once (status patched)", c.locked === true, "locked=" + c.locked);
      waitFor(function () { return c.state === "locked"; }, 3000, function (ok) {
        check("the state follows: locked", ok, c.state);
        c.setLock(false, function () { next(); }); }); }); },

    function () { c.call("config.set", { key: "crossing.edge_resistance_px", value: 40 }, function (r, e) {
      if (legacy) { check("config.set is unknown_method on an M1 daemon", e && e.code === "unknown_method", JSON.stringify(e)); next(); return; }
      check("config.set applies live", r && r.applies === "live" && r.value === 40, JSON.stringify(r || e));
      c.call("config.set", { key: "crossing.edge_resistance_px", value: 900 }, function (r2, e2) {
        check("config.set refuses a value out of range with a hint", e2 && e2.code === "bad_value" && e2.hint, JSON.stringify(e2));
        c.call("config.get", { key: "crossing.edge_resistance_px" }, function (r3) {
          check("config.get reads it back", r3 && r3.values["crossing.edge_resistance_px"] === 40, JSON.stringify(r3));
          waitFor(function () { return c.config && c.config["crossing.edge_resistance_px"] === 40; }, 2000, function (ok) {
            check("the config event updated FerryClient.config", ok, JSON.stringify(c.config));
            next(); }); }); }); }); },

    function () {
      if (legacy) { next(); return; }
      // a key that applies at a restart is saved and listed as pending by the `config` event; put
      // back, it is not pending any more (the event is the only word on it: FerryClient does not guess)
      c.call("config.set", { key: "peer.port", value: 4300 }, function (r, e) {
        check("config.set of a restart key says so", r && r.applies === "restart" && r.value === 4300, JSON.stringify(r || e));
        waitFor(function () { return c.configPending.indexOf("peer.port") >= 0; }, 2000, function (ok) {
          check("the config event lists it as pending", ok && c.config["peer.port"] === 4300, JSON.stringify(c.configPending));
          c.call("config.get", { key: "peer.port" }, function (r2) {
            check("config.get of that key carries it as pending too", r2 && r2.values["peer.port"] === 4300 && r2.pending && r2.pending[0] === "peer.port", JSON.stringify(r2));
            c.call("config.set", { key: "peer.port", value: 4243 }, function () {
              waitFor(function () { return c.configPending.length === 0; }, 2000, function (ok2) {
                check("put back, it is not pending any more", ok2, JSON.stringify(c.configPending));
                next(); }); }); }); }); });
    },

    function () {
      if (legacy) { next(); return; }
      c.call("layout.set", { origins: [{ peer: "lap", x: 100, y: 0 }] }, function (r, e) {
        check("layout.set overlapping is refused and names both peers", e && e.code === "overlap" && /lap/.test(e.message) && /desk/.test(e.message), JSON.stringify(r || e));
        c.call("layout.set", { origins: [{ peer: "lap", x: -1800, y: 300 }], base_rev: { counter: 3, peer: "0000000000000000000000000000000000000000000000000000000000000000" } }, function (r2, e2) {
          check("layout.set on a stale revision is refused", e2 && e2.code === "stale_layout", JSON.stringify(r2 || e2));
          c.call("layout.set", { origins: [{ peer: "lap", x: -1800, y: 300 }], base_rev: c.layout.revision }, function (r3, e3) {
            check("layout.set applies one revision", r3 && r3.revision.counter === 15, JSON.stringify(r3 || e3));
            waitFor(function () { return c.layout.revision.counter === 15; }, 3000, function (ok) {
              check("the layout snapshot follows", ok, JSON.stringify(c.layout.revision));
              next(); }); }); }); }); },

    function () {
      c.call("pair.accept", null, function (r, e) {
        check("pair.accept opens the window", r && r.expires_ms === 120000, JSON.stringify(r || e));
        waitFor(function () { return c.pairing && c.pairing.stage === "listening"; }, 2000, function (ok) {
          check("a pairing event says listening", ok, JSON.stringify(c.pairing));
          c.call("fake.pair_arrive", { name: "tower2" }, function () {
            waitFor(function () { return c.pairing && c.pairing.stage === "code"; }, 4000, function (ok2) {
              check("the code arrives, six digits, with the peer", ok2 && /^\d{6}$/.test(c.pairing.code) && c.pairing.peer.name === "tower2", JSON.stringify(c.pairing));
              c.call("pair.answer", { match: true }, function (r2, e2) {
                check("pair.answer is accepted", !e2, JSON.stringify(e2));
                waitFor(function () { return c.pairing && c.pairing.stage === "paired"; }, 4000, function (ok3) {
                  check("pairing ends paired", ok3, JSON.stringify(c.pairing));
                  check("no code outside the code stage", c.pairing.code === undefined || c.pairing.code === null, JSON.stringify(c.pairing));
                  waitFor(function () { return c.peers.peers.length === 3; }, 3000, function (ok4) {
                    check("the new peer is in the peers snapshot", ok4, JSON.stringify(c.peers.peers.length));
                    next(); }); }); }); }); }); }); }); },

    function () {
      if (legacy) { next(); return; }
      var got = [];
      var onN = function (n) { got.push(n); };
      c.noticeReceived.connect(onN);
      var onU = function (v) { got.push({ ui: v }); };
      c.uiRequested.connect(onU);
      c.call("fake.notice", { kind: "peer_back", title: "lap is back", peer: "lap" }, function () {
        c.call("fake.ui", { open: "pair" }, function () {
          waitFor(function () { return got.length >= 2; }, 2000, function (ok) {
            check("a notice event arrives", ok && got[0].kind === "peer_back" && c.lastNotice.title === "lap is back", JSON.stringify(got));
            check("a ui event arrives as a signal", ok && got[1].ui === "pair", JSON.stringify(got));
            c.noticeReceived.disconnect(onN);
            c.uiRequested.disconnect(onU);
            next(); }); }); }); },

    function () { c.call("fake.silent", null, function (r, e) {
      check("a call nobody answers fails with timeout", e && e.code === "timeout", JSON.stringify(r || e));
      next(); }, 700); },

    function () { c.call("no.such.method", null, function (r, e) {
      check("an unknown method is an error with a code", e && e.code === "unknown_method", JSON.stringify(r || e));
      next(); }); },

    function () { c.release(function (r, e) {
      check("release answers", !e, JSON.stringify(e));
      next(); }); },

    function () {
      var before = c.session;
      c.call("fake.dark", { seconds: 1.5 }, function () {
        waitFor(function () { return !c.connected; }, 3000, function (ok) {
          check("the daemon goes dark: connected=false, state=off", ok && c.state === "off" && c.status === null, "state=" + c.state);
          c.call("status", null, function (r, e) {
            check("a call with no daemon fails at once with not_connected", e && e.code === "not_connected", JSON.stringify(r || e));
            waitFor(function () { return c.connected && c.status; }, 6000, function (ok2) {
              check("it comes back by itself, and it is a new session", ok2 && c.session > before, "session " + before + " -> " + c.session);
              next(); }); }); }); }); }
  ]

  function finish() {
    console.log("CLIENT " + (failures === 0 ? "PASS" : "FAIL") + " " + (checks - failures) + "/" + checks + (legacy ? " (legacy daemon)" : ""));
    Qt.quit();
  }

  function waitFor(pred, ms, done) {
    var t0 = Date.now();
    var timer = Qt.createQmlObject('import QtQuick; Timer { interval: 25; repeat: true; running: true }', root);
    timer.triggered.connect(function () {
      var ok = false;
      try { ok = !!pred(); } catch (e) { ok = false; }
      if (ok || Date.now() - t0 > ms) { timer.running = false; timer.destroy(); done(ok); }
    });
  }

  Timer { interval: 400; running: true; onTriggered: root.next() }
}
