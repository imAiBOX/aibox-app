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
  /** 区域判定结果（写进 plan.netRegion → last-plan.json，诊断一眼可见） */
  netRegion?: { source: string; effective: "cn" | "global" };
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

/**
 * 内容寻址缓存文件名：`<sha256><扩展名>`（扩展名取自 destName，保留安装器语义）。
 * 与 Rust `aibox_core::download::content_name` 同构（改一处必改另一处）：
 * 跨应用/重装/断点天然去重；安装 exe 路径与 verify 路径都指向它。
 */
export function cacheFileName(destName: string, sha256: string): string {
  const sha = sha256.trim().toLowerCase();
  const i = destName.lastIndexOf(".");
  const ext = i > 0 && i + 1 < destName.length ? destName.slice(i) : "";
  return `${sha}${ext}`;
}

function downloadsPath(home: string, destName: string): string {
  const h = home.replace(/\/+$/, "");
  return `${h}/.aibox/cache/downloads/${destName}`;
}

/** 单包 → 四阶段 spec（kind 映射 + exe 绝对路径） */
export function specOf(pkg: RegistryPackage, home: string): { spec: TaskSpec; exeArgs: string[] } | null {
  const url = pkg.channels?.[0]?.url;
  // 源码归档（hashMode=content）：字节 sha 会漂移 → 用 gzip 内层内容摘要
  const hashMode = pkg.hashMode ?? "bytes";
  const sha256 = hashMode === "content" ? pkg.contentSha256 ?? pkg.sha256 : pkg.sha256;
  const install = pkg.install;
  if (!url || !sha256 || !install) return null;
  const destName = destNameOf(url);
  const destPath = downloadsPath(home, cacheFileName(destName, sha256));
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
    ...(hashMode === "content" ? { hashMode: "content" as const } : {}),
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
  const { platform, arch, region = "global", home = "~", scan, netRegion } = opts;
  const byId = new Map(manifest.packages.map((p) => [p.id, p]));

  // 路线选择（routes[net.effective] 首条可行）
  const routeList = manifest.routes?.[region] ?? manifest.routes?.global ?? [];
  const routeName = routeList[0];
  const steps = routeName ? manifest.routePlans?.[routeName]?.steps : undefined;

  let selected: RegistryPackage[] = [];
  if (steps && steps.length > 0) {
    selected = steps
      .map((id) => byId.get(resolveStepId(id, arch, byId)))
      .filter((p): p is RegistryPackage => !!p && matchesMachine(p, platform, arch))
      .filter((p, i, all) => all.findIndex((candidate) => candidate.id === p.id) === i);
  }
  if (selected.length === 0) {
    // 回落：全部匹配机型的包
    selected = manifest.packages.filter((p) => matchesMachine(p, platform, arch));
  }

  const tasks: Task[] = [];
  const routePlan = routeName ? manifest.routePlans?.[routeName] : undefined;
  // Both legacy OpenClaw routes deliver the CLI. The Hub/Node steps are prerequisites.
  const declared = routePlan?.target ?? (manifest.id === "openclaw" ? "openclaw-cli" : undefined);
  // Registry route steps commonly list mutually exclusive platform/arch installers.
  // After machine filtering, the final selected step is the delivered application;
  // OpenClaw explicitly declares its target because its final step already is the CLI.
  const targetId = declared
    ? resolveStepId(declared, arch, byId)
    : selected[selected.length - 1]?.id ?? (manifest.id === "openclaw" ? "openclaw-cli" : undefined);
  const optionalIds = new Set((routePlan?.optionalSteps ?? []).map((id) => resolveStepId(id, arch, byId)));
  const nodes: PlanNode[] = [];
  let sizeSum = 0;
  for (const pkg of selected) {
    const role = pkg.id === targetId ? "target" : optionalIds.has(pkg.id) ? "optional" : "required";
    const kind = role === "target" ? "install" : reuseKind(pkg, scan);
    nodes.push({
      id: pkg.id,
      name: pkg.name,
      versionRange: pkg.version,
      kind: role === "target" ? "target" : kind,
      children: [],
    });
    for (let i = 0; i < nodes.length - 1; i++) {
      nodes[i].children = [nodes[i + 1].id];
    }
    if (kind === "reuse") continue;
    const built = specOf(pkg, home);
    // Keep an unsupported required/target step visible and block execution at it.
    const spec: TaskSpec | undefined = built ? {
      ...built.spec,
      download: {
        ...built.spec.download,
        appId: manifest.id,
        ...(pkg.sizeBytes != null ? { sizeBytes: pkg.sizeBytes } : {}),
      },
      exec: { ...built.spec.exec, appId: manifest.id },
    } : undefined;
    tasks.push({
      id: pkg.id,
      name: pkg.name,
      version: pkg.version,
      role,
      ...(role === "target" && manifest.version ? { catalogVersion: manifest.version } : {}),
      phase: "download",
      status: "pending",
      spec,
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
    ...(netRegion ? { netRegion } : {}),
  };
}
