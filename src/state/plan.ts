/**
 * PLAN 构建：
 * registry app.json（裸文档）+ ScanReport → Plan（DAG 节点 + 任务队列 + 四阶段 spec）。
 *
 * - 路线：routes[region] 首条 → routePlans[route].steps 有序 package id；
 *   缺失/解析不到 → 回落「全部匹配 platform+arch 的 packages」。
 * - arch 变体：steps 里 `-x64` 步骤在 arm64 机器上自动换 `-arm64` 包（registry note 语义）。
 * - 差集（与 ScanReport 求差集）：node 已满足 → reuse 不入队；git 已装 → reuse 不入队。
 * - spec：DownloadTask（channels+sha256+destName）/ ExecTask（kind 映射，见 KIND_MAP）/ ProbeSpec。
 *   exe 需绝对路径（Rust exec 只展开 cwd 的 `~`）→ 由调用方传 home；mock 缺省 '~'。
 */
import type {
  AppManifest,
  RegistryPackage,
  ScanReport,
} from "../net/client";
import type { Plan, PlanNode, Task, TaskSpec } from "./types";

export interface BuildPlanOptions {
  /** ScanReport.os.platform：windows | macos | linux */
  platform: string;
  /** ScanReport.os.arch：x64 | arm64 */
  arch: string;
  region?: "cn" | "global";
  /** 用户主目录（真 Tauri homeDir()）；缺省 '~'（mock/测试） */
  home?: string;
  scan?: ScanReport | null;
}

/** install.type → ExecTask.kind 映射（registry 5 种 vs Rust ExecKind 白名单 3 种） */
export const KIND_MAP: Record<string, "nsis-silent" | "npm-prefix" | "script"> = {
  "nsis-silent": "nsis-silent",
  "npm-prefix": "npm-prefix",
  // msi-silent → script + msiexec（零提权静默：/i + registry args）
  "msi-silent": "script",
  // pkg（macOS）→ script + installer
  pkg: "script",
  script: "script",
};

/** 从 URL 取落盘文件名（destName；query/hash 去掉） */
export function destNameOf(url: string): string {
  const path = url.split("?")[0].split("#")[0];
  const seg = path.substring(path.lastIndexOf("/") + 1);
  return seg || "download.bin";
}

function downloadsPath(home: string, destName: string): string {
  const h = home.replace(/\/+$/, "");
  return `${h}/.aibox/cache/downloads/${destName}`;
}

/** 单包 → 四阶段 spec（kind 映射 + exe 绝对路径） */
export function specOf(pkg: RegistryPackage, home: string): { spec: TaskSpec; exeArgs: string[] } | null {
  const url = pkg.channels?.[0]?.url;
  const sha256 = pkg.sha256;
  const install = pkg.install;
  if (!url || !sha256 || !install) return null;
  const destName = destNameOf(url);
  const destPath = downloadsPath(home, destName);
  const args = install.args ?? [];
  const kind = KIND_MAP[install.type];
  if (!kind) return null;

  let exe: string;
  let execArgs: string[];
  switch (install.type) {
    case "nsis-silent":
      exe = destPath;
      execArgs = args;
      break;
    case "msi-silent":
      exe = "msiexec";
      execArgs = ["/i", destPath, ...args];
      break;
    case "npm-prefix":
      // Windows 上裸 "npm" 无法 CreateProcess → cmd /c npm（cwd 默认 ~/.aibox/prefix，Rust 注入）
      exe = "cmd";
      execArgs = ["/c", "npm", ...args];
      break;
    case "pkg":
      exe = "installer";
      execArgs = ["-pkg", destPath, ...args];
      break;
    default:
      // script：args[0] 即程序（如 hdiutil），其余为参数
      exe = args[0] ?? "sh";
      execArgs = args.slice(1);
      break;
  }

  const download = {
    id: pkg.id,
    channels: pkg.channels.map((c) => ({ type: c.type, url: c.url })),
    sha256,
    destName,
  };
  const exec = {
    id: pkg.id,
    kind,
    exe,
    args: execArgs,
    ...(install.probe ? { probe: install.probe } : {}),
  };
  return {
    spec: { download, exec, probe: install.probe },
    exeArgs: execArgs,
  };
}

/** 包是否匹配本机 platform/arch（arch 缺省/'any' 视为匹配） */
export function matchesMachine(pkg: RegistryPackage, platform: string, arch: string): boolean {
  if (pkg.platforms.length > 0 && !pkg.platforms.includes(platform)) return false;
  if (!pkg.arch || pkg.arch === "any") return true;
  return pkg.arch === arch;
}

/** arm64 机器上把 steps 中的 -x64 步骤换成 -arm64 变体 */
function resolveStepId(id: string, arch: string, byId: Map<string, RegistryPackage>): string {
  if (arch === "arm64" && id.endsWith("-x64")) {
    const alt = `${id.slice(0, -4)}-arm64`;
    if (byId.has(alt)) return alt;
  }
  return id;
}

/** 差集：已满足的组件标记 reuse（node 满足 runtime / git 已装） */
function reuseKind(pkg: RegistryPackage, scan?: ScanReport | null): "reuse" | "install" {
  if (scan && pkg.id.startsWith("node-") && scan.node?.ok) return "reuse";
  if (scan && pkg.id.startsWith("git-") && scan.git?.installed) return "reuse";
  return "install";
}

/**
 * 构建安装计划。纯函数（home 仅拼路径字符串），可单测。
 * 任务顺序 = 路线 steps 顺序（依赖先行）；reuse 包只入 DAG 不入任务队列。
 */
export function buildPlan(manifest: AppManifest, opts: BuildPlanOptions): Plan {
  const { platform, arch, region = "global", home = "~", scan } = opts;
  const byId = new Map(manifest.packages.map((p) => [p.id, p]));

  // 路线选择（routes[net.effective] 首条可行）
  const routeList = manifest.routes?.[region] ?? manifest.routes?.global ?? [];
  const routeName = routeList[0];
  const steps = routeName ? manifest.routePlans?.[routeName]?.steps : undefined;

  let selected: RegistryPackage[] = [];
  if (steps && steps.length > 0) {
    selected = steps
      .map((id) => byId.get(resolveStepId(id, arch, byId)))
      .filter((p): p is RegistryPackage => !!p && matchesMachine(p, platform, arch));
  }
  if (selected.length === 0) {
    // 回落：全部匹配机型的包
    selected = manifest.packages.filter((p) => matchesMachine(p, platform, arch));
  }

  const tasks: Task[] = [];
  const nodes: PlanNode[] = [];
  let sizeSum = 0;
  for (const pkg of selected) {
    const kind = reuseKind(pkg, scan);
    const isLast = pkg.id === selected[selected.length - 1]?.id;
    nodes.push({
      id: pkg.id,
      name: pkg.name,
      versionRange: pkg.version,
      kind: isLast && kind === "install" ? "target" : kind,
      children: [],
    });
    for (let i = 0; i < nodes.length - 1; i++) {
      nodes[i].children = [nodes[i + 1].id];
    }
    if (kind === "reuse") continue;
    const built = specOf(pkg, home);
    if (!built) continue;
    tasks.push({
      id: pkg.id,
      name: pkg.name,
      version: pkg.version,
      phase: "download",
      status: "pending",
      spec: built.spec,
    });
    sizeSum += pkg.sizeBytes ?? 0;
  }
  // 末位节点无 children
  if (nodes.length > 0) nodes[nodes.length - 1].children = [];

  const firstChannel = tasks[0]?.spec?.download.channels[0]?.url ?? "";
  let channel = "";
  try {
    channel = firstChannel ? new URL(firstChannel).hostname : "";
  } catch {
    channel = firstChannel;
  }

  const diskMb = sizeSum > 0 ? Math.round(sizeSum / 1e6) : 760;
  return {
    appId: manifest.id,
    nodes,
    tasks,
    route: routeName ?? "direct",
    channel: channel || "direct",
    diskMb,
    estMinutes: Math.max(2, Math.round(diskMb / 120)),
  };
}
