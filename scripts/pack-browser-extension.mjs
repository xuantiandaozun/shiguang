import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const src = path.join(root, "browser-extension");
const destDir = path.join(root, "src-tauri", "resources");
const dest = path.join(destDir, "browser-extension.zip");

if (!fs.existsSync(path.join(src, "manifest.json"))) {
  console.error("未找到 browser-extension/manifest.json");
  process.exit(1);
}

// Match the archive's top-level file set, including names so additions,
// removals and renames invalidate the cache as well as content changes.
const files = fs.readdirSync(src, { withFileTypes: true })
  .filter((entry) => entry.isFile())
  .map((entry) => entry.name)
  .sort();
const hash = createHash("sha256");
for (const name of files) {
  const content = fs.readFileSync(path.join(src, name));
  hash.update(JSON.stringify([name, content.length]));
  hash.update(content);
}
const fingerprint = hash.digest("hex");
const cache = path.join(root, "src-tauri", "target", "browser-extension-pack.json");
const archiveHash = () => createHash("sha256").update(fs.readFileSync(dest)).digest("hex");
try {
  const previous = JSON.parse(fs.readFileSync(cache, "utf8"));
  if (previous.source === fingerprint && fs.existsSync(dest) && previous.archive === archiveHash()) {
    console.log("浏览器扩展未变化，跳过压缩");
    process.exit(0);
  }
} catch {
  // Missing or invalid cache: rebuild normally.
}

fs.mkdirSync(destDir, { recursive: true });
if (fs.existsSync(dest)) fs.unlinkSync(dest);

const quote = (p) => `'${p.replace(/'/g, "''")}'`;
const archiveCommand = `Compress-Archive -LiteralPath (Get-ChildItem -LiteralPath ${quote(src)} -File | ForEach-Object { $_.FullName }) -DestinationPath ${quote(dest)} -Force`;
// PowerShell 7 ships the archive module independently from the occasionally
// broken Windows PowerShell module registration. Prefer it when installed.
let r = spawnSync("pwsh.exe", ["-NoProfile", "-Command", archiveCommand], {
  stdio: "inherit",
});
if (r.error?.code === "ENOENT") {
  r = spawnSync("powershell.exe", ["-NoProfile", "-Command", archiveCommand], {
    stdio: "inherit",
  });
}

if (r.status !== 0 || !fs.existsSync(dest)) {
  console.error("打包浏览器扩展失败");
  process.exit(r.status ?? 1);
}

fs.mkdirSync(path.dirname(cache), { recursive: true });
fs.writeFileSync(cache, JSON.stringify({ source: fingerprint, archive: archiveHash() }));
console.log("已生成", dest);
