import QtQuick
import Quickshell
import Quickshell.Wayland
import qs.Commons as Commons
import "."

// The edge glow: a short pulse along the stretch of a screen edge that the pointer just
// crossed, on this machine and, at the same moment, on the other one. ferryd says which edge in an
// `edge` event; FerryUi keeps the pulses that run and loads this file only while there is one.
//
// One thin layer-shell strip per pulse, on the edge's monitor. Its input region is empty, so it
// never takes a click, a scroll or the pointer row the barrier needs. The colour is the shell's
// accent, which follows the Omarchy theme of this machine.
Scope {
  Variants {
    model: FerryUi.glows

    PanelWindow {
      id: strip

      required property var modelData
      readonly property bool across: modelData.side === "top" || modelData.side === "bottom"
      // How far the glow reaches in from the edge.
      readonly property int depth: 14

      // Resolved apart from `screen`: the window writes `screen` back when it maps, so a `visible`
      // that read it would loop.
      readonly property var target: {
        var screens = Quickshell.screens;
        for (var i = 0; i < screens.length; i++)
          if (screens[i].name === modelData.monitor) return screens[i];
        return null;
      }
      screen: target
      visible: target !== null
      color: "transparent"
      exclusionMode: ExclusionMode.Ignore
      mask: Region {}

      WlrLayershell.namespace: "ferry-edge-glow"
      WlrLayershell.layer: WlrLayer.Overlay
      WlrLayershell.keyboardFocus: WlrKeyboardFocus.None

      anchors {
        top: modelData.side !== "bottom"
        bottom: modelData.side !== "top"
        left: modelData.side !== "right"
        right: modelData.side !== "left"
      }
      implicitWidth: across ? 0 : depth
      implicitHeight: across ? depth : 0

      Rectangle {
        id: glow

        x: strip.across ? strip.modelData.start : 0
        y: strip.across ? 0 : strip.modelData.start
        width: strip.across ? strip.modelData.end - strip.modelData.start : strip.depth
        height: strip.across ? strip.depth : strip.modelData.end - strip.modelData.start
        radius: strip.depth / 2
        opacity: 0

        // Brightest on the edge itself, gone a few pixels in.
        gradient: Gradient {
          orientation: strip.across ? Gradient.Vertical : Gradient.Horizontal
          GradientStop {
            position: 0
            color: strip.modelData.side === "top" || strip.modelData.side === "left"
              ? Commons.Color.accent : "transparent"
          }
          GradientStop {
            position: 1
            color: strip.modelData.side === "bottom" || strip.modelData.side === "right"
              ? Commons.Color.accent : "transparent"
          }
        }

        SequentialAnimation on opacity {
          running: true
          NumberAnimation { to: 0.95; duration: 90; easing.type: Easing.OutQuad }
          PauseAnimation { duration: 140 }
          NumberAnimation { to: 0; duration: 420; easing.type: Easing.InQuad }
        }
      }
    }
  }
}
