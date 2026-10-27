import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import ".."
import "../Glyphs.js" as Glyphs

// A pairing is running, and the popover says so (Model.pairBanner). While a code is waiting to be
// compared the strip is in the accent colour and asks for the user; the rest of the time it only says
// where the pairing is. The code itself is never here: it is read, and answered, in the Pair panel,
// which the button opens.
BorderSurface {
  id: root

  // the result of Model.pairBanner(): { text, urgent, clock, label }
  property var banner: ({ text: "", urgent: false, clock: "", label: "" })
  property color foreground: Commons.Color.popups.text
  property string fontFamily: Style.font.family
  property alias button: open

  readonly property color dim: Qt.darker(foreground, 1.4)
  readonly property bool asking: banner.urgent === true

  signal openRequested()

  width: parent ? parent.width : implicitWidth
  implicitHeight: row.implicitHeight + Style.space(20)
  color: Util.alpha(Commons.Color.accent, asking ? 0.12 : 0.06)
  radius: Style.cornerRadius
  borderSpec: Border.flat(Util.alpha(Commons.Color.accent, asking ? 0.8 : 0.4), 1)

  Accessible.role: Accessible.AlertMessage
  Accessible.name: banner.text + (banner.clock ? ", " + banner.clock : "")

  Row {
    id: row
    anchors.left: parent.left
    anchors.right: parent.right
    anchors.verticalCenter: parent.verticalCenter
    anchors.margins: Style.space(10)
    spacing: Style.space(10)

    // a bell when the user is waited for, a turning mark while the machines talk
    Item {
      width: Style.space(18)
      height: Style.space(18)
      anchors.verticalCenter: parent.verticalCenter

      Glyph {
        visible: root.asking
        anchors.fill: parent
        text: Glyphs.ui("bell")
        fontFamily: root.fontFamily
        fontSize: Style.font.icon
        paintedSize: Style.font.icon
        color: Commons.Color.accent
      }

      Spinner {
        visible: !root.asking
        anchors.centerIn: parent
        size: Style.space(18)
        running: visible
        color: Commons.Color.accent
      }
    }

    Column {
      width: parent.width - Style.space(28) - open.width - Style.space(10)
      anchors.verticalCenter: parent.verticalCenter
      spacing: Style.space(1)

      Text {
        width: parent.width
        textFormat: Text.PlainText
        text: root.banner.text
        color: root.foreground
        font.family: root.fontFamily
        font.pixelSize: Style.font.body
        font.bold: true
        wrapMode: Text.WordWrap
      }

      Text {
        visible: root.banner.clock !== ""
        textFormat: Text.PlainText
        text: root.banner.clock
        color: root.dim
        font.family: root.fontFamily
        font.pixelSize: Style.font.caption
        font.features: { "tnum": 1 }
      }
    }

    Button {
      id: open
      anchors.verticalCenter: parent.verticalCenter
      text: root.banner.label
      bordered: true
      active: root.asking
      focusable: true
      foreground: root.foreground
      fontFamily: root.fontFamily
      fontSize: Style.font.bodySmall
      Accessible.name: text
      onClicked: root.openRequested()
      FocusRing {}
    }
  }
}
