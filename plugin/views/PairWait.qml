import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import ".."
import "../Strings.js" as Strings

// A pairing that is in progress and has nothing for the user to do yet: the window is open and waiting
// (acceptor), the machines are setting up a secure connection, or this side has confirmed and the
// other has not. A turning mark says it is alive. While the window is open it also says how to reach
// this machine, since the other person has to type it.
Column {
  id: root

  // "listening" | "connecting" | "waiting"
  property string kind: "connecting"
  property string title: ""
  property string body: ""
  // listening: where this machine answers, and the command that pairs with it
  property string address: ""
  property string command: ""
  property string clock: ""
  property color foreground: Commons.Color.popups.text
  property string fontFamily: Style.font.family

  readonly property color dim: Qt.darker(foreground, 1.4)

  width: parent ? parent.width : implicitWidth
  spacing: Style.space(12)

  Spinner {
    anchors.horizontalCenter: parent.horizontalCenter
    size: Style.space(30)
    color: Commons.Color.accent
  }

  Text {
    width: parent.width
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

  // the way in, for the other person to type
  Column {
    visible: root.kind === "listening" && root.address !== ""
    width: parent.width
    spacing: Style.space(8)

    BorderSurface {
      anchors.horizontalCenter: parent.horizontalCenter
      width: Math.min(parent.width, addr.implicitWidth + Style.space(44))
      height: addr.implicitHeight + Style.space(18)
      color: Style.normalFill
      radius: Style.cornerRadius
      borderSpec: Border.flat(Util.alpha(Commons.Color.accent, 0.6), 1)

      Text {
        id: addr
        anchors.centerIn: parent
        textFormat: Text.PlainText
        text: root.address
        color: root.foreground
        font.family: root.fontFamily
        font.pixelSize: Style.font.heading
        font.bold: true
        font.features: { "tnum": 1 }
      }
    }

    CommandLine {
      visible: root.command !== ""
      width: parent.width
      command: root.command
      foreground: root.foreground
      fontFamily: root.fontFamily
    }
  }

  Text {
    width: parent.width
    visible: root.kind === "listening" && root.clock !== ""
    textFormat: Text.PlainText
    text: Strings.t("pair.listening.expires", [root.clock])
    color: root.dim
    font.family: root.fontFamily
    font.pixelSize: Style.font.caption
    font.features: { "tnum": 1 }
    horizontalAlignment: Text.AlignHCenter
  }
}
