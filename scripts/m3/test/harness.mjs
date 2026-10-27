// Loads the plugin's QML JavaScript libraries into node, so `node --test` runs the EXACT files
// the shell loads: no copy, no build step, no drift. (The pattern of the user's own plugins,
// ~/Projects/Omarchy/Plugins/omarchy-backup-status/test/harness.mjs.)
//
// QML-JS has two directives node cannot parse, `.pragma library` and `.import "X.js" as Name`.
// `.pragma` is dropped. `.import` loads X.js first and hands it to the file under the name
// it asked for, which is what the QML engine does.

import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import vm from "node:vm";

export const PLUGIN = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..", "plugin");

const cache = new Map();

function declaredNames(src) {
  const names = new Set();
  for (const m of src.matchAll(/^function\s+([A-Za-z_$][\w$]*)/gm)) names.add(m[1]);
  for (const m of src.matchAll(/^var\s+([A-Za-z_$][\w$]*)/gm)) names.add(m[1]);
  return [...names];
}

// Evaluated in THIS realm, not a fresh vm context: a sandboxed realm has its own Array and
// Object prototypes, and assert.deepEqual then fails on cross-realm identity.
export function load(name) {
  if (cache.has(name)) return cache.get(name);
  const file = join(PLUGIN, name);
  let src = readFileSync(file, "utf8");

  const imports = [];
  src = src.replace(/^\s*\.import\s+"([^"]+)"\s+as\s+(\w+)\s*$/gm, (_, target, alias) => {
    imports.push([alias, target]);
    return "";
  });
  src = src.replace(/^\s*\.pragma\b.*$/gm, "");

  const names = declaredNames(src);
  const params = imports.map(([alias]) => alias);
  const factory = vm.runInThisContext(
    `(function (${params.join(", ")}) {\n${src}\nreturn { ${names.join(", ")} };\n})`,
    { filename: file }
  );
  const lib = factory(...imports.map(([, target]) => load(target)));
  cache.set(name, lib);
  return lib;
}
