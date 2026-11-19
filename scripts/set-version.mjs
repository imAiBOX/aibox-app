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

for (const rel of ["src-tauri/tauri.conf.json", "package.json"]) {
  const path = join(root, rel);
  const doc = JSON.parse(readFileSync(path, "utf8"));
  doc.version = version;
  writeFileSync(path, `${JSON.stringify(doc, null, 2)}\n`);
}

/** Cargo.toml 里把首个 `version = "..."` 替换为目标版本。 */
function injectCargo(rel) {
  const path = join(root, rel);
  if (!existsSync(path)) return false;
  const cargo = readFileSync(path, "utf8");
  const next = cargo.replace(/^version = "[^"]*"$/m, `version = "${version}"`);
  if (next === cargo) {
    console.error(`[set-version] version line not found in ${rel}`);
    process.exit(1);
  }
  writeFileSync(path, next);
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
