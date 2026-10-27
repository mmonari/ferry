import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import ".."

// One key of a chord, drawn as a small cap. `held` is a key that is down right now while a chord is
// being recorded, `ghost` the place where the next key will land.
BorderSurface {
  id: root

  property string text: ""
  property bool held: false
  property bool ghost: false
  property color foreground: Commons.Color.popups.text
  property string fontFamily: Style.font.family

  implicitWidth: Math.max(Style.space(26), label.implicitWidth + Style.space(14))
  implicitHeight: label.implicitHeight + Style.space(8)
  width: implicitWidth
  height: implicitHeight
  radius: Math.max(Style.cornerRadius * 0.6, 2)
  color: root.held ? Style.selectedAccentFill : Style.normalFill
  borderSpec: root.held
    ? Border.flat(Util.alpha(Commons.Color.accent, 0.9), Math.max(1, Style.space(1)))
    : (root.ghost
      ? Border.flat(Util.alpha(root.foreground, 0.35), 1)
      : Border.controlSpec("normal", root.foreground, Commons.Color.accent))

  Behavior on color { ColorAnimation { duration: Fx.ms(100) } }

  Text {
    id: label
    anchors.centerIn: parent
    textFormat: Text.PlainText
    text: root.text
    color: root.ghost ? Qt.darker(root.foreground, 1.8) : root.foreground
    font.family: root.fontFamily
    font.pixelSize: Style.font.body
    font.bold: true
  }
}
