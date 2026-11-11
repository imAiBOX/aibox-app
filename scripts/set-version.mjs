#!/usr/bin/env node
/**
 * 版本号注入：把发布 tag 的版本写进三处（缺一不可）——
 *   1. src-tauri/tauri.conf.json  → Tauri 窗口/包版本（getVersion()）
 *   2. package.json               → 前端构建版本
 *   3. src-tauri/Cargo.toml       → CARGO_PKG_VERSION（update_check 比较基准）
 * 用法：node scripts/set-version.mjs 0.1.2
 * 版本非法（非 x.y.z）→ exit 1（CI 里让 job 失败，避免发出错误版本的产物）。
 */
import { readFileSync, writeFileSync } from "node:fs";
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

const cargoPath = join(root, "src-tauri/Cargo.toml");
const cargo = readFileSync(cargoPath, "utf8");
const next = cargo.replace(/^version = "[^"]*"$/m, `version = "${version}"`);
if (next === cargo) {
  console.error("[set-version] version line not found in src-tauri/Cargo.toml");
  process.exit(1);
}
writeFileSync(cargoPath, next);

console.log(`[set-version] injected ${version} into tauri.conf.json / package.json / Cargo.toml`);
