import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import ".."
import "../Strings.js" as Strings

// "Are you sure?" inline, for what the user cannot undo with one click (unpairing): the
// question, what follows from it, and two buttons. It sits where the thing it asks about is, so
// there is no dialog to lose the place in, and it starts with the safe button focused. Escape says no.
BorderSurface {
  id: root

  property string title: ""
  property string message: ""
  property string confirmText: ""
  property string busyText: ""
  property string cancelText: Strings.t("common.cancel")
  property bool busy: false
  property string error: ""
  property color foreground: Commons.Color.popups.text
  property string fontFamily: Style.font.family
  property alias cancelButton: cancelBtn
  property alias confirmButton: confirmBtn

  readonly property color dim: Qt.darker(foreground, 1.4)

  signal confirmed()
  signal canceled()

  function focusCancel() { cancelBtn.forceActiveFocus(); }

  width: parent ? parent.width : implicitWidth
  implicitHeight: body.implicitHeight + Style.space(20)
  color: Util.alpha(Commons.Color.urgent, 0.07)
  radius: Style.cornerRadius
  borderSpec: Border.flat(Util.alpha(Commons.Color.urgent, 0.6), 1)

  Keys.onEscapePressed: function (event) {
    event.accepted = true;
    if (!root.busy) root.canceled();
  }

  Column {
    id: body
    anchors.left: parent.left
    anchors.right: parent.right
    anchors.top: parent.top
    anchors.margins: Style.space(10)
    spacing: Style.space(6)

    Text {
      width: parent.width
      visible: text !== ""
      textFormat: Text.PlainText
      text: root.title
      color: root.foreground
      font.family: root.fontFamily
      font.pixelSize: Style.font.body
      font.bold: true
      wrapMode: Text.WordWrap
    }

    Text {
      width: parent.width
      visible: text !== ""
      textFormat: Text.PlainText
      text: root.message
      color: root.dim
      font.family: root.fontFamily
      font.pixelSize: Style.font.bodySmall
      wrapMode: Text.WordWrap
      lineHeight: 1.15
    }

    Text {
      width: parent.width
      visible: root.error !== ""
      textFormat: Text.PlainText
      text: root.error
      color: Commons.Color.urgent
      font.family: root.fontFamily
      font.pixelSize: Style.font.bodySmall
      wrapMode: Text.WordWrap
    }

    Item {
      width: parent.width
      height: confirmBtn.implicitHeight

      Row {
        anchors.right: parent.right
        spacing: Style.space(8)

        Button {
          id: cancelBtn
          text: root.cancelText
          bordered: true
          focusable: true
          enabled: !root.busy
          foreground: root.foreground
          fontFamily: root.fontFamily
          fontSize: Style.font.bodySmall
          Accessible.name: text
          onClicked: root.canceled()
          FocusRing {}
        }

        Button {
          id: confirmBtn
          text: root.busy && root.busyText !== "" ? root.busyText : root.confirmText
          bordered: true
          focusable: true
          foreground: Commons.Color.urgent
          accent: Commons.Color.urgent
          fontFamily: root.fontFamily
          fontSize: Style.font.bodySmall
          Accessible.name: text
          onClicked: if (!root.busy) root.confirmed()
          FocusRing {}
        }
      }
    }
  }
}
