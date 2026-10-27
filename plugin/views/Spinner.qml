import QtQuick
import qs.Commons
import ".."
import "../Glyphs.js" as Glyphs

// Something is in progress. A turning glyph, and a still one when the shell has animations off
// (Fx.still), because a loop that never stops is exactly what "reduce motion" asks not to see.
Glyph {
  id: root

  property bool running: true
  property real size: Style.font.icon
  // Nothing turns in a window nobody can see: the popover is kept after it closes, and a loop that went on
  // would wake the shell sixty times a second for a mark on a hidden surface.
  readonly property bool windowVisible: Window.window ? Window.window.visible : true

  width: size
  height: size
  text: Glyphs.ui("loading")
  fontSize: size
  paintedSize: size * 0.92

  RotationAnimator on rotation {
    from: 0
    to: 360
    duration: 1000
    loops: Animation.Infinite
    running: root.running && root.visible && root.windowVisible && !Fx.still
  }
}
