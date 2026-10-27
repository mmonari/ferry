import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import ".."
import "../Glyphs.js" as Glyphs
import "../Strings.js" as Strings

// FR-UI-17, U6: another peer changed the layout while an unapplied draft was open. It does not block
// anything: the draft stays on the canvas and can still be edited, and the banner offers the two ways
// out. **Keep mine** applies the draft on top of the new revision (and overwrites that change),
// **Take theirs** reloads the layout and discards the draft. In the urgent colour, with a glyph and a
// dot badge, so it never relies on colour alone (U3).
BorderSurface {
  id: root

  property string title: ""
  property string body: ""
  // False when keeping the draft would overlap what the other peer placed: Keep mine is off until the
  // draft is moved clear
  property bool keepEnabled: true
  property color foreground: Commons.Color.popups.text
  property string fontFamily: Style.font.family
  property alias keepButton: keep
  property alias takeButton: take

  signal keepRequested()
  signal takeRequested()

  readonly property color dim: Qt.darker(foreground, 1.4)

  width: parent ? parent.width : implicitWidth
  implicitHeight: Math.max(texts.implicitHeight, buttons.implicitHeight) + Style.space(20)
  color: Util.alpha(Commons.Color.urgent, 0.08)
  radius: Style.cornerRadius
  borderSpec: Border.flat(Util.alpha(Commons.Color.urgent, 0.6), 1)

  Accessible.role: Accessible.AlertMessage
  Accessible.name: title
  Accessible.description: body

  Text {
    id: glyph
    anchors.left: parent.left
    anchors.leftMargin: Style.space(12)
    anchors.top: parent.top
    anchors.topMargin: Style.space(10)
    textFormat: Text.PlainText
    text: Glyphs.ui("alert")
    color: Commons.Color.urgent
    font.family: root.fontFamily
    font.pixelSize: Style.font.iconLarge
  }

  Column {
    id: texts
    anchors.left: glyph.right
    anchors.leftMargin: Style.space(10)
    anchors.right: buttons.left
    anchors.rightMargin: Style.space(12)
    anchors.top: parent.top
    anchors.topMargin: Style.space(10)
    spacing: Style.space(2)

    Text {
      width: parent.width
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
      textFormat: Text.PlainText
      text: root.body
      color: root.dim
      font.family: root.fontFamily
      font.pixelSize: Style.font.bodySmall
      wrapMode: Text.WordWrap
    }
  }

  Row {
    id: buttons
    anchors.right: parent.right
    anchors.rightMargin: Style.space(26)
    anchors.verticalCenter: parent.verticalCenter
    spacing: Style.space(8)

    Button {
      id: take
      text: Strings.t("arrange.take")
      iconText: Glyphs.ui("undo")
      bordered: true
      active: true
      focusable: true
      foreground: root.foreground
      fontFamily: root.fontFamily
      fontSize: Style.font.bodySmall
      Accessible.name: text
      onClicked: root.takeRequested()
      FocusRing {}
    }

    Button {
      id: keep
      text: Strings.t("arrange.keep")
      iconText: Glyphs.ui("check")
      bordered: true
      focusable: true
      enabled: root.keepEnabled
      opacity: enabled ? 1 : 0.45
      foreground: root.foreground
      fontFamily: root.fontFamily
      fontSize: Style.font.bodySmall
      Accessible.name: text
      onClicked: if (root.keepEnabled) root.keepRequested()
      FocusRing {}
    }
  }

  // the dot badge in the corner, as AttentionCard and the bar button carry it
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
