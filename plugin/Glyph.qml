import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui

// One icon glyph, centred by what it paints and fitted to `paintedSize`. It wraps OpticalGlyph
// (qs.Ui), which does the centring in every shell and, from Omarchy 4.5, the fitting as well: an
// older OpticalGlyph has no `paintedSize`, and assigning it would stop the whole file from loading.
// Everything in this plugin that draws a glyph goes through here, so the next change to that
// component is fixed in one place.
Item {
  id: root

  property string text: ""
  property string fontFamily: Style.font.family
  property real fontSize: Style.font.body
  // 0 keeps the glyph at fontSize; a size fits its painted bounds to it (4.5 and later).
  property real paintedSize: 0
  property color color: Commons.Color.foreground

  OpticalGlyph {
    id: glyph
    anchors.fill: parent
    text: root.text
    fontFamily: root.fontFamily
    fontSize: root.fontSize
    color: root.color

    Component.onCompleted: {
      if ("paintedSize" in glyph) glyph.paintedSize = Qt.binding(function () { return root.paintedSize; });
    }
  }
}
