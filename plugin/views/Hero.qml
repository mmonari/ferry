import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import ".."

// The top block of a popover or a panel: a glyph, a one-line sentence, a line under it and a pill on
// the right. It mirrors PanelHero (Ui/PanelHero.qml), with two differences that PanelHero cannot give:
// the glyph and the pill can take the accent or the urgent colour, which is how the mockup says "input
// is travelling" and "something needs you", and the line under the sentence can be a key combination
// drawn as keycaps, which is how the popover says what ends it.
//
// A change of state is felt and not seen jump: the glyph settles in with a small overshoot and the
// words fade in. Both take their time from Fx.ms, so they are instant when the shell has animations off.
Item {
  id: root

  property string glyph: ""
  property string title: ""
  property string meta: ""
  property string detail: ""
  // "normal" | "accent" | "urgent", for the glyph and the pill.
  property string tone: "normal"
  property string detailTone: "normal"
  // Keys to show as keycaps in place of `meta`, with the words that follow them ("brings it back").
  property var chord: []
  property string chordTail: ""
  property color foreground: Commons.Color.popups.text
  property string fontFamily: Style.font.family

  readonly property color dim: Qt.darker(foreground, 1.4)
  readonly property bool showChord: chord.length > 0
  property bool settled: false

  function toneColor(t) {
    return t === "accent" ? Commons.Color.accent : (t === "urgent" ? Commons.Color.urgent : foreground);
  }

  width: parent ? parent.width : implicitWidth
  implicitHeight: Math.max(glyphBox.height, labels.implicitHeight)

  Accessible.role: Accessible.StaticText
  Accessible.name: root.title + ". " + (root.showChord ? root.chord.join(" plus ") + " " + root.chordTail : root.meta)
    + (root.detail !== "" ? ". " + root.detail : "")

  Component.onCompleted: root.settled = true
  onGlyphChanged: if (root.settled) pop.restart()
  onTitleChanged: if (root.settled) fade.restart()
  onMetaChanged: if (root.settled) fade.restart()

  NumberAnimation {
    id: pop
    target: glyphBox
    property: "scale"
    from: 0.72
    to: 1.0
    duration: Fx.ms(240)
    easing.type: Easing.OutBack
    easing.overshoot: 2.2
  }

  NumberAnimation {
    id: fade
    target: labels
    property: "opacity"
    from: 0.0
    to: 1.0
    duration: Fx.ms(200)
    easing.type: Easing.OutCubic
  }

  Item {
    id: glyphBox
    width: Style.space(28)
    height: Style.space(28)
    anchors.left: parent.left
    anchors.verticalCenter: parent.verticalCenter

    Glyph {
      anchors.fill: parent
      text: root.glyph
      fontFamily: root.fontFamily
      fontSize: Style.font.display
      paintedSize: Style.font.display * 0.92
      color: root.toneColor(root.tone)

      Behavior on color { ColorAnimation { duration: Fx.ms(160) } }
    }
  }

  Column {
    id: labels
    anchors.left: glyphBox.right
    anchors.leftMargin: Style.space(14)
    anchors.right: parent.right
    anchors.verticalCenter: parent.verticalCenter
    spacing: Style.space(3)

    Item {
      width: parent.width
      height: Math.max(titleText.implicitHeight, pill.visible ? pill.height : 0)

      // One sentence, and two lines at most for a long name: it wraps instead of cutting the peer's
      // name off, which is the one word of it that matters.
      Text {
        id: titleText
        anchors.left: parent.left
        anchors.verticalCenter: parent.verticalCenter
        width: Math.max(0, parent.width - (pill.visible ? pill.width + Style.space(8) : 0))
        textFormat: Text.PlainText
        text: root.title
        color: root.foreground
        font.family: root.fontFamily
        font.pixelSize: Style.font.title
        font.bold: true
        wrapMode: Text.WordWrap
        maximumLineCount: 2
        elide: Text.ElideRight
      }

      BorderSurface {
        id: pill
        visible: root.detail !== ""
        // one pixel in from the edge: a border that sits on the clip of the scroller around the
        // popover loses its right side
        anchors.right: parent.right
        anchors.rightMargin: 1
        anchors.verticalCenter: parent.verticalCenter
        width: pillText.implicitWidth + Style.space(10)
        height: pillText.implicitHeight + Style.space(4)
        color: "transparent"
        radius: Style.cornerRadius
        borderSpec: root.detailTone === "normal"
          ? Border.controlSpec("normal", root.foreground, Commons.Color.accent)
          : Border.flat(Util.alpha(root.toneColor(root.detailTone), 0.55), 1)

        Text {
          id: pillText
          anchors.centerIn: parent
          textFormat: Text.PlainText
          text: root.detail
          color: root.detailTone === "normal" ? root.dim : root.toneColor(root.detailTone)
          font.family: root.fontFamily
          font.pixelSize: Style.font.body
          font.bold: true
          font.features: { "tnum": 1 }
        }
      }
    }

    // the line under the sentence: words in the dim colour, or the key that brings input back
    Text {
      width: parent.width
      visible: !root.showChord && text !== ""
      textFormat: Text.PlainText
      text: root.meta
      color: root.dim
      font.family: root.fontFamily
      font.pixelSize: Style.font.bodySmall
      elide: Text.ElideRight
    }

    Row {
      visible: root.showChord
      spacing: Style.space(8)

      Keycaps {
        anchors.verticalCenter: parent.verticalCenter
        keys: root.chord
        foreground: root.foreground
        fontFamily: root.fontFamily
      }

      Text {
        anchors.verticalCenter: parent.verticalCenter
        textFormat: Text.PlainText
        text: root.chordTail
        color: root.dim
        font.family: root.fontFamily
        font.pixelSize: Style.font.caption
      }
    }
  }
}
