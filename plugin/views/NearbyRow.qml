import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import ".."

// One machine `discover.list` found, in the Pair panel's Nearby list: its name, where it is and whether it can
// be chosen. A machine that is already trusted is shown and cannot be chosen. Choosing one puts its address in
// the field; it does not start anything, so the user still presses Pair.
BorderSurface {
  id: root

  // { name, addr, tag, trusted }, one entry of Model.nearbyRows()
  property var entry: ({})
  property bool selected: false
  property color foreground: Commons.Color.popups.text
  property string fontFamily: Style.font.family

  readonly property color dim: Qt.darker(foreground, 1.5)
  readonly property bool choosable: entry.trusted !== true

  signal chosen()

  width: parent ? parent.width : implicitWidth
  implicitHeight: Math.max(Style.space(30), line.implicitHeight + Style.space(12))
  radius: Style.cornerRadius
  color: root.selected ? Style.selectedFillFor(root.foreground, Commons.Color.accent)
    : (mouse.containsMouse && root.choosable ? Style.hoverFillFor(root.foreground, Commons.Color.accent) : "transparent")
  borderSpec: Border.controlSpec(root.selected ? "selected" : "normal", root.foreground, Commons.Color.accent)
  opacity: root.choosable ? 1.0 : 0.55
  activeFocusOnTab: root.choosable

  Accessible.role: Accessible.RadioButton
  Accessible.name: (entry.name || "") + ", " + (entry.addr || "") + ", " + (entry.tag || "")
  Accessible.checked: root.selected

  Keys.onReturnPressed: if (root.choosable) root.chosen()
  Keys.onEnterPressed: if (root.choosable) root.chosen()
  Keys.onSpacePressed: if (root.choosable) root.chosen()

  Row {
    id: line
    anchors.left: parent.left
    anchors.leftMargin: Style.space(10)
    anchors.right: tag.left
    anchors.rightMargin: Style.space(10)
    anchors.verticalCenter: parent.verticalCenter
    spacing: Style.space(8)

    Text {
      id: name
      textFormat: Text.PlainText
      text: root.entry.name || ""
      color: root.foreground
      font.family: root.fontFamily
      font.pixelSize: Style.font.bodySmall
      font.bold: true
      elide: Text.ElideRight
      width: Math.min(implicitWidth, line.width * 0.55)
    }

    Text {
      textFormat: Text.PlainText
      text: root.entry.addr || ""
      color: root.dim
      font.family: root.fontFamily
      font.pixelSize: Style.font.bodySmall
      elide: Text.ElideRight
      width: Math.max(0, line.width - name.width - line.spacing)
    }
  }

  Text {
    id: tag
    anchors.right: parent.right
    anchors.rightMargin: Style.space(10)
    anchors.verticalCenter: parent.verticalCenter
    textFormat: Text.PlainText
    text: root.entry.tag || ""
    color: root.choosable ? Commons.Color.accent : root.dim
    font.family: root.fontFamily
    font.pixelSize: Style.font.caption
  }

  MouseArea {
    id: mouse
    anchors.fill: parent
    hoverEnabled: true
    enabled: root.choosable
    cursorShape: root.choosable ? Qt.PointingHandCursor : Qt.ArrowCursor
    onClicked: { root.forceActiveFocus(); root.chosen(); }
  }

  FocusRing {}
}
