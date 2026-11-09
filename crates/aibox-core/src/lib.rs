//! aibox-core — AiBOX 稳定原语层。
//!
//! 本 crate **必须可脱离 Tauri 独立编译与单测**（Linux 本机 `cargo test -p aibox-core`）。
//! 业务编排在 TS 侧；这里只提供可复用、可离线单测的原语：
//!
//! | 模块 | 内容 |
//! |---|---|
//! | [`paths`] | `~/` / `%LOCALAPPDATA%` 路径展开、`~/.aibox/` 数据根 |
//! | [`redact`] | key 脱敏 `sk-****1234` + 诊断导出正则扫描 |
//! | [`download`] | 包装 aibox-net `download_fallback`（dest 落 cache/downloads，进度闭包上抛） |
//! | [`verify`] | SHA-256 校验（复用 net 的 `sha256_file`，S1 零豁免） |
//! | [`exec`] | 静默安装执行（nsis-silent / npm-prefix / script，零提权，超时+4KB 截断） |
//! | [`probe`] | 运行探测命令 + regex expect → 真实版本字符串（V2 不变量） |
//! | [`fsops`] | 备份 `.bak.<ts>`、JSON/JSON5 合并保键、原子写+回读校验 |
//! | [`config`] | CONFIG C5：`config_write` 全流程（备份→合并→回读→脱敏返回） |
//! | [`scan`] | `scan_system`：OS/磁盘/内存/Node/Git/WSL/已装应用/端口 18789（cfg 双实现） |
//! | [`diagzip`] | 诊断打包（日志+ScanReport+计划+版本+系统信息）+ 强制脱敏扫描 |
//! | [`registry`] | registry 多源交叉（jsdelivr/raw/GitHub API 任二一致，S2）+ 12h 缓存 + locales |
//! | [`region`] | 区域判定（初值信号 + github 实测覆盖） |
//! | [`matrix`] | 资源-通道矩阵展开（registry 数据优先，内置 v1.1 基线兜底） |
//! | [`verify_send`] | VERIFY：经 provider 发真实 "Say OK"（max_tokens=8） |
//! | [`update`] | update_check：GitHub releases/latest（调用方做 5min 缓存） |

pub mod config;
pub mod diagzip;
pub mod download;
pub mod exec;
pub mod fsops;
pub mod matrix;
pub mod paths;
pub mod probe;
pub mod redact;
pub mod region;
pub mod registry;
pub mod scan;
pub mod update;
pub mod verify;
pub mod verify_send;

mod err;

pub use err::{CoreError, CoreResult};

/// aibox 数据根：`~/.aibox/`（隔离原则：aibox 管的一切都在此前缀内）。
pub const AIBOX_DIR: &str = ".aibox";
