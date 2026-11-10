import QtQuick
import Quickshell
import Quickshell.Wayland
import qs.Commons as Commons
import "."

// The edge glow: a short pulse along the stretch of a screen edge that the pointer just
// crossed, on this machine and, at the same moment, on the other one. ferryd says which edge in an
// `edge` event; FerryUi keeps the pulses that run and loads this file only while there is one.
//
// One layer-shell strip per pulse, on the edge's monitor, with an aurora curtain in it
// (AuroraCurtain.qml) that rises from the crossed stretch. Its input region is empty, so it never
// takes a click, a scroll or the pointer row the barrier needs. The colour is the shell's accent,
// which follows the Omarchy theme of this machine.
Scope {
  Variants {
    model: FerryUi.glows

    PanelWindow {
      id: strip

      required property var modelData
      readonly property bool across: modelData.side === "top" || modelData.side === "bottom"
      // How far the curtain's rays reach in from the edge.
      readonly property int depth: 96

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

      AuroraCurtain {
        x: strip.across ? strip.modelData.start : 0
        y: strip.across ? 0 : strip.modelData.start
        side: strip.modelData.side
        length: strip.modelData.end - strip.modelData.start
        depth: strip.depth
        tint: Commons.Color.accent
        duration: FerryUi.glowMs
        still: Fx.still
      }
    }
  }
}
