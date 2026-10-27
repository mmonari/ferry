import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui

// The bar button: one glyph, an optional peer name after it, and a dot badge when something needs
// the user. It sits on WidgetButton, as every shell bar button does, so the shell's tooltip, the
// click forwarding that lets a click on another bar icon switch popouts, and the bar's geometry
// all keep working. BarIconButton cannot be used: it draws its label before the icon, and
// "→ lap" has to read in the order of the arrow.
//
// No logic here. `glyph`, `labelText`, `tint`, `dot` come from Model.widget() through BarWidget.
WidgetButton {
  id: root

  property string glyph: ""
  property string labelText: ""
  // The colour of glyph and label: the bar foreground, the accent while input travels, the
  // bar's urgent colour for a problem.
  property color tint: foreground
  property bool dot: false
  property color dotColor: bar ? bar.urgent : Commons.Color.urgent
  property color ringColor: Commons.Color.bar.background

  // Shell tokens that older shells do not have: fall back to the sizes the bar has always used.
  readonly property real iconPaint: Style.bar.iconSize || 12
  readonly property real iconBox: Style.bar.iconCanvas || 16
  readonly property real iconFontPx: Style.bar.iconFont || 13
  readonly property bool showLabel: labelText !== "" && !vertical

  // Screen readers: Quickshell's accessibility support is unverified, but the names cost nothing.
  Accessible.role: Accessible.Button
  // What the button says to a screen reader: the tooltip's text, which stays when the tooltip is off.
  property string description: tooltipText
  Accessible.name: description.split("\n")[0]
  Accessible.description: description

  labelVisible: false
  hasVisualContent: true
  keepSpace: true
  fontSize: iconFontPx
  // The same slot a BarIconButton has, wider by the label when there is one.
  fixedWidth: vertical ? -1 : Math.ceil(content.implicitWidth + Style.spaceReal(11))
  fixedHeight: vertical ? Style.bar.iconSlot || 27 : -1

  Row {
    id: content
    anchors.centerIn: parent
    spacing: Style.space(5)

    Item {
      id: glyphBox
      width: root.iconBox
      height: root.iconBox
      anchors.verticalCenter: parent.verticalCenter

      Glyph {
        id: glyphItem
        anchors.fill: parent
        text: root.glyph
        fontFamily: root.fontFamily
        fontSize: root.iconFontPx
        paintedSize: root.iconPaint
        color: root.tint

        Behavior on color { ColorAnimation { duration: Fx.ms(160) } }
      }

      // The badge hangs off the glyph's top right corner. A hairline in the bar's own colour
      // separates it from the glyph at any contrast. It breathes, which is a third signal beside
      // its colour and its position, and it holds still when the shell has animations off.
      Rectangle {
        id: badge
        visible: root.dot
        width: Math.max(6, Math.round(root.iconPaint * 0.55))
        height: width
        radius: width / 2
        color: root.dotColor
        border.width: 1
        border.color: root.ringColor
        x: glyphBox.width / 2 + root.iconPaint / 2 - width * 0.55
        y: glyphBox.height / 2 - root.iconPaint / 2 - height * 0.45

        SequentialAnimation on opacity {
          running: badge.visible && !Fx.still
          loops: Animation.Infinite
          NumberAnimation { from: 1.0; to: 0.6; duration: 900; easing.type: Easing.InOutQuad }
          NumberAnimation { from: 0.6; to: 1.0; duration: 900; easing.type: Easing.InOutQuad }
        }
        onVisibleChanged: if (!visible) opacity = 1.0
      }
    }

    Text {
      id: label
      visible: root.showLabel
      anchors.verticalCenter: parent.verticalCenter
      // Centre the figures, not the line box, so the name lines up with the glyph beside it
      // (WidgetButton does the same for its own label).
      anchors.verticalCenterOffset: (labelMetrics.descent - labelMetrics.ascent + labelMetrics.tightBoundingRect("0").height) / 2
      textFormat: Text.PlainText
      text: root.labelText
      color: root.tint
      font.family: root.fontFamily
      font.pixelSize: root.iconFontPx
      font.bold: true
      renderType: Text.NativeRendering
      elide: Text.ElideRight
      // A very long machine name must not take the bar.
      width: Math.min(implicitWidth, Style.space(140))

      Behavior on color { ColorAnimation { duration: Fx.ms(160) } }
    }
  }

  FontMetrics {
    id: labelMetrics
    font: label.font
  }
}
