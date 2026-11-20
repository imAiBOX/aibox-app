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
 *
 * 用法：node scripts/set-version.mjs 0.1.2
 * 版本非法（非 x.y.z）→ exit 1（CI 里让 job 失败，避免发出错误版本的产物）。
 *
 * 设计约束（独立审计 2026-11-20 REJECT/ACCEPT-7）：
 * - **幂等**：同版本重复注入必须 exit 0 且文件零改动（预置版本号再打 tag 的常见流程）。
 * - **精确定位**：JSON 只改顶层 version；Cargo 只改 [package]/[workspace.package] 段。
 * - **fail-closed**：定位不到 / 改完复核不通过 → 抛错（绝不「静默成功但没改」）。
 * - **不改排版**：不做 JSON parse/stringify 往返（会把 ["dmg"] 重排版成多行数组）。
 *
 * 逻辑抽成可注入根目录的 `injectVersion(root, version)`，供 vitest 用临时目录真正锁住行为。
 */
import { readFileSync, writeFileSync, readdirSync, existsSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const VERSION_RE = /^\d+\.\d+\.\d+([.+-][0-9A-Za-z.-]+)?$/;

/**
 * 注入失败（fail-closed）。CLI 下 → exit 1；测试下 → 抛错可断言。
 */
export class SetVersionError extends Error {}

function fail(msg) {
  throw new SetVersionError(msg);
}

/**
 * 找到**顶层对象**里 "version" 键的起始引号位置；找不到返回 -1。
 * 深度感知扫描：顶层对象内的直接子键 depth === 1；数组/嵌套对象更深。
 */
export function topLevelVersionOffset(text) {
  let depth = 0;
  let inStr = false;
  let esc = false;
  let rootIsObject = false;
  for (let i = 0; i < text.length; i++) {
    const ch = text[i];
    if (inStr) {
      if (esc) {
        esc = false;
        continue;
      }
      if (ch === "\\") {
        esc = true;
        continue;
      }
      if (ch === '"') inStr = false;
      continue;
    }
    if (ch === '"') {
      if (depth === 1 && rootIsObject && /^"version"\s*:/.test(text.slice(i))) return i;
      inStr = true;
      continue;
    }
    if (ch === "{" || ch === "[") {
      depth++;
      if (depth === 1) rootIsObject = ch === "{";
    } else if (ch === "}" || ch === "]") depth--;
  }
  return -1;
}

/** JSON 版本注入：只替换顶层 version 的字符串值，保留其余排版。 */
export function injectJson(root, rel, version) {
  const path = join(root, rel);
  if (!existsSync(path)) return false;
  const text = readFileSync(path, "utf8");

  let doc;
  try {
    doc = JSON.parse(text);
  } catch (e) {
    fail(`${rel} is not valid JSON: ${e.message}`);
  }
  if (doc === null || typeof doc !== "object" || Array.isArray(doc)) {
    fail(`${rel} top level is not a JSON object`);
  }
  if (typeof doc.version !== "string") {
    fail(`${rel} has no top-level version field`);
  }

  const idx = topLevelVersionOffset(text);
  if (idx < 0) fail(`${rel} top-level version field not found`);
  const re = /"version"\s*:\s*"[^"]*"/;
  const tail = text.slice(idx);
  if (!re.test(tail)) fail(`${rel} top-level version field malformed`);
  const next = text.slice(0, idx) + tail.replace(re, `"version": "${version}"`);

  // fail-closed 复核：改完再解析一次，顶层 version 必须是目标值
  let after;
  try {
    after = JSON.parse(next);
  } catch (e) {
    fail(`${rel} became invalid JSON: ${e.message}`);
  }
  if (after.version !== version) {
    fail(`${rel} version injection failed (got ${after.version})`);
  }
  if (next !== text) writeFileSync(path, next);
  return true;
}

/**
 * Cargo 版本注入：只改 [package] / [workspace.package] 段内的 `version = "..."`。
 * 依赖段里也可能出现独立成行的 version（[dependencies.foo] 下）——改错就是改依赖版本。
 */
export function injectCargo(root, rel, version) {
  const path = join(root, rel);
  if (!existsSync(path)) return false;
  const cargo = readFileSync(path, "utf8");

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
  if (hit < 0) fail(`${rel}: no version line in [package]/[workspace.package]`);

  lines[hit] = lines[hit].replace(/"[^"]*"/, `"${version}"`);
  const next = lines.join("\n");
  // 幂等：内容没变就不写（版本已是目标值时也必须成功）
  if (next !== cargo) writeFileSync(path, next);
  return true;
}

/** 注入到给定根目录下的所有版本声明处；返回被注入的文件列表。 */
export function injectVersion(root, version) {
  if (!VERSION_RE.test(version)) {
    fail(`invalid version: ${version || "(missing)"}`);
  }
  const injected = [];
  for (const rel of ["src-tauri/tauri.conf.json", "package.json"]) {
    if (injectJson(root, rel, version)) injected.push(rel);
  }
  const cargoTargets = ["src-tauri/Cargo.toml", "Cargo.toml"];
  const cratesDir = join(root, "crates");
  if (existsSync(cratesDir)) {
    for (const name of readdirSync(cratesDir)) {
      const rel = join("crates", name, "Cargo.toml");
      if (existsSync(join(root, rel))) cargoTargets.push(rel);
    }
  }
  for (const rel of cargoTargets) {
    if (injectCargo(root, rel, version)) injected.push(rel);
  }
  return injected;
}

/* ── CLI ── */
const here = dirname(fileURLToPath(import.meta.url));
const isCli = process.argv[1] && resolve(process.argv[1]) === resolve(fileURLToPath(import.meta.url));
if (isCli) {
  const root = resolve(here, "..");
  const version = process.argv[2] ?? "";
  try {
    const injected = injectVersion(root, version);
    console.log(`[set-version] injected ${version} into ${injected.join(" / ")}`);
  } catch (e) {
    console.error(`[set-version] ${e.message}`);
    process.exit(1);
  }
}
