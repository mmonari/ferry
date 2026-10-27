import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import "../Strings.js" as Strings

// What the popover says while ferryd is not running (the `off` state): why nothing works, the one
// command that fixes it, and that the widget is already trying again. Nothing here needs ferryd.
Column {
  id: root

  property color foreground: Commons.Color.popups.text
  property string fontFamily: Style.font.family

  readonly property color dim: Qt.darker(foreground, 1.4)

  width: parent ? parent.width : implicitWidth
  spacing: Style.space(10)

  Text {
    width: parent.width
    textFormat: Text.PlainText
    text: Strings.t("off.body")
    color: root.dim
    font.family: root.fontFamily
    font.pixelSize: Style.font.bodySmall
    wrapMode: Text.WordWrap
  }

  // first-time users go through ferry setup, which also starts the service; the one who has set up starts it
  Text {
    width: parent.width
    textFormat: Text.PlainText
    text: Strings.t("off.hint.setup")
    color: root.dim
    font.family: root.fontFamily
    font.pixelSize: Style.font.bodySmall
    wrapMode: Text.WordWrap
  }

  CommandLine {
    width: parent.width
    command: Strings.t("off.cmd.setup")
    foreground: root.foreground
    fontFamily: root.fontFamily
  }

  Text {
    width: parent.width
    textFormat: Text.PlainText
    text: Strings.t("off.hint.start")
    color: root.dim
    font.family: root.fontFamily
    font.pixelSize: Style.font.bodySmall
    wrapMode: Text.WordWrap
  }

  CommandLine {
    width: parent.width
    command: Strings.t("off.cmd")
    foreground: root.foreground
    fontFamily: root.fontFamily
  }

  // it is trying again every second, and the mark says so
  Row {
    spacing: Style.space(6)

    Spinner {
      anchors.verticalCenter: parent.verticalCenter
      size: Style.font.caption
      color: root.dim
    }

    Text {
      anchors.verticalCenter: parent.verticalCenter
      textFormat: Text.PlainText
      text: Strings.t("off.retrying")
      color: root.dim
      font.family: root.fontFamily
      font.pixelSize: Style.font.caption
      font.italic: true
    }
  }
}
