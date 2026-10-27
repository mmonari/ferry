pragma Singleton
import QtQuick
import Quickshell
import Quickshell.Hyprland
import Quickshell.Io
import "."

// What the plugin does besides drawing the bar button: it owns the `ferry` IPC target,
// opens the popover on the right monitor, and shows and hides the setup panel (Arrange, Pair,
// Settings). One instance per shell, however many bar surfaces draw the widget, so the IPC target
// has one handler and there is never more than one panel.
//
// Why the panel lives here and not as a second `panel` plugin: a third-party plugin may summon
// only its own id, so the widget could not open a sibling plugin, and a manifest that carries both
// kinds sends `omarchy-shell shell toggle` and SUPER+CTRL+<n> to the panel instead of the popover.
// See plugin/README.md, "Where the panels live".
Singleton {
  id: root

  // The bar widgets that exist (one per bar surface), and the shell's scoped API, taken from them.
  property var widgets: []
  property var shell: null

  property bool panelOpen: false
  // "arrange" | "pair" | "settings"
  property string panelView: "arrange"
  readonly property var views: ["arrange", "pair", "settings"]
  // The Arrange panel's unapplied draft (plugin/Arrange.js), kept here while the panel is open so that a
  // visit to the Pair or Settings tab does not throw it away. The view clears it when the panel closes.
  property var arrangeDraft: null

  // ----------------------------------------------------------------------------- widgets
  function attach(widget) {
    if (widgets.indexOf(widget) < 0) widgets = widgets.concat([widget]);
    if (widget && widget.bar && widget.bar.shell) shell = widget.bar.shell;
  }

  function detach(widget) {
    widgets = widgets.filter(function (w) { return w !== widget; });
  }

  // The popover. Through the shell when it can answer, because the bar knows which monitor has
  // the focus; otherwise the first widget.
  function togglePopover() {
    if (root.shell && root.shell.toggle("ferry.bar", "{}")) return;
    if (widgets.length) widgets[0].toggle();
  }

  function openPopover() {
    if (root.shell && root.shell.summon("ferry.bar", "{}")) return;
    if (widgets.length) widgets[0].open();
  }

  function closePopover() {
    if (root.shell && root.shell.hide("ferry.bar")) return;
    for (var i = 0; i < widgets.length; i++) widgets[i].close();
  }

  // ------------------------------------------------------------------------------ panels
  function openPanel(view) {
    if (views.indexOf(view) >= 0) panelView = view;
    // Asked from the popover, from a keybind or from a notification: the popover is in the way
    // in all three, and a panel over an open popover would dim it and leave it there.
    closePopover();
    panelScreenName = focusedScreenName();
    panelOpen = true;
  }

  function closePanel() {
    panelOpen = false;
  }

  function togglePanel(view) {
    if (panelOpen && (!view || view === panelView)) closePanel();
    else openPanel(view || panelView);
  }

  // The panel opens centred on the monitor that has the focus when it is asked for.
  property string panelScreenName: ""

  function focusedScreenName() {
    var m = Hyprland.focusedMonitor;
    return m ? String(m.name) : "";
  }

  readonly property var panelScreen: {
    var wanted = panelScreenName;
    var screens = Quickshell.screens;
    for (var i = 0; i < screens.length; i++)
      if (screens[i].name === wanted) return screens[i];
    return screens.length ? screens[0] : null;
  }

  // The panel window exists only while it is open. It is loaded by URL, because a file that names
  // a window type cannot be compiled where there is no layer-shell (the offscreen dev harness).
  LazyLoader {
    active: root.panelOpen
    source: Qt.resolvedUrl("PanelHost.qml")
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

  // A notification action, or anything else on ferryd's side, can ask for a view.
  Connections {
    target: FerryClient

    function onUiRequested(view) {
      if (view === "popover") root.openPopover();
      else root.openPanel(view);
    }

    function onEdgeCrossed(edge) { root.glow(edge); }
  }

  // `qs ipc call ferry <function>` and `omarchy-shell ferry <function>`. Bind them in
  // Hyprland, or call them from a script.
  IpcHandler {
    target: "ferry"

    function toggle(): void { root.togglePopover(); }
    function open(): void { root.openPopover(); }
    // The panel is closed first: it cannot fail, and the popover goes through the shell.
    function close(): void { root.closePanel(); root.closePopover(); }
    function lock(): void { FerryClient.setLock(!FerryClient.locked); }
    function release(): void { FerryClient.release(); }
    // These open the view, and answer the same when it is already open, so a launcher entry or a
    // notification action never closes what it was asked to show. `panel` is the toggle.
    function arrange(): void { root.openPanel("arrange"); }
    function pair(): void { root.openPanel("pair"); }
    function settings(): void { root.openPanel("settings"); }
    function panel(): void { root.togglePanel(); }
    function state(): string { return FerryClient.state; }
  }
}
