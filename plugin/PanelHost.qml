import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import "."
// The shell's qmlscanner registers a subfolder only when a root file imports it. A folder seen only
// from a file that is loaded by URL (the panels) gets no types at all in the real shell: "SettingItem
// is not a type", and a blank Settings panel. The offscreen harness loads by file://
// and never sees this. plugin.test.mjs checks that every folder is imported here or in another root file.
import "settings"
import "views"

// The panel's window (FR-UI-04..-08, -14, -18): the shell's KeyboardPanel, a layer-shell surface that
// takes the keyboard on open, closes on a click outside and hands over to the next popout, around
// PanelCard. Nothing else lives here, so everything the panel shows can be developed without a
// window (scripts/m3/harness).
//
// One size for every tab, so switching tabs never moves the frame. It opens centred on the monitor, as a
// dialog (decided 2026-11-09: no "beside the icon" option). Centred is the shell's centerOnBar, with the gap from the bar chosen so the card sits at the screen's middle on the
// other axis too (the same sum Station's PanelPosition.centerGap does).
KeyboardPanel {
  id: panel

  property var hostWidget: null

  readonly property int wantedWidth: Style.space(920)
  readonly property int wantedHeight: Style.space(600)

  readonly property bool horizontalBar: barPos === "top" || barPos === "bottom"
  // from the wanted size and the screen only: a gap read from the fitted size would loop through
  // availableCardHeight, which depends on the gap
  readonly property int centerGap: {
    var screenSize = horizontalBar ? screenH : screenW;
    var size = horizontalBar ? wantedHeight + padding * 2 : wantedWidth + padding * 2;
    if (!(screenSize > 0)) return Style.gapsOut;
    var fitted = Math.min(size, Math.max(0, screenSize - 2 * margin));
    return Math.max(Style.gapsOut, Math.round((screenSize - fitted) / 2 - stripThickness));
  }
  // The bar strip's thickness. Newer shells (4.5) name it `barThickness`; older ones (4.0.x) place the card
  // from the bar window's own size, `barH` or `barW`. Read through `panel.` so a missing name is undefined,
  // not a ReferenceError that leaves the gap unset.
  readonly property real stripThickness: {
    var t = panel.barThickness;
    if (t !== undefined && t !== null) return Number(t) || 0;
    var w = horizontalBar ? panel.barH : panel.barW;
    if (w !== undefined && w !== null && Number(w) > 0) return Number(w);
    return bar && bar.barSize ? Number(bar.barSize) : 0;
  }

  centerOnBar: true
  gap: centerGap
  focusTarget: body.item
  contentWidth: panel.fittedContentWidth(wantedWidth)
  contentHeight: panel.cappedContentHeight(wantedHeight)

  Loader {
    id: body
    anchors.fill: parent
    // Built on the first open, and kept: a Loader that dropped its item on close would lose the
    // scroll position and rebuild the tree on every open.
    active: panel.open || item !== null

    sourceComponent: PanelCard {
      shown: panel.open
      onCloseRequested: panel.close()
      onViewRequested: function (v) { FerryUi.panelView = FerryUi.viewOf(v) || "screens"; }
    }
  }
}
