import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import ".."
import "../Model.js" as Model
import "../Strings.js" as Strings

// Rename a peer in place: a field with the current name selected, the rule of
// `peer.name` checked as the user types (the same rule ferryd applies, which has the last word), and
// Save. Enter saves, Escape cancels. What ferryd refuses with, it says here, under the field.
Column {
  id: root

  property string value: ""
  // For Model.renameCheck: { me, fingerprint, current, others: [{ name, fingerprint }] }
  property var context: ({})
  property bool busy: false
  property color foreground: Commons.Color.popups.text
  property string fontFamily: Style.font.family
  property alias field: input

  readonly property color dim: Qt.darker(foreground, 1.4)
  readonly property var check: Model.renameCheck(input.text, root.context)
  // ferryd's own answer when it refused (fail()), good for as long as the text is the one it refused
  property string refusal: ""
  property string refusedText: ""
  property bool touched: false
  // The rule's complaint appears once the text has been touched: an untouched field is not an error.
  readonly property string shownError: root.refusal !== "" && input.text === root.refusedText ? root.refusal
    : ((input.text !== root.value || root.touched) && !root.check.ok ? root.check.error : "")

  signal accepted(string name)
  signal canceled()

  width: parent ? parent.width : implicitWidth
  spacing: Style.space(4)

  // ferryd said no: show its words under the field until the text changes.
  function fail(message) {
    root.refusal = message;
    root.refusedText = input.text;
  }

  function begin() {
    input.text = root.value;
    root.touched = false;
    root.refusal = "";
    input.forceActiveFocus();
    input.selectAll();
  }

  function submit() {
    if (root.busy) return;
    var c = root.check;
    if (!c.ok) { root.touched = true; return; }
    if (c.unchanged) { root.canceled(); return; }
    root.accepted(c.name);
  }

  Row {
    width: parent.width
    spacing: Style.space(8)

    TextField {
      id: input
      width: parent.width - saveButton.width - parent.spacing
      anchors.verticalCenter: parent.verticalCenter
      text: root.value
      placeholderText: Strings.t("rename.placeholder")
      foreground: root.foreground
      font.family: root.fontFamily
      font.pixelSize: Style.font.body
      enabled: !root.busy
      selectByMouse: true
      Accessible.name: Strings.t("peer.action.rename")

      Keys.onReturnPressed: root.submit()
      Keys.onEnterPressed: root.submit()
      Keys.onEscapePressed: function (event) { event.accepted = true; root.canceled(); }
      // a single line has no use for these, and they would move the focus off the field
      Keys.onUpPressed: function (event) { event.accepted = true; }
      Keys.onDownPressed: function (event) { event.accepted = true; }
    }

    Button {
      id: saveButton
      anchors.verticalCenter: parent.verticalCenter
      text: root.busy ? Strings.t("rename.saving") : Strings.t("rename.save")
      bordered: true
      active: root.check.ok && !root.check.unchanged
      focusable: true
      enabled: !root.busy
      foreground: root.foreground
      fontFamily: root.fontFamily
      fontSize: Style.font.bodySmall
      Accessible.name: text
      onClicked: root.submit()
      FocusRing {}
    }
  }

  Text {
    width: parent.width
    textFormat: Text.PlainText
    text: root.shownError !== "" ? root.shownError : Strings.t("rename.hint")
    color: root.shownError !== "" ? Commons.Color.urgent : root.dim
    font.family: root.fontFamily
    font.pixelSize: Style.font.caption
    wrapMode: Text.WordWrap
  }
}
