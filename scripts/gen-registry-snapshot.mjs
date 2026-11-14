#!/usr/bin/env node
/**
 * gen-registry-snapshot — 构建时把 registry index.json 刷进
 * src/assets/registry-snapshot.json（Rust 侧 include_str! 嵌入，断网首开的目录兑底）。
 *
 * - 三源并行任一成功即写入（jsdelivr / raw / aibox.im；GitHub API 需 envelope 解码，不在此列）
 * - **任何失败（网络/解析/写入）都保留仓库内既有快照 → 绝不中断构建**；
 *   连既有文件都没有才落一个占位（保证 include_str! 编译期存在）
 * - 只刷新文件，**不做自动提交**（避免循环）；随下次手动提交/构建发布
 * 用法: node scripts/gen-registry-snapshot.mjs
 */
import { existsSync, mkdirSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, "..");
const OUT = resolve(root, "src/assets/registry-snapshot.json");
mkdirSync(dirname(OUT), { recursive: true }); // 首次构建：assets 目录可能尚不存在

const SOURCES = [
  "https://cdn.jsdelivr.net/gh/imAiBOX/aibox-registry@main/index.json",
  "https://raw.githubusercontent.com/imAiBOX/aibox-registry/main/index.json",
  "https://aibox.im/meta/index.json",
];

async function fetchOne(url) {
  const res = await fetch(url, {
    signal: AbortSignal.timeout(4000),
    headers: { "user-agent": "aibox-app-build" },
  });
  if (!res.ok) throw new Error(`HTTP ${res.status}`);
  const doc = JSON.parse(await res.text());
  if (!doc || !Array.isArray(doc.apps)) throw new Error("bad index shape (apps[])");
  return doc;
}

async function main() {
  try {
    const doc = await Promise.any(SOURCES.map(fetchOne));
    writeFileSync(OUT, `${JSON.stringify(doc, null, 2)}\n`);
    console.log(
      `[snapshot] refreshed: ${doc.apps.length} apps → src/assets/registry-snapshot.json`,
    );
  } catch (err) {
    // 写入失败（含目录/权限）也要兜底：既有快照绝不动，没有才落占位
    if (!existsSync(OUT)) {
      try {
        writeFileSync(
          OUT,
          `${JSON.stringify({ note: "offline catalog placeholder", apps: [] }, null, 2)}\n`,
        );
        console.warn("[snapshot] fetch failed and no previous snapshot — placeholder written");
      } catch (e2) {
        console.warn(`[snapshot] cannot write snapshot at all: ${e2?.message ?? e2}`);
      }
    } else {
      console.warn(
        `[snapshot] refresh failed (${err?.message ?? err}) — keeping previous snapshot`,
      );
    }
  }
  // 永不抛出：刷新失败沿用仓库内上一次快照，构建不中断
}

await main();
