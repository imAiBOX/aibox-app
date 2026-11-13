#!/usr/bin/env node
/**
 * i18n 覆盖率检查：en / zh-CN / zh-TW key 集合必须相等，否则 exit 1。
 * key 拍平算法与 src/i18n/loader.ts 的 flatten 一致。
 * 用法：node scripts/check-i18n.mjs [localesDir]
 */
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const dir = resolve(process.argv[2] ?? join(here, "..", "src", "locales"));

function flatten(obj, prefix = "", out = new Set()) {
  for (const [k, v] of Object.entries(obj)) {
    const key = prefix ? `${prefix}.${k}` : k;
    if (v && typeof v === "object" && !Array.isArray(v)) {
      flatten(v, key, out);
    } else {
      out.add(key);
    }
  }
  return out;
}

function load(lang) {
  const raw = readFileSync(join(dir, `${lang}.json`), "utf8");
  return flatten(JSON.parse(raw));
}

const langs = ["en", "zh-CN", "zh-TW"];
let failed = false;

let sets;
try {
  sets = Object.fromEntries(langs.map((l) => [l, load(l)]));
} catch (err) {
  console.error(`[check-i18n] failed to read locales in ${dir}: ${err.message}`);
  process.exit(1);
}

const [base, ...others] = langs;
for (const other of others) {
  const missing = [...sets[base]].filter((k) => !sets[other].has(k));
  const extra = [...sets[other]].filter((k) => !sets[base].has(k));
  if (missing.length || extra.length) {
    failed = true;
    console.error(`[check-i18n] MISMATCH ${base} ↔ ${other}`);
    for (const k of missing) console.error(`  missing in ${other}: ${k}`);
    for (const k of extra) console.error(`  extra in ${other}: ${k}`);
  }
}

if (failed) {
  console.error(`[check-i18n] FAIL — key sets must be equal`);
  process.exit(1);
}

const count = sets.en.size;
if (count === 0) {
  console.error("[check-i18n] FAIL — no keys found");
  process.exit(1);
}
console.log(`[check-i18n] OK — ${langs.join("/")} aligned, ${count} keys each`);
