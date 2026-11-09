import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import ".."

// A setting row with a switch, flat (rule 2): the label on the left, the switch on
// the right, and at most one dim line under them. No border and no fill: rows are split by hairlines.
// The whole row is the target, for the pointer and for the keyboard (Space, Enter).
Item {
  id: root

  property string label: ""
  property string description: ""
  property bool checked: false
  property color foreground: Commons.Color.popups.text
  property string fontFamily: Style.font.family
  signal clicked()

  readonly property color dim: Qt.darker(foreground, 1.4)

  implicitHeight: Math.max(labelText.implicitHeight, toggle.height) + (descText.visible ? descText.implicitHeight + Style.space(2) : 0) + Style.space(12)
  activeFocusOnTab: enabled
  opacity: enabled ? 1 : 0.6
  Accessible.role: Accessible.CheckBox
  Accessible.name: label
  Accessible.checked: checked
  Keys.onSpacePressed: if (enabled) root.clicked()
  Keys.onReturnPressed: if (enabled) root.clicked()

  Text {
    id: labelText
    anchors.left: parent.left
    anchors.right: toggle.left
    anchors.rightMargin: Style.space(10)
    anchors.verticalCenter: toggle.verticalCenter
    textFormat: Text.PlainText
    text: root.label
    color: root.foreground
    font.family: root.fontFamily
    font.pixelSize: Style.font.body
    elide: Text.ElideRight
  }

  ToggleSwitch {
    id: toggle
    anchors.right: parent.right
    anchors.top: parent.top
    anchors.topMargin: Style.space(6)
    checked: root.checked
    interactive: false
    cursorRing: false
    foreground: root.foreground
  }

  Text {
    id: descText
    visible: root.description !== ""
    anchors.left: parent.left
    anchors.right: toggle.left
    anchors.rightMargin: Style.space(10)
    anchors.top: toggle.bottom
    anchors.topMargin: Style.space(2)
    textFormat: Text.PlainText
    text: root.description
    color: root.dim
    font.family: root.fontFamily
    font.pixelSize: Style.font.caption
    wrapMode: Text.WordWrap
  }

  MouseArea {
    anchors.fill: parent
    enabled: root.enabled
    cursorShape: Qt.PointingHandCursor
    onClicked: root.clicked()
  }

  FocusRing {}
}
