//! probe 原语（V2 不变量）：运行探测命令 + regex expect → **真实版本字符串**。
//!
//! - 跨平台 shell：unix `sh -c <cmd>`，windows `cmd /C <cmd>`（覆盖 `npm`/`openclaw` 等 .cmd 包装）
//! - 超时默认 15s（`AIBOX_PROBE_TIMEOUT_MS` 覆盖）
//! - expect 命中才返回版本；未命中 → `PROBE_MISMATCH` 结构化错误（附输出摘要）
//! - 版本提取：正则有捕获组 → 组1；否则取命中处/输出中第一个 `v?\d+(\.\d+)+` 形态

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::err::{CoreError, CoreResult};
use crate::exec::{run_command, truncate_utf8, CommandSpec};
use crate::redact::scan_and_mask;

/// 探测参数（ProbeSpec）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbeSpec {
    pub cmd: String,
    pub expect: String,
}

/// `probe_run` 返回（`{version}`）。
#[derive(Debug, Clone, Serialize)]
pub struct ProbeResult {
    pub version: String,
}

/// 探测超时（毫秒）。
fn probe_timeout() -> Duration {
    let ms = std::env::var("AIBOX_PROBE_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|&v| v > 0)
        .unwrap_or(15_000);
    Duration::from_millis(ms)
}

/// 提取版本串（纯函数，单测锁语义）。
pub fn extract_version(output: &str, expect: &regex::Regex) -> Option<String> {
    let caps = expect.captures(output)?;
    if caps.len() > 1 {
        if let Some(g) = caps.get(1) {
            let s = g.as_str().trim();
            if !s.is_empty() {
                return Some(s.to_string());
            }
        }
    }
    let whole = caps.get(0)?.as_str();
    if version_like(whole) {
        return Some(whole.to_string());
    }
    // 命中处周边找第一个版本形态（如 expect=`2026\.` → 输出 `2026.11.2`）
    let re = regex::Regex::new(r"v?\d+(?:\.\d+)+").ok()?;
    re.find(output).map(|m| m.as_str().to_string())
}

fn version_like(s: &str) -> bool {
    regex::Regex::new(r"\d+(?:\.\d+)+")
        .map(|re| re.is_match(s))
        .unwrap_or(false)
}

/// 执行探测并返回版本（同步；调用方放入 spawn_blocking）。
pub fn run_probe(spec: &ProbeSpec) -> CoreResult<ProbeResult> {
    let expect_src = spec.expect.trim();
    if expect_src.is_empty() {
        return Err(CoreError::new(
            "PROBE_BAD_EXPECT",
            "Probe expect pattern must not be empty",
        ));
    }
    let expect = regex::Regex::new(expect_src)
        .map_err(|e| CoreError::new("PROBE_BAD_EXPECT", format!("Invalid expect regex: {e}")))?;

    let program;
    let args: Vec<String>;
    #[cfg(windows)]
    {
        program = "cmd".to_string();
        args = vec!["/C".to_string(), spec.cmd.clone()];
    }
    #[cfg(not(windows))]
    {
        program = "sh".to_string();
        args = vec!["-c".to_string(), spec.cmd.clone()];
    }

    let outcome = run_command(&CommandSpec {
        program: &program,
        args: &args,
        cwd: None,
        env: &[],
        timeout: probe_timeout(),
        elevation_guard: false,
    })?;

    let version = extract_version(&outcome.out, &expect).ok_or_else(|| {
        let (safe, _) = scan_and_mask(&outcome.out);
        CoreError::new(
            "PROBE_MISMATCH",
            format!(
                "Command ran (exit {}) but output did not match `{}`. Output: {}",
                outcome.code,
                expect_src,
                truncate_utf8(&safe, 500).replace('\n', " ")
            ),
        )
    })?;

    Ok(ProbeResult { version })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(cmd: &str, expect: &str) -> ProbeSpec {
        ProbeSpec {
            cmd: cmd.into(),
            expect: expect.into(),
        }
    }

    #[test]
    fn extract_version_prefers_capture_group() {
        let re = regex::Regex::new(r"v(\d+\.\d+\.\d+)").unwrap();
        assert_eq!(
            extract_version("node v26.13.0 (x64)", &re).as_deref(),
            Some("26.13.0")
        );
    }

    #[test]
    fn extract_version_full_match_when_version_like() {
        let re = regex::Regex::new(r"v\d+\.\d+\.\d+").unwrap();
        assert_eq!(
            extract_version("git version 2.56.0 / v1.2.3", &re).as_deref(),
            Some("v1.2.3")
        );
    }

    #[test]
    fn extract_version_scans_output_when_match_is_bare() {
        let re = regex::Regex::new(r"2026\.").unwrap();
        assert_eq!(
            extract_version("openclaw 2026.11.2", &re).as_deref(),
            Some("2026.11.2")
        );
    }

    #[test]
    fn no_match_returns_none() {
        let re = regex::Regex::new(r"^v26\.").unwrap();
        assert_eq!(extract_version("nothing", &re), None);
    }

    #[cfg(unix)]
    #[test]
    fn probe_local_script_returns_version() {
        let r = run_probe(&spec("echo 'openclaw 2026.11.2'", r"\d{4}\.")).unwrap();
        assert_eq!(r.version, "2026.11.2");
    }

    #[cfg(unix)]
    #[test]
    fn probe_mismatch_is_structured_error() {
        let e = run_probe(&spec("echo hello", r"^v\d+\.")).unwrap_err();
        assert_eq!(e.code, "PROBE_MISMATCH");
        assert!(e.human.contains("hello"));
    }

    #[cfg(unix)]
    #[test]
    fn probe_invalid_regex_is_structured() {
        let e = run_probe(&spec("echo x", "([unclosed")).unwrap_err();
        assert_eq!(e.code, "PROBE_BAD_EXPECT");
    }

    #[cfg(unix)]
    #[test]
    fn probe_captures_real_binary_version() {
        // 真实本机命令：git --version → "git version 2.x.y"
        let r = run_probe(&spec("git --version", r"git version \d+\.\d+\.\d+")).unwrap();
        assert!(r.version.starts_with("git version 2."), "{}", r.version);
    }
}
