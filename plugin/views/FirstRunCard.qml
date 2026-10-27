import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import "../Glyphs.js" as Glyphs
import "../Strings.js" as Strings

// With no trusted peer, the popover has nothing to list: it shows how pairing goes and one button
// that opens the Pair panel.
Column {
  id: root

  property color foreground: Commons.Color.popups.text
  property string fontFamily: Style.font.family
  // the escape chord as keys (Model.chordKeys): the first-run card is one of the three places that teach it (U1)
  property var chord: []

  signal pairRequested()

  readonly property color dim: Qt.darker(foreground, 1.4)
  property alias button: cta

  width: parent ? parent.width : implicitWidth
  spacing: Style.space(10)

  Repeater {
    model: [Strings.t("first.step1"), Strings.t("first.step2"), Strings.t("first.step3")]

    Row {
      required property int index
      required property string modelData
      width: root.width
      spacing: Style.space(10)

      Rectangle {
        width: Style.space(18)
        height: width
        radius: width / 2
        color: "transparent"
        border.width: 1
        border.color: Util.alpha(root.foreground, 0.4)

        Text {
          anchors.centerIn: parent
          textFormat: Text.PlainText
          text: String(index + 1)
          color: root.foreground
          font.family: root.fontFamily
          font.pixelSize: Style.font.caption
          font.bold: true
        }
      }

      Text {
        width: parent.width - Style.space(28)
        anchors.verticalCenter: parent.verticalCenter
        textFormat: Text.PlainText
        text: modelData
        color: root.dim
        font.family: root.fontFamily
        font.pixelSize: Style.font.bodySmall
        wrapMode: Text.WordWrap
      }
    }
  }

  Column {
    visible: root.chord.length > 0
    width: parent.width
    spacing: Style.space(4)

    Row {
      spacing: Style.space(8)

      Text {
        anchors.verticalCenter: parent.verticalCenter
        textFormat: Text.PlainText
        text: Strings.t("escape.hint.lead")
        color: root.dim
        font.family: root.fontFamily
        font.pixelSize: Style.font.caption
      }

      Keycaps {
        anchors.verticalCenter: parent.verticalCenter
        keys: root.chord
        foreground: root.foreground
        fontFamily: root.fontFamily
      }
    }

    Text {
      width: parent.width
      textFormat: Text.PlainText
      text: Strings.t("escape.hint.tail")
      color: root.dim
      font.family: root.fontFamily
      font.pixelSize: Style.font.caption
      wrapMode: Text.WordWrap
      lineHeight: 1.2
    }
  }

  Button {
    id: cta
    width: parent.width
    text: Strings.t("first.cta")
    iconText: Glyphs.ui("plus")
    bordered: true
    active: true
    focusable: true
    foreground: root.foreground
    fontFamily: root.fontFamily
    onClicked: root.pairRequested()
    FocusRing {}
  }
}
