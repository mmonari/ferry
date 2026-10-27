import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { load } from "./harness.mjs";

// The Settings view's rules: how config.schema becomes groups, controls, words and values.
// They live in Model.js so that they are tested where they are written; the QML only draws them.

const Model = load("Model.js");
const Strings = load("Strings.js");

const HERE = dirname(fileURLToPath(import.meta.url));
const SNAPSHOT = join(HERE, "..", "..", "..", "crates", "ferryd", "src", "config_api", "snapshots",
  "ferryd__config_api__tests__the_schema_of_every_key.snap");

// The schema the real ferryd serves, from the snapshot that pins it.
function realSchema() {
  const text = readFileSync(SNAPSHOT, "utf8");
  return JSON.parse(text.split("---\n").slice(2).join("---\n")).keys;
}

const int = (key, min, max, extra = {}) => ({ key, type: "int", default: min, min, max, applies: "live", doc: "doc", ...extra });
const bool = (key, extra = {}) => ({ key, type: "bool", default: false, applies: "live", doc: "doc", ...extra });

// ------------------------------------------------------------------------------- groups

test("keys group by their dotted prefix, in the order people reach for them", () => {
  const groups = Model.settingsGroups(realSchema());
  assert.deepEqual(groups.map((g) => g.id), ["crossing", "input", "clipboard", "discovery", "notify", "peer", "safety"]);
  assert.deepEqual(groups[0].keys.map((k) => k.key), [
    "crossing.edge_resistance_px", "crossing.edge_dwell_ms", "crossing.corner_dead_zone_px",
    "crossing.block_while_button_held", "crossing.escape_chord", "crossing.go_chord", "crossing.edge_glow"
  ], "keys keep the schema's order inside a group");
  assert.equal(groups.reduce((n, g) => n + g.keys.length, 0), realSchema().length, "every key is in a group");
  assert.deepEqual(groups.find((g) => g.id === "notify").keys.map((k) => k.key), ["notify.enabled"]);
  assert.equal(groups.find((g) => g.id === "notify").title, "Notifications");
});

test("a prefix this build has never heard of comes after the known ones, in schema order", () => {
  const groups = Model.settingsGroups([
    bool("future.zeta"), bool("safety.a"), bool("alpha.one"), bool("crossing.x"), bool("future.beta"), bool("loose")
  ]);
  assert.deepEqual(groups.map((g) => g.id), ["crossing", "safety", "future", "alpha", "other"]);
  assert.deepEqual(groups[2].keys.map((k) => k.key), ["future.zeta", "future.beta"]);
  assert.equal(groups[2].title, "Future", "an unknown prefix is titled from the prefix");
  assert.equal(groups[0].title, "Crossing");
  assert.equal(groups[1].title, "Safety");
});

test("a spec without a usable key is dropped, and no schema is no groups", () => {
  assert.deepEqual(Model.settingsGroups(null), []);
  assert.deepEqual(Model.settingsGroups([]), []);
  const groups = Model.settingsGroups([null, 5, {}, { key: "" }, { key: 7 }, bool("crossing.ok")]);
  assert.equal(groups.length, 1);
  assert.equal(groups[0].keys.length, 1);
});

test("peer is titled for people, not for the config file", () => {
  assert.equal(Model.groupTitle("peer"), "This machine");
});

// ------------------------------------------------------------------------------ controls

test("the control follows the type and the range", () => {
  const by = Object.fromEntries(realSchema().map((k) => [k.key, Model.controlFor(k)]));
  assert.deepEqual(by, {
    "peer.name": "text",
    "peer.port": "number",                       // 0 to 65535 is not a thing you steer by eye
    "crossing.edge_resistance_px": "slider",     // 0 to 200
    "crossing.edge_dwell_ms": "slider",          // 0 to 65535, slid over its usual 0 to 1000 (slider_max)
    "crossing.corner_dead_zone_px": "slider",
    "crossing.block_while_button_held": "toggle",
    "crossing.escape_chord": "chord",
    "crossing.go_chord": "chord",
    "crossing.edge_glow": "toggle",
    "input.capture": "dropdown",
    "input.emulation": "static",                 // one choice: nothing to choose
    "input.scroll_invert": "toggle",
    "clipboard.enabled": "toggle",
    "clipboard.max_bytes": "number",
    "discovery.mdns": "toggle",
    "discovery.advertise": "toggle",
    "safety.lock_check_budget_ms": "slider",     // 50 to 100
    "notify.enabled": "toggle"
  });
});

test("the edge of the slider range is 1000 steps", () => {
  assert.equal(Model.controlFor(int("a.b", 0, 1000)), "slider");
  assert.equal(Model.controlFor(int("a.b", 0, 1001)), "number");
  assert.equal(Model.controlFor(int("a.b", 5, 5)), "number", "a range of nothing is not steered");
  assert.equal(Model.controlFor({ key: "a.b", type: "int" }), "number", "an int with no range is a field");
});

test("a list that QML hands back as an array-like (a Repeater's modelData does) is still a list", () => {
  // Array.isArray is false for it, and the real shell showed a dropdown as a bare label because of that.
  const sequence = { length: 3, 0: "a", 1: "b", 2: "c" };
  assert.equal(Array.isArray(sequence), false);
  assert.equal(Model.controlFor({ key: "a.b", type: "enum", choices: sequence }), "dropdown");
  assert.deepEqual(Model.choiceOptions({ key: "a.b", choices: sequence }).map((o) => o.value), ["a", "b", "c"]);
  assert.equal(Model.controlFor({ key: "a.b", type: "enum", choices: { length: 1, 0: "only" } }), "static");
  assert.equal(Model.controlFor({ key: "a.b", type: "enum", choices: "abc" }), "static", "a string is not a list");
  assert.equal(Model.controlFor({ key: "a.b", type: "enum", choices: null }), "static");
});

test("a type this build does not know is shown as text, never hidden", () => {
  assert.equal(Model.controlFor({ key: "a.b", type: "unknown" }), "text");
  assert.equal(Model.controlFor({ key: "a.b", type: "string" }), "text");
  assert.equal(Model.controlFor({ key: "a.b", type: "name" }), "text");
  assert.equal(Model.controlFor({ key: "a.b", type: "enum", choices: ["x", "y"] }), "dropdown");
  assert.equal(Model.controlFor({ key: "a.b", type: "enum" }), "static");
});

test("a slider steps by a tenth of its range with Page Up and Down, never less than five", () => {
  assert.equal(Model.sliderPageStep(int("a.b", 0, 200)), 20);
  assert.equal(Model.sliderPageStep(int("a.b", 50, 100)), 5);
  assert.equal(Model.sliderPageStep(int("a.b", 0, 10)), 5);
});

// --------------------------------------------------------------------------------- words

test("the keys that exist today have words of their own", () => {
  const label = Object.fromEntries(realSchema().map((k) => [k.key, Model.settingLabel(k)]));
  assert.equal(label["crossing.edge_resistance_px"], "Edge resistance");
  assert.equal(label["crossing.escape_chord"], "Escape chord");
  assert.equal(label["crossing.corner_dead_zone_px"], "Dead corners");
  assert.equal(label["peer.name"], "Machine name");
  for (const [key, text] of Object.entries(label)) assert.ok(text.length > 0 && !text.includes("_") && !text.includes("."), `${key}: ${text}`);
});

test("a key with no words of its own is worded from its name and unit", () => {
  assert.equal(Model.settingLabel(int("net.retry_delay_ms", 0, 10, { unit: "ms" })), "Retry delay");
  assert.equal(Model.settingLabel(int("net.window_px", 0, 10, { unit: "px" })), "Window");
  assert.equal(Model.settingLabel(bool("net.use_ipv6")), "Use ipv6");
  assert.equal(Model.settingLabel(int("net.bytes", 0, 10, { unit: "bytes" })), "Bytes", "a name that is only its unit keeps it");
  assert.equal(Model.settingLabel(bool("loose")), "Loose");
});

test("enum values have words, and an unknown value is worded from itself", () => {
  const capture = realSchema().find((k) => k.key === "input.capture");
  assert.deepEqual(Model.choiceOptions(capture).map((o) => o.value), ["auto", "portal", "direct", "layer-shell", "off"]);
  assert.equal(Model.choiceLabel(capture, "layer-shell"), "Layer shell");
  assert.match(Model.choiceLabel(capture, "off"), /target only/);
  assert.equal(Model.choiceLabel({ key: "x.y" }, "dark_mode"), "Dark mode");
});

test("the help text is the schema's own doc", () => {
  assert.equal(Model.settingHelp({ doc: "  Push this far.  " }), "Push this far.");
  assert.equal(Model.settingHelp({}), "");
});

// -------------------------------------------------------------------------------- values

test("values read the way a person says them", () => {
  const k = Object.fromEntries(realSchema().map((s) => [s.key, s]));
  assert.equal(Model.formatValue(k["crossing.edge_resistance_px"], 40), "40 px");
  assert.equal(Model.formatValue(k["crossing.edge_dwell_ms"], 0), "0 ms");
  assert.equal(Model.formatValue(k["peer.port"], 4243), "4243");
  assert.equal(Model.formatValue(k["clipboard.max_bytes"], 33554432), "32 MiB");
  assert.equal(Model.formatValue(k["crossing.block_while_button_held"], true), "On");
  assert.equal(Model.formatValue(k["crossing.block_while_button_held"], false), "Off");
  assert.equal(Model.formatValue(k["crossing.escape_chord"], "Super+Ctrl+Escape"), "Super+Ctrl+Esc");
  assert.equal(Model.formatValue(k["input.capture"], "layer-shell"), "Layer shell");
  assert.equal(Model.formatValue(k["peer.name"], "desk"), "desk");
  assert.equal(Model.formatValue(k["peer.name"], null), "");
});

test("a byte count reads as a size, exactly when it can", () => {
  assert.equal(Model.formatBytes(0), "0 B");
  assert.equal(Model.formatBytes(1023), "1023 B");
  assert.equal(Model.formatBytes(1024), "1 KiB");
  assert.equal(Model.formatBytes(33554432), "32 MiB");
  assert.equal(Model.formatBytes(1500000), "1.4 MiB");
  assert.equal(Model.formatBytes(1073741824), "1 GiB");
  assert.equal(Model.formatBytes(-1), "-1", "nothing sensible to say: say the number");
});

test("typed numbers: digits, a unit the key has, a size for bytes", () => {
  const px = int("crossing.edge_resistance_px", 0, 200, { unit: "px" });
  const bytes = int("clipboard.max_bytes", 0, 9223372036854775807, { unit: "bytes" });
  const plain = int("peer.port", 0, 65535);
  assert.deepEqual(Model.parseNumber(px, "40"), { ok: true, value: 40 });
  assert.deepEqual(Model.parseNumber(px, " 40 "), { ok: true, value: 40 });
  assert.deepEqual(Model.parseNumber(px, "40px"), { ok: true, value: 40 });
  assert.deepEqual(Model.parseNumber(px, "40 px"), { ok: true, value: 40 });
  assert.deepEqual(Model.parseNumber(px, "-5"), { ok: true, value: -5 }, "the range is ferryd's to refuse, with its hint");
  assert.deepEqual(Model.parseNumber(px, "900"), { ok: true, value: 900 });
  assert.deepEqual(Model.parseNumber(plain, "4243"), { ok: true, value: 4243 });
  assert.deepEqual(Model.parseNumber(bytes, "32 MiB"), { ok: true, value: 33554432 });
  assert.deepEqual(Model.parseNumber(bytes, "64M"), { ok: true, value: 67108864 });
  assert.deepEqual(Model.parseNumber(bytes, "1.5 KiB"), { ok: true, value: 1536 });
  assert.deepEqual(Model.parseNumber(bytes, "1048576"), { ok: true, value: 1048576 });
  assert.deepEqual(Model.parseNumber(bytes, "2 GiB"), { ok: true, value: 2147483648 });
});

test("typed text that is not a whole number is refused here, before a call is wasted", () => {
  const px = int("crossing.edge_resistance_px", 0, 200, { unit: "px" });
  const bytes = int("clipboard.max_bytes", 0, 9223372036854775807, { unit: "bytes" });
  for (const text of ["", "  ", "abc", "4.5", "12 ms", "1e3", "--1", "0x10", "40 40", "9007199254740993"]) {
    const r = Model.parseNumber(px, text);
    assert.equal(r.ok, false, JSON.stringify(text));
    assert.match(r.message, /whole number/);
  }
  assert.equal(Model.parseNumber(bytes, "3 parsecs").ok, false);
  assert.match(Model.parseNumber(bytes, "x").message, /32 MiB/, "bytes tell how to write a size");
  assert.equal(Model.parseNumber(px, null).ok, false);
});

test("a text value goes on trimmed, because the edges of a name are never meant", () => {
  assert.deepEqual(Model.parseText("  lap "), { ok: true, value: "lap" });
  assert.deepEqual(Model.parseText(""), { ok: true, value: "" });
  assert.deepEqual(Model.parseText(null), { ok: true, value: "" });
});

test("the number a field can hold is exact", () => {
  assert.equal(Model.fieldMax(int("a.b", 0, 200)), 200);
  assert.equal(Model.fieldMax(int("a.b", 0, 9223372036854775807)), 9007199254740991);
  assert.equal(Model.fieldMax({ key: "a.b" }), 9007199254740991);
});

test("a value is the default when it equals it", () => {
  const spec = int("crossing.corner_dead_zone_px", 0, 200, { default: 8 });
  assert.equal(Model.isDefault(spec, 8), true);
  assert.equal(Model.isDefault(spec, 9), false);
  assert.equal(Model.isDefault({ key: "a.b", type: "name", default: "" }, ""), true);
  assert.equal(Model.isDefault({ key: "a.b", type: "bool", default: true }, false), false);
});

// ------------------------------------------------------------------------------ applies

test("the badge says when a saved value takes effect, in words", () => {
  assert.equal(Model.appliesOf({ applies: "live" }).badge, "Live");
  assert.equal(Model.appliesOf({ applies: "restart" }).badge, "Restart");
  assert.equal(Model.appliesOf({ applies: "setup" }).badge, "Setup");
  assert.match(Model.appliesOf({ applies: "restart" }).long, /ferryd restarts/);
  assert.match(Model.appliesOf({ applies: "setup" }).long, /ferry setup/);
  const unknown = Model.appliesOf({ applies: "unknown" });
  assert.equal(unknown.badge, "", "a moment this build does not know shows no badge");
  assert.equal(unknown.id, "");
});

test("keys waiting for a restart: a list from ferryd, or nothing", () => {
  assert.deepEqual(Model.pendingKeys(["peer.port", 3, "input.capture"]), ["peer.port", "input.capture"]);
  assert.deepEqual(Model.pendingKeys(undefined), []);
  assert.deepEqual(Model.pendingKeys("peer.port"), []);
  assert.equal(Model.isPending(["peer.port"], "peer.port"), true);
  assert.equal(Model.isPending([], "peer.port"), false);
  assert.equal(Model.isPending(null, "peer.port"), false);
});

test("the banner counts the changes that wait, and says nothing when none do", () => {
  assert.deepEqual(Model.pendingBanner([]), { count: 0, pill: "", text: "", command: "" });
  const one = Model.pendingBanner(["peer.port"]);
  assert.equal(one.pill, "1 to restart");
  assert.equal(one.text, "1 saved change waits for a restart of ferryd");
  assert.equal(one.command, "systemctl --user restart ferryd");
  const two = Model.pendingBanner(["peer.port", "input.capture"]);
  assert.equal(two.pill, "2 to restart");
  assert.equal(two.text, "2 saved changes wait for a restart of ferryd");
});

// -------------------------------------------------------------------------------- errors

test("ferryd's refusal loses the key path, reads as a sentence, and its hint says what is allowed", () => {
  const e = Model.settingError({ code: "bad_value", message: "crossing.edge_resistance_px: 900 is out of range", hint: "0 to 200 px" });
  assert.deepEqual(e, { code: "bad_value", message: "900 is out of range.", hint: "Allowed: 0 to 200 px." });
  assert.equal(Model.settingError({ code: "unknown_key", message: "there is no config key \"x\"" }).hint, "");
  assert.equal(Model.settingError({ code: "read_only", message: "x cannot be written", hint: "nothing was changed" }).code, "read_only");
});

test("a transport failure gets words, and no error is no line", () => {
  assert.match(Model.settingError({ code: "timeout", message: "config.set did not answer" }).message, /did not answer in time/);
  assert.match(Model.settingError({ code: "not_connected", message: "ferryd went away" }).message, /not running/);
  assert.equal(Model.settingError(null), null);
  assert.equal(Model.settingError("oops"), null);
});

// --------------------------------------------------------------------------------- states

test("what the view can do: off, a daemon without config.schema, loading, ready", () => {
  const schema = realSchema();
  assert.equal(Model.settingsState(false, true, schema, {}), "off");
  assert.equal(Model.settingsState(true, false, null, null), "unsupported");
  assert.equal(Model.settingsState(true, true, null, null), "loading");
  assert.equal(Model.settingsState(true, true, schema, null), "loading");
  assert.equal(Model.settingsState(true, true, null, {}), "loading");
  assert.equal(Model.settingsState(true, true, schema, { "peer.name": "" }), "ready");
});

// ------------------------------------------------------------------------------- columns

test("groups split over two columns in order, as evenly as their rows allow", () => {
  const groups = Model.settingsGroups(realSchema());
  const [a, b] = Model.settingsColumns(groups, 2);
  assert.deepEqual(a.concat(b).map((g) => g.id), groups.map((g) => g.id), "order is kept, so Tab reads down one column then the next");
  assert.deepEqual(a.map((g) => g.id), ["crossing", "input"]);
  assert.deepEqual(b.map((g) => g.id), ["clipboard", "discovery", "notify", "peer", "safety"]);
});

test("one column is every group, and one group cannot be split", () => {
  const groups = Model.settingsGroups(realSchema());
  assert.deepEqual(Model.settingsColumns(groups, 1), [groups]);
  assert.equal(Model.settingsColumns(groups.slice(0, 1), 2).length, 1);
  assert.deepEqual(Model.settingsColumns([], 2), [[]]);
});

// ------------------------------------------------------------------------ chord recorder

const Q = { shift: 0x02000000, ctrl: 0x04000000, alt: 0x08000000, meta: 0x10000000 };
const K = { escape: 0x01000000, f12: 0x0100003b, f1: 0x01000030, space: 0x20, a: 0x41, z: 0x5a, zero: 0x30, nine: 0x39,
  tab: 0x01000001, left: 0x01000012, ctrl: 0x01000021, meta: 0x01000022, shift: 0x01000020, alt: 0x01000023,
  home: 0x01000010, end: 0x01000011, del: 0x01000007, bs: 0x01000003, pause: 0x01000008, ret: 0x01000004 };

test("modifier flags come out in the order the default chord is spelt", () => {
  assert.deepEqual(Model.chordModifiers(Q.meta | Q.ctrl), ["Super", "Ctrl"]);
  assert.deepEqual(Model.chordModifiers(Q.shift | Q.alt | Q.ctrl | Q.meta), ["Super", "Ctrl", "Alt", "Shift"]);
  assert.deepEqual(Model.chordModifiers(0), []);
});

test("holding modifiers keeps the recorder listening", () => {
  assert.deepEqual(Model.chordFromKey(K.meta, Q.meta, ""), { state: "holding", held: ["Super"] });
  assert.deepEqual(Model.chordFromKey(K.ctrl, Q.meta | Q.ctrl, ""), { state: "holding", held: ["Super", "Ctrl"] });
  assert.equal(Model.chordFromKey(K.shift, Q.shift, "").state, "holding");
  assert.equal(Model.chordFromKey(K.alt, Q.alt, "").state, "holding");
});

test("Escape alone cancels, Escape after modifiers is the key of the chord", () => {
  assert.deepEqual(Model.chordFromKey(K.escape, 0, ""), { state: "cancel" });
  const done = Model.chordFromKey(K.escape, Q.meta | Q.ctrl, "");
  assert.equal(done.state, "done");
  assert.equal(done.chord, "Super+Ctrl+Escape");
});

test("the go chord is the modifiers, finished by any arrow key", () => {
  const K_LEFT = 0x01000012, K_DOWN = 0x01000015;
  assert.ok(Model.chordIsModifiersOnly("crossing.go_chord"));
  assert.ok(!Model.chordIsModifiersOnly("crossing.escape_chord"));
  assert.deepEqual(Model.chordFromKey(K.alt, Q.meta | Q.ctrl | Q.alt, "", true), { state: "holding", held: ["Super", "Ctrl", "Alt"] });
  const done = Model.chordFromKey(K_LEFT, Q.meta | Q.ctrl | Q.alt, "", true);
  assert.equal(done.state, "done");
  assert.equal(done.chord, "Super+Ctrl+Alt");
  assert.equal(Model.chordFromKey(K_DOWN, Q.ctrl | Q.shift, "", true).chord, "Ctrl+Shift");
  assert.equal(Model.chordFromKey(K.a, Q.meta | Q.ctrl, "a", true).state, "refused");
  assert.deepEqual(Model.chordFromKey(K.escape, 0, "", true), { state: "cancel" });
  // the escape chord still refuses an arrow
  assert.equal(Model.chordFromKey(K_LEFT, Q.meta | Q.ctrl, "").state, "refused");
});

test("the last key completes the chord, spelt as ferryd saves it", () => {
  assert.equal(Model.chordFromKey(K.f12, Q.ctrl | Q.alt, "").chord, "Ctrl+Alt+F12");
  assert.equal(Model.chordFromKey(K.f1, Q.meta | Q.shift, "").chord, "Super+Shift+F1");
  assert.equal(Model.chordFromKey(K.a, Q.meta | Q.ctrl | Q.alt, "a").chord, "Super+Ctrl+Alt+A");
  assert.equal(Model.chordFromKey(K.z, Q.ctrl | Q.alt, "z").chord, "Ctrl+Alt+Z");
  assert.equal(Model.chordFromKey(K.zero, Q.ctrl | Q.alt, "0").chord, "Ctrl+Alt+0");
  assert.equal(Model.chordFromKey(K.nine, Q.ctrl | Q.alt, "9").chord, "Ctrl+Alt+9");
  assert.equal(Model.chordFromKey(K.space, Q.meta | Q.alt, " ").chord, "Super+Alt+Space");
  assert.equal(Model.chordFromKey(K.home, Q.meta | Q.alt, "").chord, "Super+Alt+Home");
  assert.equal(Model.chordFromKey(K.end, Q.meta | Q.alt, "").chord, "Super+Alt+End");
  assert.equal(Model.chordFromKey(K.del, Q.ctrl | Q.alt, "").chord, "Ctrl+Alt+Delete");
  assert.equal(Model.chordFromKey(K.bs, Q.ctrl | Q.alt, "").chord, "Ctrl+Alt+Backspace");
  assert.equal(Model.chordFromKey(K.pause, Q.ctrl | Q.alt, "").chord, "Ctrl+Alt+Pause");
});

test("a key ferryd has no name for is refused in words, not sent", () => {
  const tab = Model.chordFromKey(K.tab, Q.meta | Q.ctrl, "");
  assert.equal(tab.state, "refused");
  assert.equal(tab.name, "Tab");
  assert.match(tab.message, /Tab cannot be part of the chord/);
  assert.match(tab.message, /F1 to F12/);
  assert.equal(Model.chordFromKey(K.left, Q.meta | Q.ctrl, "").name, "Left");
  assert.equal(Model.chordFromKey(K.ret, Q.meta | Q.ctrl, "\r").name, "Return");
  assert.equal(Model.chordFromKey(0x2c, Q.meta | Q.ctrl, ",").name, ",", "punctuation is named by itself");
  assert.equal(Model.chordFromKey(0x01000047, Q.meta | Q.ctrl, "").name, "F24");
  assert.equal(Model.chordFromKey(0x1234, Q.meta | Q.ctrl, "").name, "that key");
});

test("a chord with one modifier is complete here; ferryd says it needs two", () => {
  // The recorder reports what was pressed. The two-modifier rule is ferryd's, and its refusal
  // ("at least two modifiers", with the hint) is shown under the control as it is.
  const one = Model.chordFromKey(K.f12, Q.ctrl, "");
  assert.equal(one.state, "done");
  assert.equal(one.chord, "Ctrl+F12");
});

test("the key caps of a chord, with the word for Escape the tooltip uses", () => {
  assert.deepEqual(Model.chordCaps("Super+Ctrl+Escape"), ["Super", "Ctrl", "Esc"]);
  assert.deepEqual(Model.chordCaps("Ctrl+Alt+F12"), ["Ctrl", "Alt", "F12"]);
  assert.deepEqual(Model.chordCaps(""), []);
  assert.deepEqual(Model.chordCaps(null), []);
});

// -------------------------------------------------------------------------------- strings

test("every polished label, choice and group in Strings.js belongs to a key the schema has", () => {
  const keys = realSchema();
  const bare = Object.fromEntries(keys.map((k) => [k.key, k]));
  for (const k of keys) assert.ok(Strings.has(`settings.label.${k.key}`), `${k.key} has no label`);
  for (const k of keys.filter((s) => s.type === "enum"))
    for (const c of k.choices) assert.ok(Strings.has(`settings.choice.${k.key}.${c}`), `${k.key} = ${c} has no label`);
  for (const g of new Set(keys.map((k) => k.key.split(".")[0]))) assert.ok(Strings.has(`settings.group.${g}`), `${g} has no title`);
  assert.ok(bare["crossing.escape_chord"], "the fixture is the real schema");
});

test("Strings.has tells polish from a typo", () => {
  assert.equal(Strings.has("settings.group.crossing"), true);
  assert.equal(Strings.has("settings.group.nope"), false);
  assert.equal(Strings.t("settings.group.nope"), "settings.group.nope");
});

test("slider_max caps the slider, and the field still takes the whole range", () => {
  const dwell = realSchema().find((k) => k.key === "crossing.edge_dwell_ms");
  assert.equal(Model.sliderMax(dwell), 1000);
  assert.equal(Model.fieldMax(dwell), 65535);
  assert.equal(Model.sliderPageStep(dwell), 100);
  assert.equal(Model.sliderMax({ min: 0, max: 200 }), 200, "no slider_max: the slider goes to max");
  assert.equal(Model.sliderMax({ min: 0, max: 200, slider_max: 500 }), 200, "a slider_max above max is ignored");
});
