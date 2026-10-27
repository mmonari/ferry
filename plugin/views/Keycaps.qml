import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui

// A key combination as keycaps: [Super] + [Ctrl] + [Esc]. They are text in the shell's font, so they
// follow the theme and the font size, and a screen reader gets the combination as one phrase.
Row {
  id: root

  // ["Super", "Ctrl", "Esc"], from Model.chordKeys()
  property var keys: []
  property color foreground: Commons.Color.popups.text
  property string fontFamily: Style.font.family
  property real fontSize: Style.font.caption

  spacing: Style.space(4)

  Accessible.role: Accessible.StaticText
  Accessible.name: root.keys.join(" plus ")

  Repeater {
    model: root.keys

    Row {
      required property int index
      required property string modelData
      spacing: root.spacing
      anchors.verticalCenter: parent ? parent.verticalCenter : undefined

      Text {
        visible: index > 0
        anchors.verticalCenter: parent.verticalCenter
        textFormat: Text.PlainText
        text: "+"
        color: Qt.darker(root.foreground, 1.6)
        font.family: root.fontFamily
        font.pixelSize: root.fontSize
      }

      BorderSurface {
        anchors.verticalCenter: parent.verticalCenter
        width: cap.implicitWidth + Style.space(10)
        height: cap.implicitHeight + Style.space(4)
        color: Util.alpha(root.foreground, 0.07)
        radius: Math.min(Style.cornerRadius, Style.space(4))
        borderSpec: Border.flat(Util.alpha(root.foreground, 0.4), 1)

        Text {
          id: cap
          anchors.centerIn: parent
          textFormat: Text.PlainText
          text: modelData
          color: root.foreground
          font.family: root.fontFamily
          font.pixelSize: root.fontSize
          font.bold: true
        }
      }
    }
  }
}
