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
// choice and not a side effect of looking at this panel. Two flat panes: dial on the
// left, let a machine dial in on the right, a vertical hairline between them. They only collect the choice;
// PairPanel sends it.
Item {
  id: root

  property string error: ""
  property string hint: ""
  property string acceptError: ""
  property bool busy: false
  // ferryd is not there: nothing here can work, and the controls say so instead of failing when pressed
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
  readonly property real gap: Style.space(24)
  readonly property real leftWidth: Math.round((width - 2 * gap - 1) * 0.58)

  signal dialRequested(string addr)
  signal acceptRequested()

  function focusAddress() {
    addrField.forceActiveFocus();
    addrField.selectAll();
  }

  width: parent ? parent.width : implicitWidth
  implicitHeight: Math.max(dialPane.implicitHeight, acceptPane.implicitHeight)
  height: implicitHeight

  // ---------------------------------------------------------------- pair with a machine
  Column {
    id: dialPane
    anchors.left: parent.left
    anchors.top: parent.top
    width: root.leftWidth
    spacing: Style.space(8)

    // machines found on this network, when there is a way to find them (M4)
    PanelSectionHeader {
      visible: root.nearby.length > 0
      text: Strings.t("pair.nearby")
      font.capitalization: Font.AllUppercase
      foreground: root.foreground
      fontFamily: root.fontFamily
    }

    Column {
      visible: root.nearby.length > 0
      width: parent.width

      Repeater {
        model: root.nearbyKeys

        Column {
          id: nearbyItem
          required property string modelData
          required property int index
          width: parent.width

          PanelSeparator {
            visible: nearbyItem.index > 0
            foreground: root.foreground
          }

          NearbyRow {
            entry: root.nearbyFor(nearbyItem.modelData)
            selected: root.choosable(entry) && addrField.text === entry.addr
            foreground: root.foreground
            fontFamily: root.fontFamily
            onChosen: { addrField.text = entry.addr; root.dialRequested(entry.addr); }
          }
        }
      }
    }

    Item { width: 1; height: Style.space(6); visible: root.nearby.length > 0 }

    PanelSectionHeader {
      text: Strings.t(root.nearby.length > 0 ? "pair.dial.or" : "pair.dial.title")
      font.capitalization: Font.AllUppercase
      foreground: root.foreground
      fontFamily: root.fontFamily
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

      // the one action on this screen that commits: filled (panel-ui.md rule 3)
      Button {
        id: dialButton
        anchors.verticalCenter: parent.verticalCenter
        text: Strings.t("pair.dial.go")
        background: Commons.Color.accent
        // filled in every state: the kit's hover and focus fills would wash the label out
        color: hot || activeFocus ? Qt.darker(Commons.Color.accent, 1.12) : Commons.Color.accent
        foreground: Commons.Color.popups.background
        focusable: true
        enabled: !root.busy && !root.unavailable
        opacity: enabled ? 1.0 : 0.5
        fontFamily: root.fontFamily
        Accessible.name: text
        onClicked: root.dialRequested(addrField.text)
        FocusRing {}
      }
    }

    // what went wrong, in ferryd's words, and then what to do; else the usual one-line hint
    Text {
      width: parent.width
      visible: text !== ""
      textFormat: Text.PlainText
      text: root.error !== "" ? Glyphs.ui("alert") + "  " + root.error + (root.hint !== "" ? "\n" + root.hint : "") : Strings.t("pair.dial.hint")
      color: root.error !== "" ? Commons.Color.urgent : root.dim
      font.family: root.fontFamily
      font.pixelSize: Style.font.caption
      wrapMode: Text.WordWrap
      lineHeight: 1.15
    }
  }

  // the vertical hairline between the panes
  Rectangle {
    x: root.leftWidth + root.gap
    anchors.top: parent.top
    width: 1
    height: root.height
    color: Qt.rgba(root.foreground.r, root.foreground.g, root.foreground.b, 0.12)
  }

  // ------------------------------------------------------------ let another machine pair
  Column {
    id: acceptPane
    anchors.right: parent.right
    anchors.top: parent.top
    width: root.width - root.leftWidth - 2 * root.gap - 1
    spacing: Style.space(8)

    PanelSectionHeader {
      text: Strings.t("pair.accept.section")
      font.capitalization: Font.AllUppercase
      foreground: root.foreground
      fontFamily: root.fontFamily
    }

    Text {
      width: parent.width
      visible: root.selfAddress !== ""
      textFormat: Text.PlainText
      text: Strings.t("pair.accept.at")
      color: root.dim
      font.family: root.fontFamily
      font.pixelSize: Style.font.bodySmall
      wrapMode: Text.WordWrap
    }

    Text {
      width: parent.width
      visible: root.selfAddress !== ""
      textFormat: Text.PlainText
      text: root.selfAddress
      color: root.foreground
      font.family: root.fontFamily
      font.pixelSize: Style.font.body
      font.weight: Font.DemiBold
      font.features: { "tnum": 1 }
      elide: Text.ElideRight
    }

    Item { width: 1; height: Style.space(4) }

    Button {
      id: acceptButton
      text: Strings.t("pair.accept.go")
      bordered: true
      focusable: true
      enabled: !root.busy && !root.unavailable
      opacity: enabled ? 1.0 : 0.5
      foreground: root.foreground
      fontFamily: root.fontFamily
      Accessible.name: text
      onClicked: root.acceptRequested()
      FocusRing {}
    }

    Text {
      width: parent.width
      visible: root.acceptError !== ""
      textFormat: Text.PlainText
      text: Glyphs.ui("alert") + "  " + root.acceptError
      color: Commons.Color.urgent
      font.family: root.fontFamily
      font.pixelSize: Style.font.caption
      wrapMode: Text.WordWrap
    }
  }
}
