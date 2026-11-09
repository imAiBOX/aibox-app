/** 路径常量（C5）。全部人话可读，UI 直接展示。 */
export const PATHS = {
  openclawConfig: "~/.config/openclaw/openclaw.json5",
  openclawBackupSuffix: (ts: string) => `.bak.${ts}`,
  logsDir: "~/.aibox/logs",
  logFile: (app: string, date: string, task: string) =>
    `~/.aibox/logs/${app}/${date}-${task}.log`,
  diagZip: (date: string) => `~/Desktop/aibox-diagnostic-${date}.zip`,
  downloadCache: "~/.aibox/cache",
} as const;

/** 诊断导出前的强制正则扫描模式（硬规则） */
export const SECRET_PATTERNS: RegExp[] = [
  /anthropic/i,
  /openai/i,
  /sk-[A-Za-z0-9]/,
  /api[_-]?key/i,
  /bearer\s+[A-Za-z0-9]/i,
];

/** 对文本做脱敏扫描；命中返回替换后的文本（诊断包导出前调用） */
export function redactSecrets(text: string): { text: string; found: boolean } {
  let found = false;
  let out = text;
  for (const re of SECRET_PATTERNS) {
    if (re.test(out)) {
      found = true;
      out = out.replace(re, "sk-****redacted");
    }
  }
  return { text: out, found };
}
