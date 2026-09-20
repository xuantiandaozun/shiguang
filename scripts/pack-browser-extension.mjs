import { spawnSync } from "node:child_process";
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

console.log("已生成", dest);
