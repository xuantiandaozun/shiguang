import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import vm from "node:vm";
import ts from "typescript";

const require = createRequire(import.meta.url);
const filename = new URL("../src/lib/terminalAgentContext.ts", import.meta.url);
const compiled = ts.transpileModule(readFileSync(filename, "utf8"), { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2020 } }).outputText;
const exports = {};
vm.runInNewContext(compiled, { exports, require });
const { createTerminalCheckpoint, terminalResultExcerpt } = exports;

const marker = "__TEST_DONE__";
const command = "ps -ef | grep app";
const excerpt = terminalResultExcerpt(command, "查看服务进程", `${command}\r\n\u001b[32mPID 1234 running\u001b[0m\r\n${marker}:0\r\n`, marker, "0");
assert.ok(excerpt.includes("PID 1234 running"));
assert.ok(excerpt.includes("退出码：0"));
assert.ok(!excerpt.includes(command));
assert.ok(!excerpt.includes(marker));
assert.ok(!excerpt.includes("\u001b"));
const withoutEcho = terminalResultExcerpt(command, "检查服务", `shell prompt and wrapped command echo SHOULD_BE_DROPPED\n${marker}:BEGIN\nreal result\n${marker}:0\n`, marker, "0");
assert.ok(withoutEcho.includes("real result") && !withoutEcho.includes("SHOULD_BE_DROPPED"));
const large = terminalResultExcerpt("query", "查看日志", `START${"x".repeat(20000)}END`, marker, "0");
assert.ok(large.length < 7100);
assert.ok(large.includes("START") && large.includes("END") && large.includes("中间输出已省略"));

const checkpoint = createTerminalCheckpoint("查看 app 服务", "已确认服务运行；配置位置已记录，尚未检查业务请求。", {
  summary: "工作摘要",
  related_paths: ["/srv/app/config.yml", "/srv/app/app.log", "/srv/app/config.yml"],
  working_directory: "/srv/app",
  raw_output: "MUST_NOT_PERSIST",
  commands: ["MUST_NOT_PERSIST"],
}, "2026-01-01T00:00:00.000Z");
const encoded = JSON.stringify(checkpoint);
assert.ok(!encoded.includes("MUST_NOT_PERSIST"));
assert.equal(checkpoint.related_paths.length, 2);
assert.equal(checkpoint.checked_at, "2026-01-01T00:00:00.000Z");
assert.equal(checkpoint.working_directory, "/srv/app");
assert.ok(!("raw_output" in checkpoint) && !("commands" in checkpoint));
const bounded = createTerminalCheckpoint("q".repeat(5000), "s".repeat(12000), { summary: "", related_paths: Array.from({ length: 100 }, (_, n) => `/srv/${n}`) });
assert.equal(bounded.request.length, 1200);
assert.equal(bounded.summary.length, 4000);
assert.equal(bounded.related_paths.length, 20);
console.log("Terminal agent context: command echoes removed, output bounded, checkpoints contain only summary/paths/time; all tests passed.");
