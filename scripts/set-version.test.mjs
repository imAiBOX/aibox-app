/**
 * set-version.mjs 回归测试。
 *
 * 2026-11-20 v0.1.6 发布实测踩坑：旧实现用「替换后内容是否变化」判断版本行是否存在，
 * 于是**版本已经是目标值时反而报「version line not found」并 exit 1** →
 * release-windows / release-macos 两个 job 在「Inject version from tag」这一步全挂。
 * 本测试锁死「同版本重复注入必须幂等且 exit 0」。
 */
import { describe, expect, it } from "vitest";
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { join } from "node:path";

const root = process.cwd();
const script = join(root, "scripts", "set-version.mjs");
const TARGETS = [
  "package.json",
  "src-tauri/tauri.conf.json",
  "src-tauri/Cargo.toml",
  "Cargo.toml",
  "crates/aibox-core/Cargo.toml",
  "crates/aibox-net/Cargo.toml",
  "crates/aibox-netdoctor/Cargo.toml",
];

const readAll = () =>
  Object.fromEntries(TARGETS.map((rel) => [rel, readFileSync(join(root, rel), "utf8")]));

describe("set-version.mjs", () => {
  it("同版本重复注入 → 幂等、exit 0、文件零改动", () => {
    const current = JSON.parse(readFileSync(join(root, "package.json"), "utf8")).version;
    const before = readAll();
    execFileSync("node", [script, current], { cwd: root, stdio: "pipe" });
    execFileSync("node", [script, current], { cwd: root, stdio: "pipe" });
    expect(readAll()).toEqual(before);
  });

  it("非法版本号 → exit 1（CI 里让 job 失败，别发错版本的产物）", () => {
    let code = 0;
    try {
      execFileSync("node", [script, "not-a-version"], { cwd: root, stdio: "pipe" });
    } catch (e) {
      code = e.status ?? 1;
    }
    expect(code).toBe(1);
  });
});
