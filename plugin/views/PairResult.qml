import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import ".."
import "../Glyphs.js" as Glyphs
import "../Strings.js" as Strings

// How a pairing ended. A success names both machines, shows the key to compare with the other
// machine's, and says what is left to do (Arrange). A failure says in words what happened, that
// nothing was saved, and what to try. The glyph and the short verdict are the hero's, above this
// (#34): they are not said twice.
Column {
  id: root

  // "paired" | "failed"
  property string kind: "paired"
  property string title: ""
  property string body: ""
  property string hint: ""
  // what ferryd added, when it said more (a pin under another name)
  property string note: ""
  // success: the other machine's fingerprint, grouped, and what it was called
  property string fingerprint: ""
  property string fingerprintLabel: ""
  property color foreground: Commons.Color.popups.text
  property string fontFamily: Style.font.family

  readonly property color dim: Qt.darker(foreground, 1.4)
  readonly property bool good: kind === "paired"

  width: parent ? parent.width : implicitWidth
  spacing: Style.space(10)

  Accessible.role: Accessible.AlertMessage
  Accessible.name: root.title

  // A failure's title is the hero's line already (Model.pairFlow), so only a success shows one here.
  Text {
    width: parent.width
    visible: root.good && text !== ""
    textFormat: Text.PlainText
    text: root.title
    color: root.foreground
    font.family: root.fontFamily
    font.pixelSize: Style.font.heading
    font.bold: true
    horizontalAlignment: Text.AlignHCenter
    wrapMode: Text.WordWrap
  }

  Text {
    width: parent.width
    visible: text !== ""
    textFormat: Text.PlainText
    text: root.body
    color: root.dim
    font.family: root.fontFamily
    font.pixelSize: Style.font.bodySmall
    horizontalAlignment: Text.AlignHCenter
    wrapMode: Text.WordWrap
    lineHeight: 1.2
  }

  Text {
    width: parent.width
    visible: text !== ""
    textFormat: Text.PlainText
    text: root.hint
    color: root.dim
    font.family: root.fontFamily
    font.pixelSize: Style.font.bodySmall
    horizontalAlignment: Text.AlignHCenter
    wrapMode: Text.WordWrap
    lineHeight: 1.2
  }

  // success: the fingerprint, to compare if you want to be sure
  Column {
    visible: root.good && root.fingerprint !== ""
    anchors.horizontalCenter: parent.horizontalCenter
    spacing: Style.space(2)

    Text {
      anchors.horizontalCenter: parent.horizontalCenter
      textFormat: Text.PlainText
      text: root.fingerprintLabel
      color: root.dim
      font.family: root.fontFamily
      font.pixelSize: Style.font.caption
    }

    Text {
      anchors.horizontalCenter: parent.horizontalCenter
      textFormat: Text.PlainText
      text: root.fingerprint
      color: root.foreground
      font.family: root.fontFamily
      font.pixelSize: Style.font.bodySmall
      font.features: { "tnum": 1 }
    }
  }

  Text {
    width: parent.width
    visible: text !== ""
    textFormat: Text.PlainText
    text: root.note
    color: root.dim
    font.family: root.fontFamily
    font.pixelSize: Style.font.caption
    font.italic: true
    horizontalAlignment: Text.AlignHCenter
    wrapMode: Text.WordWrap
  }
}
