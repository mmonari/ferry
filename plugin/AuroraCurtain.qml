import QtQuick

// The edge glow drawn as an aurora curtain: rays of light that rise from the crossed edge,
// sway along it, flicker and fade, brightest on a soft rippling hem near the edge. One pulse per
// instance: it rises, shimmers for `duration` ms and is gone.
//
// Drawn in its own frame with the edge at the bottom and the rays rising, then turned to face the
// side it stands on. Colours come in from outside (the theme's accent), so this file knows no theme
// and can be drawn on its own (scripts/m3/aurora-preview.sh).
Item {
  id: root

  // "top", "bottom", "left" or "right": the screen edge the curtain stands on.
  property string side: "bottom"
  // Along the edge, and how far the light reaches in from it.
  property real length: 600
  property real depth: 96
  property color tint: "#89b4fa"
  property int duration: 1150
  // The shell's "animations off": the curtain fades in and out and holds still.
  property bool still: false

  readonly property bool across: side === "top" || side === "bottom"
  width: across ? length : depth
  height: across ? depth : length

  // Hue and light around the theme colour: a pale hem near the edge, the colour itself in the rays,
  // and tips that drift a little round the colour wheel, the way green aurora turns violet at the top.
  readonly property color hem: Qt.hsla(tint.hslHue, Math.min(1, tint.hslSaturation * 0.6),
    Math.min(0.92, tint.hslLightness + 0.3), 1)
  readonly property color tip: Qt.hsla((tint.hslHue + 0.11) % 1, tint.hslSaturation,
    tint.hslLightness, 1)

  // 0 → 1 over the pulse; everything that moves reads it.
  property real phase: 0
  // How far the rays have risen, and how bright the whole curtain is.
  property real rise: 0
  property real glow: 0

  readonly property real spacing: 6
  readonly property real taperLength: Math.min(48, length / 4)
  readonly property var washSlices: {
    var out = [], step = 4, n = Math.ceil(taperLength / step);
    for (var i = 0; i < n; i++) {
      var o = taper(i * step + step / 2);
      out.push({ x: i * step, w: step, o: o });
      out.push({ x: length - (i + 1) * step, w: step, o: o });
    }
    out.push({ x: n * step, w: Math.max(0, length - 2 * n * step), o: 1 });
    return out;
  }
  readonly property int rays: Math.max(4, Math.ceil(length / spacing))

  // How far the hem is lifted at `u` along the curtain at time `t`, 0 to 1.
  function fold(u, t) {
    return 0.5 + 0.5 * Math.sin(u * 1.3 - t * 1.6) * Math.cos(u * 0.47 + t * 0.7);
  }

  // 0 at the ends of the curtain, 1 a little way in, so it frays out instead of stopping square.
  function taper(x) {
    var d = Math.min(x, root.length - x) / root.taperLength;
    return Math.max(0, Math.min(1, d));
  }

  // A fixed pseudo-random number in [0, 1) per ray and salt, so each ray keeps its own character.
  function rnd(i, salt) {
    var v = Math.sin(i * 12.9898 + salt * 78.233) * 43758.5453;
    return v - Math.floor(v);
  }

  ParallelAnimation {
    running: true
    NumberAnimation {
      target: root; property: "phase"; from: 0; to: 1
      duration: root.duration
    }
    SequentialAnimation {
      NumberAnimation {
        target: root; property: "rise"; from: root.still ? 1 : 0.15; to: 1
        duration: root.duration * 0.3; easing.type: Easing.OutCubic
      }
    }
    SequentialAnimation {
      NumberAnimation {
        target: root; property: "glow"; from: 0; to: 1
        duration: root.duration * 0.14; easing.type: Easing.OutQuad
      }
      PauseAnimation { duration: root.duration * 0.36 }
      NumberAnimation {
        target: root; property: "glow"; to: 0
        duration: root.duration * 0.5; easing.type: Easing.InQuad
      }
    }
  }

  Item {
    id: frame

    // The curtain's own frame: the edge along the bottom, the light rising.
    width: root.length
    height: root.depth
    anchors.centerIn: parent
    rotation: root.side === "top" ? 180 : root.side === "left" ? 90 : root.side === "right" ? -90 : 0
    opacity: root.glow
    // Only the linked stretch glows: nothing spills past its ends.
    clip: true

    // The wash the curtain stands in: faint, wide, brightest at the edge. Full strength in the middle
    // and in thin slices at the ends, each as bright as the taper there.
    Repeater {
      model: root.washSlices

      Rectangle {
        required property var modelData

        x: modelData.x
        width: modelData.w
        anchors.bottom: parent.bottom
        height: root.depth * 0.55
        opacity: modelData.o
        gradient: Gradient {
          GradientStop { position: 0; color: "transparent" }
          GradientStop { position: 0.7; color: Qt.alpha(root.tint, 0.16) }
          GradientStop { position: 1; color: Qt.alpha(root.tint, 0.42) }
        }
      }
    }

    Item {
      id: curtain

      anchors.fill: parent

      Repeater {
        model: root.rays

        Item {
          id: ray

          required property int index
          readonly property real r1: root.rnd(index, 1)
          readonly property real r2: root.rnd(index, 2)
          readonly property real r3: root.rnd(index, 3)
          // Where the ray stands along the curtain, and the time, in the same unit.
          readonly property real u: index * root.spacing / 60
          readonly property real t: root.still ? 0 : root.phase * 6.2832

          // The hem ripples: a slow fold that travels along the edge, so the rays' feet lift and
          // settle like the lower border of a curtain in the wind.
          readonly property real fold: root.fold(u, t)
          readonly property real foot: root.depth * 0.18 * fold
          // The curtain gathers into bright folds with dim gaps between them, and the folds drift.
          readonly property real dense: 0.5 + 0.5 * Math.sin(u * 0.9 + t * 0.8 + 1.7 * Math.sin(u * 0.31))
          // Tall where the fold is lifted, short in the troughs, with each ray's own share.
          readonly property real reach: Math.max(0.15, Math.min(1,
            0.35 + 0.35 * fold + 0.2 * dense + 0.25 * r1 + 0.1 * Math.sin(u * 4.1 + t * 3.3)))
          // A flicker: rays brighten and dim out of step, and now and then one flares.
          readonly property real flick: 0.6 + 0.4 * Math.sin(index * 0.9 + r2 * 6.28 + t * (2 + 3 * r3))
          readonly property real bright: root.taper(index * root.spacing) * Math.max(0, Math.min(1,
            (0.2 + 0.8 * dense) * (0.45 + 0.55 * r1) * flick + (r3 > 0.94 ? 0.35 : 0)))

          x: index * root.spacing + (r3 - 0.5) * root.spacing * 0.8
          width: 1
          height: (root.depth - foot) * reach * root.rise
          y: root.depth - foot - height + 3

          // A wide, faint halo, so the light reads as soft without a blur pass.
          Rectangle {
            anchors.horizontalCenter: parent.horizontalCenter
            anchors.bottom: parent.bottom
            width: 7 + 7 * ray.r2
            height: parent.height * 0.85
            radius: width / 2
            opacity: ray.bright * 0.32
            gradient: Gradient {
              GradientStop { position: 0; color: "transparent" }
              GradientStop { position: 0.6; color: Qt.alpha(root.tint, 0.5) }
              GradientStop { position: 1; color: root.tint }
            }
          }

          // The ray: brightest at its foot, turning towards the tip colour as it fades.
          Rectangle {
            anchors.horizontalCenter: parent.horizontalCenter
            anchors.bottom: parent.bottom
            width: 1.2 + 2.2 * ray.r2
            height: parent.height
            radius: width / 2
            antialiasing: true
            opacity: ray.bright
            gradient: Gradient {
              GradientStop { position: 0; color: "transparent" }
              GradientStop { position: 0.35; color: Qt.alpha(root.tip, 0.3) }
              GradientStop { position: 0.8; color: root.tint }
              GradientStop { position: 1; color: root.hem }
            }
          }
        }
      }

      // The hem: a soft bright band along the feet of the rays, in overlapping pieces so it follows
      // the fold.
      Repeater {
        model: Math.ceil(root.length / 12) + 1

        Rectangle {
          required property int index
          readonly property real fold: root.fold(index * 12 / 60, root.still ? 0 : root.phase * 6.2832)

          x: index * 12 - 12
          width: 24
          height: 14
          radius: 7
          y: root.depth - root.depth * 0.18 * fold - 9
          opacity: (0.3 + 0.4 * fold) * root.taper(x + 12)
          gradient: Gradient {
            GradientStop { position: 0; color: "transparent" }
            GradientStop { position: 0.4; color: Qt.alpha(root.tint, 0.5) }
            GradientStop { position: 0.55; color: Qt.alpha(root.hem, 0.7) }
            GradientStop { position: 1; color: "transparent" }
          }
        }
      }
    }
  }
}
