import QtQuick
import Quickshell
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import ".."
import "../Glyphs.js" as Glyphs
import "../Model.js" as Model
import "../Strings.js" as Strings

// The things ferryd says need the user (`status.attention`), worded by us for each code and in ferryd's own
// words for one we do not know (`title` and `body` from Model.attentionActions): what happened, what to do,
// and the one button that can do something about it when there is one (`action`). A box in the urgent
// colour with a glyph and a dot, so it is never colour alone. It is the popover's "doctor
// summary" of FR-UI-04.
//
// The cards are kept by a key of their own and not rebuilt when ferryd sends the same list again,
// which it does every two seconds: a rebuilt button would lose the keyboard focus under the user.
BorderSurface {
  id: root

  // [{ code, message, hint, peer, action }], the result of Model.attentionActions()
  property var items: []
  property color foreground: Commons.Color.popups.text
  property string fontFamily: Style.font.family
  property int maxItems: 3
  // the keys of the cards shown, which change only when the list does
  property var keys: []

  readonly property color dim: Qt.darker(foreground, 1.4)
  readonly property int hidden: Math.max(0, items.length - maxItems)

  signal unpairRequested(string peer, bool again)
  signal panelRequested(string view)

  function keyOf(item) { return String(item.code) + "|" + String(item.peer || "") + "|" + String(item.message); }

  function syncKeys() {
    var next = items.slice(0, maxItems).map(keyOf);
    if (!Model.sameList(next, keys)) keys = next;
  }

  function itemFor(key) {
    for (var i = 0; i < items.length; i++) if (keyOf(items[i]) === key) return items[i];
    return ({});
  }

  onItemsChanged: syncKeys()
  Component.onCompleted: syncKeys()

  visible: items.length > 0
  width: parent ? parent.width : implicitWidth
  implicitHeight: body.implicitHeight + Style.space(20)
  color: Util.alpha(Commons.Color.urgent, 0.07)
  radius: Style.cornerRadius
  borderSpec: Border.flat(Util.alpha(Commons.Color.urgent, 0.6), 1)

  Accessible.role: Accessible.AlertMessage
  Accessible.name: Strings.t("section.attention") + ": " + (items.length > 0 ? (items[0].title || items[0].message) : "")

  Column {
    id: body
    anchors.left: parent.left
    anchors.right: parent.right
    anchors.top: parent.top
    anchors.margins: Style.space(10)
    anchors.rightMargin: Style.space(22)
    spacing: Style.space(10)

    Repeater {
      model: root.keys

      Row {
        id: card

        required property string modelData
        readonly property var item: root.itemFor(modelData)
        readonly property var action: item.action || null
        property bool copied: false

        width: body.width
        spacing: Style.space(8)

        Text {
          textFormat: Text.PlainText
          text: Glyphs.ui("alert")
          color: Commons.Color.urgent
          font.family: root.fontFamily
          font.pixelSize: Style.font.icon
        }

        Column {
          width: parent.width - Style.space(22)
          spacing: Style.space(3)

          Text {
            width: parent.width
            textFormat: Text.PlainText
            text: card.item.title !== undefined ? card.item.title : (card.item.message || "")
            color: root.foreground
            font.family: root.fontFamily
            font.pixelSize: Style.font.body
            font.bold: true
            wrapMode: Text.WordWrap
          }

          // ferryd writes a command in backticks; they are shown bold, after the text is escaped
          Text {
            width: parent.width
            visible: text !== ""
            textFormat: Text.StyledText
            text: Model.richText(card.item.body !== undefined ? card.item.body : (card.item.hint || ""))
            color: root.dim
            font.family: root.fontFamily
            font.pixelSize: Style.font.bodySmall
            wrapMode: Text.WordWrap
          }

          Button {
            visible: card.action !== null
            text: card.action ? (card.action.kind === "copy" && card.copied ? Strings.t("attn.copied") : card.action.label) : ""
            iconText: card.action ? (card.action.kind === "copy" ? Glyphs.ui(card.copied ? "check" : "copy")
              : (card.action.kind === "unpair" ? Glyphs.ui("refresh") : Glyphs.ui(card.action.view === "settings" ? "cog" : "grid"))) : ""
            bordered: true
            focusable: true
            foreground: root.foreground
            fontFamily: root.fontFamily
            fontSize: Style.font.bodySmall
            Accessible.name: text
            onClicked: {
              var a = card.action;
              if (!a) return;
              if (a.kind === "copy") {
                // wl-copy is on every Omarchy install; if it cannot run, the command is still on screen
                Quickshell.execDetached(["wl-copy", a.command]);
                card.copied = true;
                reset.restart();
              } else if (a.kind === "unpair") {
                root.unpairRequested(a.peer, a.again === true);
              } else if (a.kind === "panel") {
                root.panelRequested(a.view);
              }
            }
            FocusRing {}

            Timer {
              id: reset
              interval: 2500
              onTriggered: card.copied = false
            }
          }
        }
      }
    }

    Text {
      visible: root.hidden > 0
      textFormat: Text.PlainText
      text: Strings.t("tip.attention.more", [root.hidden])
      color: root.dim
      font.family: root.fontFamily
      font.pixelSize: Style.font.bodySmall
    }
  }

  // The dot badge in the corner: the same mark the bar button carries.
  Rectangle {
    anchors.top: parent.top
    anchors.right: parent.right
    anchors.topMargin: Style.space(9)
    anchors.rightMargin: Style.space(9)
    width: Style.space(7)
    height: width
    radius: width / 2
    color: Commons.Color.urgent
  }
}
