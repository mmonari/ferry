import QtQuick
import QtQuick.Shapes
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import ".."
import "../Geometry.js" as Geometry
import "../Strings.js" as Strings

// One machine on the Arrange canvas: its monitors as tiles to scale, its name and what is
// special about it above them, a pointer drag, and keyboard focus. The whole group moves as one
// rigid piece: there is no handle on a single monitor.
//
// The item is the *hit area*: at least `minHit` wide and high, about the centre of the group, so a
// small machine at a small scale is still easy to grab. The tiles sit inside it at their true size.
//
// What it says without colour: this machine, an offline peer and a peer not paired here
// each carry a tag in words; a peer not paired here also has a dashed outline and one not placed a
// dotted one, and both are dimmed. A drop that would overlap is tinted in the urgent colour *and*
// shows the overlap, and the status box says so in words.
//
// DragHandler.translation is already (0, 0) when onActiveChanged reports the drag over,
// So the canvas keeps the last position it computed; this file only
// reports the pointer's travel.
Item {
  id: group

  required property var canvas
  required property string fp

  readonly property var g: canvas.groupByFp[fp] || null
  readonly property bool held: drag.active
  readonly property bool invalid: canvas.invalidFps[fp] === true
  readonly property bool selected: canvas.focusedFp === fp
  readonly property bool dashed: g !== null && (!g.trusted || g.provisional === true)
  readonly property real quiet: g === null ? 1 : (g.provisional === true ? 0.8 : (!g.trusted ? 0.62 : (!g.online ? 0.55 : 1)))
  readonly property var vis: g ? canvas.toCanvasRect(g.rect) : ({ x: 0, y: 0, w: 0, h: 0 })
  readonly property var hit: Geometry.hitRect(vis, Style.space(56), Style.space(44))
  readonly property color fg: Commons.Color.popups.text
  readonly property color tint: group.invalid ? Commons.Color.urgent : ((group.held || group.selected) ? Commons.Color.accent : Util.alpha(fg, 0.34))
  readonly property bool labelInside: g !== null && canvas.labelBlocked(g, labelRow.implicitWidth, labelRow.implicitHeight + Style.space(4))
  readonly property var tags: {
    if (!g) return [];
    var out = [];
    if (g.self) out.push({ text: Strings.t("arrange.tag.self"), tone: "accent" });
    if (!g.trusted) out.push({ text: Strings.t("arrange.tag.untrusted", [canvas.meName || Strings.t("me.fallback")]), tone: "dim" });
    else if (!g.online) out.push({ text: Strings.t("arrange.tag.offline"), tone: "dim" });
    if (g.provisional === true) out.push({ text: Strings.t("arrange.tag.unplaced"), tone: "dim" });
    return out;
  }

  objectName: "arrange-group-" + (g ? g.name : fp)
  visible: g !== null && vis.w > 0
  x: hit.x
  y: hit.y
  width: hit.w
  height: hit.h
  z: held ? 8 : (selected ? 2 : 1)
  activeFocusOnTab: true

  Behavior on x { enabled: canvas.animating && !group.held; NumberAnimation { duration: Fx.ms(170); easing.type: Easing.OutCubic } }
  Behavior on y { enabled: canvas.animating && !group.held; NumberAnimation { duration: Fx.ms(170); easing.type: Easing.OutCubic } }
  Behavior on width { enabled: canvas.animating; NumberAnimation { duration: Fx.ms(170); easing.type: Easing.OutCubic } }
  Behavior on height { enabled: canvas.animating; NumberAnimation { duration: Fx.ms(170); easing.type: Easing.OutCubic } }

  Accessible.role: Accessible.Grouping
  Accessible.name: group.g ? canvas.describe(group.g) : ""
  Accessible.focusable: true

  onActiveFocusChanged: {
    if (activeFocus) canvas.focusedFp = fp;
    else if (canvas.focusedFp === fp) canvas.focusedFp = "";
  }

  // ---- arrows nudge: 10 px, Shift 100 px, Ctrl to the next edge
  Keys.onPressed: function (event) {
    var dir = event.key === Qt.Key_Left ? "left" : (event.key === Qt.Key_Right ? "right"
      : (event.key === Qt.Key_Up ? "up" : (event.key === Qt.Key_Down ? "down" : "")));
    if (dir === "") return;
    var mode = (event.modifiers & Qt.ControlModifier) ? "edge" : ((event.modifiers & Qt.ShiftModifier) ? "far" : "step");
    canvas.nudgeRequested(group.fp, dir, mode);
    event.accepted = true;
  }

  // the monitors and everything that is exactly the size of the group
  Item {
    id: visual
    x: group.hit.innerX
    y: group.hit.innerY
    width: group.vis.w
    height: group.vis.h

    // a held group lifts off the canvas
    Rectangle {
      visible: group.held
      x: Style.space(3)
      y: Style.space(4)
      width: parent.width
      height: parent.height
      radius: Math.max(2, Style.cornerRadius / 2)
      color: Qt.rgba(0, 0, 0, 0.28)
    }

    Repeater {
      // a count, not the monitors: the tiles keep their text while a drag moves them
      model: group.g ? group.g.monitors.length : 0

      Item {
        id: tile
        required property int index
        readonly property var m: group.g && index < group.g.monitors.length ? group.g.monitors[index] : null
        readonly property int tier: Geometry.labelTier(width, height)
        readonly property real ratio: m && m.scale && m.scale !== 1 ? Math.round(m.scale * 100) / 100 : 1

        visible: m !== null
        opacity: group.held ? 1 : group.quiet
        x: m ? (m.x - group.g.rect.x) * group.canvas.view.scale : 0
        y: m ? (m.y - group.g.rect.y) * group.canvas.view.scale : 0
        width: m ? m.w * group.canvas.view.scale : 0
        height: m ? m.h * group.canvas.view.scale : 0

        Behavior on x { enabled: group.canvas.animating && !group.held; NumberAnimation { duration: Fx.ms(170); easing.type: Easing.OutCubic } }
        Behavior on y { enabled: group.canvas.animating && !group.held; NumberAnimation { duration: Fx.ms(170); easing.type: Easing.OutCubic } }

        Rectangle {
          anchors.fill: parent
          radius: Math.max(2, Style.cornerRadius / 2)
          color: group.invalid ? Util.alpha(Commons.Color.urgent, 0.16)
            : (group.g && group.g.self ? Util.alpha(Commons.Color.accent, 0.12) : Util.alpha(group.fg, 0.07))
          border.width: group.dashed ? 0 : Math.max(1, Style.spaceReal(1.5))
          border.color: group.tint

          Behavior on color { ColorAnimation { duration: Fx.ms(120) } }
        }

        // a dashed outline says "not paired here", a dotted one "not placed yet": not colour alone
        Shape {
          visible: group.dashed
          anchors.fill: parent
          preferredRendererType: Shape.GeometryRenderer
          ShapePath {
            strokeColor: group.tint
            strokeWidth: Math.max(1, Style.spaceReal(1.5))
            fillColor: "transparent"
            strokeStyle: ShapePath.DashLine
            dashPattern: group.g && group.g.provisional === true ? [1, 2] : [4, 3]
            startX: 1; startY: 1
            PathLine { x: tile.width - 1; y: 1 }
            PathLine { x: tile.width - 1; y: tile.height - 1 }
            PathLine { x: 1; y: tile.height - 1 }
            PathLine { x: 1; y: 1 }
          }
        }

        Column {
          anchors.horizontalCenter: parent.horizontalCenter
          // the name of the machine is written inside the corner tile of a stack: the tile's own
          // words go below it, and the label never lands on them
          anchors.verticalCenter: parent.verticalCenter
          anchors.verticalCenterOffset: group.labelInside && group.g && tile.m.y === group.g.rect.y && tile.x < labelRow.width + Style.space(8)
            ? (labelRow.height + Style.space(6)) / 2 : 0
          width: parent.width - Style.space(16)
          spacing: Style.space(1)
          visible: tile.tier > 0

          Text {
            width: parent.width
            horizontalAlignment: Text.AlignHCenter
            textFormat: Text.PlainText
            text: tile.m ? tile.m.name : ""
            color: group.fg
            font.family: Style.font.family
            font.pixelSize: Style.font.bodySmall
            font.bold: true
            elide: Text.ElideRight
          }
          Text {
            visible: tile.tier >= 2
            width: parent.width
            horizontalAlignment: Text.AlignHCenter
            textFormat: Text.PlainText
            text: tile.m ? tile.m.w + "×" + tile.m.h + (tile.ratio !== 1 ? " ×" + tile.ratio : "") : ""
            color: Qt.darker(group.fg, 1.5)
            font.family: Style.font.family
            font.pixelSize: Style.font.caption
            elide: Text.ElideRight
          }
          Text {
            visible: tile.tier >= 3 && text !== ""
            width: parent.width
            horizontalAlignment: Text.AlignHCenter
            textFormat: Text.PlainText
            text: tile.m ? tile.m.description : ""
            color: Qt.darker(group.fg, 1.8)
            font.family: Style.font.family
            font.pixelSize: Style.font.caption
            elide: Text.ElideRight
          }
        }
      }
    }

    // the name of the machine, and the words that mark it: above its first monitor, or inside it when
    // another machine is right above (a stack), so it never lands on someone else's screen
    Rectangle {
      visible: group.labelInside
      x: labelRow.x - Style.space(3)
      y: labelRow.y - Style.space(1)
      width: labelRow.width + Style.space(6)
      height: labelRow.height + Style.space(2)
      radius: Style.space(3)
      color: Util.alpha(Commons.Color.popups.background, 0.82)
    }

    Row {
      id: labelRow
      x: group.labelInside ? Style.space(5) : 0
      y: group.labelInside ? Style.space(4) : -height - Style.space(3)
      spacing: Style.space(6)
      visible: group.g !== null

      Text {
        anchors.verticalCenter: parent.verticalCenter
        width: Math.min(implicitWidth, Style.space(220))
        textFormat: Text.PlainText
        text: group.g ? group.g.name : ""
        color: group.fg
        font.family: Style.font.family
        font.pixelSize: Style.font.bodySmall
        font.bold: true
        elide: Text.ElideRight
      }

      Repeater {
        model: group.tags

        Rectangle {
          required property var modelData
          anchors.verticalCenter: parent.verticalCenter
          width: tagText.implicitWidth + Style.space(8)
          height: tagText.implicitHeight + Style.space(2)
          radius: Style.cornerRadius
          color: modelData.tone === "accent" ? Util.alpha(Commons.Color.accent, 0.14) : "transparent"
          border.width: 1
          border.color: modelData.tone === "accent" ? Util.alpha(Commons.Color.accent, 0.7) : Util.alpha(group.fg, 0.35)

          Text {
            id: tagText
            anchors.centerIn: parent
            textFormat: Text.PlainText
            text: modelData.text
            color: modelData.tone === "accent" ? Commons.Color.accent : Qt.darker(group.fg, 1.3)
            font.family: Style.font.family
            font.pixelSize: Style.font.caption
            font.bold: modelData.tone === "accent"
          }
        }
      }
    }

    FocusRing {
      target: group
      visible: group.activeFocus && group.canvas.keyboardMode
      anchors.margins: -Style.space(4)
    }
  }

  // a press selects (so the arrow keys move this machine) and ends keyboard mode: the ring is for
  // the keyboard, a click shows the accent border instead
  TapHandler {
    acceptedButtons: Qt.LeftButton
    onPressedChanged: if (pressed) {
      group.forceActiveFocus();
      group.canvas.keyboardMode = false;
    }
  }

  DragHandler {
    id: drag
    target: null
    enabled: group.canvas.interactive
    acceptedButtons: Qt.LeftButton
    cursorShape: active ? Qt.ClosedHandCursor : Qt.OpenHandCursor
    onActiveChanged: {
      if (active) group.canvas.pointerBegan(group.fp);
      else group.canvas.pointerEnded(group.fp);
    }
    onTranslationChanged: if (active) group.canvas.pointerMoved(group.fp, translation.x, translation.y, centroid.modifiers)
  }
}
