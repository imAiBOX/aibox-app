import i18n from "i18next";
import { initReactI18next } from "react-i18next";
import en from "./locales/en.json";
import zhCN from "./locales/zh-CN.json";

const STORAGE_KEY = "aibox.lang";

// 2026-11-27 语言面收敛：只保留英文 + 简体中文（与官网两语一致；繁体内容只有
// UI 翻译、应用介绍/教程仍是英文，混搭体验比不提供更差 → 下线 zh-TW）。
export const SUPPORTED_LANGS = ["en", "zh-CN"] as const;
export type Lang = (typeof SUPPORTED_LANGS)[number];

/** 首屏语言：localStorage 保存值（用户手动切换）> 默认 en（国际版产品：面向英文与中文用户，默认英文，可选简体中文） */
function detect(): Lang {
  try {
    const saved = localStorage.getItem(STORAGE_KEY);
    if (saved && (SUPPORTED_LANGS as readonly string[]).includes(saved)) return saved as Lang;
  } catch {
    /* localStorage 不可用（隐私模式等）→ 用默认英文 */
  }
  return "en";
}

i18n.use(initReactI18next).init({
  resources: {
    en: { translation: en },
    "zh-CN": { translation: zhCN },
  },
  lng: detect(),
  fallbackLng: "en",
  interpolation: { escapeValue: false },
});

export function setLang(lang: Lang): void {
  void i18n.changeLanguage(lang);
  try {
    localStorage.setItem(STORAGE_KEY, lang);
  } catch {
    /* 忽略写入失败 */
  }
  document.documentElement.lang = lang;
}

export default i18n;
