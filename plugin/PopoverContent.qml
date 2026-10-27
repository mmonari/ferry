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

// The body of the popover (-05, -10): where the pointer is and what ends it, a pairing that
// is running, what needs the user, the trusted peers with their latency (and rename, unpair and
// Arrange behind a press), the lock, Release now, the way into Arrange, Pair and Settings, and what
// ferryd said lately. It owns no window: Popover.qml puts it in the shell's KeyboardPanel, and the dev
// harness (scripts/m3/harness) puts it in a plain card, so the same file is what gets photographed.
//
// Keyboard: Tab and Shift+Tab walk the controls, as do the arrow keys and j/k; Enter and Space press, and
// Escape closes (an open row, a rename or a question closes first). Nothing shows a focus ring until a
// key is pressed, and the control that has it is always scrolled into view.
FocusScope {
  id: root

  // False while the popover is closed, so a closed popover costs no timers.
  property bool shown: true
  signal closeRequested()

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
  // the clipboard stream is not built yet: its switch is shown off and cannot be pressed
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

  // every row closed (the next picture starts from a plain popover)
  function collapseAll() {
    root.keyboardNav = false;
    for (var i = 0; i < peerRepeater.count; i++) {
      var item = peerRepeater.itemAt(i);
      if (item) item.close(false);
    }
    root.openPeer = "";
  }

  function peerItem(fp) {
    for (var i = 0; i < peerRepeater.count; i++) {
      var item = peerRepeater.itemAt(i);
      if (item && item.row.fingerprint === fp) return item;
    }
    return null;
  }

  implicitWidth: Style.space(340)
  // The natural height. The host may give less, and the content then scrolls.
  implicitHeight: column.implicitHeight
  focus: true

  // Nothing is highlighted when the popover opens, whatever had the focus when it closed: the
  // scope hands its focus to this item on close, and a key press then moves on from here.
  Item { id: rest }
  onShownChanged: if (!shown) rest.focus = true

  // "seen 5 min ago" has to move on while the popover stays open, and a pairing counts seconds.
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

  function openPanel(view) {
    FerryUi.openPanel(view);
    root.closeRequested();
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
        glyph: Glyphs.state(root.hero.state)
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
            onArrangeRequested: root.openPanel("arrange")
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

        Toggle {
          id: lockToggle
          width: parent.width
          label: Strings.t("lock.label")
          description: root.hero.state === "off" ? "" : Strings.t("lock.description")
          checked: FerryClient.locked
          foreground: root.fg
          fontFamily: root.fontFamily
          Accessible.name: label
          onClicked: FerryClient.setLock(!FerryClient.locked)
          FocusRing {}
        }

        Toggle {
          id: clipboardToggle
          visible: root.canConfig
          width: parent.width
          label: Strings.t("clipboard.label")
          description: Strings.t("clipboard.description") + (root.clipboardAvailable ? "" : " " + Strings.t("unavailable.note"))
          enabled: root.clipboardAvailable
          opacity: root.clipboardAvailable ? 1 : 0.6
          checked: root.clipboardAvailable && root.canConfig && FerryClient.config["clipboard.enabled"] === true
          foreground: root.fg
          fontFamily: root.fontFamily
          Accessible.name: label
          onClicked: FerryClient.setConfig("clipboard.enabled", !checked)
          FocusRing {}
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

        Row {
          width: parent.width
          spacing: Style.space(8)

          Button {
            id: arrangeButton
            width: (parent.width - parent.spacing) / 2
            text: Strings.t("action.arrange")
            iconText: Glyphs.ui("grid")
            bordered: true
            focusable: true
            foreground: root.fg
            fontFamily: root.fontFamily
            fontSize: Style.font.bodySmall
            Accessible.name: text
            onClicked: root.openPanel("arrange")
            FocusRing {}
          }

          Button {
            id: pairButton
            width: (parent.width - parent.spacing) / 2
            text: Strings.t("action.pair")
            iconText: Glyphs.ui("plus")
            bordered: true
            focusable: true
            foreground: root.fg
            fontFamily: root.fontFamily
            fontSize: Style.font.bodySmall
            Accessible.name: text
            onClicked: root.openPanel("pair")
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

          Row {
            spacing: Style.space(8)

            Text {
              anchors.verticalCenter: parent.verticalCenter
              textFormat: Text.PlainText
              text: Strings.t("escape.hint.lead")
              color: root.dim
              font.family: root.fontFamily
              font.pixelSize: Style.font.caption
            }

            Keycaps {
              anchors.verticalCenter: parent.verticalCenter
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

      PanelSeparator {
        visible: root.connected
        foreground: root.fg
      }

      // ---- footer
      Item {
        visible: root.connected
        width: parent.width
        height: Math.max(settingsButton.implicitHeight, versionText.implicitHeight)

        Button {
          id: settingsButton
          anchors.left: parent.left
          anchors.verticalCenter: parent.verticalCenter
          text: Strings.t("action.settings")
          iconText: Glyphs.ui("cog")
          // flat, and no wider inset than the machines above it
          horizontalPadding: Style.space(4)
          focusable: true
          foreground: root.fg
          fontFamily: root.fontFamily
          fontSize: Style.font.bodySmall
          Accessible.name: text
          onClicked: root.openPanel("settings")
          FocusRing {}
        }

        Text {
          id: versionText
          anchors.right: parent.right
          anchors.verticalCenter: parent.verticalCenter
          textFormat: Text.PlainText
          text: Strings.t("footer.version", [FerryClient.version])
          color: root.dim
          font.family: root.fontFamily
          font.pixelSize: Style.font.caption
        }
      }
    }
  }
}
