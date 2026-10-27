import QtQuick
import qs.Commons
import ".."

// Content that is not always there: it grows from nothing to its own height and fades in when `shown`
// turns on, and folds away when it turns off, so the popover below it moves smoothly and not in a jump.
// With animations off (Fx.ms is 0) it simply appears. Put the content inside it, set its width, and
// nothing else: its height is the content's.
Item {
  id: root

  property bool shown: true
  default property alias content: holder.data

  readonly property real naturalHeight: holder.childrenRect.height
  // Content is clipped while it grows or folds, and not once it has settled: a focus ring is drawn
  // a little outside its control, and a clip at rest would cut it.
  readonly property bool settled: shown && Math.abs(height - naturalHeight) < 0.5

  clip: !settled
  height: shown ? naturalHeight : 0
  opacity: shown ? 1 : 0
  visible: height > 0.5 || opacity > 0.01

  Behavior on height { NumberAnimation { duration: Fx.ms(170); easing.type: Easing.OutCubic } }
  Behavior on opacity { NumberAnimation { duration: Fx.ms(150); easing.type: Easing.OutCubic } }

  Item {
    id: holder
    width: parent.width
    height: childrenRect.height
  }
}
