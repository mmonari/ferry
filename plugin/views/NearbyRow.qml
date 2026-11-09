import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import ".."
import "../Strings.js" as Strings

// One machine `discover.list` found, in the Pair tab's Nearby list: its name, where it is, and "Pair →" on the
// right. A flat row (panel-ui.md rule 1): no border, a faint fill only under the pointer or the keyboard. A
// machine that is already trusted is dim, says "Trusted" and cannot be chosen. Choosing one dials it.
Rectangle {
  id: root

  // { name, addr, tag, trusted }, one entry of Model.nearbyRows()
  property var entry: ({})
  property bool selected: false
  property color foreground: Commons.Color.popups.text
  property string fontFamily: Style.font.family

  readonly property color dim: Qt.darker(foreground, 1.4)
  readonly property bool choosable: entry.trusted !== true

  signal chosen()

  width: parent ? parent.width : implicitWidth
  implicitHeight: Math.max(Style.space(34), name.implicitHeight + Style.space(14))
  radius: Style.cornerRadius
  color: root.choosable && (mouse.containsMouse || root.activeFocus || root.selected) ? Commons.Color.menu.selectedBackground : "transparent"
  activeFocusOnTab: root.choosable

  Accessible.role: Accessible.Button
  Accessible.name: (entry.name || "") + ", " + (entry.addr || "") + ", " + (root.choosable ? Strings.t("pair.nearby.go") : (entry.tag || ""))

  Keys.onReturnPressed: if (root.choosable) root.chosen()
  Keys.onEnterPressed: if (root.choosable) root.chosen()
  Keys.onSpacePressed: if (root.choosable) root.chosen()

  Text {
    id: name
    anchors.left: parent.left
    anchors.leftMargin: Style.space(8)
    anchors.verticalCenter: parent.verticalCenter
    width: Math.min(implicitWidth, (root.width - tag.width) * 0.42)
    textFormat: Text.PlainText
    text: root.entry.name || ""
    color: root.choosable ? root.foreground : root.dim
    font.family: root.fontFamily
    font.pixelSize: Style.font.body
    font.weight: Font.DemiBold
    elide: Text.ElideRight
  }

  Text {
    anchors.left: name.right
    anchors.leftMargin: Style.space(14)
    anchors.right: tag.left
    anchors.rightMargin: Style.space(10)
    anchors.verticalCenter: parent.verticalCenter
    textFormat: Text.PlainText
    text: root.entry.addr || ""
    color: root.dim
    font.family: root.fontFamily
    font.pixelSize: Style.font.bodySmall
    font.features: { "tnum": 1 }
    elide: Text.ElideRight
  }

  Text {
    id: tag
    anchors.right: parent.right
    anchors.rightMargin: Style.space(8)
    anchors.verticalCenter: parent.verticalCenter
    textFormat: Text.PlainText
    text: root.choosable ? Strings.t("pair.nearby.go") : (root.entry.tag || Strings.t("pair.nearby.trusted"))
    color: root.choosable ? Commons.Color.accent : root.dim
    font.family: root.fontFamily
    font.pixelSize: Style.font.bodySmall
    font.weight: root.choosable ? Font.DemiBold : Font.Normal
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
