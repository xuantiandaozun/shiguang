// 外派全流程联调（需要正在运行的「调试构建」拾光，会消耗 Claude / Codex 订阅额度）：
//   node scripts/test-agent-runs.mjs [claude_code|codex]
// 通过调试构建才有的 debug_agent_tool 桥接动作，走与聊天相同的工具代码：
// 提问 → 回答 → 本轮结束并核对 git → 同会话追问 → 未提交改动拒绝 → worktree 隔离与合回。
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, writeFileSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const agent = process.argv[2] ?? "claude_code";
const dataDir = process.env.SHIGUANG_DATA_DIR ?? join(process.env.APPDATA, "com.deskhelper.win");
const { port, token } = JSON.parse(readFileSync(join(dataDir, "mcp-bridge.json"), "utf8"));

async function tool(name, args) {
  const res = await fetch(`http://127.0.0.1:${port}/`, {
    method: "POST",
    headers: { "content-type": "application/json", "x-shiguang-token": token },
    body: JSON.stringify({ action: "debug_agent_tool", name, arguments: args }),
    signal: AbortSignal.timeout(280_000),
  });
  const body = await res.json();
  if (body.error) throw new Error(`${name}: ${body.error}`);
  console.log(`← ${name}`, JSON.stringify(body).slice(0, 400));
  return body;
}

const git = (dir, ...args) => execFileSync("git", args, { cwd: dir, encoding: "utf8" }).trim();
const repo = mkdtempSync(join(tmpdir(), "shiguang-runs-"));
writeFileSync(join(repo, "a.txt"), "hello\n");
git(repo, "init", "-q");
git(repo, "add", ".");
git(repo, "-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "init");

const base = { root: repo, agent, timeout_secs: 240 };

// 1. 外派后智能体回问
let r = await tool("delegate_to_agent", {
  ...base,
  title: "联调：回问",
  task: "这是自动化测试。先调用 MCP 工具 ask_main 问「a.txt 应该改成什么单词？」，然后把 a.txt 全部内容改成回答里的单词。不要做别的，完成后回复 DONE。",
  acceptance: "a.txt 内容为回答给出的单词",
});
assert.equal(r.status, "asking", "应停在提问");
assert.match(String(r.question), /a\.txt/);
const runId = r.run_id;

// 2. 主 AI 回答 → 本轮结束，拾光自己核对 git
r = await tool("agent_reply", { run_id: runId, content: "改成 WORLD", timeout_secs: 240 });
assert.equal(r.status, "idle", "回答后应完成本轮");
assert.equal(readFileSync(join(repo, "a.txt"), "utf8").trim(), "WORLD");
assert.equal(r.result.changed_total, 1);
assert.equal(r.result.head_changed, false);

// 3. 同一会话追问
r = await tool("agent_reply", { run_id: runId, content: "再把 a.txt 改成 AGAIN，只改这一处，完成后回复 DONE。", timeout_secs: 240 });
assert.equal(r.status, "idle");
assert.equal(readFileSync(join(repo, "a.txt"), "utf8").trim(), "AGAIN");
await tool("stop_agent", { run_id: runId });

// 4. 有未提交改动时拒绝，提示先问用户
r = await tool("delegate_to_agent", { ...base, task: "随便改点什么" });
assert.equal(r.status, "needs_confirmation");
assert.ok(r.uncommitted.length >= 1);

// 5. worktree 隔离：原项目不受影响，合回后才出现
r = await tool("delegate_to_agent", {
  ...base,
  isolation: "worktree",
  title: "联调：worktree",
  task: "这是自动化测试。新建文件 b.txt，内容为 ISOLATED。不要做别的，完成后回复 DONE。",
});
assert.equal(r.status, "idle", JSON.stringify(r));
assert.equal(existsSync(join(repo, "b.txt")), false, "worktree 模式不应直接改原项目");
assert.equal(r.worktree.state, "active");
r = await tool("finish_agent_worktree", { run_id: r.run_id, action: "apply" });
assert.deepEqual(r.applied_files, ["b.txt"]);
assert.equal(readFileSync(join(repo, "b.txt"), "utf8").trim(), "ISOLATED");
assert.equal(readFileSync(join(repo, "a.txt"), "utf8").trim(), "AGAIN", "合回不应影响已有改动");
assert.equal(git(repo, "worktree", "list").split("\n").length, 1, "worktree 应已移除");

console.log(`\n✓ ${agent} 外派全流程通过（临时仓库 ${repo}）`);
