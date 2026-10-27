import QtQuick
import QtQuick.Controls
import qs.Commons
import qs.Commons as Commons
import qs.Ui

// The vertical bar of a panel that scrolls: always there while there is more below, so a list the window
// cuts off says so (the stock ScrollBar shows itself only while it moves, and a still picture of a cut-off
// row looks like a bug). Thin, in the dim text colour, like the strokes of the controls next to it.
// Use it as `ScrollBar.vertical: PanelScrollBar { flickable: theFlickable }`.
ScrollBar {
  id: root

  property Flickable flickable: null

  policy: flickable && flickable.interactive ? ScrollBar.AlwaysOn : ScrollBar.AlwaysOff
  implicitWidth: Style.space(6)

  contentItem: Rectangle {
    implicitWidth: Style.space(4)
    radius: width / 2
    color: Util.alpha(Commons.Color.popups.text, root.pressed ? 0.6 : 0.32)
  }
}
