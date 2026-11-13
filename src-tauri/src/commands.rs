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
use aibox_core::matrix::{self, ProbeTarget};
use aibox_core::probe::{self, ProbeSpec};
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
    let (mut doc, _) = blocking(move || registry::fetch_index(force.unwrap_or(false))).await?;
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
        dest_name,
        size_bytes,
        skip_channels: skip.clone(),
        app_id: Some(log_app.clone()),
    };

    std::thread::spawn(move || {
        let app_progress = app.clone();
        let app_log = app.clone();
        let cancel_flag = cancel.clone();
        let job_for_progress = job_id.clone();
        let log_app_progress = log_app.clone();
        let job_for_log = job_id.clone();

        // 任务日志：通道序 / 跳过源 / 期望大小 / sha 起点
        let sha_head: String = spec.sha256.chars().take(16).collect();
        let _ = aibox_core::tasklog::append(
            &log_app,
            &job_for_log,
            &format!(
                "download start: dest={} channels=[{}] skip=[{}] sizeBytes={} sha256={}…",
                spec.dest_name,
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
                    let _ = app_progress.emit(
                        "channel_switched",
                        json!({"from": prev, "to": channel, "reason": "download-fallback"}),
                    );
                    let _ = aibox_core::tasklog::append(
                        &log_app_progress,
                        &job_for_log,
                        &format!("channel switch: {prev} → {channel}"),
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

        let (note, terminal) = match outcome {
            Ok(Ok(receipt)) => {
                let n = format!(
                    "download ok: {} bytes via {} ({})",
                    receipt.bytes,
                    receipt.channel.as_str(),
                    spec_log_name(&spec)
                );
                let t = format!("{n}, sha256={}", receipt.sha256);
                (n, t)
            }
            Ok(Err(e)) => {
                let n = format!("download failed [{}]: {}", e.code, e.human);
                let t = n.clone();
                (n, t)
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
                (n, t)
            }
        };
        // 任务日志终态（含 sha 结果；与事件文本独立，不改冻结的 log_line 格式）
        let _ = aibox_core::tasklog::append(&log_app, &job_for_log, &terminal);
        let _ = app_log.emit("log_line", json!({ "text": note }));
        state::jobs().remove(&job_id);
    });

    Ok(id)
}

fn spec_log_name(spec: &DownloadSpec) -> &str {
    &spec.dest_name
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
    let task_id = id.clone();
    let outcome = blocking(move || exec::run(&spec)).await;

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
                        let v = registry::fetch_app(&id, false).ok().and_then(|(doc, _)| {
                            doc.get("version")
                                .and_then(|x| x.as_str())
                                .map(String::from)
                        });
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

/// 数据备份：dataPaths（展开 ~、只读存在的）→ `~/.aibox/backup/<ts>/<appId>/data.zip`
/// + `manifest.json`（条目↔原路径还原映射）。只碰声明路径，绝不越界。
#[tauri::command]
pub async fn manage_backup(app_id: String, data_paths: Vec<String>) -> Cmd<BackupOutcome> {
    blocking(move || manage::backup_app(&app_id, &data_paths, &manage::default_backup_root())).await
}

/// 卸载：backupFirst 自动备份 → 白名单内（residualPaths ∪ dataPaths）删除 →
/// 成功后注销 install-manifest。失败收集不中断；零提权；steps 只写日志不执行。
#[tauri::command]
pub async fn manage_remove(
    app_id: String,
    residual_paths: Vec<String>,
    data_paths: Vec<String>,
    steps: Vec<String>,
    backup: Option<bool>,
) -> Cmd<RemoveOutcome> {
    let do_backup = backup.unwrap_or(true); // backupFirst 硬规则：缺省 true
    let id_for_remove = app_id.clone();
    let out = blocking(move || {
        manage::remove_app(
            &id_for_remove,
            &residual_paths,
            &data_paths,
            &steps,
            do_backup,
            &manage::default_backup_root(),
        )
    })
    .await?;
    // 无真失败（skipped 只允许 not found）→ 注销已装登记（幂等）
    let clean = out
        .skipped
        .iter()
        .all(|s| s.reason == "not found");
    if clean {
        let id = app_id.clone();
        let _ = blocking(move || aibox_core::scan::remove_install(&id)).await;
    }
    Ok(out)
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
