//! 统一结构化错误：`{code, human}`（code 给 TS 映射，human 人话英文）。

use std::fmt;

use serde::Serialize;

pub type CoreResult<T> = Result<T, CoreError>;

/// 稳定错误结构（与 src-tauri `CommandError` 同形，直接可序列化到 TS）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CoreError {
    /// 机器可读错误码（如 `NET_ALL_CHANNELS_DEAD` / `EXEC_ELEVATION_REQUIRED`）
    pub code: String,
    /// 人话英文（TS 侧可再映射为本地化文案）
    pub human: String,
}

impl CoreError {
    pub fn new(code: impl Into<String>, human: impl Into<String>) -> Self {
        CoreError {
            code: code.into(),
            human: human.into(),
        }
    }

    /// 附加上下文但保留原 code（如通道失败原因表）。
    pub fn with_detail(mut self, detail: impl fmt::Display) -> Self {
        self.human = format!("{} ({})", self.human, detail);
        self
    }
}

impl fmt::Display for CoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.human)
    }
}

impl std::error::Error for CoreError {}

impl From<aibox_net::DownloadError> for CoreError {
    fn from(e: aibox_net::DownloadError) -> Self {
        let mut human = e.human_message();
        // 全通道失败附逐通道原因表（人话报错）
        let report = e.channel_report();
        if !report.is_empty() {
            let rows: Vec<String> = report.iter().map(|(c, r)| format!("{c}: {r}")).collect();
            human = format!("{human} [{}]", rows.join("; "));
        }
        CoreError::new(e.code(), human)
    }
}

impl From<std::io::Error> for CoreError {
    fn from(e: std::io::Error) -> Self {
        CoreError::new("IO_ERROR", format!("Local file operation failed: {e}"))
    }
}

/// 带上下文的 IO 错误。
pub fn io_err(context: impl fmt::Display, e: std::io::Error) -> CoreError {
    CoreError::new("IO_ERROR", format!("{context}: {e}"))
}
