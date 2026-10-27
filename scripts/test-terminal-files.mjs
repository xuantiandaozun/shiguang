import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import vm from "node:vm";
import ts from "typescript";

// Minimal deterministic hook runner: exercise request/loading policy without SSH
// or a browser. State setters are observed by explicitly rendering again.
const slots = [];
let index = 0;
const same = (a, b) => a && b && a.length === b.length && a.every((value, i) => Object.is(value, b[i]));
const react = {
  useState(initial) {
    const slot = index++;
    if (!(slot in slots)) slots[slot] = { value: initial };
    return [slots[slot].value, value => { slots[slot].value = typeof value === "function" ? value(slots[slot].value) : value; }];
  },
  useRef(initial) { const slot = index++; return slots[slot] ??= { current: initial }; },
  useCallback(fn, deps) { const slot = index++; if (!slots[slot] || !same(slots[slot].deps, deps)) slots[slot] = { fn, deps }; return slots[slot].fn; },
  useEffect(fn, deps) { const slot = index++; if (!slots[slot] || !same(slots[slot].deps, deps)) { slots[slot]?.cleanup?.(); slots[slot] = { deps, cleanup: fn() }; } },
};
const calls = [];
const ipc = { serverTerminal(action, server, extra) {
  assert.equal(action, "files");
  return new Promise((resolve, reject) => calls.push({ path: extra.data, resolve, reject }));
} };
const compiled = ts.transpileModule(readFileSync(new URL("../src/lib/useTerminalFiles.ts", import.meta.url), "utf8"), { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2020 } }).outputText;
const exports = {};
vm.runInNewContext(compiled, { exports, require: name => name === "react" ? react : { ipc }, setTimeout });
const render = () => { index = 0; return exports.useTerminalFiles("test-server", false); };
const listing = path => ({ path, entries: [], truncated: false });
const settle = async () => { for (let n = 0; n < 8; n++) await Promise.resolve(); };
let files = render();
const first = files.refresh();
assert.equal(render().loading, true, "initial listing should indicate loading");
calls[0].resolve(listing("/srv")); await first;
assert.equal(render().loading, false);

files = render();
const update = files.refresh();
assert.equal(render().loading, false, "same-directory background update must not disable the list");
assert.equal(render().refreshing, true);
void files.refresh();
assert.equal(calls.length, 2, "duplicate background updates must not open concurrent connections");
calls[1].resolve(listing("/srv")); await update; await settle();
assert.equal(calls.length, 3, "one trailing update preserves changes made during the first read");
assert.equal(render().loading, false, "trailing update also keeps the list interactive");
calls[2].resolve(listing("/srv")); await settle();

files = render();
const background = files.refresh();
const navigation = files.refresh("/srv/new");
assert.equal(render().loading, true, "directory navigation may disable stale directory actions");
void files.refresh();
assert.equal(calls.length, 5, "background updates must not override pending navigation");
calls[3].resolve(listing("/srv")); await background;
assert.equal(render().listing.path, "/srv");
calls[4].resolve(listing("/srv/new")); await navigation; await settle();
assert.equal(render().listing.path, "/srv/new");
assert.equal(calls[5].path, "/srv/new", "trailing update must use the newly selected directory");
calls[5].resolve(listing("/srv/new")); await settle();
assert.equal(render().loading, false);
assert.equal(render().refreshing, false);
console.log("Terminal files: background updates stay interactive; requests coalesce; navigation is not overwritten; all tests passed.");
