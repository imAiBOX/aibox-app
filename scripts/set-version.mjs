#!/usr/bin/env node
/**
 * 版本号注入：把发布 tag 的版本写进所有版本声明处（缺一不可）——
 *   1. src-tauri/tauri.conf.json  → Tauri 窗口/包版本（getVersion()）
 *   2. package.json               → 前端构建版本
 *   3. src-tauri/Cargo.toml       → CARGO_PKG_VERSION（update_check 比较基准）
 *   4. Cargo.toml（workspace 根）+ crates 目录下各 crate 的 Cargo.toml
 *      → 诊断包 version.json 的 "installer" 字段取自 aibox-core 的
 *        CARGO_PKG_VERSION。旧版只注入前 3 处，导致所有正式发布的
 *        诊断包都显示 installer=0.0.1（用户无法判断装的是哪版）。
 *        2026-11-19 起一并注入。
 * 用法：node scripts/set-version.mjs 0.1.2
 * 版本非法（非 x.y.z）→ exit 1（CI 里让 job 失败，避免发出错误版本的产物）。
 */
import { readFileSync, writeFileSync, readdirSync, existsSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, "..");
const version = process.argv[2] ?? "";

if (!/^\d+\.\d+\.\d+([.+-][0-9A-Za-z.-]+)?$/.test(version)) {
  console.error(`[set-version] invalid version: ${version || "(missing)"}`);
  process.exit(1);
}

/**
 * JSON 版本注入：**只替换 version 的字符串**，不做 parse/stringify 往返。
 *
 * 往返会把数组重排版（`["dmg"]` → 多行数组）→ 每次打 tag 都产生无意义 diff，
 * 且让「同版本重复注入幂等」的断言失败（文件内容变了）。
 */
function injectJson(rel) {
  const path = join(root, rel);
  if (!existsSync(path)) return false;
  const text = readFileSync(path, "utf8");
  const re = /("version"\s*:\s*)"[^"]*"/;
  if (!re.test(text)) {
    console.error(`[set-version] version field not found in ${rel}`);
    process.exit(1);
  }
  const next = text.replace(re, `$1"${version}"`);
  if (next !== text) writeFileSync(path, next);
  return true;
}

for (const rel of ["src-tauri/tauri.conf.json", "package.json"]) injectJson(rel);

/** Cargo.toml 里把首个 `version = "..."` 替换为目标版本。 */
function injectCargo(rel) {
  const path = join(root, rel);
  if (!existsSync(path)) return false;
  const cargo = readFileSync(path, "utf8");
  const re = /^version = "[^"]*"$/m;
  // 文件里压根没有 version 行 → 真出错
  if (!re.test(cargo)) {
    console.error(`[set-version] version line not found in ${rel}`);
    process.exit(1);
  }
  // **幂等**：已是目标版本就不改、也不报错。
  // （旧实现用「替换后内容是否变化」判断版本行是否存在 → 预置过版本号再打 tag 时
  //   会误判成「找不到版本行」→ exit 1 → release job 全挂。2026-11-20 v0.1.6 实测踩坑。）
  const next = cargo.replace(re, `version = "${version}"`);
  if (next !== cargo) writeFileSync(path, next);
  return true;
}

const cargoTargets = ["src-tauri/Cargo.toml", "Cargo.toml"];
const cratesDir = join(root, "crates");
if (existsSync(cratesDir)) {
  for (const name of readdirSync(cratesDir)) {
    const rel = join("crates", name, "Cargo.toml");
    if (existsSync(join(root, rel))) cargoTargets.push(rel);
  }
}

const injected = cargoTargets.filter(injectCargo);
console.log(
  `[set-version] injected ${version} into tauri.conf.json / package.json / ${injected.join(" / ")}`,
);
