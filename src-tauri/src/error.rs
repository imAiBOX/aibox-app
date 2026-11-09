//! 统一 command 错误：`{code, human}`（TS 侧按 code 映射，human 人话英文）。
//!
//! 与 `aibox_core::CoreError` 同形；core 的结构化错误直接 `?` 透传。

use serde::Serialize;

use aibox_core::CoreError;

#[derive(Debug, Clone, Serialize)]
pub struct CommandError {
    pub code: String,
    pub human: String,
}

impl CommandError {
    pub fn new(code: impl Into<String>, human: impl Into<String>) -> Self {
        CommandError {
            code: code.into(),
            human: human.into(),
        }
    }
}

impl From<CoreError> for CommandError {
    fn from(e: CoreError) -> Self {
        CommandError {
            code: e.code,
            human: e.human,
        }
    }
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.human)
    }
}
// Into<InvokeError> 由 tauri 的 blanket impl<T: Serialize> From<T> 提供；
// 不要手写 From<CommandError> for InvokeError（会 E0119 冲突）。
