import QtQuick
import Quickshell
import qs.Commons
import qs.Commons as Commons
import "ferry" as Ferry

// The Arrange panel's own QML, end to end against the fake daemon: what the node tests of
// plugin/Arrange.js and Geometry.js cannot say, namely that the bindings, the draft, FerryClient's
// events and the `layout.set` call work together. It drives the panel with the same functions a
// pointer or a key press ends in (simulateDrag, nudge, enter...), since offscreen there is no input,
// and reads what the panel decided. Prints one line per check and a verdict; arrange.sh turns that
// into an exit status. The real pointer is scripts/m3/vm-plugin.sh.
//
// Runs twice: against an M3 daemon, and against a daemon from before layout.set (FERRY_TEST_LEGACY=1).
ShellRoot {
  id: root

  readonly property var c: Ferry.FerryClient
  readonly property var p: loader.item
  readonly property bool legacy: Quickshell.env("FERRY_TEST_LEGACY") === "1"
  property int failures: 0
  property int checks: 0

  function check(name, ok, detail) {
    checks++;
    if (!ok) failures++;
    console.log("ARRANGE " + (ok ? "ok   " : "FAIL ") + name + (ok ? "" : "  <- " + detail));
  }

  property int step: -1
  function next() { step++; Qt.callLater(steps[step] || finish); }

  function fp(name) { return p.groupFp(name); }
  function originOf(name) {
    var peers = c.layout ? c.layout.peers : [];
    for (var i = 0; i < peers.length; i++) if (peers[i].name === name) return peers[i].origin;
    return undefined;
  }
  function fake(method, params, done) {
    c.call(method, params, function (r, e) { if (e) console.log("ARRANGE note " + method + " -> " + JSON.stringify(e)); done(r, e); });
  }

  property var steps: legacy ? legacySteps : eventSteps

  // --------------------------------------------------------------------- an M3 daemon
  property var eventSteps: [
    function () { waitFor(function () { return c.connected && c.layout && p && p.mode === "arrange"; }, 5000, function (ok) {
      check("the panel opens on the layout: three machines on the canvas", ok && p.groups.length === 3 && p.canvas.fps.length === 3, "mode=" + (p ? p.mode : "?"));
      check("nothing is dirty, nothing can be applied", ok && !p.dirty && !p.canApply, "dirty=" + p.dirty);
      check("the revision is the daemon's", p.rev === 14, "rev=" + p.rev);
      check("the status says which revision is live and on whom", p.status.text === "This layout is live on desk, lap and tower.", p.status.text);
      check("two seams: desk-lap and desk-tower", p.analysis.touches.length === 2 && p.rows.length === 2, JSON.stringify(p.rows));
      next(); }); },

    function () {
      p.simulateDrag(fp("lap"), 0, 300, true);
      check("a drag edits the draft and nothing else", p.dirty && p.draft.origins[fp("lap")][1] === 300 && c.layout.revision.counter === 14, JSON.stringify(p.draft.origins));
      check("the seam follows the draft: lap's right edge, y 300 to 1080", / · y 300–1080 · 780 px$/.test(p.rows[0].secondary), p.rows[0].secondary);
      check("Apply and Undo come alive, the revision pill says edited", p.canApply && p.draft.undo.length === 1, "canApply=" + p.canApply);
      p.undo();
      check("Undo puts it back", !p.dirty && originOfDraft("lap")[1] === 0, JSON.stringify(p.draft));
      next(); },

    function () {
      p.simulateDrag(fp("lap"), 1250, 120);
      check("a drop on top of desk is refused: the draft stays clean", !p.dirty, JSON.stringify(p.draft.origins));
      check("and says why", /^lap went back: it would have covered desk\.$/.test(p.flashText), p.flashText);
      p.reset();
      next(); },

    function () {
      p.simulateDragLive(fp("lap"), 1790, 1062);
      var d = p.liveDrag;
      check("a drag that comes near an edge snaps: left edges aligned, top on desk's bottom", d && d.valid && d.origin[0] === 0 && d.origin[1] === 1080, JSON.stringify(d && d.origin));
      check("and shows both guides while the pointer is down", d && d.guides.length === 2, JSON.stringify(d && d.guides));
      check("Apply is off while the pointer is down", !p.canApply, "canApply=" + p.canApply);
      p.simulateDragEnd();
      check("letting go keeps it in the draft", p.dirty && p.liveDrag === null && p.draft.origins[fp("lap")][1] === 1080, JSON.stringify(p.draft.origins));
      // a visit to the Pair tab builds this view again on the way back: the draft is still there
      var comp = Qt.createComponent("ferry/panels/ArrangePanel.qml");
      var again = comp.createObject(loader, { visible: false });
      check("a draft survives a visit to another tab", again !== null && again.dirty && again.draft.origins[fp("lap")][1] === 1080, JSON.stringify(again && again.draft));
      if (again) again.destroy();
      next(); },

    function () {
      check("Apply sends it", p.apply() === true && p.applying, "applying=" + p.applying);
      waitFor(function () { return !p.applying && c.layout.revision.counter === 15; }, 4000, function (ok) {
        check("one revision more, and the draft is gone", ok && !p.dirty, "rev=" + c.layout.revision.counter);
        check("lap is where the draft put it", originOf("lap")[0] === 0 && originOf("lap")[1] === 1080, JSON.stringify(originOf("lap")));
        check("the panel says it was applied, and to whom", /^Applied\. Sent to lap and tower\.$/.test(p.flashText), p.flashText);
        check("our own answer stands in only until the snapshot is there", p.applied === null, JSON.stringify(p.applied));
        next(); }); },

    function () {
      // a newer revision over a dirty draft keeps the draft and shows the banner
      p.simulateDrag(fp("lap"), 0, 200, true);
      p.canvas.keyboardMode = true;
      p.focusGroup(fp("lap"));
      fake("fake.layout_by", { peer: "tower", x: 3840, y: 100, by: "tower" }, function () {
        waitFor(function () { return p.banner !== null; }, 3000, function (ok) {
          waitFor(function () { return p.conflictView.keepButton.activeFocus; }, 1500, function (focused) {
            check("with the keyboard in use, the banner takes the focus: Keep mine has it", focused, "focus=" + (p.Window.activeFocusItem ? p.Window.activeFocusItem.objectName : "none")); });
          check("the banner names the peer and the revision", ok && p.banner.by === "tower" && p.banner.counter === 16, JSON.stringify(p.banner));
          check("the draft is kept", p.dirty && p.draft.origins[fp("lap")][1] === 1280, JSON.stringify(p.draft.origins));
          check("Apply waits for the choice", !p.canApply, "canApply=" + p.canApply);
          check("the status says what to choose", p.status.text === "Your changes are not applied yet. Choose below.", p.status.text);
          p.keepMine();
          waitFor(function () { return !p.applying && c.layout.revision.counter === 17; }, 4000, function (ok2) {
            check("Keep mine applies the draft on top of theirs", ok2 && originOf("lap")[1] === 1280 && originOf("tower")[1] === 100, JSON.stringify([originOf("lap"), originOf("tower")]));
            check("and the banner is gone", p.banner === null && !p.dirty, JSON.stringify(p.banner));
            next(); }); }); }); },

    function () {
      p.simulateDrag(fp("lap"), 0, 100, true);
      fake("fake.layout_by", { peer: "tower", x: 3840, y: 200, by: "tower" }, function () {
        waitFor(function () { return p.banner !== null; }, 3000, function (ok) {
          p.takeTheirs();
          check("Take theirs discards the draft", ok && !p.dirty && p.banner === null, JSON.stringify(p.draft));
          check("and the layout that arrived is drawn", originOf("tower")[1] === 200 && p.groups.filter(function (g) { return g.name === "tower"; })[0].origin[1] === 200, JSON.stringify(originOf("tower")));
          next(); }); }); },

    function () {
      // the race the banner must survive: our Apply reaches the daemon after another peer's edit, before its event reaches us
      p.simulateDrag(fp("lap"), 0, 50, true);
      fake("fake.layout_by", { peer: "tower", x: 3840, y: 300, by: "tower" }, function () {});
      p.apply();
      waitFor(function () { return p.banner !== null && !p.applying; }, 3000, function (ok) {
        check("a stale_layout answer shows the banner, before or after the snapshot", ok, "applyError=" + JSON.stringify(p.applyError));
        waitFor(function () { return p.conflict !== null; }, 3000, function (ok2) {
          check("and the snapshot then names the peer", ok2 && p.conflict.by === "tower", JSON.stringify(p.conflict));
          p.takeTheirs();
          next(); }); }); },

    function () {
      fake("fake.fixture", { name: "three" }, function () {
        waitFor(function () { return p.rev === 14 && p.groups.length === 3 && !p.dirty; }, 3000, function () {
          p.resetForTest();
          var lap = fp("lap");
          p.forceActiveFocus();
          check("Tab picks the first machine from the left", p.step(true) && p.canvas.focusedFp === lap && p.canvas.keyboardMode, "focused=" + p.canvas.focusedFp);
          check("Tab again picks the next one, and Shift+Tab goes back", p.step(true) && p.canvas.focusedFp === fp("desk") && p.step(false) && p.canvas.focusedFp === lap, "focused=" + p.canvas.focusedFp);
          p.step(true);
          p.step(true);
          check("the machines come before the buttons: the third Tab is the last machine, the next the zoom buttons", p.canvas.focusedFp === fp("tower") || p.canvas.focusedFp === "", "focused=" + p.canvas.focusedFp);
          p.focusGroup(lap);
          check("a machine takes the keyboard focus", p.canvas.focusedFp === lap && p.canvas.keyboardMode, "focused=" + p.canvas.focusedFp);
          p.nudge(lap, "right", "step");
          check("an arrow into desk is refused, and says so", !p.dirty && /^lap cannot go there: it would cover desk\.$/.test(p.flashText), p.flashText);
          p.nudge(lap, "down", "step");
          check("an arrow moves 10 px", p.draft.origins[lap][1] === 10, JSON.stringify(p.draft.origins));
          p.nudge(lap, "down", "far");
          check("Shift moves 100 px", p.draft.origins[lap][1] === 110, JSON.stringify(p.draft.origins));
          p.nudge(lap, "up", "edge");
          check("Ctrl goes to the next edge (back to where it was: tops aligned)", !p.dirty || p.draft.origins[lap][1] === 0 || p.draft.origins[lap][1] === -45, JSON.stringify(p.draft.origins));
          p.reset();
          p.nudge(lap, "down", "step");
          check("Esc resets a dirty draft first", p.cancelOrReset() === true && !p.dirty, "dirty=" + p.dirty);
          check("and does not stop the panel from closing the next time", p.cancelOrReset() === false, "");
          p.nudge(lap, "down", "step");
          check("Enter applies", p.enter() === true && p.applying, "applying=" + p.applying);
          waitFor(function () { return !p.applying && !p.dirty; }, 4000, function () { next(); }); }); }); },

    function () {
      fake("fake.fixture", { name: "unplaced" }, function () {
        waitFor(function () { return p.groups.some(function (g) { return g.provisional; }); }, 3000, function () {
          p.resetForTest();
          var lap = p.groups.filter(function (g) { return g.name === "lap"; })[0];
          check("a machine the layout has not placed is shown right of the others, tops level with desk", lap.provisional && lap.rect.x === 6400 && lap.rect.y === 0, JSON.stringify(lap.rect));
          check("and the panel says what to do", /^lap is not placed yet\./.test(p.status.text), p.status.text);
          check("it is not a draft until it is placed", !p.dirty, "dirty=" + p.dirty);
          p.focusGroup(lap.fp);
          check("Enter places it where it stands", p.enter() === true && p.dirty && p.draft.origins[lap.fp] !== undefined, JSON.stringify(p.draft.origins));
          p.apply();
          waitFor(function () { return !p.applying && originOf("lap") !== null && originOf("lap") !== undefined; }, 4000, function (ok) {
            check("and Apply gives it its place in the layout", ok && originOf("lap") !== null, JSON.stringify(originOf("lap")));
            next(); }); }); }); },

    function () {
      fake("fake.fixture", { name: "empty" }, function () {
        waitFor(function () { return p.mode === "empty"; }, 3000, function (ok) {
          check("with nothing paired the panel says so, and offers Pair", ok && p.groups.length === 1, "mode=" + p.mode);
          var asked = [];
          p.viewRequested.connect(function (v) { asked.push(v); });
          p.viewRequested("pair");
          check("(the empty view asks its host for the Pair tab)", asked[0] === "pair", JSON.stringify(asked));
          next(); }); }); },

    function () {
      fake("fake.fixture", { name: "three" }, function () {
        waitFor(function () { return p.mode === "arrange" && p.groups.length === 3; }, 3000, function () {
          p.resetForTest();
          var lap = fp("lap");
          p.canvas.region = { x: -60000, y: -40000, w: 120000, h: 80000 };
          var item = p.canvas.groupItem(lap);
          check("a small group still has a hit area of at least the minimum size", item.vis.w < 56 && item.width >= 56 && item.height >= 44, "vis=" + item.vis.w + " hit=" + item.width + "x" + item.height);
          p.canvas.fit();
          var fit = p.canvas.view.scale;
          p.canvas.zoomBy(2);
          check("zoom doubles the scale and Fit puts it back", p.canvas.zoomed && Math.abs(p.canvas.view.scale - 2 * fit) < 1e-9, "scale=" + p.canvas.view.scale);
          p.canvas.fit();
          check("Fit goes back to the whole picture", !p.canvas.zoomed && Math.abs(p.canvas.view.scale - fit) < 1e-9, "scale=" + p.canvas.view.scale);
          next(); }); }); },

    function () {
      // a draft survives ferryd going away and coming back
      p.simulateDrag(fp("lap"), 0, 300, true);
      fake("fake.dark", { seconds: 1.2 }, function () {
        waitFor(function () { return p.mode === "off"; }, 3000, function (ok) {
          check("with no daemon the panel says so", ok, "mode=" + p.mode);
          waitFor(function () { return p.mode === "arrange"; }, 6000, function (ok2) {
            check("it comes back by itself, with the draft still there", ok2 && p.dirty && p.draft.origins[fp("lap")][1] === 300, JSON.stringify(p.draft.origins));
            check("and no false conflict: the revision is the one it started on", p.banner === null, JSON.stringify(p.banner));
            p.reset();
            next(); }); }); }); }
  ]

  // ---------------------------------------------------- a daemon from before layout.set
  property var legacySteps: [
    function () { waitFor(function () { return c.connected && c.layout && p && p.mode === "arrange"; }, 5000, function (ok) {
      check("an M1 daemon's snapshot (no trusted, self, bounds, description) draws", ok && p.groups.length === 3 && p.groups[0].self === true, "mode=" + (p ? p.mode : "?"));
      check("it is tried for layout.set: its hello lists no methods", p.canSet === true, "canSet=" + p.canSet);
      p.simulateDrag(fp("lap"), 0, 300, true);
      check("the drag still works as a draft", p.dirty, "");
      p.apply();
      waitFor(function () { return !p.applying; }, 3000, function () {
        check("the answer unknown_method is explained, not left raw", p.applyError && p.applyError.code === "unknown_method" && /does not know layout\.set/.test(p.status.text), JSON.stringify(p.applyError));
        check("and the draft is kept", p.dirty, "");
        next(); }); }); }
  ]

  function originOfDraft(name) {
    var g = p.groups.filter(function (x) { return x.name === name; })[0];
    return g.origin;
  }

  function finish() {
    console.log("ARRANGE " + (failures === 0 ? "PASS" : "FAIL") + " " + (checks - failures) + "/" + checks + (legacy ? " (legacy daemon)" : ""));
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

  FloatingWindow {
    visible: true
    implicitWidth: 860
    implicitHeight: 600
    color: Commons.Color.popups.background

    Loader {
      id: loader
      anchors.fill: parent
      anchors.margins: 18
      source: "ferry/panels/ArrangePanel.qml"
    }
  }

  Timer { interval: 500; running: true; onTriggered: root.next() }
}
