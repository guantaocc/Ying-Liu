import { spawnSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const version = process.argv[2]?.replace(/^v/, "");
const semver = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-[0-9A-Za-z.-]+)?$/;

if (!version || !semver.test(version)) {
  console.error("用法：pnpm release:tag 0.2.0");
  process.exit(1);
}

const command = (program, args, options = {}) => {
  const executable = process.platform === "win32" && program === "pnpm" ? "pnpm.cmd" : program;
  const result = spawnSync(executable, args, { cwd: root, stdio: "inherit", ...options });
  if (result.status !== 0) process.exit(result.status ?? 1);
};

const output = (program, args) =>
  spawnSync(program, args, { cwd: root, encoding: "utf8" }).stdout.trim();

if (output("git", ["status", "--porcelain"])) {
  console.error("发布前工作区必须保持干净，请先提交或暂存现有修改。");
  process.exit(1);
}

if (output("git", ["tag", "--list", `v${version}`])) {
  console.error(`标签 v${version} 已经存在。`);
  process.exit(1);
}

const packagePath = join(root, "package.json");
const packageJson = JSON.parse(readFileSync(packagePath, "utf8"));
packageJson.version = version;
writeFileSync(packagePath, `${JSON.stringify(packageJson, null, 2)}\n`);

const tauriPath = join(root, "src-tauri", "tauri.conf.json");
const tauriJson = JSON.parse(readFileSync(tauriPath, "utf8"));
tauriJson.version = version;
writeFileSync(tauriPath, `${JSON.stringify(tauriJson, null, 2)}\n`);

const cargoPath = join(root, "src-tauri", "Cargo.toml");
const cargoToml = readFileSync(cargoPath, "utf8").replace(
  /(^\[package\][\s\S]*?^version\s*=\s*)"[^"]+"/m,
  `$1"${version}"`,
);
writeFileSync(cargoPath, cargoToml);

command("pnpm", ["install", "--lockfile-only"]);
command("pnpm", ["release:check"]);
command("git", ["add", "package.json", "pnpm-lock.yaml", "src-tauri/Cargo.toml", "src-tauri/Cargo.lock", "src-tauri/tauri.conf.json"]);
command("git", ["commit", "-m", `release: v${version}`]);
command("git", ["tag", "-a", `v${version}`, "-m", `Ying-Liu v${version}`]);

console.log(`\n已创建 v${version}。运行 pnpm release:push 即可触发 GitHub Release。`);
