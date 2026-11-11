# AiBOX（aibox-app）

**[English](README.md) | 简体中文**

**AiBOX.im（智能盒子）桌面端** —— 面向 Windows/macOS 完全小白用户的安装助手 + 日常维护神器。
口号：**Installed until it works. / 装到能用为止。**

> 当前状态：active development。

## 项目结构

```
aibox-app/
├── src/                    # 前端（React 18 + TypeScript + Vite + Tailwind CSS 4）
│   ├── App.tsx             # 空壳 UI：黑底 #0A0A0A + 主绿 #B6FB50 + 像素方块母题
│   ├── i18n.ts             # i18next 初始化（默认英文，用户手动切换 zh-CN）
│   └── locales/            # en.json（基线）/ zh-CN.json（key 对齐）
├── src-tauri/              # Tauri v2 壳（Rust）
│   ├── tauri.conf.json     # 窗口 1080×768（最小 900×640）；bundle.active=false
│   └── src/main.rs         # 空窗口 + channel_types 命令（壳 ↔ aibox-net 最小链路）
├── crates/aibox-net/       # 网络层 crate（可脱离 Tauri 独立编译单测）
│   └── src/{channel,doh,probe}.rs
│       ├── ChannelType     # DIRECT / MIRROR / PROXY / CUSTOM
│       ├── parse_doh_response  # DoH 响应 JSON → IP 列表（纯函数）
│       └── probe_head      # HEAD + Range: bytes=0-0，TTFB 计时
└── .github/workflows/ci.yml
```

技术栈（定死）：**Tauri v2（Rust 核心）+ React 18 + TypeScript + Vite + Tailwind CSS 4**，
不用重型组件库；多语言 i18next + JSON 语言包（en 为基线，zh-CN 对齐，扩展语言零发版）。

## 便携分发与升级策略

- **分发**：Windows = 单文件 portable exe（`AiBOX-vX.Y.Z-windows-x64.exe`，双击即跑，
  数据写 `%LOCALAPPDATA%\AiBOX\`，不写注册表不改 PATH）；Win10+ 自带 WebView2，无运行时依赖。
- **升级**：
  1. 启动时拉 `aibox.im/meta/latest.json`（源自 GitHub Releases，本地 5 分钟缓存）；
  2. 发现新版 → 首页顶部**黄条通知**「有新版本 vX.Y.Z → 下载」（不强制）；
  3. 点击**跳官网下载页**，下载带版本号的新文件 → 用户双击新 exe 即完成升级。
  **不做自替换机制**（杀软/文件锁失败态对小白是灾难）；自替换仅远期实验项。
- **CI 出包**：tag `v*` → Windows runner 构建 exe 并挂到同名 GitHub Release。

## 托盘常驻与开机自启

- **关闭窗口 = 隐藏到托盘**（不退出）；左键/双击托盘图标重新打开主窗口。
- **右键菜单**：打开 AiBOX / 检查更新 / 退出——只有「退出」才真正结束进程；
  安装进行中点退出会先弹确认（防误杀）。本体有新版本时，菜单首项变为「有新版本 vX → 下载」。
- **开机自启默认开启**（Windows 写 HKCU `Run` 键，免提权；macOS 走 LaunchAgent），
  可在「设置 → 通用」随时关闭；若被安全软件/策略阻止，设置页给出人话提示且不阻塞启动。
- **更新提醒三类**：本体新版（顶部黄条 + 托盘联动）/ 已装应用新版（目录卡片「可升级」徽章 + 聚合条）
  / 目录新增（顶部提示，可关）。检查节奏克制：启动时一次 + 托盘手动 + 常驻每 4 小时一次，
  全部走既有缓存（本体 5 分钟、registry 12 小时、应用更新结果 4 小时）。
- **资源目标**：托盘常驻空闲内存 < 50MB——不引入常驻重型依赖（托盘用 Tauri 内置能力，
  自启用官方 tauri-plugin-autostart，均为纯 Rust）。

## 开发命令

```bash
pnpm install            # 安装前端依赖
pnpm dev                # 纯前端 dev server (http://localhost:5173)
pnpm build              # typecheck (tsc --noEmit) + 生产构建 → dist/
pnpm tauri dev          # Tauri 开发热重载（需本机 WebView2/系统 WebView；真 invoke）
pnpm tauri build        # 本机出 exe（Linux 需 webkit2gtk；Windows 正常出包）
pnpm test               # vitest 全量单测（状态机/编排/CONFIG/i18n/事件，离线）
pnpm check:i18n         # en↔zh-CN key 对齐（CI 同款）

# Rust（三 crate 均可脱离 Tauri 离线单测）
cargo test -p aibox-net -p aibox-netdoctor -p aibox-core  # 网络+体检+原语（162 tests）
cargo check -p aibox-app                                   # 壳编译检查
```

> 三 crate 不依赖 native-tls / 系统 C 库（TLS 走 rustls），Linux/macOS/Windows 均可直接 `cargo test`。
> 纯前端开发用 `VITE_AIBOX_MOCK=1 pnpm dev` 走内存 mock（真 invoke 需 `pnpm tauri dev`）。

## CI（.github/workflows/ci.yml）

| 触发 | Runner | 内容 |
|---|---|---|
| push / PR（main） | ubuntu-latest | `check-i18n` + `pnpm test` + `pnpm build` + `cargo test -p aibox-net -p aibox-netdoctor -p aibox-core` |
| push / PR（main） | ubuntu / windows-latest | `cargo check -p aibox-app`（壳 + cfg(windows) 分支） |
| push / PR（main） | ubuntu-latest | `sanitization`：全历史内部引用扫描（`fetch-depth: 0`） |
| tag `v*` | windows-latest | `pnpm exec tauri build --no-bundle` → 重命名为 `AiBOX-vX.Y.Z-windows-x64.exe` → 上传 GitHub Release（tag 即 release） |

**不打 NSIS/MSI 安装包**（`bundle.active=false` + `--no-bundle`），只要单文件 portable exe。
