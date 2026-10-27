import QtQuick
import Quickshell
import qs.Commons as Commons
import qs.Commons
import qs.Ui
import "ferry" as Ferry

// A pretend desktop for the dev harness: a wallpaper, a bar strip with the REAL ferry widget in
// it, and below it the popover and the panel in cards that look like the shell's own. Nothing
// here is ferry logic. It only gives the plugin's files a place to be drawn, so the pixels a
// screenshot shows come from the same QML the shell loads.
//
//   barStrip   what the bar looks like around the widget (the widget is Ferry.FerryWidget)
//   popCard    the popover as the shell's KeyboardPanel frames it (Ui/KeyboardPanel.qml)
//   panelCard  the setup panel as PanelHost frames it
Item {
  id: stage

  property string panelView: "arrange"
  property bool showPopover: true
  property bool showPanel: true
  property bool showPeerName: true

  readonly property alias bar: barStrip
  readonly property alias popover: popCard
  readonly property alias panel: panelCard
  readonly property alias popoverContent: popBody
  readonly property alias panelContent: panelBody
  readonly property alias widget: ferryWidget

  implicitWidth: showPanel && !showPopover ? Style.space(900) : 960
  implicitHeight: showPanel && !showPopover ? barStrip.height + Style.space(14) + panelCard.height + Style.space(14)
    : barStrip.height + Style.gapsOut * 2 + (showPopover ? popCard.height : 0) + (showPanel ? Style.space(24) + panelCard.height : 0)

  // ---- wallpaper
  Rectangle {
    anchors.fill: parent
    gradient: Gradient {
      GradientStop { position: 0.0; color: Qt.darker(Commons.Color.background, 1.15) }
      GradientStop { position: 1.0; color: Qt.lighter(Commons.Color.background, 1.25) }
    }
  }

  // ---- the shell's own bar API, as a third-party widget gets it (Ui/PluginBarApi.qml)
  PluginBarApi {
    id: fakeBar
    pluginId: "ferry.bar"
    moduleName: "ferry.bar"
    foreground: Commons.Color.foreground
    barForeground: Commons.Color.bar.text
    background: Commons.Color.bar.background
    urgent: Commons.Color.bar.active
    fontFamily: Style.font.family
    position: "top"
    vertical: false
    barSize: Style.bar.sizeHorizontal
  }

  // ---- the bar strip
  Rectangle {
    id: barStrip
    x: 0
    y: 0
    width: stage.width
    height: Style.bar.sizeHorizontal
    color: Commons.Color.bar.background

    Row {
      anchors.left: parent.left
      anchors.leftMargin: Style.space(12)
      anchors.verticalCenter: parent.verticalCenter
      spacing: Style.space(12)
      Repeater {
        model: ["1", "2", "3", "4", "5"]
        Text {
          required property string modelData
          text: modelData
          color: modelData === "2" ? Commons.Color.bar.text : Qt.darker(Commons.Color.bar.text, 1.6)
          font.family: Style.font.family
          font.pixelSize: Style.font.body
          font.bold: modelData === "2"
        }
      }
    }

    Text {
      anchors.centerIn: parent
      text: "Wed 21 Oct  14:32"
      color: Commons.Color.bar.text
      font.family: Style.font.family
      font.pixelSize: Style.font.body
    }

    Row {
      id: rightRow
      anchors.right: parent.right
      anchors.rightMargin: Style.space(8)
      anchors.verticalCenter: parent.verticalCenter
      height: parent.height
      spacing: 0

      // neighbours, so the widget is judged next to icons it has to match
      Repeater {
        model: [String.fromCodePoint(0xF00AF), String.fromCodePoint(0xF05A9), String.fromCodePoint(0xF057E)]
        Item {
          required property string modelData
          width: Style.bar.iconSlot || 27
          height: parent.height
          OpticalGlyph {
            anchors.fill: parent
            text: modelData
            fontFamily: Style.font.family
            fontSize: Style.bar.iconFont || 13
            paintedSize: Style.bar.iconSize || 12
            color: Commons.Color.bar.text
          }
        }
      }

      Ferry.FerryWidget {
        id: ferryWidget
        bar: fakeBar
        settings: ({ showPeerName: stage.showPeerName })
        height: parent.height
      }

      Repeater {
        model: [String.fromCodePoint(0xF0079)]
        Item {
          required property string modelData
          width: Style.bar.iconSlot || 27
          height: parent.height
          OpticalGlyph {
            anchors.fill: parent
            text: modelData
            fontFamily: Style.font.family
            fontSize: Style.bar.iconFont || 13
            paintedSize: Style.bar.iconSize || 12
            color: Commons.Color.bar.text
          }
        }
      }
    }
  }

  // ---- the popover, framed as KeyboardPanel frames it
  BorderSurface {
    id: popCard
    visible: stage.showPopover
    x: stage.width - width - Style.gapsOut - Style.space(8)
    y: barStrip.height + Style.gapsOut
    width: Style.space(360)
    height: popBody.implicitHeight + contentTopInset + contentBottomInset
    color: Commons.Color.popups.background
    borderSpec: Border.localOrSurfaceSpec("popups", "border", Commons.Color.popups.border, Commons.Color.popups.border, Math.max(1, Style.space(2)))
    padding: Style.spacing.popupPadding
    radius: Style.cornerRadius

    Ferry.PopoverContent {
      id: popBody
      anchors.fill: parent
      anchors.topMargin: popCard.contentTopInset
      anchors.rightMargin: popCard.contentRightInset
      anchors.bottomMargin: popCard.contentBottomInset
      anchors.leftMargin: popCard.contentLeftInset
    }
  }

  // ---- the panel, framed as PanelHost frames it
  BorderSurface {
    id: panelCard
    visible: stage.showPanel
    x: (stage.width - width) / 2
    y: showPopover ? popCard.y + popCard.height + Style.space(24) : barStrip.height + Style.space(14)
    width: Math.min(Style.space(860), stage.width - Style.space(24))
    height: Style.space(620)
    color: Commons.Color.popups.background
    borderSpec: Border.localOrSurfaceSpec("popups", "border", Commons.Color.popups.border, Commons.Color.popups.border, Math.max(1, Style.space(2)))
    padding: Style.spacing.panelPadding
    radius: Style.cornerRadius

    Ferry.PanelCard {
      id: panelBody
      anchors.fill: parent
      anchors.topMargin: panelCard.contentTopInset
      anchors.rightMargin: panelCard.contentRightInset
      anchors.bottomMargin: panelCard.contentBottomInset
      anchors.leftMargin: panelCard.contentLeftInset
      view: stage.panelView
      onViewRequested: function (v) { stage.panelView = v; }
    }
  }
}
