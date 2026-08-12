import { execFileSync } from "node:child_process";
import { copyFileSync, existsSync, mkdirSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const scriptDir = dirname(fileURLToPath(import.meta.url));
const projectDir = resolve(scriptDir, "..");
const manifest = join(projectDir, "src-tauri", "Cargo.toml");
const release = process.argv[2] === "release";
const triple = execFileSync("rustc", ["--print", "host-tuple"], {
  cwd: projectDir,
  encoding: "utf8",
}).trim();
if (!triple) throw new Error("无法获取 Rust host target triple");
const extension = process.platform === "win32" ? ".exe" : "";
const binaryDir = join(projectDir, "src-tauri", "binaries");
const target = join(binaryDir, `shiguang-index-helper-${triple}${extension}`);
mkdirSync(binaryDir, { recursive: true });
// The package's Tauri build script validates externalBin even while Cargo is
// compiling the helper itself. Seed the expected path, then replace it below.
if (!existsSync(target)) writeFileSync(target, new Uint8Array());

const cargoArgs = ["build", "--manifest-path", manifest, "--bin", "shiguang-index-helper"];
if (release) cargoArgs.push("--release");

execFileSync("cargo", cargoArgs, { cwd: projectDir, stdio: "inherit" });
const profile = release ? "release" : "debug";
const source = join(
  projectDir,
  "src-tauri",
  "target",
  profile,
  `shiguang-index-helper${extension}`,
);
copyFileSync(source, target);
console.log(`NTFS helper ready: ${target}`);
