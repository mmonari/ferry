import QtQuick
import QtQuick.Controls
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import ".."
import "../views"
import "../Glyphs.js" as Glyphs
import "../Model.js" as Model
import "../Strings.js" as Strings

// The Pair panel (FR-PAIR-*, U7). Two ways to start, one flow after that:
//
//   Pair with a machine         pair.start: this side dials, the other machine is waiting
//   Accept a pairing request    pair.accept: this side waits, the other machine dials
//   then                        connecting -> the code -> confirm on both -> paired, or why not
//
// Everything on screen is Model.pairFlow() of the last `pairing` event and a few things this panel
// remembers (what the user asked, that they pressed Cancel, the last attempt that failed), so a panel
// opened by the "Pairing code" notification lands on the code, one opened later lands on its start,
// and the code that every connection hears can be answered from here, from a terminal or from the
// notification, whichever comes first. Below the flow: the machines this one trusts, with
// Rename and Unpair behind a question.
//
// The functions a button calls (dial, accept, answer, cancel, retry, dismiss) are public: the dev
// harness and the integration test press the same ones.
FocusScope {
  id: root

  readonly property color fg: Commons.Color.popups.text
  readonly property color dim: Qt.darker(fg, 1.4)
  readonly property string fontFamily: Style.font.family

  // True from the first key pressed until the next click: only then does a control show its focus ring.
  property bool keyboardNav: false

  // ------------------------------------------------------------------ what the panel remembers
  property real openedAt: Date.now()
  // what the user asked for here: "dial" | "accept" | ""
  property string intent: ""
  property string dialAddr: ""
  // { role, addr } between pair.start and its answer
  property var starting: null
  property real cancelAt: 0
  property real dismissedAt: 0
  property real ignoreBefore: 0
  // the last attempt that failed, for a window that stays open
  property var failure: null
  property string dialError: ""
  property string dialHint: ""
  property string acceptError: ""
  property string answerError: ""
  property string notice: ""
  property var idInfo: null
  property var nearby: []

  readonly property var event: FerryClient.pairing
  readonly property bool cancelRequested: cancelAt > 0 && FerryClient.pairingAt >= cancelAt && FerryClient.pairingAt - cancelAt < 5000

  readonly property var rows: Model.peerRows(FerryClient.status, FerryClient.peers, FerryClient.layout, FerryClient.summary, clock.now)
  readonly property var untrusted: Model.untrustedPeers(FerryClient.layout)
  readonly property var selfAddr: Model.selfAddress(idInfo)
  readonly property bool canRename: FerryClient.hasMethod("peer.rename")
  readonly property bool canDiscover: FerryClient.hasMethod("discover.list")

  readonly property var flow: Model.pairFlow({
    event: FerryClient.pairing,
    at: FerryClient.pairingAt,
    status: FerryClient.status,
    now: clock.now,
    openedAt: root.openedAt,
    dismissedAt: root.dismissedAt,
    ignoreBefore: root.ignoreBefore,
    starting: root.starting,
    intent: root.intent,
    addr: root.dialAddr,
    cancelRequested: root.cancelRequested,
    failure: root.failure,
    layout: FerryClient.layout
  })
  // for the dev harness and the integration test, which press the same controls
  property alias cancelButton: cancelButton
  property alias matchButton: matchButton
  property alias differButton: differButton
  property alias arrangeButton: arrangeButton
  property alias retryButton: retryButton
  readonly property var screenItem: stage.item
  readonly property string screen: flow.screen
  readonly property bool live: flow.active
  // Cancel has something to stop: a pairing this panel can cancel, or a dial that has not been answered yet
  readonly property bool stoppable: flow.canCancel || starting !== null
  readonly property string me: FerryClient.meName || Strings.t("me.fallback")

  // ---------------------------------------------------------------------------------- the clock
  QtObject {
    id: clock
    property real now: Date.now()
  }
  // a pairing counts seconds; the trusted list only ages
  Timer {
    interval: root.live || root.starting !== null ? 1000 : 20000
    repeat: true
    running: true
    onTriggered: clock.now = Date.now()
  }

  // ------------------------------------------------------------------------------- what it does
  // Dial the machine at `text`. False when the address is not one.
  function dial(text) {
    var check = Model.addrCheck(text);
    if (!check.ok) {
      root.dialError = check.error;
      root.dialHint = "";
      return false;
    }
    root.dialError = "";
    root.dialHint = "";
    root.notice = "";
    root.answerError = "";
    root.intent = "dial";
    root.dialAddr = check.addr;
    root.failure = null;
    root.cancelAt = 0;
    root.starting = { role: "dialer", addr: check.addr };
    FerryClient.pairStart(check.addr, function (result, error) {
      root.starting = null;
      if (!error) return;
      // the dial's events (`connecting`, and the `failed` that ends it) are over
      root.ignoreBefore = Date.now();
      root.intent = "";
      var e = Model.errorText(error);
      root.dialError = e.message;
      root.dialHint = e.hint;
    });
    return true;
  }

  function accept() {
    root.acceptError = "";
    root.notice = "";
    root.answerError = "";
    root.intent = "accept";
    root.failure = null;
    root.cancelAt = 0;
    FerryClient.pairAccept(function (result, error) {
      if (!error) return;
      root.intent = "";
      var e = Model.errorText(error);
      root.acceptError = e.message + (e.hint !== "" ? " " + e.hint : "");
    });
  }

  // "Same code?" Any connection may answer and the first answer wins: if another place got there first
  // this call fails with no_pairing, and the events say how it went, so that is not an error to show.
  function answer(match) {
    root.answerError = "";
    FerryClient.pairAnswer(match, function (result, error) {
      if (!error || error.code === "no_pairing") return;
      root.answerError = Model.errorText(error).message;
    });
  }

  // Cancel. Any connection may cancel the pairing that runs, wherever it was started; the events say
  // how it ended. A refusal means it had already ended: then there is nothing to say.
  function cancel() {
    if (!root.stoppable) return;
    root.cancelAt = Date.now();
    root.notice = "";
    FerryClient.pairCancel(function (result, error) {
      if (error) root.cancelAt = 0;
    });
  }

  // Everything as it was when the panel opened (the harness and the test start each run from here).
  function reset() {
    root.keyboardNav = false;
    root.intent = "";
    root.dialAddr = "";
    root.nearby = [];
    root.starting = null;
    root.cancelAt = 0;
    root.ignoreBefore = 0;
    root.failure = null;
    root.dialError = "";
    root.dialHint = "";
    root.acceptError = "";
    root.answerError = "";
    root.notice = "";
    root.dismissedAt = Date.now();
  }

  // back to the start, with a result put away
  function dismiss() {
    root.dismissedAt = Date.now();
    root.notice = "";
    root.answerError = "";
    root.failure = null;
    root.intent = "";
  }

  // once more, the way it was started
  function retry() {
    var again = root.intent === "accept" || root.flow.role === "acceptor";
    root.dismiss();
    if (again) root.accept();
    else if (root.dialAddr !== "") root.dial(root.dialAddr);
  }

  // Put an address in the field, as typing would (the harness and the test).
  function typeAddress(text) {
    var screen = stage.item;
    if (screen && screen.start) screen.start.address = text;
  }

  function arrangeNow() {
    FerryUi.openPanel("arrange");
  }

  // ------------------------------------------------------------------------- the trusted list
  // The row for a peer, on the start screen that holds the list (the other screens have none).
  function trustedItem(fp) {
    var screen = stage.item;
    return screen && screen.trustedList ? screen.trustedList.item(fp) : null;
  }

  function renamePeer(fp, name) {
    FerryClient.renamePeer(fp, name, function (result, error) {
      var item = root.trustedItem(fp);
      if (item) item.finishRename(error ? Model.errorText(error).message : "");
    });
  }

  function unpairPeer(fp) {
    FerryClient.unpair(fp, function (result, error) {
      var item = root.trustedItem(fp);
      if (item) item.finishUnpair(error ? Model.errorText(error).message : "");
    });
  }

  // ----------------------------------------------------------------------------- what it learns
  Connections {
    target: FerryClient

    function onPairingEvent(event) {
      // A dial that failed ends with a `failed` event too, which can reach this connection before or
      // after the error that answers pair.start; the error under the field already says it.
      if (event && event.stage === "failed" && event.role === "dialer" && !event.peer
          && (root.starting !== null || root.dialError !== "")) {
        root.ignoreBefore = Date.now();
        return;
      }
      root.failure = Model.pairRemember(root.failure, event, Date.now(), root.cancelAt > 0 && Date.now() - root.cancelAt < 5000);
    }

    // a new session is a new daemon: ask again who this machine is
    function onSessionChanged() { root.fetchId(); }
  }

  function fetchId() {
    if (!FerryClient.connected) return;
    FerryClient.call("id", null, function (result) { if (result) root.idInfo = result; });
  }

  function fetchNearby() {
    if (!root.canDiscover || root.screen !== "start") return;
    FerryClient.call("discover.list", null, function (result) {
      if (result) root.nearby = Model.nearbyRows(result, root.rows);
    });
  }

  Timer {
    interval: 3000
    repeat: true
    running: root.canDiscover && root.screen === "start"
    triggeredOnStart: true
    onTriggered: root.fetchNearby()
  }

  Component.onCompleted: {
    root.fetchId();
    focusLater.start();
  }

  // ------------------------------------------------------------------------------------- keys
  Keys.onPressed: root.keyboardNav = true

  PointHandler {
    acceptedButtons: Qt.AllButtons
    onActiveChanged: if (active) root.keyboardNav = false
  }

  // Escape stops a pairing that runs; with none, it is left alone and the panel closes.
  Keys.onEscapePressed: function (event) {
    if (root.stoppable) {
      root.cancel();
      event.accepted = true;
    } else {
      event.accepted = false;
    }
  }

  // The focus goes where the user acts next. On the code it goes to the code and not to a button: nothing
  // that confirms trust may be one stray Enter away.
  function focusScreen() {
    var item = stage.item;
    switch (root.screen) {
      case "start":
        if (item && item.focusAddress) item.focusAddress();
        break;
      case "listening":
      case "connecting":
      case "waiting":
        cancelButton.forceActiveFocus();
        break;
      case "code":
        if (item && item.codeCard) item.codeCard.forceActiveFocus();
        break;
      case "paired":
        arrangeButton.forceActiveFocus();
        break;
      case "failed":
      case "closed":
        retryButton.forceActiveFocus();
        break;
    }
  }

  onScreenChanged: focusLater.restart()

  // Timers and not Qt.callLater: they die with the panel, and a delayed function that outlives it throws
  Timer {
    id: focusLater
    interval: 0
    onTriggered: root.focusScreen()
  }

  Timer {
    id: revealLater
    interval: 0
    onTriggered: root.ensureVisible(root.Window.activeFocusItem)
  }

  function inside(item) {
    for (var p = item; p; p = p.parent)
      if (p === root) return true;
    return false;
  }

  // The control that has the focus is never out of sight: an opened row or a question that grows past the
  // bottom of the panel is scrolled to.
  function ensureVisible(item) {
    if (!item || !root.inside(item) || !scroller.contentItem) return;
    var top = item.mapToItem(scroller.contentItem, 0, 0).y - Style.space(14);
    var bottom = item.mapToItem(scroller.contentItem, 0, item.height).y + Style.space(14);
    var room = Math.max(scroller.contentHeight, stage.y + stage.height);
    if (top < scroller.contentY) scroller.contentY = Math.max(0, top);
    else if (bottom > scroller.contentY + scroller.height)
      scroller.contentY = Math.max(0, Math.min(room - scroller.height, bottom - scroller.height));
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

  // ------------------------------------------------------------------------------------- shape
  Column {
    id: head
    anchors.top: parent.top
    anchors.left: parent.left
    anchors.right: parent.right
    spacing: Style.space(14)

    Hero {
      glyph: Glyphs.ui(root.flow.hero.glyph)
      title: root.flow.hero.title
      meta: root.flow.hero.meta
      detail: root.flow.hero.detail
      tone: root.flow.hero.tone
      detailTone: root.flow.hero.detailTone
      foreground: root.fg
    }

    StepBar {
      steps: root.flow.steps
      foreground: root.fg
      fontFamily: root.fontFamily
    }
  }

  Flickable {
    id: scroller
    anchors.top: head.bottom
    anchors.topMargin: Style.space(16)
    anchors.bottom: footer.top
    anchors.bottomMargin: Style.space(10)
    anchors.left: parent.left
    anchors.right: parent.right
    contentWidth: width
    contentHeight: stage.y + stage.height
    clip: true
    boundsBehavior: Flickable.StopAtBounds
    interactive: contentHeight > height

    // a list cut off by the window says so: the bar is there for as long as there is more below
    ScrollBar.vertical: PanelScrollBar { flickable: scroller }

    Behavior on contentY {
      enabled: !scroller.moving && !scroller.dragging
      NumberAnimation { duration: Fx.ms(140); easing.type: Easing.OutCubic }
    }

    // the screen for the stage the pairing is at; each is built when it is wanted, so each arrives
    Loader {
      id: stage
      width: scroller.width
      // a screen with little on it sits in the upper part of the room and not at the very top
      y: root.screen === "start" ? 0 : Math.max(0, Math.round((scroller.height - stage.height) * 0.28))
      sourceComponent: {
        switch (root.screen) {
          case "start": return startScreen;
          case "listening": return listeningScreen;
          case "connecting": return connectingScreen;
          case "code": return codeScreen;
          case "waiting": return waitingScreen;
          case "paired": return pairedScreen;
          default: return failedScreen;
        }
      }
      onLoaded: focusLater.restart()
    }
  }

  // --------------------------------------------------------------------------------- the footer
  Item {
    id: footer
    anchors.bottom: parent.bottom
    anchors.left: parent.left
    anchors.right: parent.right
    // the start screen has no buttons of its own down here, and its room is better spent above; a pairing that
    // only status shows has none to offer either (it cannot be cancelled from here)
    visible: root.screen !== "start" && !root.flow.elsewhere
    height: visible ? Math.max(footerRow.implicitHeight, Style.space(32)) + Style.space(12) : 0

    PanelSeparator {
      anchors.top: parent.top
      foreground: root.fg
    }

    Row {
      id: footerRow
      anchors.left: parent.left
      anchors.verticalCenter: parent.verticalCenter
      anchors.verticalCenterOffset: Style.space(5)
      spacing: Style.space(8)

      Button {
        id: cancelButton
        visible: root.stoppable
        text: Strings.t(root.screen === "listening" ? "pair.cancel.window" : "pair.cancel")
        bordered: true
        focusable: true
        foreground: root.fg
        fontFamily: root.fontFamily
        Accessible.name: text
        onClicked: root.cancel()
        FocusRing {}
      }

      Button {
        id: backButton
        visible: root.screen === "failed" || root.screen === "closed"
        text: Strings.t("pair.back")
        bordered: true
        focusable: true
        foreground: root.fg
        fontFamily: root.fontFamily
        Accessible.name: text
        onClicked: root.dismiss()
        FocusRing {}
      }

      Button {
        id: againButton
        visible: root.screen === "paired"
        text: Strings.t("pair.done.again")
        bordered: true
        focusable: true
        foreground: root.fg
        fontFamily: root.fontFamily
        Accessible.name: text
        onClicked: root.dismiss()
        FocusRing {}
      }
    }

    Row {
      id: footerEnd
      anchors.right: parent.right
      anchors.verticalCenter: parent.verticalCenter
      anchors.verticalCenterOffset: Style.space(5)
      spacing: Style.space(8)

      Button {
        id: differButton
        visible: root.screen === "code"
        text: Strings.t("pair.code.differ")
        iconText: Glyphs.ui("close")
        bordered: true
        focusable: true
        // the error tone, so it never reads as a plain Cancel (#33)
        foreground: Commons.Color.urgent
        fontFamily: root.fontFamily
        Accessible.name: text
        onClicked: root.answer(false)
        FocusRing {}
      }

      Button {
        id: matchButton
        visible: root.screen === "code"
        text: Strings.t("pair.code.match")
        iconText: Glyphs.ui("check")
        bordered: true
        // the accent fill marks the one answer that goes on (#33)
        background: Util.alpha(Commons.Color.accent, 0.28)
        focusable: true
        foreground: root.fg
        fontFamily: root.fontFamily
        Accessible.name: text
        onClicked: root.answer(true)
        FocusRing {}
      }

      Button {
        id: retryButton
        visible: root.screen === "failed" || root.screen === "closed"
        text: Strings.t(root.screen === "closed" || root.flow.role === "acceptor" ? "pair.again.accept" : "pair.retry")
        iconText: Glyphs.ui("refresh")
        bordered: true
        active: true
        focusable: true
        foreground: root.fg
        fontFamily: root.fontFamily
        Accessible.name: text
        onClicked: root.retry()
        FocusRing {}
      }

      Button {
        id: arrangeButton
        visible: root.screen === "paired"
        text: Strings.t("pair.done.arrange")
        iconText: Glyphs.ui("grid")
        bordered: true
        active: true
        focusable: true
        foreground: root.fg
        fontFamily: root.fontFamily
        Accessible.name: text
        onClicked: root.arrangeNow()
        FocusRing {}
      }
    }
  }

  // ----------------------------------------------------------------------------- the screens
  Component {
    id: startScreen

    Column {
      id: screen
      property alias trustedList: trustedList
      property alias start: start
      width: stage.width
      spacing: Style.space(12)
      function focusAddress() { start.focusAddress(); }


      // ferryd is not running: say so, and how to start it, above cards that cannot work
      BorderSurface {
        width: parent.width
        visible: !FerryClient.connected
        implicitHeight: offBody.implicitHeight + Style.space(24)
        color: Util.alpha(Commons.Color.urgent, 0.07)
        radius: Style.cornerRadius
        borderSpec: Border.flat(Util.alpha(Commons.Color.urgent, 0.6), 1)

        Column {
          id: offBody
          anchors.left: parent.left
          anchors.right: parent.right
          anchors.top: parent.top
          anchors.margins: Style.space(12)
          spacing: Style.space(8)

          Text {
            width: parent.width
            textFormat: Text.PlainText
            text: Strings.t("pair.dial.unavailable")
            color: root.fg
            font.family: root.fontFamily
            font.pixelSize: Style.font.body
            font.bold: true
            wrapMode: Text.WordWrap
          }

          CommandLine {
            width: parent.width
            command: Strings.t("off.cmd")
            foreground: root.fg
            fontFamily: root.fontFamily
          }
        }
      }

      PairStart {
        id: start
        unavailable: !FerryClient.connected
        error: root.dialError
        hint: root.dialHint
        acceptError: root.acceptError
        busy: root.starting !== null
        nearby: root.nearby
        selfAddress: root.selfAddr ? root.selfAddr.addr : ""
        foreground: root.fg
        fontFamily: root.fontFamily
        Component.onCompleted: address = root.dialAddr
        onDialRequested: function (addr) { root.dial(addr); }
        onAcceptRequested: root.accept()
      }

      // an attempt that failed in a window that is still open, when the user is back at the start
      Text {
        width: parent.width
        visible: root.flow.lastFailure !== null
        textFormat: Text.PlainText
        text: root.flow.lastFailure ? Strings.t("pair.last.failed", [root.flow.lastFailure.title]) : ""
        color: Commons.Color.urgent
        font.family: root.fontFamily
        font.pixelSize: Style.font.bodySmall
        wrapMode: Text.WordWrap
      }

      TrustedList {
        id: trustedList
        visible: FerryClient.connected
        rows: root.rows
        untrusted: root.untrusted
        canRename: root.canRename
        meName: FerryClient.meName
        foreground: root.fg
        fontFamily: root.fontFamily
        onRenameRequested: function (fp, name) { root.renamePeer(fp, name); }
        onUnpairRequested: function (fp) { root.unpairPeer(fp); }
        onPairRequested: function (name) { start.focusAddress(); }
      }

      Text {
        width: parent.width
        textFormat: Text.PlainText
        text: Strings.t("panel.pair.footer")
        color: root.dim
        font.family: root.fontFamily
        font.pixelSize: Style.font.caption
        wrapMode: Text.WordWrap
      }
    }
  }

  // the window is open, and nobody has come yet
  Component {
    id: listeningScreen

    Column {
      width: stage.width
      spacing: Style.space(14)

      // the last attempt did not work, and the window is still open
      BorderSurface {
        width: Math.min(parent.width, Style.space(560))
        anchors.horizontalCenter: parent.horizontalCenter
        visible: root.flow.lastFailure !== null
        implicitHeight: failure.implicitHeight + Style.space(20)
        color: Util.alpha(Commons.Color.urgent, 0.07)
        radius: Style.cornerRadius
        borderSpec: Border.flat(Util.alpha(Commons.Color.urgent, 0.6), 1)

        Text {
          id: failure
          anchors.left: parent.left
          anchors.right: parent.right
          anchors.verticalCenter: parent.verticalCenter
          anchors.margins: Style.space(10)
          textFormat: Text.PlainText
          text: root.flow.lastFailure ? Strings.t("pair.last.failed", [root.flow.lastFailure.title]) + " " + Strings.t("pair.last.failed.open") : ""
          color: root.fg
          font.family: root.fontFamily
          font.pixelSize: Style.font.bodySmall
          wrapMode: Text.WordWrap
        }
      }

      PairWait {
        width: Math.min(parent.width, Style.space(560))
        anchors.horizontalCenter: parent.horizontalCenter
        kind: "listening"
        title: Strings.t("pair.listening.title")
        body: Strings.t("pair.listening.body")
        address: root.selfAddr ? root.selfAddr.addr : ""
        command: root.selfAddr ? root.selfAddr.command : ""
        clock: root.flow.remainingText
        foreground: root.fg
        fontFamily: root.fontFamily
      }
    }
  }

  // the exchange before the code
  Component {
    id: connectingScreen

    Item {
      width: stage.width
      implicitHeight: wait.implicitHeight
      height: implicitHeight

      PairWait {
        id: wait
        width: Math.min(parent.width, Style.space(560))
        anchors.horizontalCenter: parent.horizontalCenter
        kind: "connecting"
        title: root.flow.role === "acceptor"
          ? (root.flow.who !== "" ? Strings.t("pair.connecting.accept", [root.flow.who]) : Strings.t("pair.connecting.accept.unknown"))
          : (root.flow.peer && root.flow.peer.name !== ""
            ? Strings.t("pair.connecting.dial.peer", [root.flow.peer.name])
            : Strings.t("pair.connecting.dial", [root.flow.label]))
        body: Strings.t("pair.meta.connecting")
        foreground: root.fg
        fontFamily: root.fontFamily
      }
    }
  }

  // the one thing the user has to do with their eyes
  Component {
    id: codeScreen

    Column {
      id: screen
      property alias codeCard: card
      width: stage.width
      spacing: Style.space(14)

      CodeCard {
        id: card
        width: Math.min(parent.width, Style.space(520))
        anchors.horizontalCenter: parent.horizontalCenter
        codeText: root.flow.codeText
        spoken: root.flow.codeSpoken
        caption: root.flow.peer ? (root.flow.peer.name !== "" ? root.flow.peer.name : "") + (root.flow.peer.addr !== "" ? (root.flow.peer.name !== "" ? "  ·  " : "") + root.flow.peer.addr : "") : ""
        clock: root.flow.remainingText
        fraction: root.flow.fraction
        expiring: root.flow.expiring
        foreground: root.fg
        fontFamily: root.fontFamily
      }

      Text {
        width: Math.min(parent.width, Style.space(520))
        anchors.horizontalCenter: parent.horizontalCenter
        textFormat: Text.PlainText
        text: root.flow.role === "acceptor"
          ? Strings.t("pair.code.accept", [root.flow.labelCap])
          : Strings.t("pair.code.dial", [root.flow.label])
        color: root.fg
        font.family: root.fontFamily
        font.pixelSize: Style.font.body
        horizontalAlignment: Text.AlignHCenter
        wrapMode: Text.WordWrap
        lineHeight: 1.2
      }

      Text {
        width: Math.min(parent.width, Style.space(520))
        anchors.horizontalCenter: parent.horizontalCenter
        visible: root.flow.role === "acceptor"
        textFormat: Text.PlainText
        text: Strings.t("pair.code.also")
        color: root.dim
        font.family: root.fontFamily
        font.pixelSize: Style.font.caption
        horizontalAlignment: Text.AlignHCenter
        wrapMode: Text.WordWrap
      }

      // what went wrong with the answer, or with a Cancel that only the opener may make
      Text {
        width: Math.min(parent.width, Style.space(520))
        anchors.horizontalCenter: parent.horizontalCenter
        visible: text !== ""
        textFormat: Text.PlainText
        text: root.answerError !== "" ? root.answerError : root.notice
        color: root.answerError !== "" ? Commons.Color.urgent : root.dim
        font.family: root.fontFamily
        font.pixelSize: Style.font.bodySmall
        horizontalAlignment: Text.AlignHCenter
        wrapMode: Text.WordWrap
      }
    }
  }

  // this side has said yes
  Component {
    id: waitingScreen

    Item {
      width: stage.width
      implicitHeight: wait.implicitHeight
      height: implicitHeight

      PairWait {
        id: wait
        width: Math.min(parent.width, Style.space(560))
        anchors.horizontalCenter: parent.horizontalCenter
        kind: "waiting"
        title: root.flow.noCode ? Strings.t("pair.nocode.title") : Strings.t("pair.waiting.title", [root.flow.label])
        body: root.notice !== "" ? root.notice : Strings.t(root.flow.noCode ? "pair.nocode.body" : "pair.waiting.body")
        foreground: root.fg
        fontFamily: root.fontFamily
      }
    }
  }

  Component {
    id: pairedScreen

    Item {
      width: stage.width
      implicitHeight: result.implicitHeight
      height: implicitHeight

      PairResult {
        id: result
        width: Math.min(parent.width, Style.space(560))
        anchors.horizontalCenter: parent.horizontalCenter
        kind: "paired"
        title: Strings.t("pair.done.title", [root.me, root.flow.who])
        body: Strings.t(root.flow.placed ? "pair.done.placed" : "pair.done.unplaced", [root.flow.who])
        fingerprint: root.flow.peer ? Model.groupedFingerprint(root.flow.peer.fingerprint) : ""
        fingerprintLabel: Strings.t("pair.done.fp", [root.flow.who])
        note: root.flow.note
        foreground: root.fg
        fontFamily: root.fontFamily
      }
    }
  }

  // failed, and a window that closed
  Component {
    id: failedScreen

    Item {
      width: stage.width
      implicitHeight: result.implicitHeight
      height: implicitHeight

      PairResult {
        id: result
        width: Math.min(parent.width, Style.space(560))
        anchors.horizontalCenter: parent.horizontalCenter
        kind: "failed"
        title: root.flow.failure ? root.flow.failure.title : ""
        body: root.flow.failure ? root.flow.failure.body : ""
        hint: root.flow.failure ? root.flow.failure.hint : ""
        foreground: root.fg
        fontFamily: root.fontFamily
      }
    }
  }
}
