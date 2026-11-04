// 把内置插件同步到程序旁边的 plugins/ 目录（发布版运行时从这里读取）。
// 用法：node scripts/sync-plugins.mjs <目标目录>   例：src-tauri/target/release、release/current
// 插件不放进 Tauri resources：那样每改一个插件文件都会触发 Rust 重新编译。
// 安装包（tauri:bundle / release:local）通过 tauri.plugins.conf.json 另行带上。
import { cpSync, existsSync, mkdirSync, rmSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const projectDir = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const source = join(projectDir, "src-tauri", "plugins");
const targets = process.argv.slice(2);
if (targets.length === 0) {
  console.error("用法：node scripts/sync-plugins.mjs <目标目录>...");
  process.exit(1);
}

// browser 插件的页面脚本与浏览器扩展共用同一份源文件。
const browserAssets = ["readability.js", "page-api.js", "page-operations.js", "page-runtime.js"];

for (const target of targets) {
  const dest = join(resolve(projectDir, target), "plugins");
  if (existsSync(dest)) rmSync(dest, { recursive: true, force: true });
  mkdirSync(dest, { recursive: true });
  cpSync(source, dest, { recursive: true });
  const assets = join(dest, "browser", "assets");
  mkdirSync(assets, { recursive: true });
  for (const name of browserAssets) {
    cpSync(join(projectDir, "browser-extension", name), join(assets, name));
  }
  console.log(`内置插件已同步到 ${dest}`);
}
