import { execFileSync } from "node:child_process";
import { copyFileSync, existsSync, mkdirSync, readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const scriptDir = dirname(fileURLToPath(import.meta.url));
const projectDir = resolve(scriptDir, "..");
const manifest = join(projectDir, "src-tauri", "sidecars", "Cargo.toml");
const targetDir = join(projectDir, "src-tauri", "target", "sidecars");
const release = process.argv[2] === "release";
const triple = execFileSync("rustc", ["--print", "host-tuple"], {
  cwd: projectDir,
  encoding: "utf8",
}).trim();
if (!triple) throw new Error("无法获取 Rust host target triple");
const extension = process.platform === "win32" ? ".exe" : "";
const binaryDir = join(projectDir, "src-tauri", "binaries");
mkdirSync(binaryDir, { recursive: true });
// Independent package: no Tauri build script, placeholder binaries or desktop
// library compilation. Keep both caches and unbundled sibling EXEs separate.
const cargoArgs = ["build", "--manifest-path", manifest, "--target-dir", targetDir, "--locked", "--bins"];
if (release) cargoArgs.push("--release");

execFileSync("cargo", cargoArgs, { cwd: projectDir, stdio: "inherit" });
const profile = release ? "release" : "debug";
for (const name of ["shiguang-index-helper", "shiguang-mcp"]) {
  const source = join(targetDir, profile, `${name}${extension}`);
  const target = join(binaryDir, `${name}-${triple}${extension}`);
  if (existsSync(target) && readFileSync(source).equals(readFileSync(target))) {
    console.log(`${name} unchanged, keeping: ${target}`);
  } else {
    copyFileSync(source, target);
    console.log(`${name} ready: ${target}`);
  }
  // --no-bundle does not install externalBin files next to the main EXE.
  const sibling = join(projectDir, "src-tauri", "target", profile, `${name}${extension}`);
  mkdirSync(dirname(sibling), { recursive: true });
  if (!existsSync(sibling) || !readFileSync(source).equals(readFileSync(sibling))) {
    copyFileSync(source, sibling);
  }
}
