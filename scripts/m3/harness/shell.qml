import QtQuick
import Quickshell
import Quickshell.Io
import qs.Commons as Commons
import qs.Commons
import "ferry" as Ferry
// Quickshell builds the type list of a directory only for the directories its scan reaches by
// following `import "dir"` lines from this file. A panel is loaded by path, so a folder only a
// panel imports (the Settings controls) is never scanned and its types are "not a type" here.
// The shell proper loads a plugin from outside its config folder and has no such limit.
import "ferry/settings" as FerrySettings

// The dev harness: ferry's own QML, drawn by a standalone Quickshell, offscreen, against the fake
// daemon, with the shell's real Commons and Ui modules and the user's real theme. It saves PNGs
// and exits. No Wayland, no layer-shell, no input grab and no second omarchy-shell: nothing on
// the desktop can notice it. See plugin/README.md, "The dev loop", and scripts/m3/shots.sh, which
// is the one command that runs it.
//
// Environment (shots.sh sets all of it):
//   FERRY_SOCKET           the fake daemon's socket; FerryClient reads it, as it does in the shell
//   FERRY_HARNESS_OUT      where the PNGs go
//   FERRY_HARNESS_THEMES   a JSON file of rendered themes (render-themes.py), optional
//   FERRY_HARNESS_ONLY     comma list of steps to run (e.g. "away,attention,arrange-three"); default all
//   FERRY_HARNESS_ROUNDING corner radius to draw with, since there is no Hyprland to ask (default 10)
//   FERRY_HARNESS_SCALE    how many times larger than life the PNGs are (default 2)
ShellRoot {
  id: root

  readonly property string out: Quickshell.env("FERRY_HARNESS_OUT") || "/tmp/ferry-m3-shots"
  readonly property real shotScale: Number(Quickshell.env("FERRY_HARNESS_SCALE") || 2)
  readonly property string only: Quickshell.env("FERRY_HARNESS_ONLY") || ""
  readonly property string themesFile: Quickshell.env("FERRY_HARNESS_THEMES") || ""
  property var themes: []
  property int stepIndex: -1
  property var steps: []

  Component.onCompleted: {
    // No Hyprland to ask, so say what the user's desktop has.
    Style.cornerRadius = Number(Quickshell.env("FERRY_HARNESS_ROUNDING") || 10);
    Style.gapsOut = 5;
    // Stills must not catch an animation half way: the badge pulse and the fades hold still.
    Style.reduceMotion = Quickshell.env("FERRY_HARNESS_MOTION") !== "1";
    if (themesFile) {
      try { themes = JSON.parse(themeFile.text()).themes || []; }
      catch (e) { console.warn("harness: unreadable themes file: " + e); }
    }
    if (themes.length) applyTheme(themes[0].name);
    steps = buildSteps();
    console.log("harness: " + steps.length + " steps, output in " + out);
    start.start();
  }

  FileView {
    id: themeFile
    path: root.themesFile
    blockLoading: true
    printErrors: false
  }

  // ---------------------------------------------------------------------------------- steps
  // One step = a scene to put the daemon in (and the state to wait for), a theme, a panel view, a key to
  // press, and the pictures to take. A step may also carry
  //   before: [action...]   run once the scene is set, before the picture settles
  //   after:  [action...]   run once it has settled, before the camera
  // An action is a function (h, next) that calls next() when it is done; fake(), wait(), until() and
  // doit() below make the usual ones, and `h` is api(): the client, the popover, the panel's view.

  function fake(method, params) { return function (h, next) { h.call(method, params || {}, function () { next(); }); }; }
  function wait(ms) { return function (h, next) { h.wait(ms, next); }; }
  function until(pred, ms) { return function (h, next) { h.until(function () { return pred(h); }, ms || 5000, next); }; }
  function doit(fn) { return function (h, next) { fn(h); next(); }; }

  // what a pair step starts from: a fresh daemon, a panel that has just opened, a popover with nothing open
  function pairReset() {
    return function (h, next) {
      if (h.panel && h.panel.reset) h.panel.reset();
      if (h.pop) h.pop.collapseAll();
      h.wait(300, next);
    };
  }

  function buildSteps() {
    // no daemon at all: the one picture is the widget saying so
    if (Quickshell.env("FERRY_HARNESS_OFF") === "1") {
      return [
        { name: "off", scene: "off", expect: "off", grab: ["bar", "popover"] },
        { name: "pair-off", file: "pair-off", scene: "off", expect: "off", panel: "pair", grab: ["panel"] },
        { name: "settings-off", scene: "off", expect: "off", panel: "settings", fresh: true, grab: ["panel"] }
      ];
    }
    var s = [];
    function add(step) { s.push(step); }
    var scenes = [
      ["idle-first", "idle"], ["idle", "idle"], ["ready", "ready"], ["away", "away"],
      ["controlled", "controlled"], ["locked", "locked"], ["attention", "attention"], ["lost", "ready"]
    ];
    for (var i = 0; i < scenes.length; i++)
      add({ name: scenes[i][0], scene: scenes[i][0], expect: scenes[i][1], grab: ["bar", "popover"] });
    add({ name: "away-noname", scene: "away", expect: "away", peerName: false, grab: ["bar"] });
    add({ name: "focus-lock", file: "popover-focus", scene: "ready", expect: "ready", keys: 1, grab: ["popover"] });
    add({ name: "focus-release", file: "popover-focus-release", scene: "away", expect: "away", keys: 3, grab: ["popover"] });
    // ---- the Arrange panel (stream E). `fixture` loads a layout into the fake, `drag` is a whole drag and drop
    // (plane px from where the group is), `dragLive` stops with the pointer still down, `layoutBy` is another peer
    // applying a layout, `focus` and `keys` are the keyboard ([group, direction, mode]), `file` names the PNG.
    function arrange(name, extra) {
      var step = { name: name, file: name, scene: "ready", expect: "ready", panel: "arrange", fixture: "three", grab: ["panel"] };
      for (var k in extra) step[k] = extra[k];
      add(step);
    }
    arrange("arrange-empty", { fixture: "empty", scene: "idle-first", expect: "idle" });
    arrange("arrange-two", { fixture: "two" });
    arrange("arrange-three", {});
    arrange("arrange-four", { fixture: "four" });
    arrange("arrange-hidpi", { fixture: "hidpi" });
    arrange("arrange-stack", { fixture: "stack" });
    // lap, dragged to just under desk's left edge: it snaps (the guides), and the seam shows live
    arrange("arrange-snap", { fixture: "two", dragLive: ["lap", 1790, 1062] });
    arrange("arrange-overlap", { dragLive: ["lap", 1250, 120] });
    arrange("arrange-refused", { drag: ["lap", 1250, 120] });
    arrange("arrange-edited", { drag: ["lap", 0, 300, true] });
    arrange("arrange-applied", { drag: ["lap", 0, 300, true], apply: true });
    arrange("arrange-stale", { drag: ["lap", 0, 300, true], layoutBy: { peer: "tower", x: 3840, y: 0, by: "tower" } });
    arrange("arrange-keyboard", { focus: "lap", keys: [["lap", "down", "far"], ["lap", "down", "step"]] });
    arrange("arrange-untrusted", { fixture: "untrusted" });
    arrange("arrange-unplaced", { fixture: "unplaced" });
    arrange("arrange-lonely", { fixture: "lonely" });
    arrange("arrange-offline", { fixture: "three", peer: { name: "tower", state: "offline" } });
    arrange("arrange-zoom", { fixture: "two", zoom: 2.5 });
    arrange("arrange-error", { drag: ["lap", 0, 300, true], inject: { applyError: { code: "overlap", message: "lap overlaps desk", hint: "ferry layout show lists where each peer's monitors are" } } });
    // the first light theme of the run, if there is one
    for (var lt = 0; lt < themes.length; lt++) {
      if (themes[lt].mode === "light") {
        arrange("arrange-light", { fixture: "four", theme: themes[lt].name, drag: ["mini", 0, 300, true] });
        break;
      }
    }
    add({ name: "panel-settings", scene: "ready", expect: "ready", panel: "settings", grab: ["panel"] });

    // ---- the popover, beyond its states (stream F)
    function popStep(name, scene, expect, before, after, extra) {
      add(Object.assign({ name: "popover-" + name, file: "popover-" + name, scene: scene, expect: expect, before: before || [], after: after || [], grab: ["popover"] }, extra || {}));
    }
    var ago = function (minutes, seconds) { return Date.now() - minutes * 60000 - (seconds || 0) * 1000; };
    var notices = function () {
      return doit(function (h) {
        h.client.notices = [
          { kind: "peer_lost", title: "Lost tower", body: "", peer: "tower", urgency: "normal", at: ago(41) },
          { kind: "peer_back", title: "tower is back", body: "", peer: "tower", urgency: "normal", at: ago(38) },
          { kind: "paired", title: "Paired with lap", body: "", peer: "lap", urgency: "normal", at: ago(7) },
          { kind: "pair_failed", title: "Pairing cancelled", body: "Codes differ", urgency: "normal", at: ago(2) },
          { kind: "layout_replaced", title: "Layout replaced by lap", body: "Your last change was replaced by a newer one", peer: "lap", urgency: "normal", at: ago(0, 20) }
        ];
      });
    };
    var clearNotices = doit(function (h) { h.client.notices = []; });
    popStep("notices", "ready", "ready", [notices()], [], {});
    popStep("notices-away", "away", "away", [notices()], [], {});
    popStep("actions", "ready", "ready", [], [doit(function (h) { h.pop.peerItem(h.fp("lap")).openActions(); }), wait(150)]);
    popStep("rename", "ready", "ready", [], [doit(function (h) { h.pop.peerItem(h.fp("lap")).askRename(); }), wait(200)]);
    popStep("rename-error", "ready", "ready", [], [
      doit(function (h) { var row = h.pop.peerItem(h.fp("lap")); row.askRename(); }), wait(200),
      doit(function (h) { h.pop.peerItem(h.fp("lap")).renameField.field.text = "my laptop"; }), wait(150)]);
    popStep("unpair", "ready", "ready", [], [doit(function (h) { h.pop.peerItem(h.fp("tower")).askUnpair(false); }), wait(200)]);
    popStep("attention-unpair", "attention", "attention", [], [doit(function (h) { h.pop.askUnpair("lap", true); }), wait(250)]);
    popStep("focus-row", "ready", "ready", [], [doit(function (h) { h.pop.step(true); h.pop.step(true); }), wait(100)]);
    popStep("pairing-code", "ready", "ready", [pairReset(), fake("fake.pair_next", { connect_ms: 100, other_ms: 600000 }),
      doit(function (h) { h.client.pairAccept(function () {}); }), wait(300), fake("fake.pair_arrive", { name: "tower2", addr: "198.51.100.88:4243" }),
      until(function (h) { return h.client.pairing && h.client.pairing.stage === "code"; })], [wait(300)]);
    popStep("pairing-listening", "ready", "ready", [pairReset(), doit(function (h) { h.client.pairAccept(function () {}); }),
      until(function (h) { return h.client.pairing && h.client.pairing.stage === "listening"; })], [wait(300)]);
    // light and dark, the same state
    popStep("attention-latte", "attention", "attention", [], [], { theme: "catppuccin-latte" });
    popStep("notices-latte", "away", "away", [notices()], [], { theme: "catppuccin-latte" });
    popStep("focus-latte", "ready", "ready", [], [doit(function (h) { h.pop.step(true); })], { theme: "catppuccin-latte" });

    // ---- the Pair panel (stream F): every step of both flows
    function pairStep(name, before, after, extra) {
      add(Object.assign({ name: "pair-" + name, file: "pair-" + name, scene: "ready", expect: "ready", panel: "pair", before: [pairReset()].concat(before || []), after: after || [], grab: ["panel"] }, extra || {}));
    }
    var screenIs = function (name) { return function (h) { return h.panel.screen === name; }; };
    var dial = function (addr) { return doit(function (h) { h.panel.dial(addr); }); };
    var hold = function (script) { return fake("fake.pair_next", script); };
    pairStep("start", [], []);
    pairStep("start-focus", [], [doit(function (h) { h.panel.keyboardNav = true; h.panel.screenItem.start.acceptButton.forceActiveFocus(); }), wait(100)]);
    pairStep("start-typed", [], [doit(function (h) { h.panel.typeAddress("workshop.lan"); h.panel.focusScreen(); }), wait(150)]);
    pairStep("start-untrusted", [fake("fake.untrusted", { name: "stranger" })], [wait(400)]);
    pairStep("start-nearby", [], [doit(function (h) {
      h.panel.nearby = [
        { name: "workshop", addr: "192.0.2.124:4243", fingerprint: "", trusted: false, tag: "Ready to pair" },
        { name: "lap", addr: "198.51.100.51:4243", fingerprint: "", trusted: true, tag: "Trusted" },
        { name: "den-pc", addr: "192.0.2.140:4243", fingerprint: "", trusted: false, tag: "Ready to pair" }];
    }), wait(200)]);
    pairStep("dial-error-closed", [], [dial("10.254.0.9"), until(function (h) { return h.panel.dialError !== ""; }), wait(200)]);
    pairStep("dial-error-unreachable", [], [dial("10.255.0.9"), until(function (h) { return h.panel.dialError !== ""; }, 6000), wait(200)]);
    pairStep("dial-error-typed", [], [doit(function (h) { h.panel.dial("not an address"); }), wait(200)]);
    pairStep("dial-connecting", [hold({ connect_ms: 600000, other_ms: 600000 })], [dial("192.0.2.124"), until(screenIs("connecting")), wait(250)]);
    pairStep("dial-code", [hold({ connect_ms: 100, other_ms: 600000 })], [dial("192.0.2.124"), until(screenIs("code")), wait(300)]);
    pairStep("dial-code-focus", [hold({ connect_ms: 100, other_ms: 600000 })], [dial("192.0.2.124"), until(screenIs("code")), wait(300),
      doit(function (h) { h.panel.keyboardNav = true; h.panel.matchButton.forceActiveFocus(); }), wait(100)]);
    pairStep("dial-waiting", [hold({ connect_ms: 100, other_ms: 600000 })], [dial("192.0.2.124"), until(screenIs("code")),
      doit(function (h) { h.panel.answer(true); }), until(screenIs("waiting")), wait(250)]);
    pairStep("dial-done", [hold({ connect_ms: 100, other_ms: 200, name: "workshop", fingerprint: "7f3a91c2".padEnd(56, "d") + "5be80d4e" })],
      [dial("192.0.2.124"), until(screenIs("code")), doit(function (h) { h.panel.answer(true); }), until(screenIs("paired")), wait(450)]);
    pairStep("dial-mismatch", [hold({ connect_ms: 100, other: "mismatch", other_ms: 300 })], [dial("192.0.2.124"), until(screenIs("failed")), wait(300)]);
    pairStep("dial-differ", [hold({ connect_ms: 100, other_ms: 600000 })], [dial("192.0.2.124"), until(screenIs("code")),
      doit(function (h) { h.panel.answer(false); }), until(screenIs("failed")), wait(300)]);
    pairStep("dial-timeout", [hold({ connect_ms: 100, other: "silent", window_s: 2.5 })], [dial("192.0.2.124"), until(screenIs("code")),
      doit(function (h) { h.panel.answer(true); }), until(screenIs("failed"), 8000), wait(300)]);
    pairStep("dial-cancelled-by-peer", [hold({ connect_ms: 100, other: "cancel", other_ms: 300 })], [dial("192.0.2.124"), until(screenIs("failed")), wait(300)]);
    pairStep("dial-version", [hold({ connect_ms: 100, other: "version" })], [dial("192.0.2.124"), until(screenIs("failed")), wait(300)]);
    pairStep("dial-lost", [hold({ connect_ms: 100, other: "drop", other_ms: 300 })], [dial("192.0.2.124"), until(screenIs("failed")), wait(300)]);
    pairStep("accept-listening", [], [doit(function (h) { h.panel.accept(); }), until(screenIs("listening")), wait(300)]);
    pairStep("accept-code", [hold({ connect_ms: 100, other_ms: 600000, name: "tower2" })], [doit(function (h) { h.panel.accept(); }), until(screenIs("listening")),
      fake("fake.pair_arrive", { addr: "198.51.100.88:4243" }), until(screenIs("code")), wait(300)]);
    pairStep("accept-waiting", [hold({ connect_ms: 100, other_ms: 600000, name: "tower2" })], [doit(function (h) { h.panel.accept(); }), until(screenIs("listening")),
      fake("fake.pair_arrive", { addr: "198.51.100.88:4243" }), until(screenIs("code")), doit(function (h) { h.panel.answer(true); }), until(screenIs("waiting")), wait(250)]);
    pairStep("accept-done", [hold({ connect_ms: 100, other_ms: 200, name: "tower2" })], [doit(function (h) { h.panel.accept(); }), until(screenIs("listening")),
      fake("fake.pair_arrive", { addr: "198.51.100.88:4243" }), until(screenIs("code")), doit(function (h) { h.panel.answer(true); }), until(screenIs("paired")), wait(450)]);
    pairStep("accept-failed-listening", [hold({ connect_ms: 100, other: "mismatch", other_ms: 300, name: "tower2" })], [doit(function (h) { h.panel.accept(); }), until(screenIs("listening")),
      fake("fake.pair_arrive", { addr: "198.51.100.88:4243" }), until(function (h) { return h.panel.screen === "listening" && h.panel.flow.lastFailure !== null; }), wait(300)]);
    pairStep("accept-closed", [hold({ window_s: 1.5 })], [doit(function (h) { h.panel.accept(); }), until(screenIs("closed"), 6000), wait(300)]);
    // a pairing only status shows (a daemon that sends the code to the connection that started it): no code, no Cancel
    pairStep("elsewhere", [hold({ connect_ms: 100, other_ms: 600000, name: "tower2" })], [fake("fake.pair_elsewhere", { role: "acceptor" }),
      fake("fake.pair_arrive", { addr: "198.51.100.88:4243" }), until(screenIs("code")),
      doit(function (h) { h.client.pairing = null; }), until(function (h) { return h.panel.flow.noCode; }), wait(300)]);
    pairStep("trusted-rename", [], [doit(function (h) { h.panel.trustedItem(h.fp("lap")).askRename(); }), wait(250),
      doit(function (h) { h.panel.trustedItem(h.fp("lap")).renameField.field.text = "laptop"; }), wait(150)]);
    pairStep("trusted-unpair", [], [doit(function (h) { h.panel.trustedItem(h.fp("tower")).askUnpair(); }), wait(250)]);
    pairStep("light-start", [], [], { theme: "catppuccin-latte" });
    pairStep("light-code", [hold({ connect_ms: 100, other_ms: 600000 })], [dial("192.0.2.124"), until(screenIs("code")), wait(300)], { theme: "catppuccin-latte" });
    pairStep("light-done", [hold({ connect_ms: 100, other_ms: 200 })], [dial("192.0.2.124"), until(screenIs("code")), doit(function (h) { h.panel.answer(true); }), until(screenIs("paired")), wait(450)], { theme: "catppuccin-latte" });
    pairStep("light-failed", [hold({ connect_ms: 100, other: "mismatch", other_ms: 300 })], [dial("192.0.2.124"), until(screenIs("failed")), wait(300)], { theme: "catppuccin-latte" });

    // whatever daemon is there, as it is: shots.sh --daemon SOCKET runs only this
    add({ name: "live", grab: ["bar", "popover"] });
    add({ name: "live-arrange", panel: "arrange", grab: ["panel"] });
    // ---- Settings (stream G). `settings` is what to do once the view is up: ["set", key, value] sends a
    // value as the user would, ["focus", key] puts the keyboard on a control, ["record", held...]
    // starts recording the escape chord, ["scroll", y] scrolls, ["show", key] scrolls a setting to the
    // top. `fresh` builds the view again first,
    // so the errors of one step are not on the screen of the next.
    var settingsViews = [
      ["", []],
      ["-error", [["set", "crossing.edge_resistance_px", 900], ["set", "crossing.escape_chord", "Ctrl+Escape"], ["set", "peer.name", "two words"], ["show", "crossing.corner_dead_zone_px"]]],
      ["-pending", [["set", "peer.port", 4300], ["set", "input.capture", "off"], ["set", "crossing.edge_resistance_px", 64], ["set", "crossing.escape_chord", "Super+Alt+F12"], ["show", "crossing.escape_chord"]]],
      ["-focus", [["focus", "crossing.edge_resistance_px"]]],
      ["-focus-toggle", [["focus", "crossing.block_while_button_held"]]],
      ["-focus-dropdown", [["focus", "input.capture"], ["show", "input.capture"]]],
      ["-record", [["record", "Super", "Ctrl"], ["show", "crossing.escape_chord"]]],
      ["-scrolled", [["scroll", 99999]]]
    ];
    for (var sv = 0; sv < settingsViews.length; sv++)
      add({ name: "settings" + settingsViews[sv][0], scene: "ready", expect: "ready", panel: "settings", fresh: true,
        settings: settingsViews[sv][1], grab: ["panel"] });
    for (var t = 0; t < themes.length; t++) {
      add({ name: "theme-" + themes[t].name + "-away", theme: themes[t].name, scene: "away", expect: "away", grab: ["stage"] });
      add({ name: "theme-" + themes[t].name + "-attention", theme: themes[t].name, scene: "attention", expect: "attention", grab: ["stage"] });
      for (var tv = 0; tv < settingsViews.length; tv++)
        add({ name: "settings" + settingsViews[tv][0] + "-" + themes[t].name, theme: themes[t].name, scene: "ready", expect: "ready",
          panel: "settings", fresh: true, settings: settingsViews[tv][1], grab: ["panel"] });
    }
    // a step that names a theme the run was not given is left out, not drawn in the wrong colours
    s = s.filter(function (step) { return !step.theme || themes.some(function (th) { return th.name === step.theme; }); });
    if (only) {
      var wanted = only.split(",");
      s = s.filter(function (step) { return wanted.indexOf(step.name) >= 0; });
    }
    return s;
  }

  // the theme on screen, so a step that names none is drawn in the first one and not in whatever the
  // step before it left behind
  property string currentTheme: ""

  function applyTheme(name) {
    for (var i = 0; i < themes.length; i++) {
      if (themes[i].name !== name) continue;
      currentTheme = name;
      // exactly what the shell does for `applyTheme` (shell.qml): palette, then the shell tokens
      Commons.Color.loadColors(themes[i].colors);
      Commons.Color.loadShell(themes[i].shell);
      return true;
    }
    return false;
  }

  Timer {
    id: start
    interval: 700
    onTriggered: root.next()
  }

  function next() {
    stepIndex++;
    if (stepIndex >= steps.length) {
      console.log("harness: done");
      Qt.quit();
      return;
    }
    run(steps[stepIndex]);
  }

  // what an action can reach
  function api() {
    return {
      client: Ferry.FerryClient,
      pop: stage.popoverContent,
      panel: stage.panelContent.viewItem,
      call: function (method, params, cb) { Ferry.FerryClient.call(method, params, function (r, e) { if (e) console.warn("harness: " + method + " failed: " + JSON.stringify(e)); cb(r, e); }); },
      wait: waitMs,
      until: function (pred, ms, cb) { waitFor(pred, ms, cb); },
      // the fingerprint of a trusted peer by name
      fp: function (name) {
        var peers = Ferry.FerryClient.status ? Ferry.FerryClient.status.peers : [];
        for (var i = 0; i < peers.length; i++) if (peers[i].name === name) return peers[i].fingerprint;
        return "";
      }
    };
  }

  function seq(actions, done) {
    var i = 0;
    var h = api();
    function go() {
      if (i >= actions.length) { done(); return; }
      var action = actions[i++];
      // an action that throws must not stop the run: the picture is taken anyway, and shows what went wrong
      try { action(h, go); }
      catch (e) { console.warn("harness: an action failed: " + e); go(); }
    }
    go();
  }

  function run(step) {
    console.log("harness: " + step.name);
    var theme = step.theme || (themes.length ? themes[0].name : "");
    if (theme && theme !== currentTheme) applyTheme(theme);
    stage.showPeerName = step.peerName !== false;
    // `fresh`: pass through another view, and come back in go(), so the view is built again
    if (step.panel) stage.panelView = step.fresh ? "pair" : step.panel;
    var grabs = step.grab || [];
    // only lay out what the picture needs, so a theme shot is not tall with a panel nobody asked for
    stage.showPopover = grabs.indexOf("popover") >= 0 || grabs.indexOf("stage") >= 0;
    stage.showPanel = grabs.indexOf("panel") >= 0;
    // each picture starts from a plain popover and, when it is not about the panel, a quiet one
    if (stage.popoverContent && stage.popoverContent.collapseAll) stage.popoverContent.collapseAll();
    if (!step.keepNotices && Ferry.FerryClient.notices.length > 0 && step.name.indexOf("notices") < 0) Ferry.FerryClient.notices = [];

    var go = function () {
      if (step.fresh) stage.panelView = step.panel;
      // let the bindings, the colour animations and the layout settle before the camera
      waitMs(step.theme ? 900 : 500, function () {
        var panelItem = stage.panelContent.viewItem;
        if (panelItem && panelItem.resetForTest) panelItem.resetForTest();
        // `keys: 3` is three Tab presses in the popover; an array is the Arrange keyboard (actions, below)
        for (var k = 0; k < (typeof step.keys === "number" ? step.keys : 0); k++) stage.popoverContent.step(true);
        waitMs(120, function () {
          actions(step, panelItem, function () {
            // the Settings hooks (set, focus, record, scroll, show), one at a time
            applySettings(step.settings || [], function () {
              // then the step's own actions (the popover and Pair steps), then the camera
              seq(step.after || [], function () { waitMs(250, function () { grabAll(step, 0); }); });
            });
          });
        });
      });
    };
    var begin = function () { seq(step.before || [], go); };

    if (!step.scene) { begin(); return; }
    if (step.scene === "off") { waitFor(function () { return Ferry.FerryClient.state === "off"; }, 3000, begin); return; }
    // The world: a fresh one in the scene every time (a pairing leaves a peer behind that the next picture must
    // not see), then the layout the step asks for (`fixture`), then the scene over it, which the fixture reset
    var inScene = function () {
      Ferry.FerryClient.call("fake.scene", { name: step.scene }, function (result, error) {
        if (error) console.warn("harness: fake.scene failed: " + JSON.stringify(error));
        waitFor(function () { return Ferry.FerryClient.state === step.expect; }, 4000, begin);
      });
    };
    Ferry.FerryClient.call("fake.reset", { scene: step.scene }, function (result, error) {
      if (error) console.warn("harness: fake.reset failed: " + JSON.stringify(error));
      if (!step.fixture) { waitFor(function () { return Ferry.FerryClient.state === step.expect; }, 4000, begin); return; }
      Ferry.FerryClient.call("fake.fixture", { name: step.fixture }, function (r, e) {
        if (e) console.warn("harness: fake.fixture failed: " + JSON.stringify(e));
        inScene();
      });
    });
  }

  // What a step does to the Arrange panel once it is up, in this order: a peer's state, a whole drag
  // or one still in the pointer's hand, the keyboard, a zoom, another peer's edit, Apply, and any
  // property set directly. Each waits for its own effect before the next.
  function actions(step, item, done) {
    var queue = [];
    function later(f) { queue.push(f); }
    var fakeCall = function (method, params, next) {
      Ferry.FerryClient.call(method, params, function (r, e) {
        if (e) console.warn("harness: " + method + " failed: " + JSON.stringify(e));
        waitMs(350, next);
      });
    };
    if (step.peer) later(function (next) { fakeCall("fake.peer", step.peer, next); });
    if (item && item.simulateDrag) {
      var fpOf = function (name) { return item.groupFp(name); };
      if (step.drag) later(function (next) { item.simulateDrag(fpOf(step.drag[0]), step.drag[1], step.drag[2], step.drag[3] === true); waitMs(150, next); });
      if (step.dragLive) later(function (next) { item.simulateDragLive(fpOf(step.dragLive[0]), step.dragLive[1], step.dragLive[2], step.dragLive[3] === true); waitMs(150, next); });
      if (step.focus) later(function (next) { item.focusGroup(fpOf(step.focus)); waitMs(100, next); });
      if (Array.isArray(step.keys)) {
        step.keys.forEach(function (key) {
          later(function (next) { item.simulateKey(fpOf(key[0]), key[1], key[2]); waitMs(100, next); });
        });
      }
      if (step.zoom) later(function (next) { item.canvas.zoomBy(step.zoom); waitMs(150, next); });
    }
    if (step.layoutBy) later(function (next) { fakeCall("fake.layout_by", step.layoutBy, next); });
    if (item && step.apply) {
      later(function (next) {
        item.apply();
        waitFor(function () { return !item.applying; }, 3000, function () { waitMs(400, next); });
      });
    }
    if (item && step.inject) {
      later(function (next) { for (var key in step.inject) item[key] = step.inject[key]; waitMs(100, next); });
    }
    var i = 0;
    var next = function () { if (i < queue.length) queue[i++](next); else done(); };
    next();
  }

  // where the Arrange panel's things are in the card, for scripts/m3/vm-plugin.sh, which drives a real pointer
  function printGeometry(step) {
    var item = stage.panelContent.viewItem;
    if (!item || !item.geometry || !stage.showPanel) return;
    var g = item.geometry();
    var o = item.mapToItem(stage.panel, 0, 0);
    console.log("harness: geometry " + step.name + " " + JSON.stringify({ origin: { x: Math.round(o.x), y: Math.round(o.y) }, geometry: g }));
  }

  // The Settings view's hooks, one op at a time (an op that sends something waits for the answer).
  function applySettings(ops, done) {
    var view = stage.panelContent.viewItem;
    var i = 0;
    var next = function () {
      if (i >= ops.length || !view) { done(); return; }
      var op = ops[i++];
      if (op[0] === "set" && view.simulateSet) view.simulateSet(op[1], op[2]);
      else if (op[0] === "focus" && view.focusKey) view.focusKey(op[1]);
      else if (op[0] === "record" && view.startRecording) view.startRecording("crossing.escape_chord", op.slice(1));
      else if (op[0] === "scroll" && view.scrollTo) view.scrollTo(op[1]);
      else if (op[0] === "show" && view.showKey) view.showKey(op[1]);
      waitMs(op[0] === "set" ? 350 : 200, next);
    };
    next();
  }

  function grabAll(step, i) {
    if (i === 0) printGeometry(step);
    if (i >= (step.grab || []).length) { next(); return; }
    var what = step.grab[i];
    var item = what === "bar" ? stage.bar : (what === "popover" ? stage.popover : (what === "panel" ? stage.panel : stage));
    // a step may name its own file (the popover and pair steps do, as popover-*.png and pair-*.png)
    var file = step.file && i === 0 ? out + "/" + step.file + ".png" : out + "/" + (what === "stage" ? "" : what + "-") + step.name + ".png";
    item.grabToImage(function (result) {
      var ok = result.saveToFile(file);
      console.log("harness: " + (ok ? "saved " : "FAILED ") + file);
      grabAll(step, i + 1);
    }, Qt.size(item.width * root.shotScale, item.height * root.shotScale));
  }

  // ------------------------------------------------------------------------------- timing
  Timer {
    id: waiter
    property var done: null
    onTriggered: { var f = done; done = null; if (f) f(); }
  }
  function waitMs(ms, f) {
    waiter.done = f;
    waiter.interval = ms;
    waiter.restart();
  }

  Timer {
    id: poller
    property var pred: null
    property var done: null
    property real deadline: 0
    interval: 40
    repeat: true
    onTriggered: {
      if (pred()) { stop(); var f = done; done = null; f(); }
      else if (Date.now() > deadline) { stop(); console.warn("harness: gave up waiting for the daemon's state"); var g = done; done = null; g(); }
    }
  }
  function waitFor(pred, timeoutMs, f) {
    poller.pred = pred;
    poller.done = f;
    poller.deadline = Date.now() + timeoutMs;
    poller.restart();
  }

  // ------------------------------------------------------------------------------- the picture
  FloatingWindow {
    id: window
    visible: true
    implicitWidth: stage.implicitWidth
    implicitHeight: stage.implicitHeight
    color: "#000000"

    Stage {
      id: stage
      anchors.fill: parent
    }
  }
}
