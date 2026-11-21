import QtQuick
import Quickshell
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import "."
// The panel's second surface: an ordinary toplevel window instead of the centred popout (PanelHost.qml).
// Hyprland tiles it with the rest of the workspace and sizes it the way it sizes any window, so the
// windows behind it stay usable. Shown while the panel is open and FerryUi.panelTiled is set; the popout
// shows the panel in the other case, so the two never show together.
//
// A toplevel is not a layer surface, so none of the popout's behaviour comes with it: it does not close on
// an outside click (a window closes with its own close button, Escape, or SUPER+Q), and it takes focus
// the way any window does. Its title is the name windows.lua tiles it by (`title = "^Ferry$"`, class
// `org.quickshell`); change one and change the other.
//
// It is made by URL from FerryWidget.qml, as the popout is, so a shell without window support costs the
// widget this panel and nothing else.
FloatingWindow {
  id: win

  property var hostWidget: null

  // The window is shown while its widget's panel is open and tiled.
  readonly property bool wanted: hostWidget !== null && hostWidget.opened && FerryUi.panelTiled

  visible: wanted
  title: "Ferry"
  color: Commons.Color.popups.background
  implicitWidth: Style.space(920)
  implicitHeight: Style.space(600)
  minimumSize: Qt.size(Style.space(560), Style.space(400))

  // Closed by the window itself (its close button, SUPER+Q, Hyprland): that closes the panel. Turning the
  // tiling off hides the window too, and that is not a close, so the check is on the mode.
  onVisibleChanged: {
    if (!visible && FerryUi.panelTiled && hostWidget && hostWidget.opened) hostWidget.close();
  }

  Loader {
    id: body
    anchors.fill: parent
    anchors.margins: Style.spacing.popupPadding
    // Built on the first show and kept, as the popout keeps its card: the scroll position survives a toggle.
    active: win.visible || item !== null

    sourceComponent: PanelCard {
      shown: win.visible
      onCloseRequested: { if (win.hostWidget) win.hostWidget.close(); }
      onViewRequested: function (v) { FerryUi.panelView = FerryUi.viewOf(v) || "screens"; }
    }
  }
}
