import QtQuick
import QtQuick.Shapes
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import ".."

// One place where two machines' screen edges touch, drawn on the Arrange canvas where the edges meet:
//   link     the pointer crosses here: a bar in the accent colour with a glow, like the mockup's
//   dormant  the same place, but the peer is offline, so it is a wall until it is back: dashed
//   blocked  the peers touch, but one is not paired here, so crossings are off: dotted, in the dim colour
// The shape differs as well as the colour: a solid glowing bar, a dashed line, a dotted one.
Item {
  id: seam

  required property var canvas
  // { vertical, line, span: [lo, hi], kind } from Geometry.touches
  required property var touch

  readonly property bool vertical: touch.vertical
  readonly property real core: Math.max(3, Style.space(3))
  readonly property real halo: core * 3.2
  readonly property real along: vertical ? canvas.cy(touch.span[0]) : canvas.cx(touch.span[0])
  readonly property real length: (touch.span[1] - touch.span[0]) * canvas.view.scale
  readonly property real across: vertical ? canvas.cx(touch.line) : canvas.cy(touch.line)
  readonly property bool solid: touch.kind === "link"
  readonly property color tone: touch.kind === "blocked" ? Util.alpha(Commons.Color.popups.text, 0.55) : Commons.Color.accent

  x: vertical ? across - halo / 2 : along
  y: vertical ? along : across - halo / 2
  width: vertical ? halo : length
  height: vertical ? length : halo

  Rectangle {
    visible: seam.solid
    anchors.fill: parent
    radius: Math.min(width, height) / 2
    color: Util.alpha(Commons.Color.accent, 0.22)
  }

  Rectangle {
    visible: seam.solid
    anchors.centerIn: parent
    width: seam.vertical ? seam.core : parent.width
    height: seam.vertical ? parent.height : seam.core
    radius: seam.core / 2
    color: Commons.Color.accent
  }

  Shape {
    visible: !seam.solid
    anchors.fill: parent
    preferredRendererType: Shape.GeometryRenderer
    ShapePath {
      strokeColor: seam.tone
      strokeWidth: Math.max(2, Style.space(2))
      fillColor: "transparent"
      strokeStyle: ShapePath.DashLine
      dashPattern: seam.touch.kind === "blocked" ? [1, 2.5] : [3, 2]
      startX: seam.vertical ? seam.width / 2 : 0
      startY: seam.vertical ? 0 : seam.height / 2
      PathLine {
        x: seam.vertical ? seam.width / 2 : seam.width
        y: seam.vertical ? seam.height : seam.height / 2
      }
    }
  }
}
