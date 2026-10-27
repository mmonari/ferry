import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui

// The word that says when a saved value takes effect: Live, Restart or Setup. The word carries the
// meaning (FR-UI-11: never colour alone); the accent border only marks the two that need more than
// pressing the control.
BorderSurface {
  id: root

  property string text: ""
  property bool emphasis: false
  property color foreground: Commons.Color.popups.text
  property string fontFamily: Style.font.family

  visible: text !== ""
  implicitWidth: label.implicitWidth + Style.space(10)
  implicitHeight: label.implicitHeight + Style.space(3)
  width: implicitWidth
  height: implicitHeight
  color: "transparent"
  radius: Style.cornerRadius
  borderSpec: root.emphasis
    ? Border.flat(Util.alpha(Commons.Color.accent, 0.7), 1)
    : Border.controlSpec("normal", root.foreground, Commons.Color.accent)

  Text {
    id: label
    anchors.centerIn: parent
    textFormat: Text.PlainText
    text: root.text
    color: root.emphasis ? Commons.Color.accent : Qt.darker(root.foreground, 1.5)
    font.family: root.fontFamily
    font.pixelSize: Style.font.caption
    font.bold: true
  }
}
