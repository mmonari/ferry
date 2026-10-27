import QtQuick
import Quickshell
import "ferry" as Ferry

// The Pair panel against a REAL ferryd (a `--no-input` dev daemon, scripts/m3/dev-ferryd.sh), one
// scenario at a time, named by FERRY_REAL. pair-real.sh starts two daemons and a node script that plays
// the other machine; this file is the panel side. It prints `REAL ...` lines: the code it saw (so the
// script can compare it with the other side's), a line per check, and a verdict.
//
//   dial     the panel dials, both say yes: paired, then Rename and Unpair on the real daemon
//   differ   the panel dials and says the codes differ: ferryd fails it with `rejected`
//   refused  the panel dials a machine that is not accepting: ferryd's error, and no waiting for the event
//   cancel   the panel dials, reaches the code, and cancels
//   accept   the panel opens its window; another connection answers the code, not the panel
ShellRoot {
  id: root

  readonly property var c: Ferry.FerryClient
  readonly property var pp: card.viewItem
  readonly property string scenario: Quickshell.env("FERRY_REAL") || "dial"
  readonly property string target: Quickshell.env("FERRY_REAL_ADDR") || "127.0.0.1:4252"
  property int failures: 0
  property int checks: 0

  function check(name, ok, detail) {
    checks++;
    if (!ok) failures++;
    console.log("REAL " + (ok ? "ok   " : "FAIL ") + name + (ok ? "" : "  <- " + detail));
  }

  property int step: -1
  function next() { step++; Qt.callLater(steps[step] || finish); }

  function otherFp() {
    var peers = c.peers ? c.peers.peers : [];
    return peers.length ? peers[0].fingerprint : "";
  }

  readonly property var scenarios: ({
    dial: [
      function () { pp.dial(target); waitFor(function () { return pp.screen === "code"; }, 8000, function (ok) {
        check("the real daemon reaches the code", ok, pp.screen + " " + pp.dialError);
        console.log("REAL code=" + pp.flow.code);
        check("as the dialer, with the other machine named", pp.flow.role === "dialer" && pp.flow.peer && pp.flow.peer.name === "dev-b", JSON.stringify(pp.flow.peer));
        check("six digits, and a clock", /^\d{6}$/.test(pp.flow.code) && pp.flow.remainingText !== "", pp.flow.code + " " + pp.flow.remainingText);
        pp.answer(true);
        next(); }); },
      function () { waitFor(function () { return pp.screen === "paired"; }, 12000, function (ok) {
        check("both said yes: paired", ok && pp.flow.hero.title === "Paired with dev-b", pp.screen + " " + pp.flow.hero.title);
        check("a real fingerprint, 64 hex digits", ok && /^[0-9a-f]{64}$/.test(pp.flow.peer.fingerprint), JSON.stringify(pp.flow.peer));
        next(); }); },
      function () { waitFor(function () { return pp.rows.length === 1; }, 5000, function (ok) {
        check("the trusted list has it", ok && pp.rows[0].name === "dev-b", JSON.stringify(pp.rows.map(function (r) { return r.name; })));
        check("and the real daemon said so as a notice, 'Paired with dev-b', stamped with its time",
          c.notices.some(function (n) { return n.kind === "paired" && n.title === "Paired with dev-b" && n.at > 0; }), JSON.stringify(c.notices));
        pp.dismiss();
        next(); }); },
      function () {
        var item = pp.trustedItem(otherFp());
        check("its row is there", item !== null, "no row");
        item.askRename();
        waitFor(function () { return item.mode === "rename"; }, 1000, function () {
          item.renameField.field.text = "bee";
          item.renameField.submit();
          waitFor(function () { return pp.rows.length === 1 && pp.rows[0].name === "bee"; }, 5000, function (ok) {
            check("peer.rename on the real daemon: the row follows", ok, JSON.stringify(pp.rows.map(function (r) { return r.name; })));
            next(); }); }); },
      function () {
        var item = pp.trustedItem(otherFp());
        item.askRename();
        waitFor(function () { return item.mode === "rename"; }, 1000, function () {
          item.renameField.field.text = "dev-a";
          check("a name that is this machine's own is refused by the rule", item.renameField.check.ok === false, JSON.stringify(item.renameField.check));
          pp.renamePeer(otherFp(), "has space");
          waitFor(function () { return item.renameField.refusal !== ""; }, 3000, function (ok) {
            check("and what the rule lets through, the real daemon refuses in its words", ok && /cannot contain/.test(item.renameField.refusal), item.renameField.refusal);
            item.close(false);
            next(); }); }); },
      function () {
        pp.unpairPeer(otherFp());
        waitFor(function () { return pp.rows.length === 0; }, 5000, function (ok) {
          check("unpair on the real daemon: the list is empty", ok, JSON.stringify(pp.rows.map(function (r) { return r.name; })));
          next(); }); }
    ],

    differ: [
      function () { pp.dial(target); waitFor(function () { return pp.screen === "code"; }, 8000, function (ok) {
        check("the real daemon reaches the code", ok, pp.screen);
        console.log("REAL code=" + pp.flow.code);
        pp.answer(false);
        next(); }); },
      function () { waitFor(function () { return pp.screen === "failed"; }, 8000, function (ok) {
        check("saying the codes differ fails it: 'The codes did not match'", ok && pp.flow.failure.title === "The codes did not match", ok ? pp.flow.failure.title : pp.screen);
        check("with ferryd's reason", ok && pp.flow.failure.reason === "rejected", ok ? pp.flow.failure.reason : "");
        waitMs(500, function () {
          check("nothing was pinned", pp.rows.length === 0, JSON.stringify(pp.rows));
          next(); }); }); }
    ],

    refused: [
      function () { pp.dial(target); waitFor(function () { return pp.dialError !== ""; }, 12000, function (ok) {
        check("a machine that is not accepting: our sentence under the field", ok && /is not waiting for a pairing request/.test(pp.dialError), pp.dialError);
        console.log("REAL error=" + pp.dialError + " | " + pp.dialHint);
        waitMs(2800, function () {
          check("and the panel is at its start: the failed event that ends the dial does not take it over", pp.screen === "start" && pp.starting === null, pp.screen);
          next(); }); }); }
    ],

    cancel: [
      function () { pp.dial(target); waitFor(function () { return pp.screen === "code"; }, 8000, function (ok) {
        check("the real daemon reaches the code", ok, pp.screen);
        console.log("REAL code=" + pp.flow.code);
        pp.cancel();
        waitFor(function () { return pp.screen === "start"; }, 5000, function (ok2) {
          check("Cancel: back at the start", ok2, pp.screen);
          waitMs(800, function () {
            check("and no failure is shown for what the user did", pp.screen === "start" && pp.flow.failure === null, pp.screen);
            next(); }); }); }); }
    ],

    accept: [
      function () { pp.accept(); waitFor(function () { return pp.screen === "listening"; }, 5000, function (ok) {
        check("Accept: the real daemon opens its window and says so", ok && pp.flow.role === "acceptor", pp.screen + " " + JSON.stringify(c.pairing));
        console.log("REAL listening");
        next(); }); },
      function () { waitFor(function () { return pp.screen === "code"; }, 30000, function (ok) {
        check("another machine dials: the code, as the acceptor", ok && pp.flow.role === "acceptor" && pp.flow.peer && pp.flow.peer.name === "dev-a", ok ? JSON.stringify(pp.flow.peer) : pp.screen);
        console.log("REAL code=" + pp.flow.code);
        waitFor(function () { return c.notices.some(function (n) { return n.kind === "pair_code"; }); }, 3000, function (ok2) {
          var raw = pp.flow.code;
          var grouped = raw.slice(0, 3) + " " + raw.slice(3);
          var kept = JSON.stringify(c.notices);
          check("the real daemon's critical pair_code notice arrived, and the client kept nothing of its text (it holds the code)",
            ok2 && kept.indexOf(raw) < 0 && kept.indexOf(grouped) < 0, kept);
          console.log("REAL code-shown");
          next(); }); }); },
      function () { waitFor(function () { return pp.screen === "paired"; }, 30000, function (ok) {
        check("a terminal answered: the panel follows to paired without having answered", ok && pp.flow.hero.title === "Paired with dev-a", pp.screen);
        next(); }); }
    ]
  })

  property var steps: [
    function () { waitFor(function () { return c.connected && c.status && pp; }, 8000, function (ok) {
      check("connects to the real daemon, which pushes (subscribe)", ok && c.eventMode === true, "connected=" + c.connected + " eventMode=" + c.eventMode);
      check("and offers peer.rename", c.hasMethod("peer.rename") === true, JSON.stringify(c.hello && c.hello.methods));
      steps = steps.slice(0, 1).concat(scenarios[scenario] || []);
      next(); }); }
  ]

  function finish() {
    console.log("REAL " + (failures === 0 ? "PASS" : "FAIL") + " " + (checks - failures) + "/" + checks + " (" + scenario + ")");
    Qt.quit();
  }

  function waitMs(ms, done) {
    var timer = Qt.createQmlObject('import QtQuick; Timer { repeat: false; running: true }', root);
    timer.interval = ms;
    timer.triggered.connect(function () { timer.destroy(); done(); });
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

  FloatingWindow {
    visible: true
    implicitWidth: 900
    implicitHeight: 720
    color: "#101018"

    Ferry.PanelCard {
      id: card
      x: 5
      y: 5
      width: 890
      height: 700
      view: "pair"
    }
  }

  Timer { interval: 600; running: true; onTriggered: root.next() }
}
