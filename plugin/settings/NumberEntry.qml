import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import "../views"
import "../Model.js" as Model

// A whole number typed into a field, with its unit beside it. Bytes are typed as a size ("32 MiB").
// Enter or leaving the field commits; Up and Down step by one (Shift by ten); Esc puts the saved
// value back, or, when nothing was changed, is left for the panel (which closes). This file checks
// the shape of what was typed (Model.parseNumber); the range is ferryd's, and its refusal with the
// allowed values is what the row shows.
Item {
  id: root

  property var spec: ({})
  // The saved value, or the one being steered by a slider next to this field.
  property var value
  property color foreground: Commons.Color.popups.text
  property string fontFamily: Style.font.family
  property real fieldWidth: Style.space(spec && spec.unit === "bytes" ? 112 : 84)
  property string accessibleName: ""
  property alias field: input

  signal committed(var value)
  signal rejected(string message)
  signal edited()

  // The user typed since the field last showed `value`. Until then a change made elsewhere (the
  // `config` event, a restart) shows at once, even while the field has the focus (D-3).
  property bool typed: false
  // ferryd refused the value typed: it stays in the field to be fixed, and Esc puts the saved one back.
  property bool refused: false
  onRefusedChanged: if (refused) typed = true

  readonly property bool showUnit: spec && spec.unit !== undefined && spec.unit !== "" && spec.unit !== "bytes"

  implicitWidth: row.implicitWidth
  implicitHeight: row.implicitHeight

  function sync() {
    input.text = Model.editableNumber(root.spec, root.value);
    root.typed = false;
  }

  function commit() {
    var r = Model.parseNumber(root.spec, input.text);
    if (!r.ok) {
      root.rejected(r.message);
      return;
    }
    input.text = Model.editableNumber(root.spec, r.value);
    root.typed = false;
    if (r.value !== root.value) root.committed(r.value);
  }

  function nudge(delta) {
    var r = Model.parseNumber(root.spec, input.text);
    var base = r.ok ? r.value : Number(root.value);
    if (!isFinite(base)) return;
    var next = Math.max(Number(root.spec.min), Math.min(Model.fieldMax(root.spec), base + delta));
    input.text = Model.editableNumber(root.spec, next);
    if (next !== root.value) root.committed(next);
  }

  onValueChanged: if (!root.typed) sync()
  Component.onCompleted: sync()

  Row {
    id: row
    spacing: Style.space(6)

    TextField {
      id: input
      width: root.fieldWidth
      horizontalAlignment: TextInput.AlignRight
      foreground: root.foreground
      verticalPadding: Style.space(4)
      font.family: root.fontFamily
      font.pixelSize: Style.font.body
      font.features: { "tnum": 1 }
      selectByMouse: true
      activeFocusOnTab: true
      Accessible.name: root.accessibleName
      Accessible.role: Accessible.EditableText

      onTextEdited: {
        root.typed = true;
        root.edited();
      }
      onEditingFinished: root.commit()
      onActiveFocusChanged: if (!activeFocus) root.sync()

      Keys.onPressed: function (event) {
        if (event.key === Qt.Key_Up || event.key === Qt.Key_Down) {
          root.nudge((event.key === Qt.Key_Up ? 1 : -1) * (event.modifiers & Qt.ShiftModifier ? 10 : 1));
          event.accepted = true;
        } else if (event.key === Qt.Key_Escape && input.text !== Model.editableNumber(root.spec, root.value)) {
          root.sync();
          event.accepted = true;
        }
      }

      FocusRing {}
    }

    Text {
      visible: root.showUnit
      anchors.verticalCenter: input.verticalCenter
      textFormat: Text.PlainText
      text: root.showUnit ? String(root.spec.unit) : ""
      color: Qt.darker(root.foreground, 1.4)
      font.family: root.fontFamily
      font.pixelSize: Style.font.bodySmall
    }
  }
}
