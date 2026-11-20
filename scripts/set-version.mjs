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

  // 定位**顶层**的 "version" 键（深度感知扫描，不能只看第一个 "version" 字符串；
  // 否则嵌套字段排在前面时会改错地方 —— 独立审计 2026-11-20 ACCEPT-7）。
  const idx = topLevelVersionOffset(text);
  if (idx < 0) {
    console.error(`[set-version] top-level version field not found in ${rel}`);
    process.exit(1);
  }
  const re = /"version"\s*:\s*"[^"]*"/;
  const tail = text.slice(idx);
  if (!re.test(tail)) {
    console.error(`[set-version] top-level version field malformed in ${rel}`);
    process.exit(1);
  }
  const next = text.slice(0, idx) + tail.replace(re, `"version": "${version}"`);
  // **fail-closed 复核**：改完再解析一次，顶层 version 必须是目标值，否则 exit 1
  try {
    const doc = JSON.parse(next);
    if (doc.version !== version) {
      console.error(`[set-version] version injection failed in ${rel} (got ${doc.version})`);
      process.exit(1);
    }
  } catch (e) {
    console.error(`[set-version] ${rel} became invalid JSON: ${e.message}`);
    process.exit(1);
  }
  if (next !== text) writeFileSync(path, next);
  return true;
}

/** 找到顶层对象里 "version" 键的起始引号位置；找不到返回 -1。 */
function topLevelVersionOffset(text) {
  let depth = 0;
  let inStr = false;
  let esc = false;
  for (let i = 0; i < text.length; i++) {
    const ch = text[i];
    if (inStr) {
      if (esc) { esc = false; continue; }
      if (ch === "\\") { esc = true; continue; }
      if (ch === '"') inStr = false;
      continue;
    }
    if (ch === '"') {
      if (depth === 1 && /^"version"\s*:/.test(text.slice(i))) return i;
      inStr = true;
      continue;
    }
    if (ch === "{" || ch === "[") depth++;
    else if (ch === "}" || ch === "]") depth--;
  }
  return -1;
}

for (const rel of ["src-tauri/tauri.conf.json", "package.json"]) injectJson(rel);

/**
 * Cargo.toml：只改 **[package] / [workspace.package] 段内**的 `version = "..."`。
 *
 * 不能用「首个匹配」：依赖段里也可能有独立成行的 `version = "..."`
 * （`[dependencies.foo]` 下）—— 改错就是改依赖版本（独立审计 2026-11-20 ACCEPT-7）。
 */
function injectCargo(rel) {
  const path = join(root, rel);
  if (!existsSync(path)) return false;
  const cargo = readFileSync(path, "utf8");

  // 按 section 切分；只在 [package] / [workspace.package] 段里找 version 行
  const lines = cargo.split("\n");
  let section = "";
  let hit = -1;
  for (let i = 0; i < lines.length; i++) {
    const head = lines[i].match(/^\s*\[([^\]]+)\]/);
    if (head) {
      section = head[1].trim();
      continue;
    }
    if (
      (section === "package" || section === "workspace.package") &&
      /^version\s*=\s*"[^"]*"\s*$/.test(lines[i])
    ) {
      hit = i;
      break;
    }
  }
  if (hit < 0) {
    console.error(`[set-version] [package] version line not found in ${rel}`);
    process.exit(1);
  }
  // 就在定位到的那一行上替换（不用全局正则：首个匹配未必是包版本）
  lines[hit] = lines[hit].replace(/"[^"]*"/, `"${version}"`);
  const next = lines.join("\n");
  // 幂等：内容没变就不写（版本已是目标值时也必须 exit 0）
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
