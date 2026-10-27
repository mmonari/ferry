import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import ".."
import "../Glyphs.js" as Glyphs
import "../Model.js" as Model
import "../Strings.js" as Strings

// What ferryd said lately, in the order it happened (the `notice` events): a glyph per
// kind, the words of the toast, and how long ago. It is how a toast that was dismissed, or never shown
// because no notification daemon runs, is still found. Nothing in it can be pressed.
Column {
  id: root

  // the result of Model.activityItems()
  property var items: []
  property color foreground: Commons.Color.popups.text
  property string fontFamily: Style.font.family
  property var keys: []

  readonly property color dim: Qt.darker(foreground, 1.4)

  function keyOf(item) { return String(item.at) + "|" + item.kind + "|" + item.order; }

  function syncKeys() {
    var next = items.map(keyOf);
    if (!Model.sameList(next, keys)) keys = next;
  }

  function itemFor(key) {
    for (var i = 0; i < items.length; i++) if (keyOf(items[i]) === key) return items[i];
    return ({ kind: "", title: "", body: "", ago: "", urgent: false });
  }

  onItemsChanged: syncKeys()
  Component.onCompleted: syncKeys()

  width: parent ? parent.width : implicitWidth
  spacing: Style.space(2)

  PanelSectionHeader {
    text: Strings.t("section.recent")
    foreground: root.foreground
    fontFamily: root.fontFamily
  }

  Repeater {
    model: root.keys

    Item {
      id: line

      required property string modelData
      readonly property var item: root.itemFor(modelData)

      width: root.width
      implicitHeight: Math.max(Style.space(24), words.implicitHeight + Style.space(6))

      Accessible.role: Accessible.StaticText
      Accessible.name: (line.item.title || "") + (line.item.ago ? ", " + line.item.ago : "")

      Glyph {
        id: mark
        anchors.left: parent.left
        anchors.leftMargin: Style.space(2)
        anchors.verticalCenter: parent.verticalCenter
        width: Style.space(16)
        height: width
        text: Glyphs.notice(line.item.kind)
        fontFamily: root.fontFamily
        fontSize: Style.font.icon
        paintedSize: Style.font.icon * 0.9
        color: line.item.urgent ? Commons.Color.urgent : root.dim
      }

      // not named `text`: an id wins over a property of the element it is used in, and the Text
      // inside would then ask the Column, and not itself, whether it is empty
      Column {
        id: words
        anchors.left: mark.right
        anchors.leftMargin: Style.space(8)
        anchors.right: ago.left
        anchors.rightMargin: Style.space(8)
        anchors.verticalCenter: parent.verticalCenter
        spacing: Style.space(1)

        Text {
          width: parent.width
          textFormat: Text.PlainText
          text: line.item.title || ""
          color: line.item.urgent ? Commons.Color.urgent : root.foreground
          font.family: root.fontFamily
          font.pixelSize: Style.font.bodySmall
          elide: Text.ElideRight
        }

        Text {
          width: parent.width
          visible: text !== ""
          textFormat: Text.PlainText
          text: line.item.body || ""
          color: root.dim
          font.family: root.fontFamily
          font.pixelSize: Style.font.caption
          // two lines, so a sentence is not cut in the middle of its news
          wrapMode: Text.WordWrap
          maximumLineCount: 2
          elide: Text.ElideRight
        }
      }

      Text {
        id: ago
        anchors.right: parent.right
        anchors.rightMargin: Style.space(2)
        anchors.verticalCenter: parent.verticalCenter
        textFormat: Text.PlainText
        text: line.item.ago || ""
        color: root.dim
        font.family: root.fontFamily
        font.pixelSize: Style.font.caption
      }
    }
  }
}
