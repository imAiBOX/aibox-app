# AiBOX (aibox-app)

**AiBOX.im desktop app** — a one-click installer and maintenance companion for Windows/macOS
users who just want things to work.

Tagline: **Installed until it works.**

> Status: active development.

**English | [简体中文](README.zh-CN.md)**

## Project layout

```
aibox-app/
├── src/                    # Frontend (React 18 + TypeScript + Vite + Tailwind CSS 4)
│   ├── App.tsx             # Shell UI: near-black #0A0A0A + brand green #B6FB50, pixel-block motif
│   ├── i18n.ts             # i18next init (English default; zh-CN opt-in via user switch)
│   └── locales/            # en.json (baseline) / zh-CN.json (key-parity enforced in CI)
├── src-tauri/              # Tauri v2 shell (Rust)
│   ├── tauri.conf.json     # Window 1080×768 (min 900×640); bundle.active=false
│   └── src/                # Commands, tray, autostart wiring (shell <-> crates)
├── crates/aibox-net/       # Network layer crate (compiles & unit-tests without Tauri)
├── crates/aibox-netdoctor/ # Network diagnostics D0-D6 (offline-injectable transports)
├── crates/aibox-core/      # Stable primitives: download/verify/exec/probe/fs/diag-zip/registry
└── .github/workflows/ci.yml
```

Stack (fixed): **Tauri v2 (Rust core) + React 18 + TypeScript + Vite + Tailwind CSS 4**,
no heavy component libraries; i18next + JSON locale packs (English baseline, zh-CN parity,
new languages ship as registry files with zero app release).

## Portable distribution & upgrade

- **Distribution**: Windows = single-file portable exe (`AiBOX-vX.Y.Z-windows-x64.exe`),
  double-click to run, data lives in `%LOCALAPPDATA%\AiBOX\` (no registry writes, no PATH edits).
  Win10+ ships WebView2 — no runtime download needed.
- **Upgrade**:
  1. On startup the app checks the latest GitHub Release (5-minute local cache);
  2. If a newer version exists, a **yellow banner** appears ("New version vX.Y.Z → Download"), never forced;
  3. Clicking opens the official download page; the user double-clicks the new exe — done.
  **No self-replacing updater** (antivirus/file-lock failure modes are hostile to novice users).
- **CI**: pushing tag `v*` builds the exe on a Windows runner and attaches it to the GitHub Release
  (version injected from the tag).

## Tray, autostart & update reminders

- **Closing the window hides to tray** (does not quit). Left-click / double-click the tray icon
  to reopen the main window.
- **Right-click menu**: Open AiBOX / Check for updates / Quit — only *Quit* ends the process;
  quitting during an active install asks for confirmation first. When a new app version exists the
  first menu item becomes "New version vX → Download".
- **Autostart is ON by default** (Windows: HKCU `Run` key, no elevation; macOS: LaunchAgent),
  toggleable under Settings → General. If blocked by security software the settings page shows a
  plain-language notice and startup is never blocked.
- **Three reminder types**: new app release (banner + tray), installed-app updates
  (catalog badge + aggregate bar), newly catalogued apps (dismissible notice). Check cadence is
  deliberately quiet: once at startup + manual tray trigger + every 4h while resident, all behind
  existing caches (5 min / 12 h / 4 h).
- **Resource target**: < 50 MB idle memory while resident (built-in Tauri tray, official
  autostart plugin, pure Rust).

## Development

```bash
pnpm install            # frontend deps
pnpm dev                # plain frontend dev server (http://localhost:5173)
pnpm build              # typecheck (tsc --noEmit) + production build → dist/
pnpm tauri dev          # Tauri dev with hot reload (real invoke)
pnpm tauri build        # local exe (Linux needs webkit2gtk)
pnpm test               # vitest suite (state machine, orchestration, config, i18n — offline)
pnpm check:i18n         # en ↔ zh-CN key parity (same check as CI)

# Rust (all three crates unit-test offline, without Tauri)
cargo test -p aibox-net -p aibox-netdoctor -p aibox-core
cargo check -p aibox-app
```

> No native-tls / system C libraries (TLS via rustls) — `cargo test` runs on Linux/macOS/Windows.
> Frontend-only development: `VITE_AIBOX_MOCK=1 pnpm dev` uses the in-memory mock.

## CI (.github/workflows/ci.yml)

| Trigger | Runner | Steps |
|---|---|---|
| push / PR (main) | ubuntu | i18n parity + `pnpm test` + `pnpm build` + `cargo test` (3 crates) |
| push / PR (main) | ubuntu + windows | `cargo check -p aibox-app` (shell + cfg(windows) branches) |
| push / PR (main) | ubuntu | `sanitization`: full-history scan (`fetch-depth: 0`) |
| tag `v*` | windows | `tauri build --no-bundle` → rename `AiBOX-vX.Y.Z-windows-x64.exe` → GitHub Release |

No NSIS/MSI installers (`bundle.active=false` + `--no-bundle`) — single portable exe only.
