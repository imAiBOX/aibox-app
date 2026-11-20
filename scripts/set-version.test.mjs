/**
 * set-version.mjs 回归测试。
 *
 * 2026-11-20 v0.1.6 发布实测踩坑：旧实现用「替换后内容是否变化」判断版本行是否存在，
 * 于是**版本已经是目标值时反而报「version line not found」并 exit 1** →
 * release-windows / release-macos 两个 job 在「Inject version from tag」这一步全挂。
 *
 * 复核审计（2026-11-20 ACCEPT-7）进一步要求：测试必须**真的把文件交给脚本处理**
 * （旧测试只验证「仓库文件同版本幂等」，等于自证），因此这里用临时目录 +
 * 导出的 `injectVersion(root, version)` 直接驱动。
 */
import { describe, expect, it } from "vitest";
import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { injectVersion, SetVersionError } from "./set-version.mjs";

const repoRoot = process.cwd();

function makeTree() {
  const dir = mkdtempSync(join(tmpdir(), "aibox-setver-"));
  mkdirSync(join(dir, "src-tauri"), { recursive: true });
  mkdirSync(join(dir, "crates", "a"), { recursive: true });
  // 顶层 JSON：嵌套 version 在前（不能被误改）
  writeFileSync(
    join(dir, "package.json"),
    JSON.stringify({ nested: { version: "9.9.9" }, version: "0.0.1", name: "x" }, null, 2) + "\n",
  );
  writeFileSync(
    join(dir, "src-tauri", "tauri.conf.json"),
    JSON.stringify({ productName: "AiBOX", version: "0.0.1", bundle: { targets: ["dmg"] } }, null, 2) +
      "\n",
  );
  // workspace 根：版本在 [workspace.package] 段
  writeFileSync(
    join(dir, "Cargo.toml"),
    '[workspace]\nresolver = "2"\n\n[workspace.package]\nversion = "0.0.1"\nedition = "2021"\n',
  );
  // crate：版本在 [package] 段；依赖段里也有独立成行的 version（不能被误改）
  writeFileSync(
    join(dir, "crates", "a", "Cargo.toml"),
    '[package]\nname = "a"\nversion = "0.0.1"\n\n[dependencies]\n\n[dependencies.foo]\nversion = "9.9.9"\n',
  );
  writeFileSync(join(dir, "src-tauri", "Cargo.toml"), '[package]\nname = "app"\nversion = "0.0.1"\n');
  return dir;
}

describe("set-version.mjs", () => {
  it("JSON 只改顶层 version，嵌套同名字段不动", () => {
    const dir = makeTree();
    try {
      injectVersion(dir, "1.2.3");
      const pkg = JSON.parse(readFileSync(join(dir, "package.json"), "utf8"));
      expect(pkg.version).toBe("1.2.3");
      expect(pkg.nested.version).toBe("9.9.9");
      const tauri = JSON.parse(readFileSync(join(dir, "src-tauri", "tauri.conf.json"), "utf8"));
      expect(tauri.version).toBe("1.2.3");
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  it("Cargo 只改 [package]/[workspace.package] 段，依赖段的 version 不动", () => {
    const dir = makeTree();
    try {
      injectVersion(dir, "1.2.3");
      const root = readFileSync(join(dir, "Cargo.toml"), "utf8");
      expect(root).toMatch(/\[workspace\.package\]\s*\nversion = "1\.2\.3"/);
      const crate = readFileSync(join(dir, "crates", "a", "Cargo.toml"), "utf8");
      expect(crate).toMatch(/\[package\][\s\S]*?version = "1\.2\.3"/);
      // 依赖段里的 version 必须原样保留
      expect(crate).toMatch(/\[dependencies\.foo\]\s*\nversion = "9\.9\.9"/);
      const app = readFileSync(join(dir, "src-tauri", "Cargo.toml"), "utf8");
      expect(app).toMatch(/version = "1\.2\.3"/);
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  it("幂等：同版本重复注入 → 文件零改动", () => {
    const dir = makeTree();
    try {
      injectVersion(dir, "1.2.3");
      const snap = ["package.json", "Cargo.toml", join("crates", "a", "Cargo.toml")].map((rel) =>
        readFileSync(join(dir, rel), "utf8"),
      );
      injectVersion(dir, "1.2.3");
      const again = ["package.json", "Cargo.toml", join("crates", "a", "Cargo.toml")].map((rel) =>
        readFileSync(join(dir, rel), "utf8"),
      );
      expect(again).toEqual(snap);
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  it("fail-closed：缺 [package] version / 顶层不是对象 / 非法版本号 → 抛错", () => {
    const dir = makeTree();
    try {
      writeFileSync(join(dir, "crates", "a", "Cargo.toml"), '[package]\nname = "a"\n');
      expect(() => injectVersion(dir, "1.2.3")).toThrow(SetVersionError);
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }

    const dir2 = makeTree();
    try {
      writeFileSync(join(dir2, "package.json"), JSON.stringify([{ version: "9.9.9" }]));
      expect(() => injectVersion(dir2, "1.2.3")).toThrow(SetVersionError);
    } finally {
      rmSync(dir2, { recursive: true, force: true });
    }

    expect(() => injectVersion(repoRoot, "not-a-version")).toThrow(SetVersionError);
  });

  it("CLI：非法版本号 → exit 1；合法版本号 → exit 0", () => {
    let code = 0;
    try {
      execFileSync("node", ["scripts/set-version.mjs", "not-a-version"], {
        cwd: repoRoot,
        stdio: "pipe",
      });
    } catch (e) {
      code = e.status ?? 1;
    }
    expect(code).toBe(1);

    // 仓库当前版本（幂等）→ 必须 exit 0 且文件零改动
    const current = JSON.parse(readFileSync(join(repoRoot, "package.json"), "utf8")).version;
    const before = readFileSync(join(repoRoot, "src-tauri", "Cargo.toml"), "utf8");
    execFileSync("node", ["scripts/set-version.mjs", current], { cwd: repoRoot, stdio: "pipe" });
    expect(readFileSync(join(repoRoot, "src-tauri", "Cargo.toml"), "utf8")).toBe(before);
  });
});
