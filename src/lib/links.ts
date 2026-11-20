/**
 * 官网链接（按当前语言拼路径）。
 *
 * **为什么要这个**：站点是 `/zh/xxx/`、`/en/xxx/` 的**语言前缀结构**，
 * 而安装器里原先写死的是 `https://aibox.im/docs`、`/feedback`、`/download/`
 * —— 这三个根路径在站点上原本全是 404（安装器装完后的主 CTA「打开教程」
 * 一点就弹到错误页，小白直接判定「软件是坏的」）。
 *
 * 现在：
 * - 根路径 `/docs` `/feedback` `/download` 已有兼容跳转页（2026-11-20 补）；
 * - 但**直接拼语言版更好**（少一跳、且不发英文给中文用户）→ 用本模块。
 */

/** 按语言取站点前缀（zh-* / zh 归 zh，其余归 en） */
export function siteLang(lang?: string): "zh" | "en" {
  return (lang ?? "en").toLowerCase().startsWith("zh") ? "zh" : "en";
}

/** 站点内页 URL：siteUrl('/download/', 'zh-CN') → https://aibox.im/zh/download/ */
export function siteUrl(path: string, lang?: string): string {
  const p = path.startsWith("/") ? path : `/${path}`;
  return `https://aibox.im/${siteLang(lang)}${p}`;
}

/** 某个应用自己的教程页（落地页内含该应用完整教程） */
export function appGuideUrl(appId: string, lang?: string): string {
  return siteUrl(`/apps/${appId}/`, lang);
}
