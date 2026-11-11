//! 任务日志落盘（`~/.aibox/logs/<app>/<date>-<task>.log`）。
//!
//! - INFO 级默认：一行一条，`[UTC 时间戳] 内容`；
//! - exec：命令 / 退出码 / stdout+stderr 摘要（调用方保证 ≤4KB）；
//! - download：通道 / 进度终态 / sha 结果；
//! - 诊断包（`diagzip::default_inputs`）walk `logs/**` 收录本目录；
//! - 落盘 best-effort：失败绝不阻断主流程（日志只是可观测性）。

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::fsops;
use crate::paths;

/// 单条日志行上限（截断保护，避免异常输出撑爆日志）。
const MAX_LINE_BYTES: usize = 8 * 1024;

/// 文件名/目录名清洗：仅保留 `[A-Za-z0-9._-]`，其余替换为 `-`；
/// 清洗后为空或全是点（`.`/`..`）→ `unknown`（防路径穿越）。
pub fn sanitize(s: &str) -> String {
    let out: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() || trimmed.trim_matches('.').is_empty() {
        "unknown".to_string()
    } else {
        trimmed
    }
}

/// 日志文件路径（根目录可注入，单测离线）：`<root>/<app>/<date>-<task>.log`。
pub fn log_file_at(root: &Path, app: &str, date: &str, task: &str) -> PathBuf {
    root.join(sanitize(app))
        .join(format!("{}-{}.log", sanitize(date), sanitize(task)))
}

/// UTC 时间戳 `YYYY-MM-DDTHH:MM:SSZ`（与日期同源，均按 UTC）。
pub fn utc_stamp() -> String {
    let ms = crate::registry::now_ms();
    let secs = (ms / 1000) as u64;
    let tod = secs % 86_400;
    format!(
        "{}T{:02}:{:02}:{:02}Z",
        fsops::today_date(),
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60
    )
}

/// 追加一行到任务日志（根/日期可注入，单测离线）。
/// 返回落盘路径（失败 → `None`，不 panic 不阻断）。
pub fn append_at(root: &Path, date: &str, app: &str, task: &str, line: &str) -> Option<PathBuf> {
    let path = log_file_at(root, app, date, task);
    let parent = path.parent()?;
    let _ = std::fs::create_dir_all(parent);
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .ok()?;
    let body = truncate_line(line, MAX_LINE_BYTES);
    writeln!(f, "[{}] {}", utc_stamp(), body).ok()?;
    Some(path)
}

/// 追加一行到 `~/.aibox/logs/<app>/<date>-<task>.log`（best-effort）。
pub fn append(app: &str, task: &str, line: &str) -> Option<PathBuf> {
    append_at(&paths::logs_dir(), &fsops::today_date(), app, task, line)
}

/// 单行截断（超长加标记；UTF-8 字符边界安全）。
fn truncate_line(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let marker = format!("...[truncated, {} bytes total]", s.len());
    let mut end = max.saturating_sub(marker.len());
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{}", &s[..end], marker)
}

/// 任务名（DownloadTask/ExecTask id）→ 日志安全段。
pub fn task_file_stem(task: &str) -> String {
    sanitize(task)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "aibox-tasklog-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn sanitize_strips_path_separators() {
        assert_eq!(sanitize("openclaw-hub-win-x64"), "openclaw-hub-win-x64");
        assert_eq!(sanitize("../evil"), "..-evil", "分隔符被替换，不构成穿越");
        assert_eq!(sanitize("a/b\\c"), "a-b-c");
        assert_eq!(sanitize(".."), "unknown", "纯点目录名拒绝");
        assert_eq!(sanitize("."), "unknown");
        assert_eq!(sanitize(""), "unknown");
        assert_eq!(sanitize("中文"), "unknown");
    }

    #[test]
    fn log_path_matches_convention() {
        let root = Path::new("/data/logs");
        let p = log_file_at(root, "openclaw", "2026-11-11", "openclaw-hub-win-x64");
        assert_eq!(
            p,
            Path::new("/data/logs/openclaw/2026-11-11-openclaw-hub-win-x64.log")
        );
    }

    #[test]
    fn append_at_creates_file_with_timestamped_line() {
        let root = tmp_dir("append");
        let path = append_at(&root, "2026-11-11", "openclaw", "task-1", "download ok: 123 bytes")
            .expect("append should succeed");
        assert!(path.is_file());
        assert_eq!(
            path.file_name().unwrap().to_str().unwrap(),
            "2026-11-11-task-1.log"
        );
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("download ok: 123 bytes"), "{text}");
        assert!(text.contains(']'), "带时间戳前缀: {text}");

        // 追加（append 语义，不覆盖）
        append_at(&root, "2026-11-11", "openclaw", "task-1", "second line").unwrap();
        let text2 = std::fs::read_to_string(&path).unwrap();
        assert!(text2.contains("second line"));
        assert!(text2.contains("download ok: 123 bytes"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn truncate_line_respects_limit() {
        let long = "x".repeat(20_000);
        let t = truncate_line(&long, 1024);
        assert!(t.len() <= 1024);
        assert!(t.contains("[truncated"));
        assert_eq!(truncate_line("short", 1024), "short");
    }

    #[test]
    fn diagzip_includes_written_task_log() {
        use crate::diagzip;
        let root = tmp_dir("diag");
        let logs = root.join("logs");
        append_at(&logs, "2026-11-11", "openclaw", "openclaw-hub-win-x64", "download ok: 1 bytes via DIRECT, sha256=ab")
            .expect("write log");
        let out = root.join("out").join("aibox-diagnostic-2026-11-11.zip");
        let inputs = diagzip::DiagInputs {
            out_path: out.clone(),
            logs_dir: logs,
            extra_files: vec![],
            generated: vec![("version.json".into(), "{}".into())],
        };
        let outcome = diagzip::build_diag_zip(&inputs).unwrap();
        assert!(outcome.entries >= 3, "generated + report + log");
        let names = diagzip::zip_entry_names(&out).unwrap();
        assert!(
            names.iter().any(|n| n
                == "logs/openclaw/2026-11-11-openclaw-hub-win-x64.log"),
            "诊断包必须收录任务日志: {names:?}"
        );
        let log = diagzip::read_zip_entry(&out, "logs/openclaw/2026-11-11-openclaw-hub-win-x64.log")
            .unwrap();
        assert!(log.contains("sha256=ab"));
        let _ = std::fs::remove_dir_all(&root);
    }
}
