import { test } from "node:test";
import assert from "node:assert/strict";
import { readdirSync, readFileSync, statSync, lstatSync } from "node:fs";
import { join } from "node:path";
import { PLUGIN, load } from "./harness.mjs";

// The structural rules of this plugin that nothing else checks, and that fail silently in the
// shell: a missing qmldir line makes a type vanish, a symlink makes the manifest invalid.

const qmlAtRoot = readdirSync(PLUGIN).filter((f) => f.endsWith(".qml"));
const qmldir = readFileSync(join(PLUGIN, "qmldir"), "utf8");

// A file that declares a window (KeyboardPanel, PanelWindow, FloatingWindow), as its root or inside a
// Scope (EdgeGlowHost), cannot be compiled where
// there is no layer-shell, so it must not be reachable by type name from the files that the offscreen
// harness loads: it is loaded by URL instead, and stays out of the qmldir.
const isWindowFile = (file) => /^\s*(KeyboardPanel|PanelWindow|FloatingWindow)\s*\{/m.test(readFileSync(join(PLUGIN, file), "utf8"));

test("the qmldir lists every QML file at the plugin root, except the window files, which are loaded by URL", () => {
  const everything = qmlAtRoot.map((f) => readFileSync(join(PLUGIN, f), "utf8")).join("\n");
  for (const file of qmlAtRoot) {
    if (isWindowFile(file)) {
      assert.ok(!new RegExp(`\\b${file.replace(".", "\\.")}\\s*$`, "m").test(qmldir), `${file} is a window file and must stay out of qmldir`);
      assert.ok(everything.includes(`Qt.resolvedUrl("${file}")`), `${file} is a window file and nothing loads it by URL`);
    } else {
      assert.ok(new RegExp(`\\b${file.replace(".", "\\.")}\\s*$`, "m").test(qmldir), `${file} is not in qmldir`);
    }
  }
});

test("the qmldir names no file that does not exist, and singletons are marked as such", () => {
  for (const line of qmldir.split("\n").map((l) => l.trim()).filter(Boolean)) {
    const parts = line.split(/\s+/);
    const file = parts[parts.length - 1];
    assert.ok(qmlAtRoot.includes(file), `qmldir names ${file}, which is not at the plugin root`);
    const src = readFileSync(join(PLUGIN, file), "utf8");
    const isSingleton = /^pragma Singleton/m.test(src);
    assert.equal(parts[0] === "singleton", isSingleton, `${file}: qmldir and pragma disagree about being a singleton`);
  }
});

test("no file at the root is named like a type of qs.Ui, which an `import \".\"` would shadow it with", () => {
  const ui = readdirSync("/usr/share/omarchy/shell/Ui").filter((f) => f.endsWith(".qml"));
  for (const file of qmlAtRoot) assert.ok(!ui.includes(file), `${file} has the name of a qs.Ui type`);
});

test("the plugin folder holds no symlink (omarchy plugin validate refuses one)", () => {
  const walk = (dir) => {
    for (const name of readdirSync(dir)) {
      const path = join(dir, name);
      assert.ok(!lstatSync(path).isSymbolicLink(), `${path} is a symlink`);
      if (statSync(path).isDirectory()) walk(path);
    }
  };
  walk(PLUGIN);
});

test("the manifest names an entry point that exists, and the widget id is the one the code uses", () => {
  const m = JSON.parse(readFileSync(join(PLUGIN, "manifest.json"), "utf8"));
  assert.equal(m.schemaVersion, 1);
  assert.equal(m.id, "ferry.bar");
  assert.deepEqual(m.kinds, ["bar-widget"]);
  assert.ok(qmlAtRoot.includes(m.entryPoints.barWidget));
  const widget = readFileSync(join(PLUGIN, m.entryPoints.barWidget), "utf8");
  assert.match(widget, new RegExp(`moduleName:\\s*"${m.id.replace(".", "\\.")}"`), "moduleName must equal the manifest id (settings are written under it)");
  const ui = readFileSync(join(PLUGIN, "FerryUi.qml"), "utf8");
  assert.match(ui, /shell\.toggle\("ferry\.bar"/, "FerryUi addresses the widget by its manifest id");
});

test("the widget carries the four names the bar looks for", () => {
  const widget = readFileSync(join(PLUGIN, "FerryWidget.qml"), "utf8");
  for (const name of ["opened", "popoutSwitchClosing", "function open()", "function close()"])
    assert.ok(widget.includes(name), `FerryWidget.qml lacks ${name}`);
});

test("glyphs are never written as a literal private-use character in a source file", () => {
  const files = [];
  const walk = (dir) => {
    for (const name of readdirSync(dir)) {
      const path = join(dir, name);
      if (statSync(path).isDirectory()) walk(path);
      else if (/\.(qml|js)$/.test(name)) files.push(path);
    }
  };
  walk(PLUGIN);
  for (const f of files) {
    const text = readFileSync(f, "utf8");
    assert.ok(!/[-]|[\u{F0000}-\u{10FFFD}]/u.test(text), `${f} holds a literal private-use character; write the code point`);
  }
});

test("a file under views/, panels/ or settings/ that names a root-level type or singleton imports the parent folder", () => {
  const rootTypes = qmldir.split("\n").map((l) => l.trim().split(/\s+/)[l.trim().startsWith("singleton") ? 1 : 0]).filter(Boolean);
  for (const sub of ["views", "panels", "settings"]) {
    for (const name of readdirSync(join(PLUGIN, sub)).filter((f) => f.endsWith(".qml"))) {
      const src = readFileSync(join(PLUGIN, sub, name), "utf8");
      const body = src.replace(/^\s*\/\/.*$/gm, "");
      const used = rootTypes.filter((t) => new RegExp(`(^|[^\\w.])${t}(\\s*\\{|\\.)`, "m").test(body));
      if (used.length) assert.match(src, /^import "\.\."$/m, `${sub}/${name} uses ${used.join(", ")} but does not import ".."`);
    }
  }
});

// ---------------------------------------------------------------------------------------- the texts and glyphs

const Strings = load("Strings.js");
const Glyphs = load("Glyphs.js");

function sourceFiles() {
  const files = [];
  const walk = (dir) => {
    for (const name of readdirSync(dir)) {
      const path = join(dir, name);
      if (statSync(path).isDirectory()) walk(path);
      else if (/\.(qml|js)$/.test(name)) files.push(path);
    }
  };
  walk(PLUGIN);
  return files;
}

// The text between the parenthesis after `from` and its match, or null.
function argsAt(text, from) {
  let depth = 0;
  for (let i = from; i < text.length; i++) {
    if (text[i] === "(") depth++;
    else if (text[i] === ")" && --depth === 0) return text.slice(from + 1, i);
  }
  return null;
}

// the first argument of a call: up to the first comma that is not inside brackets or quotes
function firstArgument(args) {
  let depth = 0;
  let quote = "";
  for (let i = 0; i < args.length; i++) {
    const c = args[i];
    if (quote) { if (c === quote && args[i - 1] !== "\\") quote = ""; continue; }
    if (c === '"' || c === "'") quote = c;
    else if ("([{".includes(c)) depth++;
    else if (")]}".includes(c)) depth--;
    else if (c === "," && depth === 0) return args.slice(0, i);
  }
  return args;
}

test("every string key a plugin file names exists in Strings.js, so a typo is not found on screen", () => {
  const missing = [];
  let seen = 0;
  for (const file of sourceFiles()) {
    if (file.endsWith("Strings.js")) continue;
    const text = readFileSync(file, "utf8").replace(/^\s*\/\/.*$/gm, "");
    for (const m of text.matchAll(/Strings\.(?:t|has)\(/g)) {
      const args = argsAt(text, m.index + m[0].length - 1);
      if (args === null) continue;
      for (const k of firstArgument(args).matchAll(/"([a-z][a-z0-9_]*(?:\.[a-z0-9_]+)+)"/g)) {
        seen++;
        if (!Strings.has(k[1])) missing.push(`${file.slice(PLUGIN.length + 1)}: ${k[1]}`);
      }
    }
  }
  assert.ok(seen > 100, `only ${seen} keys were found: the scan is broken`);
  assert.deepEqual(missing, []);
});

test("every glyph a plugin file names exists in Glyphs.js", () => {
  const missing = [];
  let seen = 0;
  for (const file of sourceFiles()) {
    if (file.endsWith("Glyphs.js")) continue;
    const text = readFileSync(file, "utf8").replace(/^\s*\/\/.*$/gm, "");
    for (const m of text.matchAll(/Glyphs\.(ui|state)\(/g)) {
      const args = argsAt(text, m.index + m[0].length - 1);
      if (args === null) continue;
      // a word that is compared (`view === "settings" ? "cog" : "grid"`) is not a glyph: only what the call can return is
      for (const k of args.matchAll(/(?<!(?:===|!==|==|!=)\s*)"([A-Za-z]+)"/g)) {
        seen++;
        if (m[1] === "ui" && Glyphs.ui(k[1]) === "") missing.push(`${file.slice(PLUGIN.length + 1)}: ${k[1]}`);
        if (m[1] === "state" && !(k[1] in Glyphs.STATE)) missing.push(`${file.slice(PLUGIN.length + 1)}: ${k[1]}`);
      }
    }
  }
  assert.ok(seen > 20, `only ${seen} glyphs were found: the scan is broken`);
  assert.deepEqual(missing, []);
});

test("every glyph is a code point the font was checked for, written as a code point and not as a literal", () => {
  for (const [key, glyph] of Object.entries({ ...Glyphs.UI, ...Glyphs.STATE })) {
    assert.ok(glyph.codePointAt(0) >= 0xf0000, `${key} is not in the supplementary private use area`);
    assert.equal([...glyph].length, 1, `${key} is more than one code point`);
  }
});

// The real shell's qmlscanner gives a subfolder types only when a QML file at the plugin root
// imports it ("views", "settings"). A folder reached only from a panel, which is loaded by URL,
// has none, and every type in it "is not a type" (the blank Settings panel).
// The offscreen harness loads by file:// and cannot see this, so it is checked here.
test("every subfolder whose types other files use is imported by a QML file at the plugin root", () => {
  const rootImports = qmlAtRoot.map((f) => readFileSync(join(PLUGIN, f), "utf8")).join("\n");
  const folders = readdirSync(PLUGIN).filter((d) => statSync(join(PLUGIN, d)).isDirectory());
  for (const dir of folders) {
    const types = readdirSync(join(PLUGIN, dir)).filter((f) => /^[A-Z].*\.qml$/.test(f));
    if (types.length === 0) continue;
    const importedSomewhere = readdirSync(PLUGIN, { recursive: true })
      .filter((f) => f.endsWith(".qml"))
      .some((f) => new RegExp(`^import "(\\.\\./)?${dir}"\\s*$`, "m").test(readFileSync(join(PLUGIN, f), "utf8")));
    if (!importedSomewhere) continue;
    assert.ok(new RegExp(`^import "${dir}"\\s*$`, "m").test(rootImports), `${dir}/ is imported by a nested file but by no root file: the shell would give it no types`);
  }
});
