import QtQuick
import QtQuick.Shapes
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import ".."
import "../Arrange.js" as Arrange
import "../Geometry.js" as Geometry
import "../Glyphs.js" as Glyphs
import "../Strings.js" as Strings

// The plane of the Arrange panel: every machine's monitors to scale, the places where
// their edges touch drawn as seams, the edges that are walls, and the guides of a drag in progress.
// It draws and takes the pointer; it decides nothing. What a drag means (snapping, overlap, the
// draft) is ArrangePanel's, through the pure functions of Geometry.js and Arrange.js: this file
// converts the pointer's travel to plane pixels, reports it, and draws what comes back.
//
// The view fits everything with room to drag a machine to any side of the rest, and it only ever grows
// while the panel is open, so a drop never makes the whole picture jump. The wheel and the buttons
// zoom (to 8x), a drag on the empty plane pans a zoomed view, and Fit starts over.
BorderSurface {
  id: plane

  // ---- what to draw
  property var groups: []
  property var analysis: ({ touches: [], walls: [], overlaps: [] })
  // while a drag is in progress: { fp, guides, valid, overlapWith }
  property var liveDrag: null
  property bool interactive: true
  property string meName: ""

  // ---- state the panel and the groups share
  property bool keyboardMode: false
  property bool animating: false
  property string focusedFp: ""

  readonly property color fg: Commons.Color.popups.text
  readonly property var groupByFp: {
    var m = {};
    for (var i = 0; i < groups.length; i++) m[groups[i].fp] = groups[i];
    return m;
  }
  // The machines that overlap another, by fingerprint: they are tinted.
  readonly property var invalidFps: {
    var m = {};
    var list = analysis.overlaps || [];
    for (var i = 0; i < list.length; i++) { m[list[i].a] = true; m[list[i].b] = true; }
    return m;
  }
  readonly property var overlapAreas: {
    var out = [];
    var list = analysis.overlaps || [];
    for (var i = 0; i < list.length; i++) out = out.concat(list[i].areas);
    return out;
  }

  // Tab walks the machines left to right as they were when the set of machines last changed; the
  // order does not shuffle while one is dragged past another.
  property var fps: []
  property string fpsKey: ""

  signal dragBegan(string fp)
  // plane pixels since the press; `free` is the modifier that turns snapping off
  signal dragMoved(string fp, real dx, real dy, bool free)
  signal dragEnded(string fp)
  signal nudgeRequested(string fp, string dir, string mode)

  // ---- the view
  property var region: null
  property var manual: null
  readonly property var fitView: region !== null && width > 0 && height > 0
    ? Geometry.viewFor(region, width, height)
    : ({ scale: 1, x: 0, y: 0, w: Math.max(1, width), h: Math.max(1, height) })
  readonly property var view: manual !== null ? manual : fitView
  readonly property bool zoomed: manual !== null
  readonly property alias groupRepeater: repeater

  radius: Style.cornerRadius
  color: Commons.Color.popups.background
  borderSpec: Border.flat(Util.alpha(fg, 0.12), 1)
  clip: true

  Accessible.role: Accessible.Pane
  Accessible.name: Strings.t("arrange.canvas")

  // ---------------------------------------------------------------------- coordinates
  function cx(px) { return (px - view.x) * view.scale; }
  function cy(py) { return (py - view.y) * view.scale; }
  function toCanvasRect(r) {
    return { x: (r.x - view.x) * view.scale, y: (r.y - view.y) * view.scale, w: r.w * view.scale, h: r.h * view.scale };
  }
  function describe(g) { return Arrange.groupName(g, meName); }

  // Is something in the way of writing this machine's name above it? Then the group writes it inside.
  function labelBlocked(g, wPx, hPx) {
    var others = [];
    for (var i = 0; i < groups.length; i++) {
      if (groups[i].fp === g.fp) continue;
      others = others.concat(Geometry.groupRects(groups[i]));
    }
    return Geometry.stripBlocked(g.rect, others, wPx / view.scale, hPx / view.scale);
  }

  // ----------------------------------------------------------------- groups and the view
  function refreshFps() {
    var key = groups.map(function (g) { return g.fp; }).sort().join(",");
    if (key === fpsKey) return;
    fpsKey = key;
    fps = groups.slice().sort(function (a, b) { return (a.rect.x - b.rect.x) || (a.rect.y - b.rect.y); }).map(function (g) { return g.fp; });
  }

  function needed() {
    if (groups.length === 0) return null;
    var content = Geometry.bounding(groups.map(function (g) { return g.rect; }));
    var big = { w: 0, h: 0 };
    groups.forEach(function (g) { big.w = Math.max(big.w, g.rect.w); big.h = Math.max(big.h, g.rect.h); });
    return Geometry.fitRegion(content, big);
  }

  // Make the groups move to where the view now puts them with a short glide, then stop gliding: a
  // drag must follow the pointer exactly, and a nudge must not trail its seams.
  function glide() {
    if (Fx.still) return;
    animating = true;
    settle.restart();
  }

  function follow() {
    // never while a drag is in progress: the picture must not move under the pointer
    if (liveDrag !== null) return;
    var need = needed();
    if (need === null) return;
    // a zoomed view is the user's own, and a picture that still shows everything with room to spare
    // stays exactly where it is: dropping a machine lower does not make the whole canvas shift
    if (manual === null && region !== null && width > 0) {
      var content = Geometry.bounding(groups.map(function (g) { return g.rect; }));
      var m = Style.space(30) / view.scale;
      var wanted = { x: content.x - m, y: content.y - m, w: content.w + 2 * m, h: content.h + 2 * m };
      if (Geometry.contains({ x: view.x, y: view.y, w: view.w, h: view.h }, wanted)) return;
    }
    var grown = Geometry.growRegion(region, need);
    if (grown !== region) {
      var first = region === null;
      region = grown;
      if (!first) glide();
    }
  }

  onGroupsChanged: { refreshFps(); follow(); }
  onLiveDragChanged: if (liveDrag === null) follow()

  function fit() {
    manual = null;
    region = needed();
    glide();
  }

  function zoomBy(factor, atX, atY) {
    var px = atX === undefined ? width / 2 : atX;
    var py = atY === undefined ? height / 2 : atY;
    var next = Geometry.zoomAt(view, factor, px, py, fitView.scale, 8);
    manual = next.scale <= fitView.scale * 1.001 ? null : next;
  }

  function groupItem(fp) {
    var i = fps.indexOf(fp);
    return i < 0 ? null : repeater.itemAt(i);
  }

  function focusGroup(fp) {
    var item = groupItem(fp);
    if (!item) return false;
    keyboardMode = true;
    item.forceActiveFocus();
    return true;
  }

  // ---------------------------------------------------------------- what the groups report
  function pointerBegan(fp) { dragBegan(fp); }
  function pointerMoved(fp, tx, ty, modifiers) {
    var free = (modifiers & (Qt.AltModifier | Qt.ControlModifier)) !== 0;
    dragMoved(fp, tx / view.scale, ty / view.scale, free);
  }
  function pointerEnded(fp) { dragEnded(fp); }

  Timer {
    id: settle
    interval: 240
    onTriggered: plane.animating = false
  }

  // ------------------------------------------------------------------------------ the grid
  // a hairline every 24 px, as the mockup's canvas
  Canvas {
    anchors.fill: parent
    property color line: Util.alpha(plane.fg, 0.05)
    onLineChanged: requestPaint()
    onWidthChanged: requestPaint()
    onHeightChanged: requestPaint()
    onPaint: {
      var ctx = getContext("2d");
      ctx.reset();
      ctx.strokeStyle = String(line);
      ctx.lineWidth = 1;
      ctx.beginPath();
      for (var x = 0; x <= width; x += 24) { ctx.moveTo(x + 0.5, 0); ctx.lineTo(x + 0.5, height); }
      for (var y = 0; y <= height; y += 24) { ctx.moveTo(0, y + 0.5); ctx.lineTo(width, y + 0.5); }
      ctx.stroke();
    }
  }

  // The empty plane pans a zoomed view; the wheel zooms about the pointer. A group's own drag wins
  // where it starts on a group, because its threshold is the smaller one.
  DragHandler {
    id: pan
    target: null
    enabled: plane.zoomed
    acceptedButtons: Qt.LeftButton | Qt.MiddleButton
    dragThreshold: 14
    cursorShape: active ? Qt.ClosedHandCursor : Qt.ArrowCursor
    property var origin: null
    onActiveChanged: if (active) origin = plane.view
    onActiveTranslationChanged: if (active && origin !== null) plane.manual = Geometry.panBy(origin, activeTranslation.x, activeTranslation.y)
  }

  WheelHandler {
    acceptedDevices: PointerDevice.Mouse | PointerDevice.TouchPad
    onWheel: function (event) {
      plane.zoomBy(event.angleDelta.y > 0 ? 1.18 : 1 / 1.18, point.position.x, point.position.y);
    }
  }

  // --------------------------------------------------------------------------- the machines
  Repeater {
    id: repeater
    model: plane.fps

    ArrangeGroup {
      required property string modelData
      canvas: plane
      fp: modelData
    }
  }

  // ---------------------------------------------------------------- overlaps, walls, seams
  // Where two machines would cover each other: the tint is the overlap itself
  Repeater {
    model: plane.overlapAreas

    Rectangle {
      required property var modelData
      readonly property var r: plane.toCanvasRect(modelData)
      x: r.x
      y: r.y
      width: r.w
      height: r.h
      z: 12
      radius: 2
      color: Util.alpha(Commons.Color.urgent, 0.38)
      border.width: 1
      border.color: Commons.Color.urgent
    }
  }

  // Edges nothing is on the other side of: the pointer stops there
  Repeater {
    model: plane.analysis.walls || []

    Rectangle {
      required property var modelData
      readonly property real t: Math.max(3, Style.space(3))
      readonly property real px: plane.view.scale
      z: 10
      x: modelData.vertical ? plane.cx(modelData.line) - t / 2 : plane.cx(modelData.span[0])
      y: modelData.vertical ? plane.cy(modelData.span[0]) : plane.cy(modelData.line) - t / 2
      width: modelData.vertical ? t : (modelData.span[1] - modelData.span[0]) * px
      height: modelData.vertical ? (modelData.span[1] - modelData.span[0]) * px : t
      radius: 1.5
      color: Util.alpha(plane.fg, 0.72)
    }
  }

  // Where they touch
  Repeater {
    model: plane.analysis.touches || []

    ArrangeSeam {
      required property var modelData
      canvas: plane
      touch: modelData
      z: 11
    }
  }

  // The lines a snap is lining up with, while a drag runs
  Repeater {
    model: plane.liveDrag ? plane.liveDrag.guides : []

    Shape {
      id: guide
      required property var modelData
      readonly property bool vertical: modelData.axis === "x"
      readonly property real margin: Style.space(24)
      z: 13
      x: vertical ? plane.cx(modelData.at) : plane.cx(modelData.from) - margin
      y: vertical ? plane.cy(modelData.from) - margin : plane.cy(modelData.at)
      width: vertical ? 1 : (modelData.to - modelData.from) * plane.view.scale + margin * 2
      height: vertical ? (modelData.to - modelData.from) * plane.view.scale + margin * 2 : 1
      preferredRendererType: Shape.GeometryRenderer
      ShapePath {
        strokeColor: Commons.Color.accent
        strokeWidth: 1.5
        fillColor: "transparent"
        strokeStyle: ShapePath.DashLine
        dashPattern: [4, 3]
        startX: 0
        startY: 0
        PathLine { x: guide.vertical ? 0 : guide.width; y: guide.vertical ? guide.height : 0 }
      }
    }
  }

  // ------------------------------------------------------------------------------ legend
  // In the corner opposite the zoom controls, out of the way of the screens (#26).
  // the key to the picture: a solid glowing bar is a link, a thick line a wall, a dashed one is off
  Row {
    anchors.left: parent.left
    anchors.bottom: parent.bottom
    anchors.margins: Style.space(8)
    spacing: Style.space(10)
    z: 20

    Row {
      spacing: Style.space(4)
      Rectangle { anchors.verticalCenter: parent.verticalCenter; width: Style.space(14); height: Math.max(3, Style.space(3)); radius: height / 2; color: Commons.Color.accent }
      Text { anchors.verticalCenter: parent.verticalCenter; textFormat: Text.PlainText; text: Strings.t("arrange.legend.link"); color: Qt.darker(plane.fg, 1.4); font.family: Style.font.family; font.pixelSize: Style.font.caption }
    }
    Row {
      spacing: Style.space(4)
      Rectangle { anchors.verticalCenter: parent.verticalCenter; width: Style.space(14); height: Math.max(2, Style.space(2.5)); radius: 1; color: Util.alpha(plane.fg, 0.5) }
      Text { anchors.verticalCenter: parent.verticalCenter; textFormat: Text.PlainText; text: Strings.t("arrange.legend.wall"); color: Qt.darker(plane.fg, 1.4); font.family: Style.font.family; font.pixelSize: Style.font.caption }
    }
    Row {
      spacing: Style.space(4)
      Row {
        anchors.verticalCenter: parent.verticalCenter
        spacing: Style.space(2)
        Repeater {
          model: 3
          Rectangle { width: Style.space(3); height: Math.max(2, Style.space(2)); color: Util.alpha(Commons.Color.accent, 0.8) }
        }
      }
      Text { anchors.verticalCenter: parent.verticalCenter; textFormat: Text.PlainText; text: Strings.t("arrange.legend.off"); color: Qt.darker(plane.fg, 1.4); font.family: Style.font.family; font.pixelSize: Style.font.caption }
    }
  }

  // ------------------------------------------------------------------------------ zoom
  Row {
    anchors.top: parent.top
    anchors.right: parent.right
    anchors.margins: Style.space(8)
    spacing: Style.space(4)
    z: 20

    PanelActionButton {
      iconText: Glyphs.ui("zoomOut")
      tooltipText: Strings.t("arrange.zoom.out")
      foreground: plane.fg
      bordered: true
      focusable: true
      enabled: plane.zoomed
      Accessible.name: Strings.t("arrange.zoom.out")
      onClicked: plane.zoomBy(1 / 1.4)
      FocusRing {}
    }

    PanelActionButton {
      iconText: Glyphs.ui("fit")
      tooltipText: Strings.t("arrange.fit")
      foreground: plane.fg
      bordered: true
      focusable: true
      Accessible.name: Strings.t("arrange.fit")
      onClicked: plane.fit()
      FocusRing {}
    }

    PanelActionButton {
      iconText: Glyphs.ui("zoomIn")
      tooltipText: Strings.t("arrange.zoom.in")
      foreground: plane.fg
      bordered: true
      focusable: true
      enabled: plane.view.scale < plane.fitView.scale * 7.9
      Accessible.name: Strings.t("arrange.zoom.in")
      onClicked: plane.zoomBy(1.4)
      FocusRing {}
    }
  }
}
