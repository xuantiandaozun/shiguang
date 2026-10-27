import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import vm from "node:vm";
import ts from "typescript";

const compiled = ts.transpileModule(readFileSync(new URL("../src/lib/terminalInput.ts", import.meta.url), "utf8"), { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2020 } }).outputText;
const exports = {};
vm.runInNewContext(compiled, { exports });
const { terminalTabSequence, createTerminalInputQueue, terminalCompletionStatus } = exports;
assert.equal(terminalCompletionStatus("readline").enhanced, true);
assert.ok(terminalCompletionStatus("readline").label.includes("Bash 4"));
assert.equal(terminalCompletionStatus("initial").enhanced, true);
for (const status of ["probe_failed", "unsupported", "custom", "timeout", "error", "unknown"]) assert.equal(terminalCompletionStatus(status).enhanced, false);
const tab = { type: "keydown", key: "Tab", shiftKey: false, ctrlKey: false, altKey: false, metaKey: false, isComposing: false };
assert.equal(terminalTabSequence(tab), "\t");
assert.equal(terminalTabSequence({ ...tab, shiftKey: true }), "\u001b[Z");
for (const changed of [{ type: "keyup" }, { type: "keypress" }, { isComposing: true }, { ctrlKey: true }, { altKey: true }, { metaKey: true }, { key: "Enter" }]) {
  assert.equal(terminalTabSequence({ ...tab, ...changed }), undefined);
}

const sent = [];
let resolvePrefix;
const write = createTerminalInputQueue(data => {
  sent.push(data);
  return data === "java" ? new Promise(resolve => { resolvePrefix = resolve; }) : Promise.resolve();
});
const prefix = write("java");
const completion = write("\t");
for (let n = 0; n < 5; n++) await Promise.resolve();
assert.deepEqual(sent, ["java"], "Tab must wait for the prefix write");
resolvePrefix();
await Promise.all([prefix, completion]);
assert.deepEqual(sent, ["java", "\t"], "deliver exactly one Tab after the prefix");

let fail = true;
const recovered = [];
const recoverableWrite = createTerminalInputQueue(async data => {
  if (fail) { fail = false; throw new Error("temporary IPC failure"); }
  recovered.push(data);
});
await assert.rejects(recoverableWrite("x"), /temporary IPC failure/);
await recoverableWrite("\t");
assert.deepEqual(recovered, ["\t"], "a failed write must not permanently block keyboard input");
console.log("Terminal input: Tab/Shift+Tab passthrough, IME/modifier preservation and ordered IPC writes all passed.");
