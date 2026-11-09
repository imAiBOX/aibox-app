//! aibox-net — AiBOX 网络层
//!
//! 本 crate **必须可脱离 Tauri 独立编译与单测**（网页 Hybrid Helper 复用），
//! 不依赖任何系统 GUI；TLS 走 rustls（无系统 C 库）。
//!
//! | 模块 | 内容 |
//! |---|---|
//! | [`channel`] | 通道类型 ChannelType |
//! | [`region`] | RegionPolicy：初值判定 / 实测覆盖 / 通道排序 |
//! | [`score`] | ProbeScore：记分 / 15min 缓存 / 剔除与复活 |
//! | `download` | 8MB 分段 + 断点续传 + 降级 + S1 SHA-256 |
//! | [`doh`] | DohResolver：并发池竞速 + TTL 300s + 触发语义 |
//! | [`proxy`] | ProxyAwareness：env/注册表只读检测 + 活死判定 + 直连决策 |

mod channel;
mod clock;
mod doh;
mod download;
mod probe;
mod proxy;
mod region;
mod score;

#[cfg(test)]
mod testutil;

pub use channel::ChannelType;
pub use clock::{system_clock, Clock, ManualClock, SharedClock, SystemClock};
pub use doh::{
    doh_should_enable, parse_doh_response, DohError, DohFetcher, DohResolver, DOH_ENDPOINTS,
    DOH_TTL_SECS,
};
pub use download::{
    download_fallback, sha256_file, sha256_hex, ChannelFailure, ChannelUrl, DownloadError,
    DownloadOptions, DownloadProgress, DownloadReceipt, DEFAULT_TIMEOUT, SEGMENT_RETRIES,
    SEGMENT_SIZE,
};
pub use probe::{probe_head, probe_head_with, ProbeOutcome};
pub use proxy::{
    check_proxy, decide_proxy, detect_effective_proxy, detect_env_proxies,
    detect_registry_proxies, proxies_from_env, ProxyConfig, ProxyLiveness, ProxySource,
};
pub use region::{
    detect_initial_region, initial_region, order_channels, override_from_probe, resolve_region,
    signals_from_env, RegionKind, RegionSignals, RegionSource, RegionState,
};
pub use score::{
    ProbeRecord, ProbeScore, FAIL_STRIKE_LIMIT, REVIVE_AFTER_MS, SCORE_TTL_MS,
};
