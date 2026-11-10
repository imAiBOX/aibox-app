/**
 * PLAN 构建测试：路线选择、arch 变体、ScanReport 差集（reuse）、
 * kind 映射（registry 5 种 → Rust ExecKind 3 种）、destName/spec 展开。
 */
import { describe, expect, it } from "vitest";
import { buildPlan, destNameOf, KIND_MAP, matchesMachine, specOf } from "./plan";
import type { AppManifest, RegistryPackage, ScanReport } from "../net/client";

const SHA = "a".repeat(64);

function pkg(over: Partial<RegistryPackage> = {}): RegistryPackage {
  return {
    id: "node-win-x64",
    name: "Node.js",
    version: "v26.13.0",
    platforms: ["windows"],
    arch: "x64",
    sha256: SHA,
    sizeBytes: 36_000_000,
    channels: [{ type: "direct", url: "https://nodejs.org/dist/node-v26.msi" }],
    install: { type: "msi-silent", args: ["/qn"], probe: { cmd: "node --version", expect: "^v26" } },
    ...over,
  };
}

function manifest(packages: RegistryPackage[], extra: Partial<AppManifest> = {}): AppManifest {
  return {
    id: "openclaw",
    name: "OpenClaw",
    packages,
    routes: { global: ["nsis-hub"], cn: ["npm-prefix"] },
    routePlans: {
      "nsis-hub": { steps: ["hub", "cli"] },
      "npm-prefix": { steps: ["node-win-x64", "cli"] },
    },
    ...extra,
  };
}

const cli = pkg({
  id: "cli",
  name: "OpenClaw CLI",
  platforms: ["windows", "macos"],
  arch: "any",
  install: { type: "npm-prefix", args: ["install", "-g", "openclaw@1"], probe: { cmd: "oc -v", expect: "1" } },
});
const hub = pkg({
  id: "hub",
  name: "Hub",
  install: { type: "nsis-silent", args: ["/S"], probe: { cmd: "hub -v", expect: "1" } },
  channels: [{ type: "proxy", url: "https://ghfast.top/https://github.com/x/Hub-Setup.exe" }],
});

describe("destNameOf", () => {
  it("取 URL 末段并去 query/hash", () => {
    expect(destNameOf("https://a.b/c/d.msi?x=1#f")).toBe("d.msi");
    expect(destNameOf("https://ghfast.top/https://github.com/x/Hub.exe")).toBe("Hub.exe");
    expect(destNameOf("https://a.b/")).toBe("download.bin");
  });
});

describe("matchesMachine", () => {
  it("platform 不匹配 → false；arch any/缺省 → true", () => {
    expect(matchesMachine(pkg({ platforms: ["macos"] }), "windows", "x64")).toBe(false);
    expect(matchesMachine(pkg({ arch: undefined }), "windows", "arm64")).toBe(true);
    expect(matchesMachine(pkg({ arch: "arm64" }), "windows", "x64")).toBe(false);
  });
});

describe("KIND_MAP（registry → Rust ExecKind 白名单）", () => {
  it("5 种 install.type 全部可映射", () => {
    expect(KIND_MAP["nsis-silent"]).toBe("nsis-silent");
    expect(KIND_MAP["npm-prefix"]).toBe("npm-prefix");
    expect(KIND_MAP["msi-silent"]).toBe("script");
    expect(KIND_MAP["pkg"]).toBe("script");
    expect(KIND_MAP["script"]).toBe("script");
  });
});

describe("specOf", () => {
  it("msi-silent → script + msiexec /i <dest>（零提权静默）", () => {
    const r = specOf(pkg(), "/home/u")!;
    expect(r.spec.exec.kind).toBe("script");
    expect(r.spec.exec.exe).toBe("msiexec");
    expect(r.spec.exec.args[0]).toBe("/i");
    expect(r.spec.exec.args[1]).toBe("/home/u/.aibox/cache/downloads/node-v26.msi");
    expect(r.spec.download.destName).toBe("node-v26.msi");
    expect(r.spec.download.sha256).toBe(SHA);
    expect(r.spec.probe?.cmd).toBe("node --version");
  });

  it("nsis-silent → exe=安装包绝对路径；npm-prefix → cmd /c npm（Win 可 CreateProcess）", () => {
    const nsis = specOf(hub, "C:/Users/u")!;
    expect(nsis.spec.exec.kind).toBe("nsis-silent");
    expect(nsis.spec.exec.exe).toBe("C:/Users/u/.aibox/cache/downloads/Hub-Setup.exe");

    const npm = specOf(cli, "~")!;
    expect(npm.spec.exec.kind).toBe("npm-prefix");
    expect(npm.spec.exec.exe).toBe("cmd");
    expect(npm.spec.exec.args).toEqual(["/c", "npm", "install", "-g", "openclaw@1"]);
  });

  it("缺 sha256/channels/install → null（不入队）", () => {
    expect(specOf(pkg({ sha256: undefined }), "~")).toBeNull();
    expect(specOf(pkg({ channels: [] }), "~")).toBeNull();
    expect(specOf(pkg({ install: undefined }), "~")).toBeNull();
    expect(specOf(pkg({ install: { type: "dmg" } }), "~")).toBeNull();
  });
});

describe("buildPlan", () => {
  const win = (over: Partial<ScanReport> = {}): ScanReport => ({
    os: { platform: "windows", version: "11", arch: "x64" },
    diskFreeGb: 100,
    memTotalGb: 16,
    node: { installed: false },
    git: { installed: false },
    wsl: { installed: false },
    installedApps: [],
    port18789: { occupied: false },
    checks: [],
    ...over,
  });

  it("global 路线 nsis-hub 步骤序 → 任务序 + DAG 链 + target=末位", () => {
    const p = buildPlan(manifest([hub, cli]), {
      platform: "windows",
      arch: "x64",
      region: "global",
      home: "/h",
    });
    expect(p.route).toBe("nsis-hub");
    expect(p.tasks.map((t) => t.id)).toEqual(["hub", "cli"]);
    expect(p.nodes[0].children).toEqual(["cli"]);
    expect(p.nodes[1].kind).toBe("target");
    expect(p.channel).toBe("ghfast.top"); // 首任务首通道 host
    expect(p.diskMb).toBeGreaterThan(0);
    expect(p.estMinutes).toBeGreaterThanOrEqual(2);
  });

  it("cn 路线 npm-prefix；arm64 机器自动换 -arm64 步骤", () => {
    const armNode = pkg({ id: "node-win-arm64", arch: "arm64" });
    const p = buildPlan(manifest([pkg(), armNode, cli]), {
      platform: "windows",
      arch: "arm64",
      region: "cn",
      home: "/h",
    });
    expect(p.route).toBe("npm-prefix");
    // node-win-x64 步骤在 arm64 上解析为 arm64 包
    expect(p.tasks.map((t) => t.id)).toEqual(["node-win-arm64", "cli"]);
  });

  it("ScanReport 差集：node 已满足 → reuse 入 DAG 不入任务队列", () => {
    const p = buildPlan(manifest([pkg(), cli]), {
      platform: "windows",
      arch: "x64",
      region: "cn",
      home: "/h",
      scan: win({ node: { installed: true, version: "v26.13.0", ok: true } }),
    });
    expect(p.nodes.find((n) => n.id === "node-win-x64")?.kind).toBe("reuse");
    expect(p.tasks.map((t) => t.id)).toEqual(["cli"]);
  });

  it("路线步骤全被平台过滤 → 回落全部匹配包", () => {
    const macOnly = pkg({ id: "mac-only", platforms: ["macos"] });
    const p = buildPlan(manifest([macOnly, cli], { routes: { global: ["r"] }, routePlans: { r: { steps: ["mac-only"] } } }), {
      platform: "windows",
      arch: "x64",
      region: "global",
      home: "/h",
    });
    // 步骤 mac-only 不匹配 windows → 回落：windows 可用包 = cli
    expect(p.tasks.map((t) => t.id)).toEqual(["cli"]);
  });

  it("mock 同构 manifest（mock-full 4 步）→ 4 任务 + spec 齐全", () => {
    const pkgs = [pkg(), pkg({ id: "git", install: { type: "nsis-silent", args: ["/S"] } }), cli, hub];
    const p = buildPlan(
      manifest(pkgs, { routes: { global: ["mock-full"], cn: ["mock-full"] }, routePlans: { "mock-full": { steps: ["node-win-x64", "git", "cli", "hub"] } } }),
      { platform: "windows", arch: "x64", region: "cn", home: "/h" },
    );
    expect(p.tasks).toHaveLength(4);
    expect(p.tasks.every((t) => t.spec?.download && t.spec.exec)).toBe(true);
  });
});
