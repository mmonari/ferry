import QtQuick
import QtQuick.Shapes
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import ".."
import "../Glyphs.js" as Glyphs
import "../Strings.js" as Strings

// What the Arrange panel shows when there is nothing to arrange (one machine, or none paired): this
// machine's screens, a dashed place where the next one will go, and a way into the Pair panel.
// Nothing here moves and nothing needs ferryd to answer more than it has.
Item {
  id: root

  // this machine's group (plane px), drawn small; null draws only the dashed place
  property var group: null
  property string title: Strings.t("arrange.empty.title")
  property string body: Strings.t("arrange.empty.body")
  // a second line under the body: who has not reported its screens yet, for one
  property string note: ""
  property bool showButton: true
  property color foreground: Commons.Color.popups.text
  property string fontFamily: Style.font.family
  property alias button: cta

  signal pairRequested()

  readonly property color dim: Qt.darker(foreground, 1.4)
  readonly property real tileH: Style.space(78)
  readonly property real unit: group && group.rect.h > 0 ? Math.min(Style.space(190) / group.rect.w, tileH / group.rect.h) : 0

  Column {
    anchors.centerIn: parent
    width: Math.min(parent.width - Style.space(40), Style.space(520))
    spacing: Style.space(14)

    // the picture: this machine, and a dashed place beside it
    Row {
      anchors.horizontalCenter: parent.horizontalCenter
      spacing: Style.space(14)

      Item {
        visible: root.group !== null
        width: root.group ? root.group.rect.w * root.unit : 0
        height: root.tileH

        Repeater {
          model: root.group ? root.group.monitors.length : 0

          Rectangle {
            required property int index
            readonly property var m: root.group ? root.group.monitors[index] : null
            x: m ? (m.x - root.group.rect.x) * root.unit : 0
            y: m ? (m.y - root.group.rect.y) * root.unit + (root.tileH - root.group.rect.h * root.unit) / 2 : 0
            width: m ? m.w * root.unit : 0
            height: m ? m.h * root.unit : 0
            radius: Math.max(2, Style.cornerRadius / 2)
            color: Util.alpha(Commons.Color.accent, 0.12)
            border.width: Math.max(1, Style.spaceReal(1.5))
            border.color: Util.alpha(root.foreground, 0.45)

            Text {
              anchors.centerIn: parent
              width: parent.width - Style.space(6)
              horizontalAlignment: Text.AlignHCenter
              textFormat: Text.PlainText
              text: parent.m ? parent.m.name : ""
              color: root.foreground
              font.family: root.fontFamily
              font.pixelSize: Style.font.bodySmall
              font.bold: true
              elide: Text.ElideRight
              visible: parent.width > Style.space(54)
            }
          }
        }
      }

      Item {
        width: Style.space(120)
        height: root.tileH

        Shape {
          anchors.fill: parent
          preferredRendererType: Shape.GeometryRenderer
          ShapePath {
            strokeColor: Util.alpha(root.foreground, 0.5)
            strokeWidth: Math.max(1, Style.spaceReal(1.5))
            fillColor: Util.alpha(root.foreground, 0.04)
            strokeStyle: ShapePath.DashLine
            dashPattern: [4, 3]
            startX: 1; startY: 1
            PathLine { x: Style.space(120) - 1; y: 1 }
            PathLine { x: Style.space(120) - 1; y: root.tileH - 1 }
            PathLine { x: 1; y: root.tileH - 1 }
            PathLine { x: 1; y: 1 }
          }
        }

        Column {
          anchors.centerIn: parent
          spacing: Style.space(2)

          Text {
            anchors.horizontalCenter: parent.horizontalCenter
            textFormat: Text.PlainText
            text: Glyphs.ui("plus")
            color: root.dim
            font.family: root.fontFamily
            font.pixelSize: Style.font.iconLarge
          }
          Text {
            anchors.horizontalCenter: parent.horizontalCenter
            textFormat: Text.PlainText
            text: Strings.t("arrange.empty.ghost")
            color: root.dim
            font.family: root.fontFamily
            font.pixelSize: Style.font.caption
          }
        }
      }
    }

    Text {
      width: parent.width
      horizontalAlignment: Text.AlignHCenter
      textFormat: Text.PlainText
      text: root.title
      color: root.foreground
      font.family: root.fontFamily
      font.pixelSize: Style.font.title
      font.bold: true
      wrapMode: Text.WordWrap
    }

    Text {
      width: parent.width
      horizontalAlignment: Text.AlignHCenter
      textFormat: Text.PlainText
      text: root.body
      color: root.dim
      font.family: root.fontFamily
      font.pixelSize: Style.font.bodySmall
      wrapMode: Text.WordWrap
      lineHeight: 1.2
    }

    Text {
      visible: root.note !== ""
      width: parent.width
      horizontalAlignment: Text.AlignHCenter
      textFormat: Text.PlainText
      text: root.note
      color: root.dim
      font.family: root.fontFamily
      font.pixelSize: Style.font.caption
      font.italic: true
      wrapMode: Text.WordWrap
    }

    Button {
      id: cta
      visible: root.showButton
      anchors.horizontalCenter: parent.horizontalCenter
      text: Strings.t("arrange.empty.cta")
      iconText: Glyphs.ui("plus")
      bordered: true
      active: true
      focusable: true
      foreground: root.foreground
      fontFamily: root.fontFamily
      Accessible.name: text
      onClicked: root.pairRequested()
      FocusRing {}
    }
  }
}
