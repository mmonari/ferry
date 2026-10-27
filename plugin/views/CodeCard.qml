import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import ".."
import "../Strings.js" as Strings

// The pairing code, big, in two groups of three for reading aloud, with how long it is good for. This is
// the one thing in ferry that the user has to compare by eye, so it gets the room, the contrast and a
// name for a screen reader that spells the digits out. It can take the keyboard (so the flow can park
// the focus here, on something that does nothing when Enter is pressed) and shows a ring when it does.
BorderSurface {
  id: root

  property string codeText: ""
  property string spoken: ""
  // who it is shared with, under the digits: "lap · 198.51.100.51:4243"
  property string caption: ""
  property string clock: ""
  // 1 -> 0 over the two minutes the pairing lasts
  property real fraction: 1
  property bool expiring: false
  property color foreground: Commons.Color.popups.text
  property string fontFamily: Style.font.family

  readonly property color dim: Qt.darker(foreground, 1.4)

  width: parent ? parent.width : implicitWidth
  implicitHeight: body.implicitHeight + Style.space(36)
  color: Style.normalFill
  radius: Style.cornerRadius
  borderSpec: Border.flat(Util.alpha(Commons.Color.accent, 0.7), Math.max(1, Style.space(2)))
  activeFocusOnTab: true

  Accessible.role: Accessible.StaticText
  Accessible.name: Strings.t("pair.code.label") + " " + root.spoken
  Accessible.description: root.caption

  // it arrives, and does not just appear
  Component.onCompleted: appear.start()
  ParallelAnimation {
    id: appear
    NumberAnimation { target: root; property: "opacity"; from: 0.0; to: 1.0; duration: Fx.ms(220); easing.type: Easing.OutCubic }
    NumberAnimation { target: root; property: "scale"; from: 0.96; to: 1.0; duration: Fx.ms(260); easing.type: Easing.OutBack }
  }

  Column {
    id: body
    anchors.left: parent.left
    anchors.right: parent.right
    anchors.verticalCenter: parent.verticalCenter
    anchors.margins: Style.space(18)
    spacing: Style.space(8)

    Text {
      anchors.horizontalCenter: parent.horizontalCenter
      textFormat: Text.PlainText
      text: Strings.t("pair.code.label")
      color: root.dim
      font.family: root.fontFamily
      font.pixelSize: Style.font.caption
      font.bold: true
    }

    // two groups, with a gap that is wider than a letter space, so "418 207" does not read as one number
    Row {
      id: digits
      anchors.horizontalCenter: parent.horizontalCenter
      spacing: Style.font.displayLarge * 0.7

      Repeater {
        model: root.codeText === "" ? [] : root.codeText.split(" ")

        Text {
          required property string modelData
          textFormat: Text.PlainText
          text: modelData
          color: root.foreground
          font.family: root.fontFamily
          font.pixelSize: Style.font.displayLarge * 1.7
          font.bold: true
          font.letterSpacing: Style.font.displayLarge * 0.22
          font.features: { "tnum": 1 }
        }
      }
    }

    Text {
      anchors.horizontalCenter: parent.horizontalCenter
      width: Math.min(implicitWidth, parent.width)
      visible: root.caption !== ""
      textFormat: Text.PlainText
      text: root.caption
      color: root.dim
      font.family: root.fontFamily
      font.pixelSize: Style.font.bodySmall
      horizontalAlignment: Text.AlignHCenter
      elide: Text.ElideMiddle
    }

    // how long is left: a bar that runs down, and the same in words
    Item {
      width: parent.width
      height: Style.space(16)
      visible: root.clock !== ""

      Rectangle {
        id: track
        anchors.left: parent.left
        anchors.right: clockText.left
        anchors.rightMargin: Style.space(10)
        anchors.verticalCenter: parent.verticalCenter
        height: Style.space(4)
        radius: height / 2
        color: Util.alpha(root.foreground, 0.15)

        Rectangle {
          width: track.width * Math.max(0, Math.min(1, root.fraction))
          height: parent.height
          radius: parent.radius
          color: root.expiring || root.fraction < 0.15 ? Commons.Color.urgent : Commons.Color.accent

          Behavior on width { NumberAnimation { duration: Fx.ms(900); easing.type: Easing.Linear } }
        }
      }

      Text {
        id: clockText
        anchors.right: parent.right
        anchors.verticalCenter: parent.verticalCenter
        textFormat: Text.PlainText
        text: root.expiring ? Strings.t("pair.code.expiring") : Strings.t("pair.code.expires", [root.clock])
        color: root.expiring || root.fraction < 0.15 ? Commons.Color.urgent : root.dim
        font.family: root.fontFamily
        font.pixelSize: Style.font.caption
        font.features: { "tnum": 1 }
      }
    }
  }

  FocusRing {}
}
