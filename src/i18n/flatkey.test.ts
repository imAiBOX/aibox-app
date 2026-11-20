/**
 * 扁平化点号键（`{"phaseHint.download": "..."}`）在 i18next 里能否被
 * `t("s4.phaseHint.download")` 解析到。
 *
 * 为什么要有这个测试：本项目的 i18n 字典里**两种写法混用**——
 * 一部分键是嵌套对象（`s4: { phaseHint: { download } }`），
 * 一部分是**带点号的扁平键**（`s4: { "phaseHint.download": ... }`）。
 * i18next 默认 keySeparator="."，扁平键能否解析取决于它的 deepFind
 * 是否先试「整条路径的字面查找」—— 这是不能靠猜的，必须测出来。
 */
import { describe, expect, it, beforeAll } from "vitest";
import i18n from "../i18n";

beforeAll(async () => {
  await i18n.changeLanguage("zh-CN");
});

describe("扁平点号键解析", () => {
  it("s4.phaseHint.download 能解析到（字典里是扁平键写法）", () => {
    expect(i18n.t("s4.phaseHint.download")).toBe(
      "正在下载安装包，还没开始安装。",
    );
  });

  it("嵌套写法也能解析（对照）", () => {
    expect(i18n.t("s4.title")).toBe("安装中");
  });

  it("未知键返回键名本身（不是空串）", () => {
    expect(i18n.t("s4.definitely_not_a_key")).toBe("s4.definitely_not_a_key");
  });
});
