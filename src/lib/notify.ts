/**
 * 系统通知（桌面通知中心）：**托盘常驻时唯一能让用户感知的通道**。
 *
 * **为什么必须有这一层**：AiBOX 的定位是「运行后收进托盘、长期在后台」。
 * 窗口关着时，界面里的黄条用户根本看不到 —— 只有桌面通知能穿过「窗口不在眼前」
 * 这一层。所以「有升级 / 有上新」必须能推到通知中心。
 *
 * 原则（体验优先）：
 * - **克制**：同一事件不重复通知（去重键由调用方给）
 * - **可关**：通知偏好关掉对应类别后一律不发
 * - **不打扰**：通知正文写人话，不写错误码/内部词
 */
import { isPermissionGranted, requestPermission, sendNotification } from "@tauri-apps/plugin-notification";
import { isMacPlatform } from "../platform";
import { isMock } from "../net/client";

/** 权限只申请一次（记在内存，避免每次都弹系统对话框） */
let asked = false;

/** 是否可以发系统通知（非 Tauri / mock → false，静默跳过） */
export async function canNotify(): Promise<boolean> {
  if (isMock()) return false;
  try {
    if (await isPermissionGranted()) return true;
    if (!asked) {
      asked = true;
      const res = await requestPermission();
      return res === "granted";
    }
    return false;
  } catch {
    return false;
  }
}

/** 发一条系统通知（失败静默：通知不是关键路径，绝不能因此报错） */
export async function notifyDesktop(title: string, body: string): Promise<void> {
  if (!(await canNotify())) return;
  try {
    sendNotification({ title, body });
  } catch {
    /* 静默 */
  }
}

/**
 * 「AiBOX 现在在哪」的人话说明（**按平台自适应**）。
 *
 * - Windows：右下角**任务栏图标区**（托盘）
 * - macOS：**菜单栏右上角**（macOS 没有右下角托盘；Tauri 托盘在 macOS 就是菜单栏图标）
 */
export function trayLocationHint(lang?: string): string {
  const zh = (lang ?? "en").toLowerCase().startsWith("zh");
  if (isMacPlatform()) {
    return zh
      ? "屏幕右上角的菜单栏图标里"
      : "the menu bar at the top-right of your screen";
  }
  return zh ? "屏幕右下角的任务栏图标区里" : "the taskbar tray at the bottom-right";
}
