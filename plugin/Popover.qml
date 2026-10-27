import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import "."

// The popover window: the shell's KeyboardPanel (a layer-shell surface that takes keyboard focus
// on open, dismisses on an outside click and hands over to the next popout), around
// PopoverContent. Nothing else lives here, so everything the popover shows can be developed
// without a window (scripts/m3/harness).
//
// The content is built the first time the popover opens and kept after that, which keeps a
// popover that is never opened free, per monitor. KeyboardPanel has a `content` Component for
// the same purpose in newer shells; an ordinary child and a Loader work in every shell version.
KeyboardPanel {
  id: panel

  property var hostWidget: null

  readonly property int wantedWidth: Style.space(360)

  focusTarget: body.item
  contentWidth: panel.fittedContentWidth(wantedWidth)
  contentHeight: panel.fittedContentHeight(body.item ? body.item.implicitHeight : Style.space(120),
    Math.round(panel.availableCardHeight * 0.9))

  Loader {
    id: body
    anchors.fill: parent
    // Built on the first open, and kept: a Loader that dropped its item on close would lose the
    // scroll position and rebuild the tree on every open.
    active: panel.open || item !== null

    sourceComponent: PopoverContent {
      shown: panel.open
      onCloseRequested: panel.close()
    }
  }
}
