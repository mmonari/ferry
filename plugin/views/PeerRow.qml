import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import ".."
import "../Glyphs.js" as Glyphs
import "../Strings.js" as Strings

// One trusted peer in the popover: a state dot, the kind of machine, its name and where it is, and on
// the right its round-trip time and what it is doing. `row` is one entry of Model.peerRows().
//
// Press it (Enter, Space, a click or a right click) and it opens: Rename, Unpair… and Arrange…, where
// renaming is a field in place and unpairing a question in place, with Cancel focused (FR-PAIR-07,
// FR-PAIR-10). Nothing here calls ferryd: it says what the user asked (`renameSubmitted`,
// `unpairConfirmed`), the popover sends it, and `finishRename()` / `finishUnpair()` bring the answer back.
//
// The dot is a hint and never the message: the right-hand text says "You're steering",
// "Offline · seen 5 min ago" or why a peer was refused, so the row reads the same in greyscale, and a
// peer that holds the pointer carries an arrow that says which way input goes.
Item {
  id: root

  property var row: ({})
  property color foreground: Commons.Color.popups.text
  property string fontFamily: Style.font.family
  // what this daemon can do (hello.methods), and where the row may send the user
  property bool canRename: false
  property bool canArrange: true
  // for Model.renameCheck: this machine's name, and every trusted peer
  property string meName: ""
  property var others: []
  // "": closed, "actions": the buttons, "rename", "unpair"
  property string mode: ""
  // after an unpair, go on to Pair: a changed key is fixed by unpairing and pairing again
  property bool thenPair: false
  property bool busy: false
  property string problem: ""
  readonly property bool open: mode !== ""
  property alias header: head
  property alias renameField: rename

  // once the strip is on screen: a Timer, which dies with the row (a Qt.callLater that outlives it throws)
  Timer { id: beginRename; interval: 0; onTriggered: rename.begin() }
  Timer { id: focusConfirm; interval: 0; onTriggered: confirm.focusCancel() }

  signal toggled()
  signal arrangeRequested()
  signal renameSubmitted(string name)
  signal unpairConfirmed()

  readonly property color dim: Qt.darker(foreground, 1.5)
  readonly property string tone: row.tone || "ok"
  // An active peer reads in the accent, a refused or silent one in the urgent colour; "seen 5 min
  // ago" is information, not an alarm, and stays quiet.
  readonly property color statusColor: tone === "active" ? Commons.Color.accent
    : ((row.state === "refused" || tone === "warn") ? Commons.Color.urgent : dim)

  width: parent ? parent.width : implicitWidth
  implicitHeight: shell.implicitHeight

  Accessible.role: Accessible.ListItem
  Accessible.name: (row.name || "") + ", " + (row.st || "") + (row.lat && row.lat !== "—" ? ", " + row.lat : "")

  // ---------------------------------------------------------------------------- what the user does
  function openActions() { root.mode = "actions"; }

  function close(restoreFocus) {
    root.mode = "";
    root.busy = false;
    root.problem = "";
    root.thenPair = false;
    if (restoreFocus) head.forceActiveFocus();
  }

  function toggle() {
    if (root.open) root.close(false);
    else { root.openActions(); root.toggled(); }
  }

  function askRename() {
    if (!root.canRename) return;
    root.mode = "rename";
    root.toggled();
    beginRename.start();
  }

  function askUnpair(again) {
    root.thenPair = !!again;
    root.mode = "unpair";
    root.toggled();
    focusConfirm.start();
  }

  function finishRename(message) {
    root.busy = false;
    if (message === "") root.close(true);
    else rename.fail(message);
  }

  function finishUnpair(message) {
    root.busy = false;
    root.problem = message === "" ? "" : Strings.t("unpair.failed", [message]);
    if (message === "") root.close(false);
  }

  // ---------------------------------------------------------------------------------------- shape
  // The open row sits in a box of its own, so the buttons or the question read as part of this peer.
  BorderSurface {
    id: shell
    width: parent.width
    implicitHeight: head.height + reveal.height
    color: root.open ? Style.normalFill : "transparent"
    radius: Style.cornerRadius
    borderSpec: root.open ? Border.controlSpec("normal", root.foreground, Commons.Color.accent) : Border.none()

    Behavior on color { ColorAnimation { duration: Fx.ms(120) } }

    // ---- the line itself: focusable, a click or Enter opens it
    Item {
      id: head
      width: parent.width
      height: Math.max(Style.space(38), info.implicitHeight + Style.space(8))
      activeFocusOnTab: true

      Accessible.role: Accessible.Button
      Accessible.name: root.Accessible.name
      Accessible.description: Strings.t("peer.actions", [root.row.name || ""])
      Accessible.onPressAction: root.toggle()

      Keys.onReturnPressed: root.toggle()
      Keys.onEnterPressed: root.toggle()
      Keys.onSpacePressed: root.toggle()
      // Escape closes an open row first, and the popover only when none is open
      Keys.onEscapePressed: function (event) {
        if (root.open) { root.close(false); event.accepted = true; }
        else event.accepted = false;
      }

      // hover and press, as every control of the kit paints them
      Rectangle {
        anchors.fill: parent
        radius: Style.cornerRadius
        color: mouse.pressed ? Style.pressedFillFor(root.foreground, Commons.Color.accent)
          : (mouse.containsMouse ? Style.hoverFillFor(root.foreground, Commons.Color.accent) : "transparent")
        Behavior on color { ColorAnimation { duration: Fx.ms(100) } }
      }

      // ---- the state dot: filled, haloed while it holds the pointer, hollow when it is lost
      Item {
        id: dotBox
        width: Style.space(12)
        height: width
        anchors.left: parent.left
        anchors.leftMargin: Style.space(6)
        anchors.verticalCenter: parent.verticalCenter

        Rectangle {
          id: halo
          visible: root.tone === "active"
          anchors.centerIn: parent
          width: Style.space(12)
          height: width
          radius: width / 2
          color: Util.alpha(Commons.Color.accent, 0.25)

          // the pointer is here: a slow breath, and still when the shell has animations off or the window is hidden
          SequentialAnimation on opacity {
            running: halo.visible && !Fx.still && (halo.Window.window ? halo.Window.window.visible : true)
            loops: Animation.Infinite
            NumberAnimation { from: 1.0; to: 0.35; duration: 1200; easing.type: Easing.InOutSine }
            NumberAnimation { from: 0.35; to: 1.0; duration: 1200; easing.type: Easing.InOutSine }
          }
          onVisibleChanged: if (!visible) opacity = 1.0
        }

        Rectangle {
          anchors.centerIn: parent
          width: Style.space(8)
          height: width
          radius: width / 2
          color: root.tone === "lost" ? "transparent"
            : (root.tone === "active" ? Commons.Color.accent
              : (root.tone === "wait" ? Commons.Color.muted
                : (root.tone === "warn" ? Commons.Color.urgent : Util.alpha(Commons.Color.accent, 0.75))))
          border.width: root.tone === "lost" ? Math.max(2, Style.space(2)) : 0
          border.color: root.row.state === "refused" ? Commons.Color.urgent : root.dim
          Behavior on color { ColorAnimation { duration: Fx.ms(160) } }
        }
      }

      // A box of one width, so the names line up whichever glyph a machine gets.
      Item {
        id: kindGlyph
        width: Style.space(18)
        height: Style.space(18)
        anchors.left: dotBox.right
        anchors.leftMargin: Style.space(8)
        anchors.verticalCenter: parent.verticalCenter

        Glyph {
          anchors.fill: parent
          text: Glyphs.ui(root.row.kind === "laptop" ? "laptop" : "desktop")
          fontFamily: root.fontFamily
          fontSize: Style.font.icon
          paintedSize: Style.font.icon
          color: root.dim
        }
      }

      Column {
        id: info
        anchors.left: kindGlyph.right
        anchors.leftMargin: Style.space(8)
        anchors.right: status.left
        anchors.rightMargin: Style.space(8)
        anchors.verticalCenter: parent.verticalCenter
        spacing: Style.space(1)

        Row {
          spacing: Style.space(6)
          width: parent.width

          Text {
            id: nameText
            textFormat: Text.PlainText
            text: root.row.name || ""
            color: root.foreground
            font.family: root.fontFamily
            font.pixelSize: Style.font.body
            font.bold: true
            elide: Text.ElideRight
            width: Math.min(implicitWidth, parent.width - (tag.visible ? tag.width + parent.spacing : 0)
              - (arrow.visible ? arrow.width + parent.spacing : 0))
          }

          // where this peer sits in the layout, while input goes out to it or comes in from it
          Glyph {
            id: arrow
            visible: root.row.arrow === "away" || root.row.arrow === "controlled"
            anchors.verticalCenter: parent.verticalCenter
            width: Style.space(14)
            height: width
            text: Glyphs.pointing(root.row.side)
            fontFamily: root.fontFamily
            fontSize: Style.font.icon
            paintedSize: Style.font.icon * 0.9
            color: Commons.Color.accent
          }

          BorderSurface {
            id: tag
            visible: root.row.locked === true
            anchors.verticalCenter: parent.verticalCenter
            width: tagText.implicitWidth + Style.space(8)
            height: tagText.implicitHeight + Style.space(2)
            color: "transparent"
            radius: Style.cornerRadius
            borderSpec: Border.controlSpec("normal", root.foreground, Commons.Color.accent)

            Text {
              id: tagText
              anchors.centerIn: parent
              textFormat: Text.PlainText
              text: Strings.t("peer.lockedTag")
              color: root.dim
              font.family: root.fontFamily
              font.pixelSize: Style.font.caption
              font.bold: true
            }
          }
        }

        Text {
          width: parent.width
          textFormat: Text.PlainText
          // a refused peer says why under its name, where the room is; its geometry can wait
          text: root.row.state === "refused" && root.row.st ? root.row.st : (root.row.sub || "")
          color: root.row.state === "refused" ? Commons.Color.urgent : root.dim
          font.family: root.fontFamily
          font.pixelSize: Style.font.caption
          elide: Text.ElideRight
        }
      }

      Column {
        id: status
        anchors.right: chevron.left
        anchors.rightMargin: Style.space(6)
        anchors.verticalCenter: parent.verticalCenter
        spacing: Style.space(1)
        width: Math.max(latText.implicitWidth, stText.implicitWidth)

        Text {
          id: latText
          anchors.right: parent.right
          textFormat: Text.PlainText
          text: root.row.lat || ""
          color: root.row.state === "refused" ? Commons.Color.urgent : (root.tone === "lost" ? root.dim : root.foreground)
          font.family: root.fontFamily
          font.pixelSize: Style.font.bodySmall
          font.bold: true
          font.features: { "tnum": 1 }
        }

        Text {
          id: stText
          anchors.right: parent.right
          visible: !(root.row.state === "refused" && root.row.st)
          textFormat: Text.PlainText
          text: root.row.st || ""
          color: root.statusColor
          font.family: root.fontFamily
          font.pixelSize: Style.font.caption
          horizontalAlignment: Text.AlignRight
        }
      }

      // says "there is more here", and turns when it is open
      Glyph {
        id: chevron
        anchors.right: parent.right
        anchors.rightMargin: Style.space(6)
        anchors.verticalCenter: parent.verticalCenter
        width: Style.space(14)
        height: width
        text: Glyphs.ui("chevronDown")
        fontFamily: root.fontFamily
        fontSize: Style.font.icon
        paintedSize: Style.font.icon * 0.85
        color: root.open || mouse.containsMouse || head.activeFocus ? root.foreground : Qt.darker(root.foreground, 2.0)
        rotation: root.open ? 180 : 0

        Behavior on rotation { NumberAnimation { duration: Fx.ms(160); easing.type: Easing.OutCubic } }
        Behavior on color { ColorAnimation { duration: Fx.ms(120) } }
      }

      MouseArea {
        id: mouse
        anchors.fill: parent
        hoverEnabled: true
        cursorShape: Qt.PointingHandCursor
        acceptedButtons: Qt.LeftButton | Qt.RightButton
        onClicked: function () {
          head.forceActiveFocus();
          root.toggle();
        }
      }

      FocusRing {}
    }

    // ---- what opens under it
    Reveal {
      id: reveal
      y: head.height
      width: parent.width
      shown: root.open

      Column {
        id: strip
        width: parent.width
        spacing: Style.space(6)
        topPadding: Style.space(2)
        bottomPadding: Style.space(10)
        leftPadding: Style.space(10)
        rightPadding: Style.space(10)
        readonly property real inner: width - leftPadding - rightPadding

        // the three buttons, and what is known about the peer under them
        Column {
          visible: root.mode === "actions"
          width: strip.inner
          spacing: Style.space(6)

          Flow {
            width: parent.width
            spacing: Style.space(6)

            Button {
              visible: root.canRename
              text: Strings.t("peer.action.rename")
              iconText: Glyphs.ui("pencil")
              bordered: true
              focusable: true
              foreground: root.foreground
              fontFamily: root.fontFamily
              fontSize: Style.font.bodySmall
              Accessible.name: text
              onClicked: root.askRename()
              FocusRing {}
            }

            Button {
              text: Strings.t("peer.action.unpair")
              iconText: Glyphs.ui("trash")
              bordered: true
              focusable: true
              foreground: root.foreground
              fontFamily: root.fontFamily
              fontSize: Style.font.bodySmall
              Accessible.name: text
              onClicked: root.askUnpair(false)
              FocusRing {}
            }

            Button {
              visible: root.canArrange
              text: Strings.t("peer.action.arrange")
              iconText: Glyphs.ui("grid")
              bordered: true
              focusable: true
              foreground: root.foreground
              fontFamily: root.fontFamily
              fontSize: Style.font.bodySmall
              Accessible.name: text
              onClicked: root.arrangeRequested()
              FocusRing {}
            }
          }

          Text {
            width: parent.width
            textFormat: Text.PlainText
            text: Strings.t("pair.trusted.fp", [root.row.shortFp || ""]) + (root.row.addr ? " · " + root.row.addr : "")
            color: root.dim
            font.family: root.fontFamily
            font.pixelSize: Style.font.caption
            elide: Text.ElideRight
          }
        }

        RenameField {
          id: rename
          visible: root.mode === "rename"
          width: strip.inner
          value: root.row.name || ""
          busy: root.busy
          foreground: root.foreground
          fontFamily: root.fontFamily
          context: ({ me: root.meName, fingerprint: root.row.fingerprint, current: root.row.name, others: root.others })
          onAccepted: function (name) { root.busy = true; root.renameSubmitted(name); }
          onCanceled: root.close(true)
        }

        ConfirmStrip {
          id: confirm
          visible: root.mode === "unpair"
          width: strip.inner
          title: Strings.t("unpair.title", [root.row.name || ""])
          message: Strings.t("unpair.body", [root.row.name || ""])
          confirmText: Strings.t(root.thenPair ? "unpair.confirm.again" : "unpair.confirm", [root.row.name || ""])
          busyText: Strings.t("unpair.busy")
          busy: root.busy
          error: root.problem
          foreground: root.foreground
          fontFamily: root.fontFamily
          onConfirmed: { root.problem = ""; root.busy = true; root.unpairConfirmed(); }
          onCanceled: root.close(true)
        }
      }
    }
  }
}
