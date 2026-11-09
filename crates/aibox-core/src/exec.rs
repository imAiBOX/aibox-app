//! 静默安装执行原语（权限原则：**零提权**——任何步骤要求提权 = 设计缺陷）。
//!
//! - `nsis-silent`：NSIS 静默参数 `/S`（缺省补上）
//! - `npm-prefix`：npm 前缀路线，注入 `npm_config_prefix=~/.aibox/prefix`（不污染系统 PATH）
//! - `script`：脚本/任意命令直跑
//!
//! 约束：超时可配（默认 15min，`AIBOX_EXEC_TIMEOUT_MS` 覆盖）；输出合并截断 4KB；
//! Windows 下 `CREATE_NO_WINDOW`（「0 终端」红线）；
//! 提权失败 → 结构化错误 `EXEC_ELEVATION_REQUIRED`（**绝不自动提权**）。

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::err::{io_err, CoreError, CoreResult};
use crate::paths;

/// 输出截断上限（stdout/stderr 摘要截断 4KB/任务）。
pub const MAX_OUT_BYTES: usize = 4096;

/// 任务类型（ExecTask.kind）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecKind {
    NsisSilent,
    NpmPrefix,
    Script,
}

impl ExecKind {
    pub fn parse(s: &str) -> CoreResult<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "nsis-silent" => Ok(ExecKind::NsisSilent),
            "npm-prefix" => Ok(ExecKind::NpmPrefix),
            "script" => Ok(ExecKind::Script),
            other => Err(CoreError::new(
                "EXEC_BAD_KIND",
                format!("Unknown exec kind: {other} (expected nsis-silent|npm-prefix|script)"),
            )),
        }
    }
}

/// 执行参数（ExecTask；probe 字段由 `probe_run` 阶段消费，此处不执行）。
pub struct ExecSpec {
    pub id: String,
    pub kind: ExecKind,
    pub exe: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
}

/// 执行结果（`{code, out}`；`timed_out` 供日志/事件区分）。
#[derive(Debug, Clone, Serialize)]
pub struct ExecOutcome {
    pub code: i32,
    pub out: String,
    pub timed_out: bool,
}

/// 按 kind 计算最终参数（纯函数，单测锁语义）。
/// - nsis-silent：确保含 `/S`（NSIS 静默安装，不弹 UI）
/// - npm-prefix：保留原参数（`--prefix` 注入在 env 层，避免与显式参数冲突）
/// - script：原样
pub fn build_args(kind: ExecKind, args: &[String]) -> Vec<String> {
    let mut out = args.to_vec();
    if kind == ExecKind::NsisSilent {
        let has_silent = out
            .iter()
            .any(|a| a == "/S" || a == "-S" || a.eq_ignore_ascii_case("/silent"));
        if !has_silent {
            out.push("/S".to_string());
        }
    }
    out
}

/// 超时毫秒（`AIBOX_EXEC_TIMEOUT_MS` 覆盖，默认 15 分钟）。
pub fn timeout_from_env() -> Duration {
    let ms = std::env::var("AIBOX_EXEC_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|&v| v > 0)
        .unwrap_or(900_000);
    Duration::from_millis(ms)
}

/// 截断到 ≤ `max` 字节（UTF-8 字符边界安全），超出追加行标记。
pub fn truncate_utf8(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let marker = format!("\n...[truncated, {} bytes total]", s.len());
    let mut end = max.saturating_sub(marker.len());
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let head: String = s[..end].to_string();
    format!("{head}{marker}")
}

/// npm 前缀路线的默认 cwd：`~/.aibox/prefix`（隔离原则，不写系统目录）。
pub fn npm_prefix_dir() -> std::path::PathBuf {
    paths::aibox_root().join("prefix")
}

/// 通用子进程执行（带超时/输出收集/4KB 截断；`elevation_guard` 仅安装路径启用）。
pub struct CommandSpec<'a> {
    pub program: &'a str,
    pub args: &'a [String],
    pub cwd: Option<std::path::PathBuf>,
    pub env: &'a [(String, String)],
    pub timeout: Duration,
    /// 零提权红线：识别提权类失败 → `EXEC_ELEVATION_REQUIRED`
    pub elevation_guard: bool,
}

/// 执行安装（同步；调用方负责放入 spawn_blocking）。
pub fn run(spec: &ExecSpec) -> CoreResult<ExecOutcome> {
    run_with_timeout(spec, timeout_from_env())
}

/// 带显式超时的执行（单测用短超时，避免进程级 env 竞态）。
pub fn run_with_timeout(spec: &ExecSpec, timeout: Duration) -> CoreResult<ExecOutcome> {
    let args = build_args(spec.kind, &spec.args);

    // cwd：显式优先；npm-prefix 默认落本地前缀目录
    let cwd = match &spec.cwd {
        Some(c) => Some(paths::expand(c)),
        None if spec.kind == ExecKind::NpmPrefix => {
            let p = npm_prefix_dir();
            let _ = paths::ensure_dir(&p);
            Some(p)
        }
        None => std::env::current_dir().ok(),
    };

    // npm 前缀注入（仅当调用方没显式给 --prefix）
    let mut env: Vec<(String, String)> = Vec::new();
    if spec.kind == ExecKind::NpmPrefix && !args.iter().any(|a| a.starts_with("--prefix")) {
        env.push((
            "npm_config_prefix".into(),
            npm_prefix_dir().to_string_lossy().into_owned(),
        ));
    }

    let outcome = run_command(&CommandSpec {
        program: &spec.exe,
        args: &args,
        cwd,
        env: &env,
        timeout,
        elevation_guard: true,
    })?;

    Ok(outcome)
}

/// 跑一个命令：spawn → 收集 stdout/stderr → 超时杀 → 截断 4KB。
pub fn run_command(cs: &CommandSpec<'_>) -> CoreResult<ExecOutcome> {
    let mut cmd = Command::new(cs.program);
    cmd.args(cs.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(cwd) = &cs.cwd {
        cmd.current_dir(cwd);
    }
    for (k, v) in cs.env {
        cmd.env(k, v);
    }

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // 0x08000000 = CREATE_NO_WINDOW：不弹终端窗口（0 终端红线）
        cmd.creation_flags(0x0800_0000);
    }

    let mut child = cmd.spawn().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => CoreError::new(
            "EXEC_NOT_FOUND",
            format!("Program not found: {} ({})", cs.program, e),
        ),
        std::io::ErrorKind::PermissionDenied => CoreError::new(
            "EXEC_SPAWN_FAILED",
            format!("Not allowed to run {}: {}", cs.program, e),
        ),
        _ => io_err(format!("spawn {}", cs.program), e),
    })?;

    let mut out_buf: Vec<u8> = Vec::new();
    let (tx_o, rx_o) = std::sync::mpsc::channel::<Vec<u8>>();
    let (tx_e, rx_e) = std::sync::mpsc::channel::<Vec<u8>>();

    if let Some(mut so) = child.stdout.take() {
        std::thread::spawn(move || {
            let mut b = Vec::new();
            let _ = so.read_to_end(&mut b);
            let _ = tx_o.send(b);
        });
    }
    if let Some(mut se) = child.stderr.take() {
        std::thread::spawn(move || {
            let mut b = Vec::new();
            let _ = se.read_to_end(&mut b);
            let _ = tx_e.send(b);
        });
    }

    let deadline = Instant::now() + cs.timeout;
    let mut timed_out = false;
    let status_opt = loop {
        match child.try_wait() {
            Ok(Some(st)) => break Some(st),
            Ok(None) => {
                if Instant::now() >= deadline {
                    timed_out = true;
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(io_err("wait for installer", e)),
        }
    };

    // 收集输出（读线程随子进程退出 EOF）
    if let Ok(b) = rx_o.recv_timeout(Duration::from_secs(5)) {
        out_buf.extend_from_slice(&b);
    }
    if let Ok(b) = rx_e.recv_timeout(Duration::from_secs(5)) {
        out_buf.extend_from_slice(&b);
    }

    let raw = String::from_utf8_lossy(&out_buf);
    let out = truncate_utf8(&raw, MAX_OUT_BYTES);

    if timed_out {
        return Ok(ExecOutcome {
            code: -1,
            out: format!("{out}\n...[timed out after {:?}]", cs.timeout),
            timed_out: true,
        });
    }

    let status = status_opt.expect("exit status present when not timed out");
    let code = status.code().unwrap_or(-1);

    // 零提权红线：识别「需要管理员」类失败 → 结构化错误（绝不自动提权）
    if cs.elevation_guard && code != 0 {
        let lower = raw.to_ascii_lowercase();
        if code == 740
            || lower.contains("requires elevation")
            || lower.contains("require administrator")
            || lower.contains("requested operation requires elevation")
            || lower.contains("access is denied")
        {
            return Err(CoreError::new(
                "EXEC_ELEVATION_REQUIRED",
                format!(
                    "This step needs administrator rights, but AiBOX never runs elevated. Exit code {code}. Output: {}",
                    truncate_utf8(&raw, 512).replace('\n', " ")
                ),
            ));
        }
    }

    Ok(ExecOutcome {
        code,
        out,
        timed_out: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(kind: ExecKind, exe: &str, args: &[&str]) -> ExecSpec {
        ExecSpec {
            id: "t1".into(),
            kind,
            exe: exe.into(),
            args: args.iter().map(|s| s.to_string()).collect(),
            cwd: None,
        }
    }

    #[test]
    fn nsis_adds_capital_s_once() {
        let args = vec!["/D=C:\\App".to_string()];
        assert_eq!(
            build_args(ExecKind::NsisSilent, &args),
            vec!["/D=C:\\App", "/S"]
        );
        let args2 = vec!["/S".to_string()];
        assert_eq!(build_args(ExecKind::NsisSilent, &args2), vec!["/S"]);
        // script 不动参数
        assert_eq!(
            build_args(ExecKind::Script, &["a".to_string()]),
            vec!["a".to_string()]
        );
    }

    #[test]
    fn kind_parse_and_reject() {
        assert_eq!(
            ExecKind::parse("nsis-silent").unwrap(),
            ExecKind::NsisSilent
        );
        assert_eq!(ExecKind::parse("NPM-Prefix").unwrap(), ExecKind::NpmPrefix);
        assert!(ExecKind::parse("msi").is_err());
    }

    #[test]
    fn truncate_utf8_respects_boundary() {
        assert_eq!(truncate_utf8("hello", 100), "hello");
        let long = "中".repeat(100); // 300 bytes
        let t = truncate_utf8(&long, 50);
        assert!(t.len() <= 50 + 60);
        assert!(t.contains("[truncated"));
        // 不 panic = 字符边界处理正确
    }

    #[cfg(unix)]
    #[test]
    fn script_runs_and_captures_exit_code() {
        let out = run(&spec(
            ExecKind::Script,
            "sh",
            &["-c", "echo out; echo err 1>&2; exit 3"],
        ))
        .unwrap();
        assert_eq!(out.code, 3);
        assert!(out.out.contains("out"));
        assert!(out.out.contains("err"));
        assert!(!out.timed_out);
    }

    #[cfg(unix)]
    #[test]
    fn npm_prefix_injects_env() {
        let out = run(&spec(
            ExecKind::NpmPrefix,
            "sh",
            &["-c", "echo $npm_config_prefix"],
        ))
        .unwrap();
        assert!(out.out.contains(".aibox/prefix"), "got: {}", out.out);
    }

    #[cfg(unix)]
    #[test]
    fn timeout_kills_and_marks() {
        let out = run_with_timeout(
            &spec(ExecKind::Script, "sh", &["-c", "sleep 10"]),
            Duration::from_millis(400),
        )
        .unwrap();
        assert!(out.timed_out);
        assert_eq!(out.code, -1);
        assert!(out.out.contains("timed out"));
    }

    #[cfg(unix)]
    #[test]
    fn output_is_truncated_to_4kb() {
        // ~24KB 输出
        let out = run(&spec(
            ExecKind::Script,
            "sh",
            &[
                "-c",
                "i=0; while [ $i -lt 3000 ]; do echo aaaaaaaa; i=$((i+1)); done",
            ],
        ))
        .unwrap();
        assert!(out.out.len() <= MAX_OUT_BYTES, "len={}", out.out.len());
        assert!(out.out.contains("[truncated"));
    }

    #[cfg(unix)]
    #[test]
    fn missing_exe_is_structured_error() {
        let e = run(&spec(ExecKind::Script, "/no/such/binary-xyz", &[])).unwrap_err();
        assert_eq!(e.code, "EXEC_NOT_FOUND");
        assert!(!e.human.is_empty());
    }
}
