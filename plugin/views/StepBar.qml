import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import ".."
import "../Glyphs.js" as Glyphs

// The three steps of a pairing: pick, compare, done (the mockup's strip). Each is a numbered mark and
// a name, and what state it is in is shown by more than colour: the current one is filled, a finished
// one carries a tick, one that stopped carries a cross.
Row {
  id: root

  // Model.pairFlow().steps: [{ n, label, state: "todo" | "current" | "done" | "failed" }], always three
  property var steps: []
  property color foreground: Commons.Color.popups.text
  property string fontFamily: Style.font.family

  readonly property color dim: Qt.darker(foreground, 1.6)

  spacing: Style.space(8)

  Accessible.role: Accessible.List
  Accessible.name: root.steps.map(function (s) { return s.n + " " + s.label + (s.state === "current" ? ", now" : (s.state === "done" ? ", done" : (s.state === "failed" ? ", stopped" : ""))); }).join(". ")

  Repeater {
    model: 3

    Row {
      id: step

      required property int index
      readonly property var info: root.steps[index] || ({ n: index + 1, label: "", state: "todo" })
      readonly property bool current: info.state === "current"
      readonly property bool done: info.state === "done"
      readonly property bool failed: info.state === "failed"

      spacing: Style.space(8)

      // the line that joins it to the step before
      Rectangle {
        visible: step.index > 0
        anchors.verticalCenter: parent.verticalCenter
        width: Style.space(22)
        height: 1
        color: step.done || step.current || step.failed ? Util.alpha(root.foreground, 0.5) : Util.alpha(root.foreground, 0.2)
      }

      Rectangle {
        id: mark
        anchors.verticalCenter: parent.verticalCenter
        width: Style.space(20)
        height: width
        radius: width / 2
        color: step.current ? root.foreground : (step.done ? Util.alpha(Commons.Color.accent, 0.3) : "transparent")
        border.width: 1
        border.color: step.failed ? Commons.Color.urgent : (step.current ? root.foreground : (step.done ? Commons.Color.accent : Util.alpha(root.foreground, 0.35)))

        Behavior on color { ColorAnimation { duration: Fx.ms(160) } }

        Glyph {
          visible: step.done || step.failed
          anchors.centerIn: parent
          width: Style.space(12)
          height: width
          text: Glyphs.ui(step.failed ? "close" : "check")
          fontFamily: root.fontFamily
          fontSize: Style.font.caption
          paintedSize: Style.space(10)
          color: step.failed ? Commons.Color.urgent : Commons.Color.accent
        }

        Text {
          visible: !step.done && !step.failed
          anchors.centerIn: parent
          textFormat: Text.PlainText
          text: String(step.info.n)
          color: step.current ? Commons.Color.popups.background : root.dim
          font.family: root.fontFamily
          font.pixelSize: Style.font.caption
          font.bold: true
        }
      }

      Text {
        anchors.verticalCenter: parent.verticalCenter
        textFormat: Text.PlainText
        text: step.info.label
        color: step.current || step.failed ? root.foreground : root.dim
        font.family: root.fontFamily
        font.pixelSize: Style.font.bodySmall
        font.bold: step.current
      }
    }
  }
}
