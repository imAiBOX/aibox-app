//! aibox-net — AiBOX 网络层
//!
//! 本 crate **必须可脱离 Tauri 独立编译与单测**（网页 Hybrid Helper 复用），
//! 不依赖任何系统 GUI/网络库之外的原生依赖（TLS 走 rustls）。

mod channel;
mod doh;
mod probe;

pub use channel::ChannelType;
pub use doh::{parse_doh_response, DohError, DohResolver, DOH_ENDPOINTS};
pub use probe::{probe_head, ProbeOutcome};
