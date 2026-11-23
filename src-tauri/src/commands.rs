//! 全量 command 实现（签名冻结）。
//!
//! 薄封装原则：参数解组 → `aibox-core` 原语（可单测）→ 结果/事件。
//! 重活一律 `spawn_blocking`（async command 内不得阻塞 async runtime）。

use std::panic::AssertUnwindSafe;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};

use aibox_core::config::{self, ConfigWriteInput, ConfigWriteResult};
use aibox_core::diagzip;
use aibox_core::download::{self, DownloadSpec};
use aibox_core::exec::{self, ExecKind, ExecSpec};
use aibox_core::manage::{self, BackupOutcome, ManifestEntry, RemoveOutcome};
use aibox_core::manage_policy::MaintenancePolicy;
use aibox_core::matrix::{self, ProbeTarget};
use aibox_core::probe::{self, ProbeSpec};
use aibox_core::prefetch::{self, PrefetchStatus};
use aibox_core::restore::{self, BackupRecord, RestoreOutcome, RestorePlan};
use aibox_core::scan::{self, ScanReport};
use aibox_core::update::{self, UpdateInfo};
use aibox_core::verify_send::{self, VerifyInput, VerifyResult};
use aibox_core::{paths, registry};
use aibox_core::{CoreError, CoreResult};

use crate::error::CommandError;
use crate::state::{self, JobHandle};

type Cmd<T> = Result<T, CommandError>;

/// 把 core 的同步原语丢进阻塞线程池。
async fn blocking<T, F>(f: F) -> Cmd<T>
where
    T: Send + 'static,
    F: FnOnce() -> CoreResult<T> + Send + 'static,
{
    let inner = tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| CommandError::new("INTERNAL", format!("background task failed: {e}")))?;
    inner.map_err(CommandError::from)
}

// ── 系统与注册表 ─────────────────────────────────────────────────────

#[tauri::command]
pub async fn scan_system() -> Cmd<ScanReport> {
    blocking(|| Ok(scan::scan_system())).await
}

#[tauri::command]
pub async fn registry_index(force: Option<bool>) -> Cmd<Value> {
    let (mut doc, src) = blocking(move || registry::fetch_index(force.unwrap_or(false))).await?;
    if src == "snapshot" {
        // 网络+缓存全挂 → 构建时嵌入快照兑底；标记给前端渲染「离线目录（快照）」徽章
        doc["offlineSnapshot"] = json!(true);
    }
    enrich_installed(&mut doc);
    Ok(doc)
}

/// 给 index.apps[] 注入 installedVersion（来自 ~/.aibox/install-manifest.json，尽力而为）。
fn enrich_installed(doc: &mut Value) {
    let manifest = std::fs::read_to_string(paths::install_manifest_file())
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok());
    let Some(manifest) = manifest else {
        return;
    };
    let Some(apps) = doc.get_mut("apps").and_then(|a| a.as_array_mut()) else {
        return;
    };
    for app in apps.iter_mut() {
        let Some(id) = app.get("id").and_then(|i| i.as_str()) else {
            continue;
        };
        let version = manifest
            .get("apps")
            .and_then(|a| a.as_array())
            .and_then(|arr| {
                arr.iter()
                    .find(|x| x.get("id").and_then(|i| i.as_str()) == Some(id))
                    .and_then(|x| x.get("version"))
                    .and_then(|v| v.as_str())
                    .map(String::from)
            })
            .or_else(|| {
                manifest
                    .get(id)
                    .and_then(|x| x.get("version"))
                    .and_then(|v| v.as_str())
                    .map(String::from)
            });
        if let Some(v) = version {
            app["installedVersion"] = json!(v);
        }
    }
}

#[tauri::command]
pub async fn registry_app(id: String) -> Cmd<Value> {
    let (doc, _) = blocking(move || registry::fetch_app(&id, false)).await?;
    Ok(doc)
}

// ── 打开目录（让用户看见「东西装到哪了」）─────────────────────────────

/// 打开一个目录（系统默认文件管理器）。路径里的 `~` 会展开。
///
/// **为什么需要**：装完后小白最想知道「装到哪了、怎么打开」。
/// 旧版完成页只列名字+版本，什么都不说 → 用户自己去网上搜「XXX 怎么启动」。
/// 这里给出**真实可点的入口**：点一下就打开那个文件夹。
///
/// 安全：
/// - 只开目录（不执行任何东西）；路径来自 registry 声明或安装前缀；
/// - **只允许打开 home 之内的目录**（审计 P2-6）：前端传什么就开什么 = 前端被注入 /
///   被改就能让系统打开任意目录（快捷方式、网络位置 → 信息泄露 / 钓鱼）。
///   Rust 侧兜底：展开 `~` → canonicalize（解析软链）→ 必须落在 canonical home 之内，
///   否则 `REVEAL_FORBIDDEN`。
#[tauri::command]
pub async fn reveal_dir(path: String) -> Cmd<()> {
    let p = paths::expand(&path);
    // 解析软链后的真实路径（不解析就挡不住「home 里的软链指向别处」）
    let real = std::fs::canonicalize(&p).map_err(|e| {
        CommandError::new(
            "REVEAL_NOT_FOUND",
            format!("not a directory: {} ({e})", p.display()),
        )
    })?;
    // 必须落在 home 之内（两边都 canonicalize → Windows 的 `\\?\` 前缀一致 → 可直接比）
    //（**先判范围再判类型**：范围外的路径一律报「不允许」，不泄露它的细节）
    if !paths::is_within_home(&p, &paths::home_dir()) {
        return Err(CommandError::new(
            "REVEAL_FORBIDDEN",
            format!(
                "{} is outside the home directory; refusing to open it",
                p.display()
            ),
        ));
    }
    if !real.is_dir() {
        return Err(CommandError::new(
            "REVEAL_NOT_FOUND",
            format!("not a directory: {}", p.display()),
        ));
    }
    // 用**展开后的原始路径**去打开（Windows 的 `\\?\` 前缀 explorer 处理不了）
    let dir = p.to_string_lossy().into_owned();
    let (prog, args): (&str, Vec<String>) = if cfg!(target_os = "windows") {
        ("explorer", vec![dir])
    } else if cfg!(target_os = "macos") {
        ("open", vec![dir])
    } else {
        ("xdg-open", vec![dir])
    };
    let mut cmd = std::process::Command::new(prog);
    cmd.args(&args);
    cmd.spawn()
        .map_err(|e| CommandError::new("REVEAL_FAILED", format!("open folder failed: {e}")))?;
    Ok(())
}

// ── 网络（net 层）─────────────────────────────────────────────────────

#[tauri::command]
pub async fn net_region() -> Cmd<aibox_net::RegionState> {
    blocking(|| Ok(aibox_core::region::region_state())).await
}

#[tauri::command]
pub async fn net_region_set(v: String) -> Cmd<()> {
    blocking(move || aibox_core::region::set_region(&v).map(|_| ())).await
}

/// net_probe 行（ProbeResult 超集：D4 同源测速表）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetProbeRow {
    pub resource: String,
    pub channel: String,
    pub name: String,
    pub url: String,
    pub ok: bool,
    pub ttfb_ms: u64,
    pub status: Option<u16>,
    pub error: Option<String>,
    pub rank: usize,
}

fn to_channel_targets(targets: &[ProbeTarget]) -> Vec<aibox_netdoctor::ChannelTarget> {
    targets
        .iter()
        .map(|t| aibox_netdoctor::ChannelTarget {
            resource: t.resource.clone(),
            channel: t.channel,
            name: t.name.clone(),
            url: t.url.clone(),
        })
        .collect()
}

#[tauri::command]
pub async fn net_probe() -> Cmd<Vec<NetProbeRow>> {
    blocking(|| {
        let targets = matrix::build_targets(false);
        let deps = aibox_netdoctor::DoctorDeps::default();
        let rows = aibox_netdoctor::probe_matrix(&to_channel_targets(&targets), &deps);
        Ok(rows
            .into_iter()
            .map(|r| NetProbeRow {
                resource: r.resource,
                channel: r.channel.as_str().to_string(),
                name: r.name,
                url: r.url,
                ok: r.ok,
                ttfb_ms: r.ttfb_ms,
                status: r.status,
                error: r.error,
                rank: r.rank,
            })
            .collect())
    })
    .await
}

/// netdoctor 报告 → DiagReport 形状（summary {en, zh-CN} / timestampMs / fixes 字符串数组）。
fn diag_report_to_contract(r: &aibox_netdoctor::DiagReport) -> Value {
    let items: Vec<Value> = r
        .items
        .iter()
        .map(|c| {
            json!({
                "item": c.item.as_str(),
                "status": c.status,
                "summary": { "en": c.summary.en, "zh-CN": c.summary.zh },
                // detail：内部是 JSON 对象；契约定为 string → 序列化后下发（TS 按 string 渲染）
                "detail": c.detail.to_string(),
                "timestampMs": c.timestamp_ms,
                "fixes": c.fixes.iter().map(|f| json!(f.level)).collect::<Vec<Value>>(),
            })
        })
        .collect();
    json!({
        "schema": r.schema,
        "generatedAtMs": r.generated_at_ms,
        "items": items,
    })
}

#[tauri::command]
pub async fn netdoctor_run(items: Option<Vec<String>>) -> Cmd<Value> {
    blocking(move || {
        let targets = matrix::build_targets(false);
        let deps = aibox_netdoctor::DoctorDeps::default();
        let report = aibox_netdoctor::run_names_with_matrix(
            items.as_deref(),
            &deps,
            &to_channel_targets(&targets),
        )
        .map_err(|e| CoreError::new("NETDOCTOR_FAIL", e))?;
        Ok(diag_report_to_contract(&report))
    })
    .await
}

// ── 执行任务（EXECUTE 四阶段；进度经事件）─────────────────────────────

/// 下载节流：≥100ms 或 ≥4MB 或完成时 emit 一次。
const EMIT_INTERVAL: Duration = Duration::from_millis(100);
const EMIT_BYTES: u64 = 4 * 1024 * 1024;

const CANCEL_PANIC: &str = "aibox-download-cancelled";

#[tauri::command]
pub async fn download_start(
    app: AppHandle,
    id: String,
    channels: Vec<Value>,
    sha256: String,
    // 校验方式："bytes"（默认）| "content"（gzip 内层内容摘要；源码归档用）
    hash_mode: Option<String>,
    dest_name: String,
    app_id: Option<String>,
    size_bytes: Option<u64>,
    skip_channels: Option<Vec<String>>,
) -> Cmd<String> {
    if id.trim().is_empty() {
        return Err(CommandError::new(
            "DOWNLOAD_BAD_ID",
            "Download task id is empty",
        ));
    }
    // 提前校验通道与落点（快速失败，不进 job 表）
    let channel_urls = download::parse_channels(&channels).map_err(CommandError::from)?;
    download::dest_for(&dest_name).map_err(CommandError::from)?;
    let skip = match &skip_channels {
        Some(list) => download::parse_skip_channels(list).map_err(CommandError::from)?,
        None => Vec::new(),
    };

    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let mut jobs = state::jobs();
        if jobs.contains_key(&id) {
            return Err(CommandError::new(
                "DOWNLOAD_BUSY",
                format!("Download job already running: {id}"),
            ));
        }
        jobs.insert(
            id.clone(),
            JobHandle {
                cancel: cancel.clone(),
            },
        );
    }

    let job_id = id.clone();
    let log_app = app_id.unwrap_or_else(|| "aibox".to_string());
    let spec = DownloadSpec {
        channels: channel_urls,
        sha256,
        hash_mode: hash_mode.clone(),
        dest_name,
        size_bytes,
        skip_channels: skip.clone(),
        app_id: Some(log_app.clone()),
    };

    std::thread::spawn(move || {
        // 安装下载开跑前：取消在途后台预下载并等它退出（≤15s）——
        // 避免同一内容寻址文件 / `.part` 被并发写（预下载是 best-effort，让路安装）
        cancel_running_prefetch();
        let app_progress = app.clone();
        let app_log = app.clone();
        let cancel_flag = cancel.clone();
        let job_for_progress = job_id.clone();
        let log_app_progress = log_app.clone();
        let job_for_log = job_id.clone();

        // 任务日志：通道序 / 跳过源 / 期望大小 / sha 起点（dest = 内容寻址文件名）
        let sha_head: String = spec.sha256.chars().take(16).collect();
        let _ = aibox_core::tasklog::append(
            &log_app,
            &job_for_log,
            &format!(
                "download start: dest={} channels=[{}] skip=[{}] sizeBytes={} sha256={}…",
                download::content_name(&spec.dest_name, &spec.sha256),
                spec.channels
                    .iter()
                    .map(|c| c.channel.as_str())
                    .collect::<Vec<_>>()
                    .join(","),
                spec.skip_channels
                    .iter()
                    .map(|c| c.as_str())
                    .collect::<Vec<_>>()
                    .join(","),
                spec.size_bytes
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "unknown".into()),
                sha_head,
            ),
        );

        let mut last_emit = Instant::now()
            .checked_sub(EMIT_INTERVAL)
            .unwrap_or_else(Instant::now);
        let mut last_done: u64 = 0;
        let mut last_channel: Option<String> = None;

        let mut progress = |p: aibox_net::DownloadProgress| {
            if cancel_flag.load(Ordering::Relaxed) {
                panic!("{CANCEL_PANIC}");
            }
            let channel = p.channel.as_str().to_string();
            let channel_changed = last_channel.as_deref() != Some(channel.as_str());
            if channel_changed {
                if let Some(prev) = last_channel.replace(channel.clone()) {
                    // 换源原因：**不再写死**「download-fallback」——
                    // 网络层会把「本次是因为线路太慢被放弃」还是「线路真坏了」带上来，
                    // UI 据此给用户不同的人话提示（慢 → 继续等；坏 → 换网络/代理）。
                    // 原因缺失时回落旧值，保持向后兼容。
                    let reason = p
                        .switch_reason
                        .as_ref()
                        .map(|k| k.reason_code())
                        .unwrap_or("download-fallback");
                    let _ = app_progress.emit(
                        "channel_switched",
                        json!({"from": prev, "to": channel, "reason": reason}),
                    );
                    let _ = aibox_core::tasklog::append(
                        &log_app_progress,
                        &job_for_log,
                        &format!("channel switch: {prev} → {channel} (reason: {reason})"),
                    );
                }
            }
            let due = channel_changed
                || p.downloaded >= p.total
                || last_emit.elapsed() >= EMIT_INTERVAL
                || p.downloaded.saturating_sub(last_done) >= EMIT_BYTES;
            if due {
                let _ = app_progress.emit(
                    "download_progress",
                    json!({
                        "jobId": job_for_progress.as_str(),
                        "done": p.downloaded,
                        "total": p.total,
                        "channel": channel,
                    }),
                );
                last_emit = Instant::now();
                last_done = p.downloaded;
            }
        };

        let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
            download::download_to_cache(&spec, &mut progress)
        }));

        let (note, terminal, fin) = match outcome {
            Ok(Ok(receipt)) => {
                let n = format!(
                    "download ok: {} bytes via {} ({})",
                    receipt.bytes,
                    receipt.channel.as_str(),
                    spec_log_name(&spec)
                );
                let t = format!("{n}, sha256={}", receipt.sha256);
                let f = json!({ "ok": true, "jobId": job_id, "bytes": receipt.bytes,
                                "channel": receipt.channel.as_str(), "sha256": receipt.sha256 });
                (n, t, f)
            }
            Ok(Err(e)) => {
                let n = format!("download failed [{}]: {}", e.code, e.human);
                let t = n.clone();
                // 带 human：界面要能显示「真实原因」（如「这个软件需要管理员权限」），
                // 只给 code 的话用户只能看到一句笼统的「安装失败」。
                let f = json!({ "ok": false, "jobId": job_id, "code": e.code, "human": e.human });
                (n, t, f)
            }
            Err(payload) => {
                let cancelled = payload
                    .downcast_ref::<&'static str>()
                    .map(|s| *s == CANCEL_PANIC)
                    .or_else(|| payload.downcast_ref::<String>().map(|s| s == CANCEL_PANIC))
                    .unwrap_or(false);
                let n = if cancelled {
                    "download cancelled (partial kept for resume)".to_string()
                } else {
                    "download panicked".to_string()
                };
                let t = n.clone();
                let code = if cancelled { "USER_CANCELLED" } else { "DOWNLOAD_PANIC" };
                let f = json!({ "ok": false, "jobId": job_id, "code": code });
                (n, t, f)
            }
        };
        // 任务日志终态（含 sha 结果；与事件文本独立，不改冻结的 log_line 格式）
        let _ = aibox_core::tasklog::append(&log_app, &job_for_log, &terminal);
        let _ = app_log.emit("log_line", json!({ "text": note }));
        // 结构化下载终态（契约事件 download_finished；log_line 文本保留为兼容回退）
        let _ = app_log.emit("download_finished", fin);
        state::jobs().remove(&job_id);
    });

    Ok(id)
}

fn spec_log_name(spec: &DownloadSpec) -> String {
    // 内容寻址文件名（与实际落盘一致，日志/诊断一眼对上）
    download::content_name(&spec.dest_name, &spec.sha256)
}

/// 安装下载开跑前取消在途预下载并等它退出（≤15s）。
/// 预下载 job 以 `prefetch:` 前缀登记；取消位置位后其进度闭包中断（保留 `.part` 断点），
/// 线程退出才移除 job → 等到 job 消失即保证不会并发写同一文件。
fn cancel_running_prefetch() {
    const WAIT_MS: u64 = 15_000;
    let ids: Vec<String> = {
        let jobs = state::jobs();
        jobs.keys()
            .filter(|k| k.starts_with("prefetch:"))
            .cloned()
            .collect()
    };
    for id in &ids {
        if let Some(job) = state::jobs().get(id) {
            job.cancel.store(true, Ordering::Relaxed);
        }
    }
    let mut waited: u64 = 0;
    loop {
        let running = {
            let jobs = state::jobs();
            jobs.keys().any(|k| k.starts_with("prefetch:"))
        };
        if !running || waited >= WAIT_MS {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
        waited += 50;
    }
}

#[tauri::command]
pub async fn download_cancel(job_id: String) -> Cmd<()> {
    let jobs = state::jobs();
    if let Some(job) = jobs.get(&job_id) {
        job.cancel.store(true, Ordering::Relaxed);
    }
    // job 不存在也返回成功（幂等；TS 可能对已完成任务补点取消）
    Ok(())
}

/// 取消正在运行的安装进程（后端杀掉子进程；幂等：任务已结束也返回成功）。
#[tauri::command]
pub fn exec_cancel(task_id: String) -> Cmd<()> {
    let execs = state::execs();
    if let Some(h) = execs.get(&task_id) {
        h.cancel.store(true, Ordering::Relaxed);
    }
    Ok(())
}

/// **gzip 内层内容摘要校验**（源码归档用）。
///
/// 字节 sha256 对 GitHub 自动生成的源码包**必然漂移**（每次 fetch 重新 gzip），
/// 而内层 tar 内容稳定 → 用它做完整性校验，既不漂移又不丢完整性保证。
#[tauri::command]
pub async fn verify_gz_inner_sha256(path: String, expected: String) -> Cmd<bool> {
    blocking(move || {
        let p = paths::expand(&path);
        match aibox_core::content_digest::gz_inner_sha256(&p) {
            Ok(actual) => Ok(actual.eq_ignore_ascii_case(expected.trim())),
            Err(_) => Ok(false),
        }
    })
    .await
}

#[tauri::command]
pub async fn verify_sha256(path: String, expected: String) -> Cmd<bool> {
    blocking(move || {
        let p = paths::expand(&path);
        aibox_core::verify::verify_sha256(&p, &expected)
    })
    .await
}

#[tauri::command]
pub async fn exec_install(
    app: AppHandle,
    id: String,
    kind: String,
    exe: String,
    args: Vec<String>,
    cwd: Option<String>,
    probe: Option<ProbeSpec>,
    app_id: Option<String>,
) -> Cmd<Value> {
    let _ = probe; // probe 阶段由 TS 调 probe_run（四阶段之一，不在 install 内联执行）
    let kind = ExecKind::parse(&kind).map_err(CommandError::from)?;
    let spec = ExecSpec {
        id: id.clone(),
        kind,
        exe,
        args,
        cwd,
    };
    let log_app = app_id.unwrap_or_else(|| "aibox".to_string());
    let log_task = id.clone();
    let log_cmd = format!("{} {}", spec.exe, spec.args.join(" "));

    let _ = app.emit("task_phase", json!({"taskId": id, "phase": "install"}));

    // 取消标志：用户点「取消」→ 后端杀掉安装进程（前端停编排 ≠ 进程退出）
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let mut execs = state::execs();
        execs.insert(id.clone(), state::ExecHandle { cancel: cancel.clone() });
    }

    let task_id = id.clone();
    let outcome = blocking(move || exec::run_cancellable(&spec, exec::timeout_from_env(), Some(&cancel))).await;
    {
        let mut execs = state::execs();
        execs.remove(&task_id);
    }

    // 任务日志：命令 / 退出码 / stdout+stderr 摘要（上游已截 4KB）
    let exec_log = match &outcome {
        Ok(o) => format!(
            "exec install: cmd={} → exit {}{}\nout:\n{}",
            log_cmd,
            o.code,
            if o.timed_out { " (timeout)" } else { "" },
            o.out
        ),
        Err(e) => format!("exec install: cmd={} → error[{}]: {}", log_cmd, e.code, e.human),
    };
    let _ = aibox_core::tasklog::append(&log_app, &log_task, &exec_log);

    match outcome {
        Ok(o) => {
            let phase = if o.code == 0 {
                "installed"
            } else {
                "install-failed"
            };
            let _ = app.emit(
                "task_phase",
                json!({"taskId": task_id.as_str(), "phase": phase}),
            );
            let _ = app.emit(
                "log_line",
                json!({ "text": format!("exec {} exit {}{}", task_id, o.code, if o.timed_out { " (timeout)" } else { "" }) }),
            );
            Ok(json!({ "code": o.code, "out": o.out }))
        }
        Err(e) => {
            let _ = app.emit(
                "task_phase",
                json!({"taskId": task_id.as_str(), "phase": "install-failed"}),
            );
            let _ = app.emit(
                "log_line",
                json!({ "text": format!("exec {} failed [{}]: {}", task_id, e.code, e.human) }),
            );
            Err(e)
        }
    }
}

#[tauri::command]
pub async fn probe_run(cmd: String, expect: String) -> Cmd<probe::ProbeResult> {
    blocking(move || probe::run_probe(&ProbeSpec { cmd, expect })).await
}

// ── CONFIG / VERIFY ──────────────────────────────────────────────────

#[tauri::command]
pub async fn config_write(
    app: AppHandle,
    app_id: String,
    provider: String,
    api_key: Option<String>,
    base_url: Option<String>,
    model: Option<String>,
    target_path: Option<String>,
) -> Cmd<ConfigWriteResult> {
    let input = ConfigWriteInput {
        app_id,
        provider,
        api_key,
        base_url,
        model,
        target_path,
    };
    let result = blocking(move || config::config_write(&input)).await;
    let result = result?;
    // 日志只记键路径，绝不记 key 值
    let _ = app.emit(
        "log_line",
        json!({ "text": format!(
            "config written: {} keys → {}",
            result.merged_keys.join(", "),
            if result.backup_path.is_empty() { "(first write)" } else { "backed up" }
        ) }),
    );
    Ok(result)
}

#[tauri::command]
pub async fn verify_send(
    provider: String,
    api_key: Option<String>,
    model: Option<String>,
    base_url: Option<String>,
) -> Cmd<VerifyResult> {
    let input = VerifyInput {
        provider,
        api_key,
        model,
        base_url,
    };
    // 网络失败在 core 内转成 {ok:false, error:"VERIFY_*: ..."}，不抛异常（契约返回体）
    blocking(move || Ok(verify_send::verify_send(&input))).await
}

// ── 诊断与更新 ───────────────────────────────────────────────────────

/// TS 侧在 PLAN_READY/CONFIRM 时写入最后计划 → `~/.aibox/cache/last-plan.json`
/// （diagzip::default_inputs 已收录该文件，诊断包无需额外接线）。
#[tauri::command]
pub async fn save_plan(plan: Value) -> Cmd<String> {
    blocking(move || {
        let dir = paths::aibox_root().join("cache");
        std::fs::create_dir_all(&dir)
            .map_err(|e| CoreError::new("SAVE_PLAN", format!("create cache dir: {e}")))?;
        let file = dir.join("last-plan.json");
        let text = serde_json::to_string_pretty(&plan)
            .map_err(|e| CoreError::new("SAVE_PLAN", format!("serialize plan: {e}")))?;
        std::fs::write(&file, text)
            .map_err(|e| CoreError::new("SAVE_PLAN", format!("write last-plan: {e}")))?;
        Ok(file.to_string_lossy().into_owned())
    })
    .await
}

#[tauri::command]
pub async fn diag_export(app: AppHandle) -> Cmd<String> {
    let outcome = blocking(move || {
        let inputs = diagzip::default_inputs(&aibox_core::fsops::today_date());
        diagzip::build_diag_zip(&inputs)
    })
    .await?;
    let _ = app.emit(
        "log_line",
        json!({ "text": format!(
            "diagnostic export: {} entries, {} redactions → {}",
            outcome.entries, outcome.redactions, outcome.path.display()
        ) }),
    );
    Ok(outcome.path.to_string_lossy().into_owned())
}

#[tauri::command]
pub async fn update_check() -> Cmd<UpdateInfo> {
    // 5min 内存缓存
    {
        let cache = state::update_cache();
        if let Some((at, info)) = cache.as_ref() {
            if at.elapsed() < Duration::from_secs(5 * 60) {
                return Ok(info.clone());
            }
        }
    }
    let current = env!("CARGO_PKG_VERSION").to_string();
    let info = blocking(move || update::check_update(&current)).await?;
    // 成功与「无新版」都缓存 5min（避免每次启动都打 GitHub）；失败不缓存（下次重试）
    let mut cache = state::update_cache();
    *cache = Some((Instant::now(), info.clone()));
    Ok(info)
}

#[tauri::command]
pub async fn locales_load(lang: String) -> Cmd<std::collections::BTreeMap<String, String>> {
    blocking(move || {
        let (doc, _) = registry::fetch_locales(&lang, false)?;
        Ok(registry::flatten(&doc))
    })
    .await
}

// ── 应用统计（site 边缘 API；两条链路均 best-effort）──────────

/// S1 卡片计数拉取（10min 内存缓存；失败 → Err，前端不显示零阻塞）。
#[tauri::command]
pub async fn stats_fetch() -> Cmd<aibox_core::stats::StatsData> {
    {
        let cache = state::stats_cache();
        if let Some((at, data)) = cache.as_ref() {
            if at.elapsed() < Duration::from_secs(10 * 60) {
                return Ok(data.clone());
            }
        }
    }
    let data = blocking(aibox_core::stats::fetch_stats).await?;
    // 成功才缓存（失败不缓存 → 下次重试）
    let mut cache = state::stats_cache();
    *cache = Some((Instant::now(), data.clone()));
    Ok(data)
}

/// 安装上报：EXECUTE 任务队列启动（首个任务 download_start 前）调一次。
/// 5s 超时；失败 → Err 由前端静默吞，绝不阻塞安装流。
#[tauri::command]
pub async fn track_install(app_id: String) -> Cmd<()> {
    blocking(move || aibox_core::stats::track_install(&app_id)).await
}

// ── 保留：壳 ↔ net 最小链路 ──────────────────────────────────

#[tauri::command]
pub fn channel_types() -> Vec<&'static str> {
    aibox_net::ChannelType::all()
        .iter()
        .map(|c| c.as_str())
        .collect()
}

// ── 托盘 / 开机自启 / 应用更新 ─────────────────────────────

/// 已装应用更新检查：读 install-manifest → 并行拉 registry app.json（12h 缓存）
/// → 比对上游 latest version → `[{appId, installed?, latest?, hasUpdate}]`。
/// 结果缓存（4h）由前端 kv 时间戳门控；本命令只做拉取+比对。
#[tauri::command]
pub async fn check_app_updates() -> Cmd<Vec<update::AppUpdateRow>> {
    blocking(|| {
        let installed = aibox_core::scan::installed_apps();
        if installed.is_empty() {
            return Ok(Vec::new());
        }
        let latest: Vec<(String, Option<String>)> = std::thread::scope(|s| {
            let handles: Vec<_> = installed
                .iter()
                .map(|a| {
                    let id = a.id.clone();
                    s.spawn(move || {
                        let v = match registry::fetch_app(&id, false) {
                            Ok((doc, _)) => doc
                                .get("version")
                                .and_then(|x| x.as_str())
                                .map(String::from),
                            Err(e) if !registry::is_availability_err(&e) => {
                                // 拒绝类错误（含完整性失败）显式留痕，不伪装成"版本未知"
                                eprintln!(
                                    "[aibox] registry rejected for {id}: {} ({})",
                                    e.code, e.human
                                );
                                None
                            }
                            Err(_) => None, // 可用性失败：版本未知属正常降级
                        };
                        (id, v)
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().unwrap_or_else(|_| (String::new(), None)))
                .collect()
        });
        Ok(update::app_update_rows(&installed, &latest))
    })
    .await
}

/// 安装完成登记：upsert `~/.aibox/install-manifest.json`（前端在任务全部完成后调用）。
#[tauri::command]
pub async fn install_record(app_id: String, version: Option<String>) -> Cmd<String> {
    blocking(move || aibox_core::scan::record_install(&app_id, version.as_deref())).await
}

// ── MANAGE 维护模式（已装应用 升级复用安装链 / 卸载 / 备份 / 清单）──────

/// 数据备份：dataPaths（展开 ~、只读存在的）→ `~/.aibox/backup/<ts>/<appId>/`
/// （原相对路径树 + `manifest.json` 还原清单：条目↔原路径映射 + 每文件 sha256 + home 指纹）。
/// 只碰声明路径，绝不越界。
#[tauri::command]
pub async fn manage_backup(app_id: String, data_paths: Vec<String>) -> Cmd<BackupOutcome> {
    blocking(move || {
        let policy = MaintenancePolicy::fetch(&app_id)?;
        policy.confirm_backup(&data_paths)?;
        manage::backup_app(&app_id, &policy.data_paths, &manage::default_backup_root())
    })
    .await
}

/// 备份列表（本机备份库；含旧格式标记）。
///
/// `restorable` = v1 清单 + appId 与目录名一致 + home 指纹一致 + 创建时间已知 + 未超龄。
/// 这里用**Rust 硬上限** [`restore::MAX_RESTORE_AGE_HOURS`]（24h）判定——与真正还原时
/// 同一口径（registry 只能往小里收，不能放宽）。
#[tauri::command]
pub async fn manage_backup_list() -> Cmd<Vec<BackupRecord>> {
    blocking(|| {
        let home = paths::home_dir();
        restore::list_backups_in(
            &manage::default_backup_root(),
            &home,
            restore::MAX_RESTORE_AGE_HOURS,
        )
    })
    .await
}

/// 自动还原暂停，独立于 registry 灰度。保留 IPC 签名供旧 UI 接收明确错误。
#[tauri::command]
pub async fn manage_restore_preview(backup_dir: String, app_id: String) -> Cmd<RestorePreview> {
    let _ = backup_dir;
    automatic_restore_paused(&app_id)
}

fn automatic_restore_paused<T>(app_id: &str) -> Cmd<T> {
    registry::validate_id(app_id)?;
    Err(CoreError::new(
        "RESTORE_AUTOMATIC_PAUSED",
        "Automatic restore is paused. Open the backup folder to recover files manually",
    ).into())
}

/// 还原预览（命令层结构体；camelCase 序列化）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestorePreview {
    /// 应用 id
    pub app_id: String,
    /// 还原计划（逐条路径 + 摘要）
    pub plan: RestorePlan,
    /// registry 来源（网络来源名：github-api / raw / jsdelivr / aibox-im）
    pub registry_source: String,
    /// registry 声明的白名单（人话展示）
    pub data_paths: Vec<String>,
    /// 允许的最大可还原时长（小时）
    pub max_age_hours: u32,
}

/// 自动还原执行与预览共用本地停用闸门，旧预览摘要或旧 registry 也不能开启。
#[tauri::command]
pub async fn manage_restore(
    backup_dir: String,
    app_id: String,
    plan_digest: String,
) -> Cmd<RestoreOutcome> {
    let _ = (backup_dir, plan_digest);
    automatic_restore_paused(&app_id)
}

/// Destructive uninstall is paused for the protection release, independent of registry policy.
#[tauri::command]
pub async fn manage_remove(
    app_id: String,
    residual_paths: Vec<String>,
    data_paths: Vec<String>,
    steps: Vec<String>,
    backup: Option<bool>,
) -> Cmd<RemoveOutcome> {
    // Keep the existing IPC arguments so older callers receive a stable error.
    let _ = (app_id, residual_paths, data_paths, steps, backup);
    Err(CoreError::new(
        "MANAGE_REMOVE_PAUSED",
        "Automatic uninstall is paused. See the manual uninstall guide",
    )
    .into())
}

#[cfg(test)]
mod maintenance_pause_tests {
    use super::manage_remove;

    #[test]
    fn remove_is_paused_for_all_backup_values_and_manifest_inputs() {
        for backup in [None, Some(true), Some(false)] {
            let err = tauri::async_runtime::block_on(manage_remove(
                "openclaw".into(),
                vec!["~/example-program".into()],
                vec!["~/example-data".into()],
                vec!["Quit the app".into()],
                backup,
            ))
            .expect_err("uninstall must always be paused");
            assert_eq!(err.code, "MANAGE_REMOVE_PAUSED");
        }
        let err = tauri::async_runtime::block_on(manage_remove(
            "openclaw".into(),
            vec![],
            vec![],
            vec![],
            None,
        ))
        .expect_err("uninstall must also be paused without registry data");
        assert_eq!(err.code, "MANAGE_REMOVE_PAUSED");
    }
}

/// 已装清单：`[{appId, version?, installedAt?}]`（读 install-manifest；MANAGE 屏数据源）。
#[tauri::command]
pub async fn manage_manifest_list() -> Cmd<Vec<ManifestEntry>> {
    blocking(|| Ok(manage::manifest_list())).await
}

/// 托盘文案同步（前端 i18n 后下发；tooltip + 菜单项 + 首项动作）。
#[tauri::command]
pub fn tray_sync(ui: crate::tray::TrayUi) -> Cmd<()> {
    let mut ui = ui;
    if ui.first_action != "download" {
        ui.first_action = "open".to_string();
    }
    crate::tray::apply_ui(&ui);
    *state::tray_ui() = ui;
    Ok(())
}

/// 退出（真正结束进程；托盘「退出」经前端确认后调用）。
#[tauri::command]
pub fn app_quit(app: AppHandle) {
    app.exit(0);
}

/// 「使用系统代理」开关状态（默认开 = 自动跟随 Windows 系统代理）。
#[tauri::command]
pub fn net_use_system_proxy_get() -> Cmd<bool> {
    Ok(aibox_core::region::use_system_proxy())
}

/// 写「使用系统代理」开关（合并写 + 回读校验）。
#[tauri::command]
pub fn net_use_system_proxy_set(enabled: bool) -> Cmd<bool> {
    aibox_core::region::set_use_system_proxy(enabled).map_err(CommandError::from)
}

/// 开机自启状态（enabled = OS 真相；error = 最近一次注册失败）。
#[tauri::command]
pub fn autostart_get(app: AppHandle) -> crate::autostart::AutostartState {
    crate::autostart::status(&app)
}

/// 设置开机自启（即时生效；失败返回 error，设置页人话提示）。
#[tauri::command]
pub fn autostart_set(app: AppHandle, enabled: bool) -> crate::autostart::AutostartState {
    crate::autostart::set(&app, enabled)
}

// ── 后台预下载（空闲资源榨干；设置-通用页开关 `prefetch.enabled` 默认开）────

/// 预下载开关状态（`prefetch_get` / `prefetch_set` 返回）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrefetchState {
    pub enabled: bool,
}

/// 预下载结果（`prefetch_start` 返回；全程静默：不发进度事件，只写任务日志）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrefetchOutcome {
    pub app_id: String,
    /// downloaded | cached | busy | disabled | no-package | failed | cancelled
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

fn pf_outcome(app_id: &str, status: &str, detail: Option<String>) -> PrefetchOutcome {
    PrefetchOutcome {
        app_id: app_id.to_string(),
        status: status.to_string(),
        detail,
    }
}

/// 读预下载开关（settings.json `prefetch.enabled`；缺省 = 默认开）。
#[tauri::command]
pub async fn prefetch_get() -> Cmd<PrefetchState> {
    blocking(|| Ok(PrefetchState {
        enabled: prefetch::enabled(),
    }))
    .await
}

/// 写预下载开关（合并保键；关 = 不再触发新预下载，已有缓存保留）。
#[tauri::command]
pub async fn prefetch_set(enabled: bool) -> Cmd<PrefetchState> {
    blocking(move || {
        prefetch::set_enabled(enabled)?;
        Ok(PrefetchState { enabled })
    })
    .await
}

/// 空闲预下载主安装包（TS 在 check_app_updates 发现新版 + IDLE 时顺序调用）：
/// - 单飞 + 不抢带宽：job 表非空（安装中/另一路预下载）→ `busy`
/// - 幂等：内容寻址缓存已命中 → `cached`（零网络）
/// - 全程 best-effort：失败/取消 → 静默状态码，下轮 4h 再试；不 emit 任何事件
#[tauri::command]
pub async fn prefetch_start(app_id: String) -> Cmd<PrefetchOutcome> {
    blocking(move || {
        let id = app_id.trim().to_string();
        if id.is_empty() {
            return Err(CoreError::new("PREFETCH_BAD_APP", "App id is empty"));
        }
        if !prefetch::enabled() {
            return Ok(pf_outcome(&id, "disabled", None));
        }
        let job_id = format!("prefetch:{id}");
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        {
            let mut jobs = state::jobs();
            if !jobs.is_empty() {
                return Ok(pf_outcome(&id, "busy", None));
            }
            // job 先登记（覆盖拉 manifest 阶段）→ download_start 能取消/等待它
            jobs.insert(job_id.clone(), JobHandle { cancel: cancel.clone() });
        }
        // 选主安装包（routes 当前序；manifest 12h 缓存，检查更新刚拉过通常直接命中）
        let plan = match prefetch::plan_for_app(&id) {
            Ok(p) => p,
            Err(e) => {
                state::jobs().remove(&job_id);
                let status = if e.code == "PREFETCH_NO_PACKAGE" {
                    "no-package"
                } else {
                    "failed"
                };
                return Ok(pf_outcome(&id, status, Some(e.human)));
            }
        };
        if cancel.load(Ordering::Relaxed) {
            state::jobs().remove(&job_id);
            return Ok(pf_outcome(&id, "cancelled", None));
        }
        let target = prefetch::target_path(&plan);
        let _ = aibox_core::tasklog::append(
            &id,
            &job_id,
            &format!(
                "prefetch start: pkg={} dest={} size={}",
                plan.pkg_id,
                target.display(),
                plan.size_bytes
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "unknown".into()),
            ),
        );
        let outcome = prefetch::run(&plan, &cancel);
        state::jobs().remove(&job_id);
        let out = match outcome {
            Ok(PrefetchStatus::Cached) => {
                let _ = aibox_core::tasklog::append(
                    &id,
                    &job_id,
                    &format!("prefetch ok: cache hit ({})", target.display()),
                );
                pf_outcome(&id, "cached", None)
            }
            Ok(PrefetchStatus::Downloaded { bytes, channel }) => {
                let _ = aibox_core::tasklog::append(
                    &id,
                    &job_id,
                    &format!(
                        "prefetch ok: {bytes} bytes via {channel} ({})",
                        target.display()
                    ),
                );
                pf_outcome(&id, "downloaded", None)
            }
            Err(e) if e.code == "PREFETCH_CANCELLED" => {
                let _ = aibox_core::tasklog::append(
                    &id,
                    &job_id,
                    "prefetch cancelled (partial kept for resume)",
                );
                pf_outcome(&id, "cancelled", None)
            }
            Err(e) => {
                let _ = aibox_core::tasklog::append(
                    &id,
                    &job_id,
                    &format!("prefetch failed [{}]: {}", e.code, e.human),
                );
                pf_outcome(&id, "failed", Some(e.human))
            }
        };
        Ok(out)
    })
    .await
}
