import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import "../views"
import "../Model.js" as Model

// A bounded whole number: a track you drag or step with the keys, and the number beside it for an
// exact value. `steered` fires on every step, `settled` when the value is final (the pointer lets
// go, or a number is typed); the row debounces the first and sends the second at once.
//
// Keyboard: Left and Right step by one, Page Up and Page Down by a tenth of the range, Home and End
// jump to the ends. Up and Down are left alone, so they still move between the settings.
Item {
  id: root

  property var spec: ({})
  property var value
  property color foreground: Commons.Color.popups.text
  property string fontFamily: Style.font.family
  property string accessibleName: ""
  property bool refused: false
  property alias entry: numberEntry

  signal steered(var value)
  signal settled(var value)
  signal rejected(string message)
  signal edited()

  readonly property real minimum: Number(spec.min)
  readonly property real maximum: Model.sliderMax(spec)
  readonly property real live: slider.liveValue

  implicitWidth: Style.space(400)
  implicitHeight: Math.max(track.implicitHeight, numberEntry.implicitHeight)

  function clamp(v) {
    return Math.max(root.minimum, Math.min(root.maximum, Math.round(v)));
  }

  function step(delta) {
    var base = isFinite(Number(root.value)) ? Number(root.value) : root.minimum;
    var next = root.clamp(base + delta);
    if (next === base) return;
    root.steered(next);
  }

  Item {
    id: track
    anchors.left: parent.left
    anchors.right: numberEntry.left
    anchors.rightMargin: Style.space(14)
    anchors.verticalCenter: parent.verticalCenter
    implicitHeight: slider.implicitHeight
    height: implicitHeight
    activeFocusOnTab: true

    Accessible.role: Accessible.Slider
    Accessible.name: root.accessibleName
    Accessible.description: Model.formatValue(root.spec, root.value)

    Keys.onPressed: function (event) {
      var page = Model.sliderPageStep(root.spec);
      switch (event.key) {
        case Qt.Key_Left: root.step(-1); break;
        case Qt.Key_Right: root.step(1); break;
        case Qt.Key_PageDown: root.step(-page); break;
        case Qt.Key_PageUp: root.step(page); break;
        case Qt.Key_Home: root.step(root.minimum - Number(root.value)); break;
        case Qt.Key_End: root.step(root.maximum - Number(root.value)); break;
        default: return;
      }
      event.accepted = true;
    }

    PanelSlider {
      id: slider
      anchors.left: parent.left
      anchors.right: parent.right
      anchors.verticalCenter: parent.verticalCenter
      bar: QtObject {
        readonly property color foreground: root.foreground
        readonly property color background: Commons.Color.popups.background
      }
      minimum: root.minimum
      maximum: root.maximum
      step: 1
      integer: true
      value: isFinite(Number(root.value)) ? Number(root.value) : root.minimum
      onMoved: function (v) { root.steered(v) }
      onReleased: function (v) { root.settled(v) }
    }

    FocusRing {}
  }

  NumberEntry {
    id: numberEntry
    anchors.right: parent.right
    anchors.verticalCenter: parent.verticalCenter
    spec: root.spec
    value: root.value
    refused: root.refused
    foreground: root.foreground
    fontFamily: root.fontFamily
    accessibleName: root.accessibleName
    onCommitted: function (v) { root.settled(v) }
    onRejected: function (m) { root.rejected(m) }
    onEdited: root.edited()
  }
}
