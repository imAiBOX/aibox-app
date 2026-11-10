#!/usr/bin/env node
/**
 * t() 引用核对：
 * 1) 代码里静态引用的 t("key") / labelKey/messageKey 字面量必须存在于 en.json；
 * 2) 动态模板前缀（net.status.${...} 等）按已知族抽查存在；
 * 3) 输出 en.json 中零引用的候选 key（人工复核后删除，勿自动删）。
 * 用法：node scripts/check-tkeys.mjs
 */
import { readFileSync, readdirSync, statSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, "..");
const localesDir = join(root, "src", "locales");

function flatten(obj, prefix = "", out = new Set()) {
  for (const [k, v] of Object.entries(obj)) {
    const key = prefix ? `${prefix}.${k}` : k;
    if (v && typeof v === "object" && !Array.isArray(v)) flatten(v, key, out);
    else out.add(key);
  }
  return out;
}

const keys = flatten(JSON.parse(readFileSync(join(localesDir, "en.json"), "utf8")));

/** 已知动态模板族（前缀必须存在至少一个 key） */
const DYNAMIC_FAMILIES = [
  "net.status.",
  "net.region.",
  "net.fixHint.",
  "scan.hint.",
  "scan.",
  "s4.phase",
  "s5.nav",
  "err.",
  "btn.",
  "nav.",
];

function walk(dir, acc = []) {
  for (const name of readdirSync(dir)) {
    const p = join(dir, name);
    if (name === "locales" || name === "node_modules") continue;
    if (statSync(p).isDirectory()) walk(p, acc);
    else if (/\.tsx?$/.test(name) && !/\.test\.ts$/.test(name)) acc.push(p);
  }
  return acc;
}

/** 形似 i18n key 但实际是 KV/localStorage key 的字面量（排除误报） */
const NON_I18N_LITERALS = new Set(["net.settings"]);

const used = new Set();
const dynTemplates = new Set();
for (const file of walk(join(root, "src"))) {
  const src = readFileSync(file, "utf8");
  for (const m of src.matchAll(/\bt\(\s*"([^"]+)"/g)) used.add(m[1]);
  for (const m of src.matchAll(/\bt\(\s*'([^']+)'/g)) used.add(m[1]);
  for (const m of src.matchAll(/\bt\(\s*`([^`]+)`/g)) dynTemplates.add(m[1]);
  for (const m of src.matchAll(/(?:labelKey|messageKey|hintKey|noteKey):\s*"([^"]+)"/g)) used.add(m[1]);
  // 所有形如 family.key 的字符串字面量（含数组里的 ss.step1、config.p.*.name 等间接 t() 引用）
  for (const m of src.matchAll(/"((?:scan|err|btn|nav|ss|s[1-7]|net|e1|update|resume|config|diag|app)\.[A-Za-z0-9_.]+)"/g)) {
    if (!NON_I18N_LITERALS.has(m[1])) used.add(m[1]);
  }
}

let failed = false;
const missing = [...used].filter((k) => !k.includes("${") && !keys.has(k)).sort();
if (missing.length) {
  failed = true;
  console.error("[check-tkeys] MISSING keys (used in code, absent in en.json):");
  for (const k of missing) console.error("  ", k);
}

for (const fam of DYNAMIC_FAMILIES) {
  const hit = [...keys].some((k) => k.startsWith(fam));
  if (!hit) {
    failed = true;
    console.error(`[check-tkeys] dynamic family has no keys: ${fam}`);
  }
}

// 零引用候选（仅提示；测试文件与动态族覆盖的不算）
const covered = (k) => {
  if (used.has(k)) return true;
  return DYNAMIC_FAMILIES.some((f) => k.startsWith(f));
};
const unused = [...keys].filter((k) => !covered(k)).sort();
if (unused.length) {
  console.warn("[check-tkeys] zero-reference candidates (review before deleting):");
  for (const k of unused) console.warn("  ", k);
}

if (failed) {
  console.error("[check-tkeys] FAIL");
  process.exit(1);
}
console.log(`[check-tkeys] OK — ${used.size} static refs resolved, ${dynTemplates.size} dynamic templates, ${keys.size} keys`);
