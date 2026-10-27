import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import ".."
import "../views"
import "../Arrange.js" as Arrange
import "../Geometry.js" as Geometry
import "../Glyphs.js" as Glyphs
import "../Strings.js" as Strings

// The Arrange panel (-07, -10, -17, journey J3): every peer's monitors to scale on one plane,
// dragged and nudged as whole machines, edited in a local draft until Apply sends one revision.
//
// This file is the state and the layout of the panel. The picture is views/ArrangeCanvas, the
// decisions are plugin/Arrange.js (the draft, the conflict, the words) and plugin/Geometry.js (snapping,
// overlap, links, nudging), all with node tests; nothing here is a rule.
//
//   draft      the origins the user moved, on top of what the daemon last said. Nothing is sent until
//              Apply, which calls `layout.set { origins, base_rev }` with the groups that moved and the
//              revision the draft began on. Reset throws it away, Undo steps back, and a drop that
//              would overlap goes back where it was, with the reason.
//   conflict   a newer revision over a dirty draft keeps the draft and shows a banner:
//              Keep mine applies it on top of the new revision, Take theirs reloads. Without a draft the
//              newer layout simply appears, with a short glide.
//   keyboard   Tab picks a machine, the arrows move it 10 px, Shift 100 px, Ctrl to the next edge, Enter
//              applies, Esc resets and then closes, Ctrl+Z undoes, + - 0 zoom and fit.
//
// For the dev harness (no pointer offscreen): simulateDrag, simulateDragLive, simulateDragEnd and
// simulateKey run the same code a pointer or a key press does.
FocusScope {
  id: root

  // asks the panel's host to show another tab (the Pair panel, from the empty state)
  signal viewRequested(string view)

  readonly property color fg: Commons.Color.popups.text
  readonly property color dim: Qt.darker(fg, 1.4)
  readonly property string fontFamily: Style.font.family
  property alias canvas: canvasView
  property alias conflictView: bannerView

  // ---------------------------------------------------------------- what the daemon says
  // Our own Apply's answer stands in for the snapshot until the real one catches up (a daemon that
  // cannot push is polled once a second, and the groups must not flash back to the old places).
  property var applied: null
  readonly property var live: applied !== null ? applied : FerryClient.layout
  readonly property string meFp: FerryClient.status && FerryClient.status.me ? FerryClient.status.me.fingerprint : ""
  readonly property var parsed: Arrange.groupsOf(live, meFp)
  readonly property var base: parsed.groups
  readonly property bool canSet: Arrange.canSet(FerryClient.hello)
  readonly property int rev: live && live.revision ? live.revision.counter : 0

  // ------------------------------------------------------------------------------- the draft
  property var draft: Arrange.emptyDraft()
  // the group in the pointer's hand: { fp, startOrigin, startRects, startBounds, origin, valid, overlapWith, guides, rev }
  property var liveDrag: null
  property bool applying: false
  property var applyError: null
  property bool staleSeen: false
  property string flashText: ""
  property string flashTone: "ok"

  readonly property var groups: Arrange.effectiveGroups(base, draft, liveDrag)
  readonly property var analysis: Arrange.analyse(groups)
  readonly property bool dirty: Arrange.isDirty(draft, base)
  readonly property var conflict: Arrange.conflictOf(draft, live, base, meFp)
  // a stale_layout answer shows the banner even before the newer snapshot has arrived
  readonly property var staleInfo: Arrange.staleInfo(applyError)
  readonly property var banner: conflict !== null ? conflict
    : (staleSeen && dirty ? { counter: staleInfo ? staleInfo.counter : rev, by: staleInfo ? staleInfo.by : Strings.t("me.fallback"), mine: false, pending: true } : null)
  readonly property bool overlapping: analysis.overlaps.length > 0
  readonly property bool canApply: Arrange.canApply({
    connected: FerryClient.connected, dirty: dirty, canSet: canSet, overlap: overlapping,
    conflict: banner !== null, applying: applying, dragging: liveDrag !== null
  })
  readonly property var rows: Arrange.linkRows(analysis, groups)
  readonly property var status: Arrange.statusOf({
    groups: groups, analysis: analysis, drag: liveDrag, dirty: dirty, conflict: banner,
    error: banner !== null ? null : applyError, flash: flashText, flashTone: flashTone, rev: rev,
    canSet: canSet, meName: FerryClient.meName, waiting: parsed.waiting
  })
  readonly property var others: groups.filter(function (g) { return !g.self && g.trusted; })

  // what the panel shows: arrange, or one of the reasons there is nothing to arrange
  readonly property string mode: !FerryClient.connected ? "off"
    : (live === null ? "loading" : (groups.length === 0 ? "nomonitors" : (groups.length === 1 ? "empty" : "arrange")))

  focus: true

  // ----------------------------------------------------------------------------- messages
  Timer {
    id: flashTimer
    interval: 6000
    onTriggered: root.flashText = ""
  }

  function flash(text, tone) {
    flashText = text;
    flashTone = tone || "ok";
    flashTimer.restart();
  }

  function namesOf(fps) {
    return Arrange.joinNames(fps.map(function (fp) { return Arrange.nameOf(groups, fp); }));
  }

  // -------------------------------------------------------------------------- drag and drop
  function othersOf(fp) {
    var rects = [], fps = [];
    for (var i = 0; i < groups.length; i++) {
      var g = groups[i];
      if (g.fp === fp || !g.placed) continue;
      rects.push(Geometry.groupRects(g));
      fps.push(g.fp);
    }
    return { rects: rects, fps: fps };
  }

  function dragBegan(fp) {
    var g = Arrange.groupBy(groups, fp);
    if (!g) return;
    applyError = null;
    flashText = "";
    liveDrag = {
      fp: fp,
      startOrigin: [g.origin[0], g.origin[1]],
      startRects: Geometry.groupRects(g),
      startBounds: g.rect,
      origin: [g.origin[0], g.origin[1]],
      valid: true,
      overlapWith: [],
      guides: [],
      rev: live ? live.revision : null
    };
  }

  // dx, dy: how far the pointer has taken the group, in plane pixels since the press
  function dragMoved(fp, dx, dy, free) {
    var d = liveDrag;
    if (!d || d.fp !== fp) return;
    var o = othersOf(fp);
    var delta = Geometry.keepVisible(d.startBounds, canvasView.view, [dx, dy]);
    var r = Geometry.snapMove({
      start: d.startRects, others: o.rects, origin: d.startOrigin, delta: delta,
      threshold: Geometry.snapThreshold(canvasView.view.scale, 14), free: free === true
    });
    liveDrag = Object.assign({}, d, {
      origin: r.origin, valid: r.valid, guides: r.guides,
      overlapWith: r.overlapWith.map(function (i) { return o.fps[i]; })
    });
  }

  function dragEnded(fp) {
    var d = liveDrag;
    if (!d || d.fp !== fp) return;
    if (!d.valid) {
      // refused: it goes back where it was, and says why. The glide starts before the
      // group leaves the pointer's hand, or it would jump.
      flash(Strings.t("arrange.status.snapback", [Arrange.nameOf(groups, fp), namesOf(d.overlapWith)]), "warn");
      console.log("ferry arrange: " + Arrange.nameOf(groups, fp) + " refused: it would cover " + namesOf(d.overlapWith));
      canvasView.glide();
      liveDrag = null;
      return;
    }
    liveDrag = null;
    if (d.origin[0] === d.startOrigin[0] && d.origin[1] === d.startOrigin[1]) return;
    draft = Arrange.withOrigin(draft, base, d.rev, fp, d.origin);
    console.log("ferry arrange: " + Arrange.nameOf(groups, fp) + " dropped at " + d.origin[0] + "," + d.origin[1]);
  }

  // ---------------------------------------------------------------------------- the keyboard
  function nudge(fp, dir, mode) {
    var g = Arrange.groupBy(groups, fp);
    if (!g || !canSet) return;
    var o = othersOf(fp);
    var r = Geometry.nudgeMove({ start: Geometry.groupRects(g), others: o.rects, dir: dir, mode: mode });
    if (!r.moved) {
      flash(Strings.t("arrange.status.blocked.nudge", [g.name, namesOf(r.overlapWith.map(function (i) { return o.fps[i]; }))]), "warn");
      return;
    }
    flashText = "";
    applyError = null;
    draft = Arrange.withOrigin(draft, base, live ? live.revision : null, fp, [g.origin[0] + r.delta[0], g.origin[1] + r.delta[1]]);
    console.log("ferry arrange: " + g.name + " nudged " + mode + " " + dir + " to " + (g.origin[0] + r.delta[0]) + "," + (g.origin[1] + r.delta[1]));
  }

  // Enter: place the machine that has no place yet where it stands, else Apply
  function enter() {
    var g = canvasView.focusedFp !== "" ? Arrange.groupBy(groups, canvasView.focusedFp) : null;
    if (g && g.provisional === true && canSet) {
      draft = Arrange.withOrigin(draft, base, live ? live.revision : null, g.fp, g.origin);
      return true;
    }
    return root.apply();
  }

  // Esc: let go of a drag, then throw the draft away, and only then close the panel
  function cancelOrReset() {
    if (liveDrag !== null) {
      canvasView.glide();
      liveDrag = null;
      return true;
    }
    if (dirty) {
      root.reset();
      return true;
    }
    return false;
  }

  function undo() {
    if (!Arrange.canUndo(draft)) return;
    canvasView.glide();
    applyError = null;
    draft = Arrange.undo(draft);
  }

  function reset() {
    canvasView.glide();
    applyError = null;
    staleSeen = false;
    flashText = "";
    draft = Arrange.reset(draft, base);
  }

  // ------------------------------------------------------------------------ apply, conflict
  function apply() {
    if (!canApply) return false;
    applying = true;
    applyError = null;
    staleSeen = false;
    flashText = "";
    FerryClient.call("layout.set", Arrange.payload(draft, base), function (result, error) {
      root.applying = false;
      if (error) {
        root.applyError = error;
        if (error.code === "stale_layout") root.staleSeen = true;
        console.log("ferry arrange: layout.set refused: " + error.code + " " + error.message);
        return;
      }
      console.log("ferry arrange: applied revision " + (result && result.revision ? result.revision.counter : "?"));
      root.applied = result;
      root.draft = Arrange.emptyDraft();
      root.flash(Arrange.appliedText(result, Arrange.groupsOf(result, root.meFp).groups), "ok");
      FerryClient.refreshSoon();
    });
    return true;
  }

  // Keep mine: the same draft, on top of the revision that arrived (or on whatever is there, when
  // that has not arrived yet), applied now
  function keepMine() {
    draft = Arrange.rebase(draft, conflict !== null && live ? live.revision : null);
    staleSeen = false;
    applyError = null;
    apply();
  }

  // Take theirs: the draft goes, and the layout that arrived is drawn
  function takeTheirs() {
    canvasView.glide();
    draft = Arrange.emptyDraft();
    staleSeen = false;
    applyError = null;
    flashText = "";
  }

  onBaseChanged: if (live !== null) draft = Arrange.forgetMissing(draft, base)
  onBannerChanged: {
    if (banner === null) return;
    console.log("ferry arrange: " + banner.by + " changed the layout (rev " + banner.counter + ") over a draft");
    // The choice is the next thing a keyboard user has to make, so the focus goes to Keep mine (the
    // mockup does the same), and only when the keyboard was already in use here: a mouse user gets no
    // ring they did not ask for, and Tab and Shift+Tab walk to either button from wherever they are.
    Qt.callLater(function () {
      if (root.banner === null || !root.canvas.keyboardMode || !root.inside(root.Window.activeFocusItem)) return;
      (bannerView.keepEnabled ? bannerView.keepButton : bannerView.takeButton).forceActiveFocus();
    });
  }

  // The draft outlives a visit to another tab (the view is built again on the way back), not the panel:
  // it is picked up from FerryUi when this view is built, written back on every change, and dropped when
  // the view goes away because the panel closed.
  property bool restored: false
  onDraftChanged: if (restored) FerryUi.arrangeDraft = draft
  Component.onCompleted: {
    if (FerryUi.arrangeDraft !== null) draft = FerryUi.arrangeDraft;
    restored = true;
  }
  Component.onDestruction: if (!FerryUi.panelOpen) FerryUi.arrangeDraft = null

  Connections {
    target: FerryClient

    // the daemon's own snapshot has caught up with what we applied
    function onLayoutChanged() {
      var l = FerryClient.layout;
      if (root.applied !== null && l && l.revision && l.revision.counter >= root.applied.revision.counter) root.applied = null;
      // a layout that arrives over no draft is followed with a glide
      if (root.liveDrag === null && !root.dirty) canvasView.glide();
    }
  }

  // ---------------------------------------------------------------------- keys of the panel
  // Tab and Shift+Tab walk the panel's own controls: the banner's buttons, the machines, the zoom buttons
  // and then the buttons below. Qt's own Tab, in a window that is a stack of focus scopes (the card, its
  // Loader, this view), starts again from the top of the card (the tabs and the close button) whatever
  // has the focus (found in the real shell), so it is done here, from the item that
  // has the focus. At either end it lets the key go on, and the card's header is next.
  function inside(item) {
    for (var q = item; q; q = q.parent)
      if (q === root) return true;
    return false;
  }

  function step(forward) {
    var current = root.Window.activeFocusItem;
    var start = current && root.inside(current) ? current : root;
    var next = start.nextItemInFocusChain(forward);
    if (!next || next === root || !root.inside(next)) return false;
    canvasView.keyboardMode = true;
    next.forceActiveFocus(forward ? Qt.TabFocusReason : Qt.BacktabFocusReason);
    return true;
  }

  Keys.onTabPressed: function (event) { event.accepted = root.step(true); }
  Keys.onBacktabPressed: function (event) { event.accepted = root.step(false); }

  Keys.onPressed: function (event) {
    root.canvas.keyboardMode = true;
    if (event.modifiers & (Qt.AltModifier | Qt.MetaModifier)) return;
    if ((event.modifiers & Qt.ControlModifier) && event.key === Qt.Key_Z) { root.undo(); event.accepted = true; return; }
    if (event.modifiers & Qt.ControlModifier) return;
    if (event.key === Qt.Key_Plus || event.key === Qt.Key_Equal) { canvasView.zoomBy(1.4); event.accepted = true; }
    else if (event.key === Qt.Key_Minus || event.key === Qt.Key_Underscore) { canvasView.zoomBy(1 / 1.4); event.accepted = true; }
    else if (event.key === Qt.Key_0) { canvasView.fit(); event.accepted = true; }
  }
  Keys.onEscapePressed: function (event) { event.accepted = root.cancelOrReset(); }
  Keys.onReturnPressed: function (event) { event.accepted = root.enter(); }
  Keys.onEnterPressed: function (event) { event.accepted = root.enter(); }

  // ------------------------------------------------------------------------- for the harness
  // The same code a pointer or a key press runs, called with no pointer (offscreen there is none).
  function groupFp(name) {
    for (var i = 0; i < groups.length; i++) if (groups[i].name === name) return groups[i].fp;
    return "";
  }
  function simulateDragLive(fp, planeDx, planeDy, free) {
    dragBegan(fp);
    dragMoved(fp, planeDx, planeDy, free === true);
  }
  function simulateDragEnd() {
    if (liveDrag !== null) dragEnded(liveDrag.fp);
  }
  function simulateDrag(fp, planeDx, planeDy, free) {
    simulateDragLive(fp, planeDx, planeDy, free);
    simulateDragEnd();
  }
  function simulateKey(fp, dir, mode) {
    canvasView.focusGroup(fp);
    nudge(fp, dir, mode || "step");
  }
  function focusGroup(fp) {
    return canvasView.focusGroup(fp);
  }
  // as when the panel opens: no draft, no message, the view fitted to what is there
  function resetForTest() {
    liveDrag = null;
    draft = Arrange.emptyDraft();
    applied = null;
    applying = false;
    applyError = null;
    staleSeen = false;
    flashText = "";
    canvasView.keyboardMode = false;
    canvasView.fit();
  }
  // Where things are, in this item's coordinates, for a script that drives a real pointer: the centre of
  // every group's tile area, the canvas, and the buttons.
  function geometry() {
    function box(item) {
      var p = item.mapToItem(root, 0, 0);
      return { x: Math.round(p.x), y: Math.round(p.y), w: Math.round(item.width), h: Math.round(item.height) };
    }
    var out = { groups: {}, canvas: box(canvasView), apply: box(applyButton), reset: box(resetButton), undo: box(undoButton) };
    for (var i = 0; i < groups.length; i++) {
      var item = canvasView.groupItem(groups[i].fp);
      if (!item) continue;
      var c = item.mapToItem(root, item.width / 2, item.height / 2);
      out.groups[groups[i].name] = { x: Math.round(c.x), y: Math.round(c.y), w: Math.round(item.vis.w), h: Math.round(item.vis.h) };
    }
    return out;
  }

  // The same, in the window's coordinates: a layer-shell panel covers the screen from its corner, so these
  // are screen pixels. Said once when the panel has settled, in the shell's log: it is what
  // scripts/m3/vm-plugin.sh aims a real pointer with, and a line a bug report can carry.
  Timer {
    interval: 500
    running: root.mode === "arrange"
    onTriggered: {
      var o = root.mapToItem(null, 0, 0);
      var g = root.geometry();
      function shift(b) { return { x: b.x + Math.round(o.x), y: b.y + Math.round(o.y), w: b.w, h: b.h }; }
      var w = { groups: {}, canvas: shift(g.canvas), apply: shift(g.apply), reset: shift(g.reset), undo: shift(g.undo), scale: canvasView.view.scale };
      for (var name in g.groups) w.groups[name] = { x: g.groups[name].x + Math.round(o.x), y: g.groups[name].y + Math.round(o.y), w: g.groups[name].w, h: g.groups[name].h };
      console.log("ferry arrange: opened " + JSON.stringify(w));
    }
  }

  // ---------------------------------------------------------------------------------- layout
  Column {
    id: top
    anchors.top: parent.top
    anchors.left: parent.left
    anchors.right: parent.right
    spacing: Style.space(8)

    Hero {
      glyph: Glyphs.ui("grid")
      title: Strings.t("panel.arrange.title")
      meta: Strings.t("panel.arrange.meta")
      // no layout number here: only whether this draft differs from the live layout
      detail: root.mode === "arrange" && root.dirty ? Strings.t("arrange.edited") : ""
      detailTone: root.banner !== null ? "urgent" : (root.dirty ? "accent" : "normal")
      foreground: root.fg
      fontFamily: root.fontFamily
    }

    Text {
      visible: root.mode === "arrange" && root.banner === null
      width: parent.width
      textFormat: Text.PlainText
      text: root.canSet ? Strings.t("arrange.hint") : Strings.t("arrange.legacy")
      color: root.dim
      font.family: root.fontFamily
      font.pixelSize: Style.font.bodySmall
      wrapMode: Text.WordWrap
      lineHeight: 1.15
    }

    ConflictBanner {
      id: bannerView
      visible: root.mode === "arrange" && root.banner !== null
      title: root.banner !== null ? Arrange.bannerTitle(root.banner) : ""
      body: root.banner !== null ? Arrange.bannerBody(root.banner) : ""
      keepEnabled: !root.overlapping && !root.applying
      foreground: root.fg
      fontFamily: root.fontFamily
      onKeepRequested: root.keepMine()
      onTakeRequested: root.takeTheirs()
    }
  }

  // ---- the stage between them: the canvas, or the reason there is none
  Item {
    id: stage
    anchors.top: top.bottom
    anchors.topMargin: Style.space(10)
    anchors.left: parent.left
    anchors.right: parent.right
    anchors.bottom: root.mode === "arrange" ? bottom.top : parent.bottom
    anchors.bottomMargin: root.mode === "arrange" ? Style.space(10) : 0

    ArrangeCanvas {
      id: canvasView
      anchors.fill: parent
      visible: root.mode === "arrange"
      groups: root.groups
      analysis: root.analysis
      liveDrag: root.liveDrag
      interactive: root.canSet && !root.applying
      meName: FerryClient.meName

      onDragBegan: function (fp) { root.dragBegan(fp); }
      onDragMoved: function (fp, dx, dy, free) { root.dragMoved(fp, dx, dy, free); }
      onDragEnded: function (fp) { root.dragEnded(fp); }
      onNudgeRequested: function (fp, dir, mode) { root.nudge(fp, dir, mode); }
    }

    ArrangeEmpty {
      anchors.fill: parent
      visible: root.mode === "empty" || root.mode === "nomonitors"
      group: root.groups.length === 1 ? root.groups[0] : null
      title: root.mode === "nomonitors" ? Strings.t("arrange.nomonitors.title") : Strings.t("arrange.empty.title")
      body: root.mode === "nomonitors" ? Strings.t("arrange.nomonitors.body") : Strings.t("arrange.empty.body")
      note: root.parsed.waiting.length === 0 ? ""
        : Strings.t(root.parsed.waiting.length > 1 ? "arrange.waiting.many" : "arrange.waiting",
          [Arrange.joinNames(root.parsed.waiting.map(function (g) { return g.name; }))])
      showButton: root.mode === "empty" && root.parsed.waiting.length === 0
      foreground: root.fg
      fontFamily: root.fontFamily
      onPairRequested: root.viewRequested("pair")
    }

    OffCard {
      visible: root.mode === "off"
      anchors.centerIn: parent
      width: Math.min(parent.width - Style.space(40), Style.space(420))
      foreground: root.fg
      fontFamily: root.fontFamily
    }

    Text {
      visible: root.mode === "loading"
      anchors.centerIn: parent
      textFormat: Text.PlainText
      text: Strings.t("arrange.loading")
      color: root.dim
      font.family: root.fontFamily
      font.pixelSize: Style.font.body
    }
  }

  Column {
    id: bottom
    visible: root.mode === "arrange"
    anchors.bottom: parent.bottom
    anchors.left: parent.left
    anchors.right: parent.right
    spacing: Style.space(10)

    // ---- edge links, and the status
    Item {
      id: meta
      width: parent.width
      height: Math.max(linksColumn.implicitHeight, statusColumn.implicitHeight)

      Column {
        id: linksColumn
        anchors.left: parent.left
        anchors.top: parent.top
        width: Math.round(parent.width * 0.6)
        spacing: Style.space(3)

        Item {
          width: parent.width
          height: linksHeader.implicitHeight

          PanelSectionHeader {
            id: linksHeader
            anchors.left: parent.left
            text: Strings.t("arrange.links")
            foreground: root.fg
            fontFamily: root.fontFamily
          }
        }

        Repeater {
          model: root.rows.slice(0, 3)

          Row {
            id: linkRow
            required property var modelData
            width: linksColumn.width
            spacing: Style.space(6)
            clip: true

            Rectangle {
              anchors.verticalCenter: parent.verticalCenter
              width: Style.space(10)
              height: Math.max(3, Style.space(3))
              radius: height / 2
              color: linkRow.modelData.kind === "link" ? Commons.Color.accent : Util.alpha(Commons.Color.accent, 0.4)
            }

            Text {
              id: linkPrimary
              anchors.verticalCenter: parent.verticalCenter
              width: Math.min(implicitWidth, linkRow.width - Style.space(16))
              textFormat: Text.PlainText
              text: linkRow.modelData.primary
              color: root.fg
              font.family: root.fontFamily
              font.pixelSize: Style.font.bodySmall
              elide: Text.ElideRight
            }

            Text {
              anchors.verticalCenter: parent.verticalCenter
              width: Math.max(0, linkRow.width - Style.space(10) - linkPrimary.width - linkRow.spacing * 2)
              textFormat: Text.PlainText
              text: linkRow.modelData.secondary + (linkRow.modelData.kind === "dormant" ? " · " + Strings.t("arrange.link.dormant") : "")
              color: root.dim
              font.family: root.fontFamily
              font.pixelSize: Style.font.caption
              elide: Text.ElideRight
            }
          }
        }

        Text {
          visible: root.rows.length === 0
          textFormat: Text.PlainText
          text: Strings.t("arrange.links.none")
          color: root.dim
          font.family: root.fontFamily
          font.pixelSize: Style.font.bodySmall
        }

        Text {
          visible: root.rows.length > 3
          textFormat: Text.PlainText
          text: Strings.t("arrange.links.more", [root.rows.length - 3])
          color: root.dim
          font.family: root.fontFamily
          font.pixelSize: Style.font.caption
        }
      }

      Column {
        id: statusColumn
        anchors.right: parent.right
        anchors.top: parent.top
        width: parent.width - linksColumn.width - Style.space(14)
        spacing: Style.space(3)

        PanelSectionHeader {
          text: Strings.t("arrange.status")
          foreground: root.fg
          fontFamily: root.fontFamily
        }

        BorderSurface {
          width: parent.width
          implicitHeight: statusRow.implicitHeight + Style.space(14)
          height: implicitHeight
          radius: Style.cornerRadius
          color: root.status.tone === "err" ? Util.alpha(Commons.Color.urgent, 0.07) : "transparent"
          // red only for what is wrong (overlap, a refusal); a warning (lonely, unplaced, untrusted)
          // takes the accent, with the alert glyph (#28)
          borderSpec: Border.flat(root.status.tone === "err" ? Util.alpha(Commons.Color.urgent, 0.6)
            : (root.status.tone === "warn" ? Util.alpha(Commons.Color.accent, 0.6)
              : (root.status.tone === "ok" ? Util.alpha(Commons.Color.accent, 0.6) : Util.alpha(root.fg, 0.15))), 1)

          Accessible.role: root.status.tone === "err" || root.status.tone === "warn" ? Accessible.AlertMessage : Accessible.StaticText
          Accessible.name: root.status.text

          Row {
            id: statusRow
            anchors.left: parent.left
            anchors.right: parent.right
            anchors.verticalCenter: parent.verticalCenter
            anchors.margins: Style.space(8)
            spacing: Style.space(6)

            // a glyph for every tone: a problem is never colour alone
            Text {
              textFormat: Text.PlainText
              text: Glyphs.ui(root.status.glyph === "alert" ? "alert" : (root.status.glyph === "check" ? "check" : "info"))
              color: root.status.tone === "err" ? Commons.Color.urgent
                : (root.status.tone === "ok" || root.status.tone === "warn" ? Commons.Color.accent : root.dim)
              font.family: root.fontFamily
              font.pixelSize: Style.font.icon
            }

            Text {
              width: parent.width - Style.space(24)
              textFormat: Text.PlainText
              text: root.status.text
              color: root.status.tone === "err" ? Commons.Color.urgent : root.fg
              font.family: root.fontFamily
              font.pixelSize: Style.font.bodySmall
              wrapMode: Text.WordWrap
              lineHeight: 1.1
            }
          }
        }
      }
    }

    // ---- undo, reset, apply
    Item {
      id: footer
      width: parent.width
      height: Math.max(applyButton.implicitHeight, undoButton.implicitHeight)

      Button {
        id: undoButton
        anchors.left: parent.left
        anchors.verticalCenter: parent.verticalCenter
        text: Strings.t("arrange.undo")
        iconText: Glyphs.ui("undo")
        bordered: true
        focusable: true
        enabled: Arrange.canUndo(root.draft) && !root.applying
        // nothing to undo or discard on a clean layout: the row then holds only Apply
        visible: root.dirty
        opacity: enabled ? 1 : 0.45
        foreground: root.fg
        fontFamily: root.fontFamily
        fontSize: Style.font.bodySmall
        Accessible.name: text
        onClicked: root.undo()
        FocusRing {}
      }

      Button {
        id: resetButton
        anchors.left: undoButton.right
        anchors.leftMargin: Style.space(8)
        anchors.verticalCenter: parent.verticalCenter
        text: Strings.t("arrange.reset", [root.rev])
        iconText: Glyphs.ui("refresh")
        bordered: true
        focusable: true
        enabled: root.dirty && !root.applying
        visible: root.dirty
        opacity: enabled ? 1 : 0.45
        foreground: root.fg
        fontFamily: root.fontFamily
        fontSize: Style.font.bodySmall
        Accessible.name: text
        onClicked: root.reset()
        FocusRing {}
      }

      Text {
        anchors.left: resetButton.right
        anchors.leftMargin: Style.space(12)
        anchors.right: applyButton.left
        anchors.rightMargin: Style.space(12)
        anchors.verticalCenter: parent.verticalCenter
        textFormat: Text.PlainText
        text: root.others.length > 0
          ? Strings.t("arrange.sync", [Arrange.joinNames(root.others.map(function (g) { return g.name; }))])
          : Strings.t("arrange.sync.alone")
        color: root.dim
        font.family: root.fontFamily
        font.pixelSize: Style.font.caption
        wrapMode: Text.WordWrap
        maximumLineCount: 2
        elide: Text.ElideRight
      }

      Button {
        id: applyButton
        anchors.right: parent.right
        anchors.verticalCenter: parent.verticalCenter
        text: root.applying ? Strings.t("arrange.apply.busy") : Strings.t("arrange.apply")
        iconText: Glyphs.ui("check")
        bordered: true
        active: true
        focusable: true
        enabled: root.canApply
        opacity: enabled ? 1 : 0.45
        foreground: root.fg
        fontFamily: root.fontFamily
        fontSize: Style.font.bodySmall
        Accessible.name: text
        onClicked: root.apply()
        FocusRing {}
      }
    }
  }
}
