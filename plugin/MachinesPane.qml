import QtQuick
import QtQuick.Controls
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import "."
import "views"
import "Glyphs.js" as Glyphs
import "Model.js" as Model
import "Strings.js" as Strings

// The right-hand pane of the Screens tab (-05, -10): where the pointer is and what ends it,
// a pairing that is running, what needs the user, the trusted machines with their latency (rename and
// unpair behind a press, or a click on the machine in the layout), clipboard sharing, Release now,
// the escape chord, and what ferryd said lately. It owns no window: ArrangePanel puts it beside the
// canvas, and the dev harness can draw it alone.
//
// Keyboard: Tab and Shift+Tab walk the controls, as do the arrow keys and j/k; Enter and Space press, and
// Escape closes (an open row, a rename or a question closes first). Nothing shows a focus ring until a
// key is pressed, and the control that has it is always scrolled into view.
FocusScope {
  id: root

  // False while the panel is closed, so a closed panel costs no timers.
  property bool shown: true
  signal closeRequested()
  // the Screens tab's canvas has room for the layout: the pane does not repeat what the canvas shows
  property bool besideCanvas: true

  readonly property string fontFamily: Style.font.family
  readonly property color fg: Commons.Color.popups.text
  readonly property color dim: Qt.darker(fg, 1.4)

  readonly property bool connected: FerryClient.connected
  readonly property string state: FerryClient.state
  readonly property bool travelling: state === "away" || state === "controlled"

  readonly property var hero: Model.hero({
    connected: FerryClient.connected,
    status: FerryClient.status,
    layout: FerryClient.layout,
    summary: FerryClient.summary,
    attention: FerryClient.attention,
    hello: FerryClient.hello,
    chord: FerryClient.escapeChord
  })
  readonly property var rows: Model.peerRows(FerryClient.status, FerryClient.peers, FerryClient.layout, FerryClient.summary, clock.now)
  readonly property var attentionItems: Model.attentionActions(FerryClient.attention)
  readonly property var pairing: Model.pairFlow({
    event: FerryClient.pairing,
    at: FerryClient.pairingAt,
    status: FerryClient.status,
    now: clock.now
  })
  readonly property var banner: Model.pairBanner(pairing)
  readonly property var activity: Model.activityItems(FerryClient.notices, clock.now, 5, FerryClient.attention)
  readonly property var chordKeys: Model.chordKeys(FerryClient.escapeChord)
  readonly property bool canConfig: FerryClient.hasMethod("config.set") && FerryClient.config !== null
  // a ferryd without clipboard sharing marks the key unavailable: its switch is shown off and cannot be pressed
  readonly property bool clipboardAvailable: Model.keyAvailable(FerryClient.configSchema, "clipboard.enabled")
    && FerryClient.config !== null && FerryClient.config["clipboard.enabled"] !== undefined
  readonly property bool canRename: FerryClient.hasMethod("peer.rename")

  // True from the first key pressed until the next click: only then does a control show its focus ring.
  property bool keyboardNav: false
  // The open row, if any: one at a time, so the popover stays short.
  property string openPeer: ""
  // The peers as keys, which change only when the set of peers or their order does: a Repeater that
  // were handed a new array every two seconds would rebuild every row, and lose the focus and the
  // half-typed name of the one the user is in.
  property var peerKeys: []

  function syncPeerKeys() {
    var next = rows.map(function (r) { return r.fingerprint; });
    if (!Model.sameList(next, peerKeys)) peerKeys = next;
  }
  onRowsChanged: syncPeerKeys()
  Component.onCompleted: syncPeerKeys()

  function rowFor(fp) {
    for (var i = 0; i < rows.length; i++) if (rows[i].fingerprint === fp) return rows[i];
    return ({});
  }

  // every row closed (the next picture starts from a plain pane)
  function collapseAll() {
    root.keyboardNav = false;
    for (var i = 0; i < peerRepeater.count; i++) {
      var item = peerRepeater.itemAt(i);
      if (item) item.close(false);
    }
    root.openPeer = "";
  }

  // a machine picked on the canvas: its row opens, and the pane scrolls to it; this machine, or a
  // machine that is not trusted, closes whatever was open
  function selectPeer(fp) {
    var item = fp !== "" ? root.peerItem(fp) : null;
    if (!item) {
      if (root.openPeer !== "") { var was = root.peerItem(root.openPeer); if (was) was.close(false); }
      root.openPeer = "";
      return;
    }
    root.openPeer = fp;
    if (!item.open) item.openActions();
    revealItem.target = item;
    revealItem.restart();
  }
  Timer {
    id: revealItem
    property Item target: null
    interval: 0
    onTriggered: if (target) root.ensureVisible(target)
  }

  function peerItem(fp) {
    for (var i = 0; i < peerRepeater.count; i++) {
      var item = peerRepeater.itemAt(i);
      if (item && item.row.fingerprint === fp) return item;
    }
    return null;
  }

  implicitWidth: Style.space(300)
  // The natural height. The host may give less, and the content then scrolls.
  implicitHeight: column.implicitHeight
  focus: true

  // Nothing is highlighted when the panel opens, whatever had the focus when it closed: the
  // scope hands its focus to this item on close, and a key press then moves on from here.
  Item { id: rest }
  onShownChanged: if (!shown) rest.focus = true

  // "seen 5 min ago" has to move on while the panel stays open, and a pairing counts seconds.
  QtObject {
    id: clock
    property real now: Date.now()
  }
  Timer {
    interval: root.banner !== null ? 1000 : 20000
    repeat: true
    running: root.shown
    onTriggered: clock.now = Date.now()
  }

  // ---------------------------------------------------------------------------------- keys
  function inside(item) {
    for (var p = item; p; p = p.parent)
      if (p === root) return true;
    return false;
  }

  function step(forward) {
    root.keyboardNav = true;
    var current = root.Window.activeFocusItem;
    var start = current && root.inside(current) ? current : root;
    var next = start.nextItemInFocusChain(forward);
    if (next && next !== root && root.inside(next))
      next.forceActiveFocus(forward ? Qt.TabFocusReason : Qt.BacktabFocusReason);
  }

  Keys.onEscapePressed: root.closeRequested()
  Keys.onPressed: function (event) {
    root.keyboardNav = true;
    if (event.modifiers & (Qt.ControlModifier | Qt.AltModifier | Qt.MetaModifier)) return;
    if (event.key === Qt.Key_Down || event.text === "j") { root.step(true); event.accepted = true; }
    else if (event.key === Qt.Key_Up || event.text === "k") { root.step(false); event.accepted = true; }
  }

  // The control that has the keyboard is never out of sight: the content scrolls to it.
  function ensureVisible(item) {
    if (!item || !root.inside(item) || item === root) return;
    var top = item.mapToItem(column, 0, 0).y - Style.space(8);
    var bottom = item.mapToItem(column, 0, item.height).y + Style.space(8);
    if (top < scroller.contentY) scroller.contentY = Math.max(0, top);
    else if (bottom > scroller.contentY + scroller.height)
      scroller.contentY = Math.max(0, Math.min(scroller.contentHeight - scroller.height, bottom - scroller.height));
  }

  Connections {
    target: root.Window
    function onActiveFocusItemChanged() { root.ensureVisible(root.Window.activeFocusItem); }
  }

  // a row that opens grows the content after the focus has moved into it: look again once the layout has caught up
  Connections {
    target: scroller
    function onContentHeightChanged() { revealLater.restart(); }
  }

  // a Timer and not Qt.callLater: it dies with the item, and a delayed function that outlives it throws
  Timer {
    id: revealLater
    interval: 0
    onTriggered: root.ensureVisible(root.Window.activeFocusItem)
  }

  // another tab of the same panel
  function openPanel(view) {
    if (view === "arrange" || view === "screens") return;
    FerryUi.openPanel(view);
  }

  // ----------------------------------------------------------------------- what a row asks for
  function renamePeer(fp, name) {
    FerryClient.renamePeer(fp, name, function (result, error) {
      var item = root.peerItem(fp);
      if (item) item.finishRename(error ? Model.errorText(error).message : "");
    });
  }

  function unpairPeer(fp) {
    var before = root.peerItem(fp);
    var again = before ? before.thenPair : false;
    FerryClient.unpair(fp, function (result, error) {
      var item = root.peerItem(fp);
      if (item) item.finishUnpair(error ? Model.errorText(error).message : "");
      // a changed key is fixed by unpairing and pairing again: go straight on to Pair
      if (!error && again) root.openPanel("pair");
    });
  }

  // From an attention card: the peer's own row asks, so the question is where the peer is.
  function askUnpair(name, again) {
    for (var i = 0; i < rows.length; i++) {
      if (rows[i].name !== name) continue;
      var item = root.peerItem(rows[i].fingerprint);
      if (!item) return;
      root.openPeer = rows[i].fingerprint;
      item.askUnpair(again);
      return;
    }
  }

  // a click ends keyboard mode; this handler only watches, and takes nothing from the controls
  PointHandler {
    acceptedButtons: Qt.AllButtons
    onActiveChanged: if (active) root.keyboardNav = false
  }

  // -------------------------------------------------------------------------------- content
  Flickable {
    id: scroller
    anchors.fill: parent
    contentWidth: width
    contentHeight: column.implicitHeight
    clip: true
    boundsBehavior: Flickable.StopAtBounds
    interactive: column.implicitHeight > height

    // a list cut off by the window says so: the bar is there for as long as there is more below
    ScrollBar.vertical: PanelScrollBar { flickable: scroller }

    Behavior on contentY {
      enabled: !scroller.moving && !scroller.dragging
      NumberAnimation { duration: Fx.ms(140); easing.type: Easing.OutCubic }
    }

    Column {
      id: column
      width: scroller.width
      spacing: Style.space(12)

      Hero {
        glyph: Glyphs.travel(root.hero.state, root.hero.side)
        title: root.hero.title
        meta: root.hero.meta
        detail: root.hero.detail
        tone: root.hero.tone
        detailTone: root.hero.detailTone
        chord: root.hero.chord
        chordTail: root.hero.chordTail
        foreground: root.fg
        fontFamily: root.fontFamily
      }

      // ---- a pairing is running (a request that arrives while the popover is open is seen)
      Reveal {
        width: parent.width
        shown: root.connected && root.banner !== null

        PairBanner {
          banner: root.banner || ({ text: "", urgent: false, clock: "", label: "" })
          foreground: root.fg
          fontFamily: root.fontFamily
          onOpenRequested: root.openPanel("pair")
        }
      }

      // ---- ferryd is not running
      OffCard {
        visible: !root.connected
        foreground: root.fg
        fontFamily: root.fontFamily
      }

      // ---- nothing is paired yet
      FirstRunCard {
        id: firstRun
        visible: root.connected && FerryClient.firstRun
        showButton: !root.besideCanvas
        chord: root.chordKeys
        foreground: root.fg
        fontFamily: root.fontFamily
        onPairRequested: root.openPanel("pair")
      }

      // ---- what ferryd says needs the user
      Reveal {
        width: parent.width
        shown: root.connected && root.attentionItems.length > 0

        AttentionCard {
          items: root.attentionItems
          foreground: root.fg
          fontFamily: root.fontFamily
          onUnpairRequested: function (peer, again) { root.askUnpair(peer, again); }
          onPanelRequested: function (view) { root.openPanel(view); }
        }
      }

      // ---- the trusted peers
      Column {
        id: peerSection
        visible: root.connected && !FerryClient.firstRun
        width: parent.width
        spacing: Style.space(2)

        PanelSectionHeader {
          text: Strings.t("section.peers")
          foreground: root.fg
          fontFamily: root.fontFamily
        }

        Repeater {
          id: peerRepeater
          model: root.peerKeys

          PeerRow {
            id: peerRow
            required property string modelData
            row: root.rowFor(modelData)
            foreground: root.fg
            fontFamily: root.fontFamily
            canRename: root.canRename
            meName: FerryClient.meName
            others: root.rows.map(function (r) { return { name: r.name, fingerprint: r.fingerprint }; })

            onToggled: root.openPeer = modelData
            canArrange: false
            onRenameSubmitted: function (name) { root.renamePeer(modelData, name); }
            onUnpairConfirmed: root.unpairPeer(modelData)

            // one row open at a time
            Connections {
              target: root
              function onOpenPeerChanged() {
                if (root.openPeer !== peerRow.modelData && peerRow.open) peerRow.close(false);
              }
            }
          }
        }
      }

      PanelSeparator {
        visible: root.connected && !FerryClient.firstRun
        foreground: root.fg
      }

      // ---- the controls
      Column {
        id: controls
        visible: root.connected && !FerryClient.firstRun
        width: parent.width
        spacing: Style.space(8)

        SwitchRow {
          id: clipboardToggle
          visible: root.canConfig
          width: parent.width
          label: Strings.t("clipboard.label")
          description: root.clipboardAvailable ? "" : Strings.t("unavailable.note")
          enabled: root.clipboardAvailable
          checked: root.clipboardAvailable && root.canConfig && FerryClient.config["clipboard.enabled"] === true
          foreground: root.fg
          fontFamily: root.fontFamily
          onClicked: FerryClient.setConfig("clipboard.enabled", !checked)
        }

        // only while input is away or controlled: there is nothing to release otherwise
        Reveal {
          width: parent.width
          shown: root.travelling

          Button {
            id: releaseButton
            width: parent.width
            text: Strings.t("action.release")
            iconText: Glyphs.ui("eject")
            tooltipText: Strings.t("action.release.tip")
            bordered: true
            active: true
            focusable: true
            foreground: root.fg
            fontFamily: root.fontFamily
            Accessible.name: text
            onClicked: FerryClient.release()
            FocusRing {}
          }
        }
      }

      // ---- the one thing to know when something goes wrong; while input travels the hero says it
      Reveal {
        width: parent.width
        shown: root.connected && !FerryClient.firstRun && !root.travelling

        Column {
          width: parent.width
          spacing: Style.space(4)

          // wraps: the pane is narrower than the sentence and its keys
          Flow {
            width: parent.width
            spacing: Style.space(8)

            Text {
              height: chordCaps.height
              verticalAlignment: Text.AlignVCenter
              textFormat: Text.PlainText
              text: Strings.t("escape.hint.lead")
              color: root.dim
              font.family: root.fontFamily
              font.pixelSize: Style.font.caption
            }

            Keycaps {
              id: chordCaps
              keys: root.chordKeys
              foreground: root.fg
              fontFamily: root.fontFamily
            }
          }

          Text {
            width: parent.width
            textFormat: Text.PlainText
            text: Strings.t("escape.hint.tail")
            color: root.dim
            font.family: root.fontFamily
            font.pixelSize: Style.font.caption
            wrapMode: Text.WordWrap
            lineHeight: 1.2
          }
        }
      }

      // ---- what ferryd said lately
      Reveal {
        width: parent.width
        shown: root.connected && !FerryClient.firstRun && root.activity.length > 0

        ActivityList {
          items: root.activity
          foreground: root.fg
          fontFamily: root.fontFamily
        }
      }
    }
  }
}
