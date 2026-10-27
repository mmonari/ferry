import QtQuick
import Quickshell
import Quickshell.Wayland
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import "."
// The shell's qmlscanner registers a subfolder only when a root file imports it. A folder seen only
// from a file that is loaded by URL (the panels) gets no types at all in the real shell: "SettingItem
// is not a type", and a blank Settings panel. The offscreen harness loads by file://
// and never sees this. plugin.test.mjs checks that every folder is imported here or in another root file.
import "settings"
import "views"

// The setup panel's window (-08, -14, -18): a layer-shell surface over the focused
// monitor, a scrim that closes the panel when clicked, and the card with PanelCard in its middle.
// FerryUi creates it when the panel is asked for and drops it when it is closed, so a closed
// panel costs nothing.
//
// It is a layer-shell surface and not a FloatingWindow because the placement is ours: Hyprland
// decides where a toplevel goes (it may tile it), and FR-UI-18 wants the panel centred on the
// monitor the user is working on.
PanelWindow {
  id: win

  // The card's size: wide enough for the Arrange canvas, never more than the screen allows.
  readonly property real cardWidth: Math.min(Style.space(860), (screen ? screen.width : 1280) - Style.space(48))
  readonly property real cardHeight: Math.min(Style.space(620), (screen ? screen.height : 800) - Style.space(96))
  // Keyboard focus is taken exclusively for a moment, which is what makes a freshly mapped surface
  // receive keys even when the previous window had the pointer locked, and then released to
  // on-demand so a click on another output still reaches the compositor (see Ui/KeyboardPanel.qml).
  property bool primed: false

  screen: FerryUi.panelScreen
  visible: true
  color: "transparent"
  exclusionMode: ExclusionMode.Ignore

  WlrLayershell.namespace: "ferry-panel"
  WlrLayershell.layer: WlrLayer.Overlay
  WlrLayershell.keyboardFocus: win.primed ? WlrKeyboardFocus.OnDemand : WlrKeyboardFocus.Exclusive

  anchors {
    top: true
    bottom: true
    left: true
    right: true
  }

  Timer {
    interval: 75
    running: true
    onTriggered: win.primed = true
  }

  Rectangle {
    anchors.fill: parent
    color: Commons.Color.menu.scrim
    opacity: 0

    Component.onCompleted: opacity = 1
    Behavior on opacity { NumberAnimation { duration: Fx.ms(140); easing.type: Easing.OutCubic } }

    MouseArea {
      anchors.fill: parent
      acceptedButtons: Qt.AllButtons
      onClicked: FerryUi.closePanel()
    }
  }

  BorderSurface {
    id: card
    anchors.centerIn: parent
    width: win.cardWidth
    height: win.cardHeight
    color: Commons.Color.popups.background
    borderSpec: Border.localOrSurfaceSpec("popups", "border", Commons.Color.popups.border, Commons.Color.popups.border, Math.max(1, Style.space(2)))
    padding: Style.spacing.panelPadding
    radius: Style.cornerRadius
    opacity: 0
    scale: 0.985

    Component.onCompleted: { opacity = 1; scale = 1; }
    Behavior on opacity { NumberAnimation { duration: Fx.ms(140); easing.type: Easing.OutCubic } }
    Behavior on scale { NumberAnimation { duration: Fx.ms(140); easing.type: Easing.OutCubic } }

    // Swallow clicks on the card, so they do not reach the scrim behind it.
    MouseArea {
      anchors.fill: parent
      acceptedButtons: Qt.AllButtons
    }

    PanelCard {
      id: content
      anchors.fill: parent
      anchors.topMargin: card.contentTopInset
      anchors.rightMargin: card.contentRightInset
      anchors.bottomMargin: card.contentBottomInset
      anchors.leftMargin: card.contentLeftInset
      LayoutMirroring.enabled: Commons.I18n.rightToLeft
      LayoutMirroring.childrenInherit: true

      onCloseRequested: FerryUi.closePanel()
      onViewRequested: function (v) { FerryUi.panelView = v; }
    }
  }

  Component.onCompleted: Qt.callLater(function () { content.forceActiveFocus(); })
}
