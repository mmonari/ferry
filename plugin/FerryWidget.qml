import QtQuick
import Quickshell
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import "."
import "Glyphs.js" as Glyphs
import "Model.js" as Model

// ferry in the bar (-02, -11, -12, -16). This is the plugin's entry point
// (manifest.json, entryPoints.barWidget); the shell builds one of these per bar surface, so one
// per monitor, and every one reads the same FerryClient.
//
// The widget draws and forwards clicks. What each state looks like and says is decided in
// Model.js, which node tests run, and what ferryd said is read from FerryClient.
BarWidget {
  id: root
  moduleName: "ferry.bar"

  // ---------------------------------------------------------------- the shell's name contract
  // The bar finds a popout widget by duck-typing, and nothing reports a missing name: without
  // these four `omarchy-shell shell toggle ferry.bar`, SUPER+CTRL+<n> and Tab between popouts all
  // silently skip this widget. See plugin/README.md.
  property bool popoutSwitchClosing: false
  readonly property bool opened: controller.open

  function open() { controller.show(); }
  function close() { controller.hide(); }
  function toggle() { controller.toggle(); }
  function closeForPopoutSwitch() {
    popoutSwitchClosing = true;
    close();
    Qt.callLater(function () { popoutSwitchClosing = false; });
  }

  PanelController { id: controller }

  // A tooltip shown before the click stays up until the pointer leaves: take it down on open.
  onOpenedChanged: if (opened && root.bar && root.bar.hideTooltip) root.bar.hideTooltip(button)

  // ---------------------------------------------------------------------------- what to draw
  // `showPeerName` is this widget's one setting, on its layout entry in shell.json (default on).
  readonly property bool showPeerName: root.setting("showPeerName", true) !== false

  readonly property var view: Model.widget({
    connected: FerryClient.connected,
    status: FerryClient.status,
    layout: FerryClient.layout,
    summary: FerryClient.summary,
    attention: FerryClient.attention,
    hello: FerryClient.hello,
    chord: FerryClient.escapeChord,
    showPeerName: root.showPeerName
  })

  readonly property color tint: view.tone === "accent" ? Commons.Color.accent
    : (view.tone === "urgent" ? (bar ? bar.urgent : Commons.Color.urgent) : button.foreground)

  implicitWidth: button.implicitWidth
  implicitHeight: button.implicitHeight

  FerryButton {
    id: button
    anchors.fill: parent
    bar: root.bar
    glyph: Glyphs.state(root.view.state)
    labelText: root.view.label
    tint: root.tint
    dot: root.view.dot
    dimmed: root.view.dim
    // The shell's tooltip is centred under the button, so the open popover covers its text and
    // leaves an empty rounded outline beside it (D-4). No tooltip while the popover is open.
    tooltipText: root.opened ? "" : root.view.tooltip
    description: root.view.tooltip

    onPressed: function (b) {
      if (b === Qt.LeftButton) root.toggle();
    }
  }

  // The popover is a layer-shell window, and a file that names a window type cannot even be
  // compiled where there is no layer-shell (the offscreen dev harness, a nested test). It is made
  // here, by URL, so that failing costs this widget its popover and nothing else.
  property var popover: null

  function makePopover() {
    var component = Qt.createComponent(Qt.resolvedUrl("Popover.qml"));
    if (component.status !== Component.Ready) {
      console.info("ferry: no popover window here: " + component.errorString().split("\n")[0]);
      return;
    }
    // `bar` is a binding, not a value: the shell hands it to this widget after it is built, and a
    // popover made with null would place itself in the screen's corner for good.
    root.popover = component.createObject(root, {
      anchorItem: button,
      owner: root,
      bar: Qt.binding(function () { return root.bar; }),
      hostWidget: root,
      open: Qt.binding(function () { return root.opened; })
    });
  }

  Component.onCompleted: {
    FerryUi.attach(root);
    makePopover();
  }
  Component.onDestruction: {
    FerryUi.detach(root);
    if (root.popover) root.popover.destroy();
  }
}
