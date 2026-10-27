import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import ".."
import "../Glyphs.js" as Glyphs
import "../Model.js" as Model
import "../Strings.js" as Strings

// Where pairing starts, and the two ways it can:
//   Pair with a machine         this side dials. The other machine is waiting; the user types its address
//                               (or picks it from Nearby, when ferryd can list what it finds)
//   Accept a pairing request    this side waits. It opens a two-minute window and says where it can be reached
// Nothing is opened until the user asks: the window lets another machine start a pairing, so it is a
// choice and not a side effect of looking at this panel. The cards only collect the choice; PairPanel
// sends it.
Row {
  id: root

  property string error: ""
  property string hint: ""
  property string acceptError: ""
  property bool busy: false
  // ferryd is not there: nothing here can work, and the cards say so instead of failing when pressed
  property bool unavailable: false
  // Model.nearbyRows(): machines found on this network, when the daemon lists them
  property var nearby: []
  // where this machine answers (Model.selfAddress)
  property string selfAddress: ""
  property color foreground: Commons.Color.popups.text
  property string fontFamily: Style.font.family
  property alias address: addrField.text
  property alias addressField: addrField
  property alias dialButton: dialButton
  property alias acceptButton: acceptButton

  // the nearby machines as keys (their addresses), which change only when the list does: ferryd's list is asked
  // for every three seconds, and a row that were rebuilt each time would lose the keyboard under the user
  property var nearbyKeys: []

  function syncNearby() {
    var next = nearby.map(function (n) { return n.addr + "|" + n.name; });
    if (!Model.sameList(next, nearbyKeys)) nearbyKeys = next;
  }

  function nearbyFor(key) {
    for (var i = 0; i < nearby.length; i++) if (nearby[i].addr + "|" + nearby[i].name === key) return nearby[i];
    return ({});
  }

  function choosable(entry) { return entry && entry.trusted !== true && entry.addr; }

  onNearbyChanged: syncNearby()
  Component.onCompleted: syncNearby()

  readonly property color dim: Qt.darker(foreground, 1.4)
  readonly property real cardWidth: (width - spacing) / 2
  readonly property real cardHeight: Math.max(dialCard.implicitHeight, acceptCard.implicitHeight)

  signal dialRequested(string addr)
  signal acceptRequested()

  function focusAddress() {
    addrField.forceActiveFocus();
    addrField.selectAll();
  }

  width: parent ? parent.width : implicitWidth
  height: cardHeight
  spacing: Style.space(12)

  // ---------------------------------------------------------------- pair with a machine
  BorderSurface {
    id: dialCard
    width: root.cardWidth
    height: implicitHeight
    implicitHeight: dialBody.implicitHeight + Style.space(28)
    color: Style.normalFill
    radius: Style.cornerRadius
    borderSpec: Border.controlSpec("normal", root.foreground, Commons.Color.accent)

    Column {
      id: dialBody
      anchors.left: parent.left
      anchors.right: parent.right
      anchors.top: parent.top
      anchors.margins: Style.space(14)
      spacing: Style.space(8)

      Row {
        spacing: Style.space(8)

        Glyph {
          anchors.verticalCenter: parent.verticalCenter
          width: Style.space(20)
          height: width
          text: Glyphs.ui("link")
          fontFamily: root.fontFamily
          fontSize: Style.font.iconLarge
          paintedSize: Style.font.iconLarge
          color: Commons.Color.accent
        }

        Text {
          anchors.verticalCenter: parent.verticalCenter
          textFormat: Text.PlainText
          text: Strings.t("pair.dial.title")
          color: root.foreground
          font.family: root.fontFamily
          font.pixelSize: Style.font.title
          font.bold: true
        }
      }

      Text {
        width: parent.width
        textFormat: Text.PlainText
        text: Strings.t("pair.dial.body")
        color: root.dim
        font.family: root.fontFamily
        font.pixelSize: Style.font.bodySmall
        wrapMode: Text.WordWrap
        lineHeight: 1.15
      }

      // machines found on this network, when there is a way to find them (M4)
      Column {
        visible: root.nearby.length > 0
        width: parent.width
        spacing: Style.space(4)

        PanelSectionHeader {
          text: Strings.t("pair.nearby")
          foreground: root.foreground
          fontFamily: root.fontFamily
        }

        Repeater {
          model: root.nearbyKeys

          NearbyRow {
            required property string modelData
            entry: root.nearbyFor(modelData)
            selected: root.choosable(entry) && addrField.text === entry.addr
            foreground: root.foreground
            fontFamily: root.fontFamily
            onChosen: { addrField.text = entry.addr; addrField.forceActiveFocus(); }
          }
        }
      }

      Text {
        width: parent.width
        textFormat: Text.PlainText
        text: Strings.t("pair.dial.label")
        color: root.foreground
        font.family: root.fontFamily
        font.pixelSize: Style.font.bodySmall
        font.bold: true
      }

      Row {
        width: parent.width
        spacing: Style.space(8)

        TextField {
          id: addrField
          width: parent.width - dialButton.width - parent.spacing
          anchors.verticalCenter: parent.verticalCenter
          placeholderText: Strings.t("pair.dial.placeholder")
          foreground: root.foreground
          font.family: root.fontFamily
          font.pixelSize: Style.font.body
          enabled: !root.busy && !root.unavailable
          selectByMouse: true
          inputMethodHints: Qt.ImhUrlCharactersOnly | Qt.ImhNoPredictiveText
          Accessible.name: Strings.t("pair.dial.label")

          Keys.onReturnPressed: root.dialRequested(text)
          Keys.onEnterPressed: root.dialRequested(text)
          Keys.onUpPressed: function (event) { event.accepted = true; }
          Keys.onDownPressed: function (event) { event.accepted = true; }
        }

        Button {
          id: dialButton
          anchors.verticalCenter: parent.verticalCenter
          text: Strings.t("pair.dial.go")
          bordered: true
          active: addrField.text.trim() !== ""
          focusable: true
          enabled: !root.busy && !root.unavailable
          foreground: root.foreground
          fontFamily: root.fontFamily
          Accessible.name: text
          onClicked: root.dialRequested(addrField.text)
          FocusRing {}
        }
      }

      // what went wrong, in ferryd's words, and then what to do; else the usual hint
      Text {
        width: parent.width
        visible: text !== ""
        textFormat: Text.PlainText
        text: root.error !== "" ? root.error + (root.hint !== "" ? "\n" + root.hint : "") : Strings.t("pair.dial.hint")
        color: root.error !== "" ? Commons.Color.urgent : root.dim
        font.family: root.fontFamily
        font.pixelSize: root.error !== "" ? Style.font.bodySmall : Style.font.caption
        wrapMode: Text.WordWrap
        lineHeight: 1.15
      }
    }
  }

  // ------------------------------------------------------------ accept a pairing request
  BorderSurface {
    id: acceptCard
    width: root.cardWidth
    height: implicitHeight
    implicitHeight: acceptBody.implicitHeight + Style.space(28)
    color: Style.normalFill
    radius: Style.cornerRadius
    borderSpec: Border.controlSpec("normal", root.foreground, Commons.Color.accent)

    Column {
      id: acceptBody
      anchors.left: parent.left
      anchors.right: parent.right
      anchors.top: parent.top
      anchors.margins: Style.space(14)
      spacing: Style.space(8)

      Row {
        spacing: Style.space(8)

        Glyph {
          anchors.verticalCenter: parent.verticalCenter
          width: Style.space(20)
          height: width
          text: Glyphs.ui("bell")
          fontFamily: root.fontFamily
          fontSize: Style.font.iconLarge
          paintedSize: Style.font.iconLarge
          color: Commons.Color.accent
        }

        Text {
          anchors.verticalCenter: parent.verticalCenter
          textFormat: Text.PlainText
          text: Strings.t("pair.accept.title")
          color: root.foreground
          font.family: root.fontFamily
          font.pixelSize: Style.font.title
          font.bold: true
        }
      }

      Text {
        width: parent.width
        textFormat: Text.PlainText
        text: Strings.t("pair.accept.body")
        color: root.dim
        font.family: root.fontFamily
        font.pixelSize: Style.font.bodySmall
        wrapMode: Text.WordWrap
        lineHeight: 1.15
      }

      Text {
        width: parent.width
        visible: root.selfAddress !== ""
        textFormat: Text.PlainText
        text: Strings.t("pair.accept.addr", [root.selfAddress])
        color: root.dim
        font.family: root.fontFamily
        font.pixelSize: Style.font.caption
        wrapMode: Text.WordWrap
      }

      Text {
        width: parent.width
        visible: root.acceptError !== ""
        textFormat: Text.PlainText
        text: root.acceptError
        color: Commons.Color.urgent
        font.family: root.fontFamily
        font.pixelSize: Style.font.bodySmall
        wrapMode: Text.WordWrap
      }

      Button {
        id: acceptButton
        width: parent.width
        text: Strings.t("pair.accept.go")
        iconText: Glyphs.ui("bell")
        bordered: true
        focusable: true
        enabled: !root.busy && !root.unavailable
        foreground: root.foreground
        fontFamily: root.fontFamily
        Accessible.name: text
        onClicked: root.acceptRequested()
        FocusRing {}
      }
    }
  }
}
