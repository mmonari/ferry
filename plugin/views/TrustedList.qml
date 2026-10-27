import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import ".."
import "../Glyphs.js" as Glyphs
import "../Model.js" as Model
import "../Strings.js" as Strings

// The machines this one trusts, with Rename and Unpair behind a question, and
// under them the machines the layout knows that are not paired with this one: a
// crossing between them is off, and the Pair button says where to start. Like the popover's list, its
// rows are kept by fingerprint and not rebuilt when ferryd repeats the same facts.
Column {
  id: root

  // Model.peerRows() and Model.untrustedPeers()
  property var rows: []
  property var untrusted: []
  property bool canRename: false
  property string meName: ""
  property color foreground: Commons.Color.popups.text
  property string fontFamily: Style.font.family
  property string openPeer: ""
  property var keys: []
  property alias repeater: rowRepeater

  readonly property color dim: Qt.darker(foreground, 1.4)

  signal renameRequested(string fingerprint, string name)
  signal unpairRequested(string fingerprint)
  signal pairRequested(string name)

  function syncKeys() {
    var next = rows.map(function (r) { return r.fingerprint; });
    if (!Model.sameList(next, keys)) keys = next;
  }

  function rowFor(fp) {
    for (var i = 0; i < rows.length; i++) if (rows[i].fingerprint === fp) return rows[i];
    return ({});
  }

  function item(fp) {
    for (var i = 0; i < rowRepeater.count; i++) {
      var it = rowRepeater.itemAt(i);
      if (it && it.row.fingerprint === fp) return it;
    }
    return null;
  }

  onRowsChanged: syncKeys()
  Component.onCompleted: syncKeys()

  width: parent ? parent.width : implicitWidth
  spacing: Style.space(2)

  PanelSectionHeader {
    text: Strings.t("pair.trusted")
    foreground: root.foreground
    fontFamily: root.fontFamily
  }

  Text {
    width: parent.width
    visible: root.rows.length === 0
    topPadding: Style.space(4)
    textFormat: Text.PlainText
    text: Strings.t("pair.trusted.none")
    color: root.dim
    font.family: root.fontFamily
    font.pixelSize: Style.font.bodySmall
  }

  Repeater {
    id: rowRepeater
    model: root.keys

    TrustedRow {
      id: trusted
      required property string modelData
      row: root.rowFor(modelData)
      foreground: root.foreground
      fontFamily: root.fontFamily
      canRename: root.canRename
      meName: root.meName
      others: root.rows.map(function (r) { return { name: r.name, fingerprint: r.fingerprint }; })

      onOpened: root.openPeer = modelData
      onRenameSubmitted: function (name) { root.renameRequested(modelData, name); }
      onUnpairConfirmed: root.unpairRequested(modelData)

      Connections {
        target: root
        function onOpenPeerChanged() {
          if (root.openPeer !== trusted.modelData && trusted.open) trusted.close(false);
        }
      }
    }
  }

  // peers the layout knows and this machine does not trust
  Column {
    visible: root.untrusted.length > 0
    width: parent.width
    spacing: Style.space(2)
    topPadding: Style.space(10)

    PanelSectionHeader {
      text: Strings.t("pair.untrusted")
      foreground: root.foreground
      fontFamily: root.fontFamily
    }

    Repeater {
      model: root.untrusted

      Item {
        required property var modelData
        width: parent.width
        implicitHeight: Math.max(Style.space(40), note.implicitHeight + Style.space(12))

        Glyph {
          id: mark
          anchors.left: parent.left
          anchors.leftMargin: Style.space(8)
          anchors.verticalCenter: parent.verticalCenter
          width: Style.space(18)
          height: width
          text: Glyphs.ui("linkOff")
          fontFamily: root.fontFamily
          fontSize: Style.font.icon
          paintedSize: Style.font.icon
          color: root.dim
        }

        Text {
          id: note
          anchors.left: mark.right
          anchors.leftMargin: Style.space(12)
          anchors.right: pairButton.left
          anchors.rightMargin: Style.space(12)
          anchors.verticalCenter: parent.verticalCenter
          textFormat: Text.PlainText
          text: Strings.t("pair.untrusted.body", [modelData.name])
          color: root.dim
          font.family: root.fontFamily
          font.pixelSize: Style.font.bodySmall
          wrapMode: Text.WordWrap
        }

        Button {
          id: pairButton
          anchors.right: parent.right
          anchors.rightMargin: Style.space(8)
          anchors.verticalCenter: parent.verticalCenter
          text: Strings.t("pair.untrusted.action")
          iconText: Glyphs.ui("plus")
          bordered: true
          focusable: true
          foreground: root.foreground
          fontFamily: root.fontFamily
          fontSize: Style.font.bodySmall
          Accessible.name: Strings.t("pair.untrusted.action") + " " + modelData.name
          onClicked: root.pairRequested(modelData.name)
          FocusRing {}
        }
      }
    }
  }
}
