import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import ".."
import "../Glyphs.js" as Glyphs
import "../Strings.js" as Strings

// One trusted machine in the Pair panel: its name, the short fingerprint, the last address
// and when it was last seen, whether it is up, and the two things the user can do about it, Rename and
// Unpair. Both happen in place, under the row: a field for the name, and for unpairing a question
// with Cancel focused. Like PeerRow, it calls nothing: it says what was asked, the panel
// sends it, and finishRename() / finishUnpair() bring the answer back.
Item {
  id: root

  // one entry of Model.peerRows()
  property var row: ({})
  property color foreground: Commons.Color.popups.text
  property string fontFamily: Style.font.family
  property bool canRename: false
  property string meName: ""
  property var others: []
  // "": closed, "rename", "unpair"
  property string mode: ""
  property bool busy: false
  property string problem: ""
  readonly property bool open: mode !== ""
  property alias renameField: rename

  signal renameSubmitted(string name)
  signal unpairConfirmed()
  // once the strip is on screen: a Timer, which dies with the row (a Qt.callLater that outlives it throws)
  Timer { id: beginRename; interval: 0; onTriggered: rename.begin() }
  Timer { id: focusConfirm; interval: 0; onTriggered: confirm.focusCancel() }

  signal opened()

  readonly property color dim: Qt.darker(foreground, 1.5)
  readonly property string tone: row.tone || "ok"
  readonly property color statusColor: tone === "active" ? Commons.Color.accent
    : ((row.state === "refused" || tone === "warn") ? Commons.Color.urgent : dim)

  width: parent ? parent.width : implicitWidth
  implicitHeight: shell.implicitHeight

  Accessible.role: Accessible.ListItem
  Accessible.name: (row.name || "") + ", " + (row.st || "") + ", " + Strings.t("pair.trusted.fp", [row.shortFp || ""])

  function close(restoreFocus) {
    root.mode = "";
    root.busy = false;
    root.problem = "";
    if (restoreFocus) (root.canRename ? renameButton : unpairButton).forceActiveFocus();
  }

  function askRename() {
    if (!root.canRename) return;
    root.mode = "rename";
    root.opened();
    beginRename.start();
  }

  function askUnpair() {
    root.mode = "unpair";
    root.opened();
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

  BorderSurface {
    id: shell
    width: parent.width
    implicitHeight: line.height + reveal.height
    color: root.open ? Style.normalFill : "transparent"
    radius: Style.cornerRadius
    borderSpec: root.open ? Border.controlSpec("normal", root.foreground, Commons.Color.accent) : Border.none()

    Behavior on color { ColorAnimation { duration: Fx.ms(120) } }

    Item {
      id: line
      width: parent.width
      height: Math.max(Style.space(46), info.implicitHeight + Style.space(12))

      Rectangle {
        id: dot
        anchors.left: parent.left
        anchors.leftMargin: Style.space(10)
        anchors.verticalCenter: parent.verticalCenter
        width: Style.space(8)
        height: width
        radius: width / 2
        color: root.tone === "lost" ? "transparent"
          : (root.tone === "active" ? Commons.Color.accent
            : (root.tone === "wait" ? Commons.Color.muted
              : (root.tone === "warn" ? Commons.Color.urgent : root.foreground)))
        border.width: root.tone === "lost" ? Math.max(2, Style.space(2)) : 0
        border.color: root.row.state === "refused" ? Commons.Color.urgent : root.dim
      }

      Glyph {
        id: kind
        anchors.left: dot.right
        anchors.leftMargin: Style.space(12)
        anchors.verticalCenter: parent.verticalCenter
        width: Style.space(18)
        height: width
        text: Glyphs.ui(root.row.kind === "laptop" ? "laptop" : "desktop")
        fontFamily: root.fontFamily
        fontSize: Style.font.icon
        paintedSize: Style.font.icon
        color: root.dim
      }

      Column {
        id: info
        anchors.left: kind.right
        anchors.leftMargin: Style.space(10)
        anchors.right: status.left
        anchors.rightMargin: Style.space(10)
        anchors.verticalCenter: parent.verticalCenter
        spacing: Style.space(2)

        Text {
          width: parent.width
          textFormat: Text.PlainText
          text: root.row.name || ""
          color: root.foreground
          font.family: root.fontFamily
          font.pixelSize: Style.font.subtitle
          font.bold: true
          elide: Text.ElideRight
        }

        // The short fingerprint, the last address, and how it was paired
        Text {
          width: parent.width
          textFormat: Text.PlainText
          text: Strings.t("pair.trusted.fp", [root.row.shortFp || ""]) + " · "
            + (root.row.addr ? root.row.addr : Strings.t("pair.trusted.addr.none"))
            + (root.row.paired ? " · " + Strings.t(root.row.paired === "manual" ? "pair.trusted.how.manual" : "pair.trusted.how.sas") : "")
          color: root.dim
          font.family: root.fontFamily
          font.pixelSize: Style.font.caption
          elide: Text.ElideRight
        }
      }

      Column {
        id: status
        anchors.right: buttons.left
        anchors.rightMargin: Style.space(14)
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
          textFormat: Text.PlainText
          text: root.row.st || ""
          color: root.statusColor
          font.family: root.fontFamily
          font.pixelSize: Style.font.caption
          horizontalAlignment: Text.AlignRight
        }
      }

      Row {
        id: buttons
        anchors.right: parent.right
        anchors.rightMargin: Style.space(8)
        anchors.verticalCenter: parent.verticalCenter
        spacing: Style.space(6)

        Button {
          id: renameButton
          visible: root.canRename
          text: Strings.t("peer.action.rename")
          iconText: Glyphs.ui("pencil")
          bordered: true
          focusable: true
          selected: root.mode === "rename"
          foreground: root.foreground
          fontFamily: root.fontFamily
          fontSize: Style.font.bodySmall
          Accessible.name: Strings.t("peer.action.rename") + " " + (root.row.name || "")
          onClicked: root.askRename()
          FocusRing {}
        }

        Button {
          id: unpairButton
          text: Strings.t("peer.action.unpair")
          iconText: Glyphs.ui("trash")
          bordered: true
          focusable: true
          selected: root.mode === "unpair"
          foreground: root.foreground
          fontFamily: root.fontFamily
          fontSize: Style.font.bodySmall
          Accessible.name: Strings.t("peer.action.unpair") + " " + (root.row.name || "")
          onClicked: root.askUnpair()
          FocusRing {}
        }
      }
    }

    Reveal {
      id: reveal
      y: line.height
      width: parent.width
      shown: root.open

      Column {
        id: strip
        width: parent.width
        topPadding: Style.space(2)
        bottomPadding: Style.space(10)
        leftPadding: Style.space(10)
        rightPadding: Style.space(10)
        readonly property real inner: width - leftPadding - rightPadding

        RenameField {
          id: rename
          visible: root.mode === "rename"
          width: Math.min(strip.inner, Style.space(460))
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
          width: Math.min(strip.inner, Style.space(560))
          title: Strings.t("unpair.title", [root.row.name || ""])
          message: Strings.t("unpair.body", [root.row.name || ""])
          confirmText: Strings.t("unpair.confirm", [root.row.name || ""])
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
