pragma Singleton
import QtQuick
import Quickshell
import Quickshell.Io
import "."

// What the plugin does besides drawing the bar button: it owns the `ferry` IPC target,
// and it is where the one panel's state lives: open or not, which tab, where it opens, and the
// Arrange draft that must survive a visit to another tab. One instance per shell, however many bar
// surfaces draw the widget, so the IPC target has one handler.
//
// The panel itself is the widget's popout (PanelHost.qml, the shell's KeyboardPanel), so the shell's
// own `shell toggle ferry.bar`, SUPER+CTRL+<n>, Tab between popouts and the click outside all work on
// it. A second `panel` plugin could not be summoned from the widget, and a manifest with both kinds
// sends those keys to the panel loader instead. See plugin/README.md, "Where the panel lives".
Singleton {
  id: root

  // The bar widgets that exist (one per bar surface), and the shell's scoped API, taken from them.
  property var widgets: []
  property var shell: null

  // True while the panel is open on any monitor. The widget that shows it reports here (attach and
  // panelShown), so the singleton knows without owning a window of its own.
  property bool panelOpen: false
  // "screens" | "pair" | "settings". "arrange" is the old name of the first tab and is read as it.
  property string panelView: "screens"
  readonly property var views: ["screens", "pair", "settings"]
  // The Arrange panel's unapplied draft (plugin/Arrange.js), kept here while the panel is open so that a
  // visit to the Pair or Settings tab does not throw it away. The view clears it when the panel closes.
  property var arrangeDraft: null
  // The group the Settings tab shows (a group id from Model.settingsGroups); "" is the first.
  property string settingsGroup: ""

  // ----------------------------------------------------------------------------- widgets
  function attach(widget) {
    if (widgets.indexOf(widget) < 0) widgets = widgets.concat([widget]);
    if (widget && widget.bar && widget.bar.shell) shell = widget.bar.shell;
  }

  function detach(widget) {
    widgets = widgets.filter(function (w) { return w !== widget; });
    panelShown();
  }

  // a widget's panel opened or closed
  function panelShown() {
    var open = false;
    for (var i = 0; i < widgets.length; i++) if (widgets[i].opened) open = true;
    if (open === panelOpen) return;
    panelOpen = open;
    if (!open) arrangeDraft = null;
  }

  function viewOf(view) {
    if (view === "arrange") return "screens";
    return views.indexOf(view) >= 0 ? view : "";
  }

  // ------------------------------------------------------------------------------ the panel
  // Through the shell when it can answer, because the bar knows which monitor has the focus;
  // otherwise the first widget.
  function openPanel(view) {
    var v = viewOf(view);
    if (v !== "") panelView = v;
    if (panelOpen) return;
    if (root.shell && root.shell.summon("ferry.bar", "{}")) return;
    if (widgets.length) widgets[0].open();
  }

  function closePanel() {
    if (root.shell && root.shell.hide("ferry.bar")) return;
    for (var i = 0; i < widgets.length; i++) widgets[i].close();
  }

  // the bar icon, `ferry toggle` and `ferry panel`: open on the view it was on, or close
  function togglePanel(view) {
    var v = viewOf(view);
    if (panelOpen && (v === "" || v === panelView)) { closePanel(); return; }
    if (panelOpen) { panelView = v; return; }
    if (v !== "") panelView = v;
    if (root.shell && root.shell.toggle("ferry.bar", "{}")) return;
    if (widgets.length) widgets[0].toggle();
  }

  // ----------------------------------------------------------------------- edge glow
  // The pulses that run: one per edge, `{ key, monitor, side, start, end }`, each dropped
  // when its pulse is over. A new crossing of an edge that still glows starts its pulse again.
  property var glows: []
  readonly property int glowMs: 700

  function glow(edge) {
    var key = edge.monitor + "/" + edge.side + "/" + Date.now();
    var same = function (g) { return g.monitor === edge.monitor && g.side === edge.side; };
    root.glows = root.glows.filter(function (g) { return !same(g); }).concat([{
      key: key,
      monitor: String(edge.monitor),
      side: String(edge.side),
      start: Number(edge.start),
      end: Number(edge.end),
    }]);
    var t = glowTimer.createObject(root, { key: key });
    t.start();
  }

  Component {
    id: glowTimer
    Timer {
      property string key
      interval: root.glowMs
      onTriggered: {
        var k = key;
        root.glows = root.glows.filter(function (g) { return g.key !== k; });
        destroy();
      }
    }
  }

  // Loaded by URL like the panel, and only while a pulse runs, so an idle plugin has no window.
  LazyLoader {
    active: root.glows.length > 0
    source: Qt.resolvedUrl("EdgeGlowHost.qml")
  }

  // A notification action, or anything else on ferryd's side, can ask for a view. "popover" is
  // what an older ferryd says for the panel's first tab.
  Connections {
    target: FerryClient

    function onUiRequested(view) {
      root.openPanel(view === "popover" ? "screens" : view);
    }

    function onEdgeCrossed(edge) { root.glow(edge); }
  }

  // `qs ipc call ferry <function>` and `omarchy-shell ferry <function>`. Bind them in
  // Hyprland, or call them from a script.
  IpcHandler {
    target: "ferry"

    function toggle(): void { root.togglePanel(); }
    function open(): void { root.openPanel(""); }
    function close(): void { root.closePanel(); }
    function lock(): void { FerryClient.setLock(!FerryClient.locked); }
    function release(): void { FerryClient.release(); }
    // These open the view, and answer the same when it is already open, so a launcher entry or a
    // notification action never closes what it was asked to show. `panel` is the toggle.
    function arrange(): void { root.openPanel("screens"); }
    function screens(): void { root.openPanel("screens"); }
    function pair(): void { root.openPanel("pair"); }
    function settings(): void { root.openPanel("settings"); }
    function panel(): void { root.togglePanel(); }
    function state(): string { return FerryClient.state; }
  }
}
