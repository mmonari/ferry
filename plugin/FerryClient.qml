pragma Singleton
import QtQuick
import Quickshell
import Quickshell.Io
import "Model.js" as Model

// The one connection from the shell to ferryd (contract section 5). Every view of this
// plugin, on every bar surface and in every panel, reads from here and sends through here.
//
//   * Connects to $XDG_RUNTIME_DIR/ferry/ferryd.sock (FERRY_SOCKET overrides it, for the dev
//     loop), reads `hello`, and when the daemon offers `subscribe` takes every topic: a snapshot
//     at once, then one on every change. A daemon from before M3 has no `subscribe`; this file
//     polls status, peers and layout.get every second instead, so one plugin works with both.
//   * Reconnects when the daemon's socket file appears (an inotify watch, instant when the socket
//     directory exists) and, after a connection is lost, every second for 15 s, so a restart is
//     live again well within the 2 s of FR-UI-03. A machine where ferryd stays off is then tried
//     every 5 s, so it does not fill the journal with a failed connect per second.
//     All I/O is asynchronous: nothing here can stall the bar.
//   * `call(method, params, callback)` answers once, with a result or an error (a timeout is an
//     error too). Pairing progress arrives as events on the same connection and lands in
//     `pairing`, whichever way it started.
//
// A QML singleton is created once per shell, however many bar surfaces draw the widget, so ferryd
// serves this one socket and not one per monitor. Singletons are not hot-reloaded: a change to
// this file needs `omarchy restart shell`.
Singleton {
  id: root

  // ------------------------------------------------------------------ where and whether
  readonly property string socketPath: {
    var forced = Quickshell.env("FERRY_SOCKET");
    if (forced) return forced;
    var run = Quickshell.env("XDG_RUNTIME_DIR");
    return (run && run.length ? run : "/run/user/1000") + "/ferry/ferryd.sock";
  }

  // How often to try again while ferryd is away: every second for the first 15 failures, which is
  // what a restart needs, then every 5 s. And how often to poll a daemon that cannot push.
  readonly property int retryFastMs: 1000
  readonly property int retrySlowMs: 5000
  readonly property int retryFastCount: 15
  readonly property int pollMs: 1000
  readonly property int callTimeoutMs: 5000

  property var hello: null
  readonly property bool connected: sock.connected && hello !== null
  readonly property bool eventMode: connected && Model.supportsEvents(hello)
  // Bumped on every (re)connection, so a view holding local state can tell it is talking to a
  // new daemon session.
  property int session: 0

  // ---------------------------------------------------------------------------- snapshots
  // null until the first one arrives; cleared when the connection drops, so a view never shows
  // the peers of a daemon that is gone.
  property var status: null
  property var peers: null
  property var layout: null
  property var config: null
  // The keys whose saved value is not the one this ferryd runs with: they apply after a restart.
  // From `config.get` and the `config` event (`pending`); empty when none wait.
  property var configPending: []
  property var configSchema: null
  // The last pairing event on this machine, or null. Stage values: listening, connecting, code,
  // waiting_peer, paired, failed, closed. `pairingAt` is when it arrived (ms): its `expires_ms` is
  // what was left then, so a view that counts down needs it.
  property var pairing: null
  property real pairingAt: 0
  // Every notice carries `at`, when it arrived (a notice has no time of its own), and the last five are kept.
  property var lastNotice: null
  property var notices: []

  // ---------------------------------------------------------------------- derived, one place
  readonly property var attention: connected ? Model.attentionOf(status, layout) : []
  readonly property var summary: connected ? Model.summaryOf(status, layout) : ({ state: "off" })
  readonly property string state: summary.state
  readonly property string meName: Model.meName(status, hello)
  readonly property string escapeChord: Model.chordOf(status)
  readonly property bool locked: status ? status.locked === true : false
  readonly property bool firstRun: Model.isFirstRun(connected, status, peers)
  readonly property string version: hello && hello.version ? String(hello.version) : ""
  readonly property int revision: layout && layout.revision ? layout.revision.counter : 0

  // ----------------------------------------------------------------------------- signals
  // Every toast ferryd shows is also a notice, and a notification action can ask for a view.
  signal noticeReceived(var notice)
  signal uiRequested(string view)
  // A crossing over an edge of this machine: `{ role, home, peer, monitor, side, start, end }`.
  signal edgeCrossed(var edge)
  signal pairingEvent(var event)

  function hasMethod(name) {
    return connected && Model.hasMethod(hello, name);
  }

  // call("lock", { on: true }, function (result, error) { ... }). Exactly one of the two is set.
  // Without a connection the callback runs at once with a `not_connected` error.
  function call(method, params, callback, timeoutMs) {
    var id = d.nextId++;
    if (!sock.connected) {
      if (callback) Qt.callLater(function () { callback(null, { code: "not_connected", message: "ferryd is not running" }); });
      return 0;
    }
    var request = { id: id, method: method };
    if (params !== undefined && params !== null) request.params = params;
    d.pending[id] = {
      method: method,
      callback: callback || null,
      deadline: Date.now() + (timeoutMs || root.callTimeoutMs)
    };
    d.pendingCount++;
    sock.write(JSON.stringify(request) + "\n");
    sock.flush();
    guard.running = true;
    return id;
  }

  // ----------------------------------------------------------------- the actions views use
  // Each one is one socket command. Their answers patch the snapshot at once, so a
  // toggle moves when it is pressed even when the daemon cannot push (a poll is a second away).

  function setLock(on, callback) {
    root.call("lock", { on: !!on }, function (result, error) {
      if (result && root.status) root.status = Object.assign({}, root.status, { locked: result.locked === true });
      root.refreshSoon();
      if (callback) callback(result, error);
    });
  }

  function release(callback) {
    root.call("release", null, function (result, error) {
      root.refreshSoon();
      if (callback) callback(result, error);
    });
  }

  function setConfig(key, value, callback) {
    root.call("config.set", { key: key, value: value }, function (result, error) {
      if (result && root.config) {
        var next = Object.assign({}, root.config);
        next[result.key] = result.value;
        root.config = next;
        // `configPending` is not patched here: whether the saved value differs from the one in
        // force is ferryd's to say, and its `config` event does (a guess from `applies` would stay
        // wrong when a value is put back before the event, which sends nothing).
      }
      if (callback) callback(result, error);
    });
  }

  // ----------------------------------------------------------- pairing and peers (Pair panel, popover)
  // Each one is one socket command, as above. `callback(result, error)` is optional, and runs once.

  // Dial the machine at `addr` ("host", "host:port"). The `pairing` events follow; an error here (the
  // other machine is not accepting, or does not answer) comes with no event after it.
  function pairStart(addr, callback) {
    root.call("pair.start", { addr: addr }, function (result, error) {
      if (callback) callback(result, error);
    }, 15000);
  }

  // Open the pairing window (two minutes) for another machine to pair with this one.
  function pairAccept(callback) {
    root.call("pair.accept", null, function (result, error) { if (callback) callback(result, error); });
  }

  // "Same code?": `match` true or false. Any connection may answer while a code is shown, and the
  // first answer wins (a later one is a `no_pairing` error that only says it came second).
  function pairAnswer(match, callback) {
    root.call("pair.answer", { match: !!match }, function (result, error) { if (callback) callback(result, error); });
  }

  // Only the connection that opened the pairing may cancel it; any other gets `no_pairing`.
  function pairCancel(callback) {
    root.call("pair.cancel", null, function (result, error) { if (callback) callback(result, error); });
  }

  // `peer` is a name or a fingerprint.
  function unpair(peer, callback) {
    root.call("unpair", { peer: peer }, function (result, error) {
      root.refreshSoon();
      if (callback) callback(result, error);
    });
  }

  function renamePeer(peer, name, callback) {
    root.call("peer.rename", { peer: peer, name: name }, function (result, error) {
      root.refreshSoon();
      if (callback) callback(result, error);
    });
  }

  // The saved values, and which of them wait for a restart: the `config.get` result and the
  // `config` event have this one shape (a bare map is accepted too).
  function applyConfig(data) {
    if (data && data.values) {
      root.config = data.values;
      root.configPending = Array.isArray(data.pending) ? data.pending : [];
    } else {
      root.config = data;
      root.configPending = [];
    }
  }

  // A daemon that pushes will say so itself; one that cannot is asked again now, not in a second.
  function refreshSoon() {
    if (!Model.supportsEvents(root.hello)) pollNow();
  }

  // ------------------------------------------------------------------------------ internals
  QtObject {
    id: d
    property int failures: 0
    property int nextId: 1
    property var pending: ({})
    property int pendingCount: 0
    property bool pollBusy: false
  }

  function settle(id, result, error) {
    var entry = d.pending[id];
    if (!entry) return;
    delete d.pending[id];
    d.pendingCount--;
    if (entry.callback) {
      try { entry.callback(result === undefined ? null : result, error || null); }
      catch (e) { console.warn("ferry: callback of " + entry.method + " threw: " + e); }
    }
  }

  function failAll(error) {
    var ids = Object.keys(d.pending);
    for (var i = 0; i < ids.length; i++) settle(Number(ids[i]), null, error);
  }

  function onLine(line) {
    var msg;
    try { msg = JSON.parse(line); }
    catch (e) { console.warn("ferry: unreadable line from ferryd: " + String(line).slice(0, 80)); return; }
    if (msg === null || typeof msg !== "object") return;

    if (msg.event !== undefined) { onEvent(String(msg.event), msg.data); return; }
    if (msg.id === undefined) return;
    if (msg.error !== undefined) settle(msg.id, null, msg.error);
    else settle(msg.id, msg.result, null);
  }

  function onEvent(name, data) {
    switch (name) {
      case "hello":
        root.hello = data || ({});
        root.session++;
        afterHello();
        break;
      case "state": root.status = data; break;
      case "peers": root.peers = data; break;
      case "layout": root.layout = data; break;
      case "config": root.applyConfig(data); break;
      case "pairing":
        root.pairing = data;
        root.pairingAt = Date.now();
        root.pairingEvent(data);
        break;
      case "notice":
        var notice = Object.assign({}, data, { at: Date.now() });
        // The text of a pair_code toast holds the pairing code. It is a prompt that ferryd closes when the
        // pairing moves on, and nothing that outlives the pairing may keep it.
        if (notice.kind === "pair_code") { notice.title = ""; notice.body = ""; }
        root.lastNotice = notice;
        var kept = root.notices.slice(-4);
        kept.push(notice);
        root.notices = kept;
        root.noticeReceived(notice);
        break;
      case "ui":
        if (data && data.open) root.uiRequested(String(data.open));
        break;
      case "edge":
        if (data && data.monitor && data.end > data.start) root.edgeCrossed(data);
        break;
    }
  }

  // The daemon spoke first: either subscribe to it or start polling it.
  function afterHello() {
    if (Model.supportsEvents(root.hello)) {
      root.call("subscribe", null, function (result, error) {
        if (error) console.warn("ferry: subscribe refused: " + error.message);
      });
    } else {
      pollNow();
    }
    if (Model.hasMethod(root.hello, "config.schema")) {
      root.call("config.schema", null, function (result) { if (result) root.configSchema = result.keys || null; });
    }
    if (Model.hasMethod(root.hello, "config.get") && !Model.supportsEvents(root.hello)) {
      root.call("config.get", null, function (result) { if (result) root.applyConfig(result); });
    }
  }

  // One poll round: three requests, never a second round while one is in flight.
  function pollNow() {
    if (!sock.connected || d.pollBusy || Model.supportsEvents(root.hello)) return;
    d.pollBusy = true;
    var left = 3;
    function done() { if (--left === 0) d.pollBusy = false; }
    root.call("status", null, function (r) { if (r) root.status = r; done(); });
    root.call("peers", null, function (r) { if (r) root.peers = r; done(); });
    root.call("layout.get", null, function (r) { if (r) root.layout = r; done(); });
  }

  function dropSession() {
    root.hello = null;
    root.status = null;
    root.peers = null;
    root.layout = null;
    root.config = null;
    root.configPending = [];
    root.configSchema = null;
    root.pairing = null;
    root.pairingAt = 0;
    d.pollBusy = false;
    failAll({ code: "not_connected", message: "ferryd went away" });
  }

  Socket {
    id: sock
    path: root.socketPath
    parser: SplitParser {
      splitMarker: "\n"
      onRead: function (line) { root.onLine(line); }
    }
    onConnectedChanged: {
      // one line per change, so `quickshell log` says when ferryd came and went
      console.log("ferry: socket " + (connected ? "up" : "down") + " " + root.socketPath);
      if (connected) d.failures = 0;
      else root.dropSession();
    }
  }

  // Toggling `connected` is how Socket is asked to try again: a failed attempt leaves it false,
  // and a constant `true` binding is not re-run.
  function attempt() {
    if (sock.connected) return;
    d.failures++;
    sock.connected = false;
    sock.connected = true;
  }

  // The watch. The socket file appears when ferryd starts and goes when it stops, and FileView
  // reports both (it also reports a file that did not exist yet, which is how the shell notices a
  // new shell.toml). Reading a socket as a file fails at once and harmlessly; the failure is the
  // point of printErrors being off.
  FileView {
    path: root.socketPath
    watchChanges: true
    printErrors: false
    onFileChanged: root.attempt()
  }

  // The first attempt, and the timer under the watch. The watch cannot see a socket whose
  // directory is gone, and it re-arms three seconds after a deletion.
  Component.onCompleted: root.attempt()
  Timer {
    interval: d.failures < root.retryFastCount ? root.retryFastMs : root.retrySlowMs
    repeat: true
    running: !sock.connected
    onTriggered: root.attempt()
  }

  // Polls a daemon that cannot push.
  Timer {
    interval: root.pollMs
    repeat: true
    running: sock.connected && root.hello !== null && !Model.supportsEvents(root.hello)
    onTriggered: root.pollNow()
  }

  // Calls that never got an answer fail after their deadline instead of hanging a view.
  Timer {
    id: guard
    interval: 500
    repeat: true
    running: false
    onTriggered: {
      var now = Date.now();
      var ids = Object.keys(d.pending);
      for (var i = 0; i < ids.length; i++) {
        var entry = d.pending[ids[i]];
        if (entry && entry.deadline < now)
          root.settle(Number(ids[i]), null, { code: "timeout", message: entry.method + " did not answer" });
      }
      if (d.pendingCount <= 0) guard.running = false;
    }
  }
}
