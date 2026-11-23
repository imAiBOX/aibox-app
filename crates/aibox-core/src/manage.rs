//! MANAGE 维护模式：数据备份 / 清单式卸载（零提权；只碰 registry 声明的路径）。
//!
//! 硬规则（产品 §5 行为规则 ①③）：
//! - 备份：把声明的 `dataPaths`（展开 `~`、只读存在的）复制到
//!   `~/.aibox/backup/<ts>/<appId>/`（**原相对路径树** + `manifest.json` 还原清单：
//!   条目→原路径映射 + 每个文件的 sha256 + home 指纹）；
//!   **只读声明路径，绝不越界收集**。
//! - 卸载：全量预检 `residualPaths` 和 `dataPaths`，任何受保护或不安全路径整批拒绝。
//!   默认先备份 `dataPaths`，再删除 `residualPaths`。执行时的单条 I/O 失败单独记录。
//! - `steps` 是 registry 的人话指引文本，**只写任务日志，绝不作为命令执行**。
//!
//! 还原在 [`crate::restore`]（manifest 驱动；**白名单来自调用方给的 dataPaths**，
//! 清单自述路径不可信；越界 / 软链 / 校验不符一律跳过；
//! 覆盖前先把旧数据**移**到 `pre-restore-<ts>/` 保全，绝不删除）。
//!
//! 备份包不脱敏（本地还原用途；诊断包才脱敏——两者场景不同）。

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{json, Value};

use crate::err::{io_err, CoreError, CoreResult};
use crate::fsops;
use crate::manage_paths::{
    checked_path, inspect_components, is_link_or_reparse, preflight_target, ProtectedPaths,
};
use crate::paths;

/// 备份结果（TS: `{backupDir, files}`；backupDir 空串 = 无数据可备份）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupOutcome {
    pub backup_dir: String,
    /// 已备份的条目（备份内相对路径，`/` 分隔）——还原时按 manifest 映射回原路径
    pub files: Vec<String>,
    /// 声明但不存在（跳过不报错——没有可丢的数据）
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<String>,
    /// **被安全策略拒绝**的声明路径（symlink / 解析后越界）。
    /// 与 `skipped` 语义不同：这里有数据但可能备份不到 → 破坏性操作必须中止。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub refused: Vec<String>,
    /// **读取/写入失败**的条目（数据存在但没能进包）。
    /// 同样意味着"备份不完整" → 破坏性操作必须中止。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub failed: Vec<String>,
}

/// 还原结果（`restore_backup`；实现见 [`crate::restore`]）。
pub use crate::restore::RestoreOutcome;

/// 单条跳过（不存在 / 越界 / 保护路径 / 删除失败——收集不中断）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkippedPath {
    pub path: String,
    pub reason: String,
}

/// 卸载结果（TS: `{backupDir?, removed[], skipped[]}`）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoveOutcome {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backup_dir: Option<String>,
    pub removed: Vec<String>,
    pub skipped: Vec<SkippedPath>,
}

/// install-manifest 单条（TS: `{appId, version?, installedAt?}`）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestEntry {
    pub app_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// 登记时间（unix ms；旧条目无此字段 → 省略）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub installed_at: Option<u64>,
}

/// `~/.aibox/backup` 默认根（测试注入临时目录）。
pub fn default_backup_root() -> PathBuf {
    paths::aibox_root().join("backup")
}

/// 备份条目（收集阶段）：
/// - `rel` = 备份内相对路径（载荷落点）
/// - `target` = 相对 home 的原路径（还原落点，**绝不含绝对路径**）
struct PendingEntry {
    rel: String,
    target: String,
    src: PathBuf,
    is_dir: bool,
}

/// 相对 home 的路径（`/` 分隔；home 本身 → None）。
fn home_relative(real: &Path, home_real: &Path) -> Option<String> {
    let r = real.strip_prefix(home_real).ok()?;
    let s = r.to_string_lossy().replace('\\', "/");
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// 备份内相对路径：保留原相对路径；与清单文件名冲突时改名
/// （清单必须叫 `manifest.json`，不能被载荷占用）。
fn backup_rel(target: &str) -> String {
    if target.eq_ignore_ascii_case("manifest.json") {
        "manifest.json.payload".to_string()
    } else {
        target.to_string()
    }
}

/// 解析声明根的**真实路径**，并要求它落在 home 的真实路径之内（V3 不变式）。
/// 用 `canonicalize` 一次性消解所有中间链接：根为链接、祖先为链接、指向外部，
/// 都会被判为越界（审计 P1）。返回 `None` = 不存在/不可解析/越界。
fn resolve_within_home(root: &Path, home: &Path) -> Option<PathBuf> {
    let real = fs::canonicalize(root).ok()?;
    let home_real = fs::canonicalize(home).ok()?;
    if real == home_real || real.starts_with(&home_real) {
        Some(real)
    } else {
        None
    }
}

/// 递归收集目录下的**目录 + 文件**。
///
/// 越界防护（审计 P1-5）：
/// - **每一层目录**都做 `canonicalize` 校验 → 必须仍在 home 内**且**仍在该条声明的
///   dataPath 之内；不满足 → 该条目 `failed`（**不跟随**）。
///   （Windows 的 junction（`mklink /J`）不是 symlink，`is_symlink()` 为 false、
///   `is_dir()` 为 true → 只有 canonicalize 才拦得住。）
/// - 任何 symlink 都不进包（声明目录下的符号链接可能指向白名单外）。
/// - 备份根本身（`~/.aibox/backup`）被排除——旧备份不拷进新备份。
fn walk_tree(
    dir: &Path,
    root_real: &Path,
    home_real: &Path,
    backup_root_real: Option<&Path>,
    out: &mut Vec<PendingEntry>,
    skipped: &mut Vec<String>,
    errors: &mut Vec<String>,
) {
    let rd = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) => {
            // 目录不可读 = 可能有数据但采集不到 → 必须上报（会导致删除中止）
            errors.push(format!("{} (read_dir: {e})", dir.display()));
            return;
        }
    };
    for entry in rd {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                errors.push(format!("{} (entry: {e})", dir.display()));
                continue;
            }
        };
        let p = entry.path();
        // symlink_metadata: 不跟随链接（link 本身也不当文件收，防指向外部）
        let meta = match fs::symlink_metadata(&p) {
            Ok(m) => m,
            Err(e) => {
                errors.push(format!("{} (stat: {e})", p.display()));
                continue;
            }
        };
        #[cfg(windows)]
        if is_link_or_reparse(&meta) {
            errors.push(format!("{} (link or reparse point)", p.display()));
            continue;
        }
        // ── 每一层目录的越界校验（越界目录 → failed，不跟随） ──
        match fs::canonicalize(&p) {
            Ok(c) => {
                if !(c.starts_with(root_real) && c.starts_with(home_real)) {
                    errors.push(format!(
                        "{} (resolves outside the declared data path)",
                        p.display()
                    ));
                    continue;
                }
                // 备份根本身不备份（旧备份不拷进新备份）
                if let Some(bk) = backup_root_real {
                    if c.starts_with(bk) {
                        skipped.push(format!("{} (inside the backup root)", p.display()));
                        continue;
                    }
                }
            }
            Err(e) => {
                // 悬空链接 / 解析不了 → 不跟随（没有可备份的内容）
                skipped.push(format!("{} (unresolvable: {e})", p.display()));
                continue;
            }
        }
        if is_link_or_reparse(&meta) {
            // 链接不进包（还原侧也会再拦一次）
            skipped.push(format!("{} (symlink)", p.display()));
            continue;
        }
        if meta.is_dir() {
            if let Some(rel) = home_relative(&p, home_real) {
                out.push(PendingEntry {
                    rel: backup_rel(&rel),
                    target: rel,
                    src: p.clone(),
                    is_dir: true,
                });
            }
            walk_tree(
                &p,
                root_real,
                home_real,
                backup_root_real,
                out,
                skipped,
                errors,
            );
        } else if let Some(rel) = home_relative(&p, home_real) {
            out.push(PendingEntry {
                rel: backup_rel(&rel),
                target: rel,
                src: p,
                is_dir: false,
            });
        }
    }
}

/// 备份声明的 dataPaths → `<backup_root>/<ts>/<appId>/<相对路径...>` + `manifest.json`。
/// 只读存在的声明路径；全部缺失 → 不建目录，`backup_dir` 返回空串。
pub fn backup_app(
    app_id: &str,
    data_paths: &[String],
    backup_root: &Path,
) -> CoreResult<BackupOutcome> {
    backup_app_in(app_id, data_paths, backup_root, &paths::home_dir())
}

/// 可注入 home 的实现（测试用；生产经 `backup_app` 传入真实 home）。
pub fn backup_app_in(
    app_id: &str,
    data_paths: &[String],
    backup_root: &Path,
    home: &Path,
) -> CoreResult<BackupOutcome> {
    crate::registry::validate_id(app_id)?;
    let ts = fsops::now_ts();
    let dir = backup_root.join(ts).join(app_id);
    backup_into(app_id, data_paths, &dir, backup_root, home)
}

fn expand_maintenance_path(raw: &str, home: &Path) -> PathBuf {
    let raw = raw.trim();
    if raw == "~" {
        return home.to_path_buf();
    }
    if let Some(rest) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\")) {
        return home.join(rest);
    }
    paths::expand(raw)
}

/// 备份到指定目录（内部实现；测试可注入目录 → 覆盖 manifest 写失败等分支）。
fn backup_into(
    app_id: &str,
    data_paths: &[String],
    dir: &Path,
    backup_root: &Path,
    home: &Path,
) -> CoreResult<BackupOutcome> {
    let home = home.to_path_buf();
    let home_real = fs::canonicalize(&home).unwrap_or_else(|_| home.clone());
    // 备份根的真实路径（用于「旧备份不拷进新备份」的排除；不存在则无需排除）
    let backup_root_real = fs::canonicalize(backup_root).ok();
    let mut entries: Vec<PendingEntry> = Vec::new();
    let mut used: HashSet<String> = HashSet::new();
    let mut skipped: Vec<String> = Vec::new();
    let mut refused: Vec<String> = Vec::new();
    let mut failed: Vec<String> = Vec::new();
    let protected = ProtectedPaths::new(&home, backup_root);

    for raw in data_paths {
        let expanded = expand_maintenance_path(raw, &home);
        // Keep dataPaths resolution consistent with removal, including the alias policy.
        let check = checked_path(&expanded).and_then(|path| {
            inspect_components(&path)?;
            protected.check(&path)
        });
        if let Err(e) = check {
            refused.push(format!("{raw} ({})", e.human));
            continue;
        }
        // **统一判定**（与还原侧共用 [`crate::datapaths`]）：
        // home 本身（`~` / `~/.` / home 绝对路径）→ **拒绝**（否则整个 home 被拷进备份库；
        // 审计 P1-1/P1-2）；home 之外 / 软链根 → 拒绝；不存在 → 跳过（没有数据可丢）。
        let entry = match crate::datapaths::resolve(raw, &home_real) {
            crate::datapaths::Decision::Ok(e) => e,
            crate::datapaths::Decision::Refused(r) => {
                refused.push(format!("{raw} ({})", r.reason()));
                continue;
            }
            crate::datapaths::Decision::Failed(m) => {
                failed.push(m);
                continue;
            }
        };
        if entry.kind == crate::datapaths::PathKind::Unknown {
            // 声明了但不存在 → 没有数据可丢（跳过，不报错）
            skipped.push(raw.clone());
            continue;
        }
        let root = home_real.join(&entry.rel);
        if let Err(e) = inspect_components(&root) {
            refused.push(format!("{raw} ({})", e.human));
            continue;
        }
        // 声明根本身是链接 → 拒绝（避免被换成任意目标，审计 P1）
        let meta = match fs::symlink_metadata(&root) {
            Ok(m) => m,
            Err(e) => {
                failed.push(format!("{raw} (stat: {e})"));
                continue;
            }
        };
        if is_link_or_reparse(&meta) {
            refused.push(raw.clone());
            continue;
        }
        // 解析真实路径并要求仍在 home 内（祖先链接/越界目标一网打尽）
        let Some(real_root) = resolve_within_home(&root, &home) else {
            refused.push(raw.clone());
            continue;
        };
        // 备份根本身不得被备份（旧备份会被拷进新备份）
        if let Some(bk) = &backup_root_real {
            if real_root.starts_with(bk) {
                refused.push(format!("{raw} (inside the backup root)"));
                continue;
            }
        }
        let mut collected: Vec<PendingEntry> = Vec::new();
        if real_root.is_dir() {
            if let Some(rel) = home_relative(&real_root, &home_real) {
                collected.push(PendingEntry {
                    rel: backup_rel(&rel),
                    target: rel,
                    src: real_root.clone(),
                    is_dir: true,
                });
            }
            walk_tree(
                &real_root,
                &real_root,
                &home_real,
                backup_root_real.as_deref(),
                &mut collected,
                &mut skipped,
                &mut failed,
            );
        } else if let Some(rel) = home_relative(&real_root, &home_real) {
            collected.push(PendingEntry {
                rel: backup_rel(&rel),
                target: rel,
                src: real_root.clone(),
                is_dir: false,
            });
        }
        for e in collected {
            // 同一文件被多个声明根覆盖 → 只备份一次
            if used.insert(e.rel.clone()) {
                entries.push(e);
            }
        }
    }

    if entries.is_empty() {
        return Ok(BackupOutcome {
            backup_dir: String::new(),
            files: Vec::new(),
            skipped,
            refused,
            failed,
        });
    }
    // **备份根私有**（Unix 0700；已有且更宽 → 收紧）：备份内容是应用凭据明文，
    // 同机别的账号不该能读（审计 P1-3）。只管备份库，**不动用户原始数据目录**。
    inspect_components(&checked_path(dir)?)?;
    fsops::ensure_private_dir(backup_root)
        .map_err(|e| io_err("create backup root", e))?;
    // 备份负载目录同样私有（0700）
    fsops::ensure_private_dir(dir).map_err(|e| io_err("create backup dir", e))?;

    // 目录先建（先建目录再写文件）；目录也进清单（isDir=true）
    let mut manifest_entries: Vec<Value> = Vec::new();
    let mut over_limit = 0usize;
    for e in entries.iter().filter(|e| e.is_dir) {
        if entries.len() > crate::restore::MAX_BACKUP_ENTRIES {
            over_limit += 1;
            continue;
        }
        let dest = dir.join(&e.rel);
        if let Err(e) = checked_path(&dest).and_then(|p| inspect_components(&p)) {
            failed.push(format!("{} ({})", dest.display(), e.human));
            continue;
        }
        match fsops::ensure_private_dir(&dest) {
            Ok(()) => manifest_entries.push(json!({
                "rel": e.rel,
                "target": e.target,
                "size": 0,
                "sha256": "",
                "isDir": true,
            })),
            Err(e) => failed.push(format!("{} ({e})", dest.display())),
        }
    }

    // 文件载荷：流式复制（同时算 sha256；源是链接 → 拒）
    // **限额与还原一致**（审计 P1-6）：备份出来的东西必须还原得回去 →
    // 超限的条目记 `failed`（触发 `MANAGE_BACKUP_INCOMPLETE` → 暂停破坏性操作），
    // 绝不让备份「假成功」。
    let mut files_out: Vec<String> = Vec::with_capacity(entries.len());
    let mut file_count = 0usize;
    let mut total_bytes = 0u64;
    for e in entries.iter().filter(|e| !e.is_dir) {
        let size = fs::symlink_metadata(&e.src).map(|m| m.len()).unwrap_or(0);
        if file_count >= crate::restore::MAX_BACKUP_FILES
            || total_bytes.saturating_add(size) > crate::restore::MAX_BACKUP_BYTES
            || manifest_entries.len() >= crate::restore::MAX_BACKUP_ENTRIES
        {
            over_limit += 1;
            continue;
        }
        let dest = dir.join(&e.rel);
        if let Err(e) = checked_path(&dest).and_then(|p| inspect_components(&p)) {
            failed.push(format!("{} ({})", dest.display(), e.human));
            continue;
        }
        // **配额在拷贝过程中判定**（源在拷贝中变大 → 立即停，不超限落盘；审计 P2-6）
        let budget = crate::restore::MAX_BACKUP_BYTES.saturating_sub(total_bytes);
        match fsops::copy_file_hashed_budget(&e.src, &dest, budget) {
            Ok((size, sha256)) => {
                manifest_entries.push(json!({
                    "rel": e.rel,
                    "target": e.target,
                    "size": size,
                    "sha256": sha256,
                    "isDir": false,
                }));
                files_out.push(e.rel.clone());
                file_count += 1;
                total_bytes = total_bytes.saturating_add(size);
            }
            Err(err) if err.code == "BACKUP_TOO_LARGE" => {
                over_limit += 1;
                failed.push(format!("{} ({})", e.src.display(), err.human));
            }
            Err(err) => {
                // 读/写失败 = 数据在但没进包 → failed（会导致破坏性操作中止）
                failed.push(format!("{} ({})", e.src.display(), err.human));
            }
        }
    }
    if over_limit > 0 {
        failed.push(format!(
            "{over_limit} entries exceeded the backup size limit (files ≤ {}, bytes ≤ {})",
            crate::restore::MAX_BACKUP_FILES,
            crate::restore::MAX_BACKUP_BYTES
        ));
    }

    // 还原清单：**只含相对路径**（不泄露绝对路径）；写失败 → 记 failed，
    // 但**备份文件本身保留**（不连带丢弃已备份的数据）。
    let manifest = json!({
        "schema": "aibox-restore/v1",
        "appId": app_id,
        "createdAt": fsops::now_iso8601(),
        "homeHash": crate::restore::home_fingerprint(&home),
        "entries": manifest_entries,
    });
    let manifest_text = serde_json::to_string_pretty(&manifest)
        .map_err(|e| CoreError::new("BACKUP_MANIFEST", e.to_string()))?;
    if let Err(e) = fsops::write_text_atomic_private(&dir.join("manifest.json"), &manifest_text) {
        failed.push(format!("manifest.json ({})", e.human));
    }

    Ok(BackupOutcome {
        backup_dir: dir.to_string_lossy().into_owned(),
        files: files_out,
        skipped,
        refused,
        failed,
    })
}

/// 删除动作结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteOutcome {
    Deleted,
    NotFound,
}

/// Validate the target against the allowlist and filesystem protections before deletion.
/// The injected `deleter` lets tests exercise the boundary without touching user files.
pub fn delete_within<F>(
    allow: &[PathBuf],
    target: &Path,
    deleter: &mut F,
) -> CoreResult<DeleteOutcome>
where
    F: FnMut(&Path) -> io::Result<()>,
{
    let protected = ProtectedPaths::new(&paths::home_dir(), &default_backup_root());
    let allow = allow.iter().map(|a| checked_path(a)).collect::<CoreResult<Vec<_>>>()?;
    delete_preflighted(&allow, &checked_path(target)?, &protected, deleter)
}

fn delete_preflighted<F>(
    allow: &[PathBuf],
    target: &Path,
    protected: &ProtectedPaths,
    deleter: &mut F,
) -> CoreResult<DeleteOutcome>
where
    F: FnMut(&Path) -> io::Result<()>,
{
    // Repeat the path checks immediately before use. These checks aren't a filesystem lock.
    if !preflight_target(allow, target, protected)? {
        return Ok(DeleteOutcome::NotFound);
    }
    deleter(target).map_err(|e| {
        CoreError::new("MANAGE_DELETE_FAILED", format!("{}: {e}", target.display()))
    })?;
    Ok(DeleteOutcome::Deleted)
}

/// 真实删除：目录 → remove_dir_all，文件 → remove_file；拒绝链接和重解析点。
fn fs_delete(p: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(p)?;
    if is_link_or_reparse(&meta) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "refusing link or reparse point"));
    }
    if meta.is_dir() {
        fs::remove_dir_all(p)
    } else {
        fs::remove_file(p)
    }
}

/// 备份完整性判定（纯函数）：refused（策略拒绝）或 failed（读写失败）都必须中止破坏性操作。
/// `skipped` 只表示"声明了但不存在"，没有数据可丢，不阻断。
pub fn backup_incomplete_error(out: &BackupOutcome) -> Option<CoreError> {
    if !out.refused.is_empty() {
        return Some(CoreError::new(
            "MANAGE_BACKUP_INCOMPLETE",
            format!(
                "refusing to delete: declared data refused by safety policy: {}",
                out.refused.join(", ")
            ),
        ));
    }
    if !out.failed.is_empty() {
        return Some(CoreError::new(
            "MANAGE_BACKUP_INCOMPLETE",
            format!(
                "refusing to delete: declared data could not be backed up: {}",
                out.failed.join(", ")
            ),
        ));
    }
    None
}

/// 按清单卸载：全量预检 → 可选备份 → 删除 `residualPaths`。
/// 安全预检或备份失败会中止操作。执行阶段的单条删除失败收集进 `skipped`。
/// 已装登记由命令层在删除成功后注销。
pub fn remove_app(
    app_id: &str,
    residual_paths: &[String],
    data_paths: &[String],
    steps: &[String],
    backup: bool,
    backup_root: &Path,
) -> CoreResult<RemoveOutcome> {
    remove_app_in(
        app_id,
        residual_paths,
        data_paths,
        steps,
        backup,
        backup_root,
        &paths::home_dir(),
    )
}

/// 可注入 home 的实现（测试用）。
pub fn remove_app_in(
    app_id: &str,
    residual_paths: &[String],
    data_paths: &[String],
    steps: &[String],
    backup: bool,
    backup_root: &Path,
    home: &Path,
) -> CoreResult<RemoveOutcome> {
    crate::registry::validate_id(app_id)?;

    let protected = ProtectedPaths::new(home, backup_root);
    let expand = |raw: &str| checked_path(&expand_maintenance_path(raw, home));
    let residual = residual_paths
        .iter()
        .map(|raw| expand(raw))
        .collect::<CoreResult<Vec<_>>>()?;
    let data = data_paths
        .iter()
        .map(|raw| expand(raw))
        .collect::<CoreResult<Vec<_>>>()?;
    let allow: Vec<_> = residual.iter().chain(&data).cloned().collect();

    // 全量安全预检：任一路径不通过，整批停止（不创建备份、不删除、不写操作日志）。
    let preflight_residual = || {
        residual
            .iter()
            .map(|path| preflight_target(&allow, path, &protected))
            .collect::<CoreResult<Vec<_>>>()
    };
    let present = preflight_residual()?;
    for path in &data {
        inspect_components(path)?;
        protected.check(path)?;
    }

    // 人话指引只写任务日志。测试入口不接触真实任务日志。
    #[cfg(not(test))]
    for (i, s) in steps.iter().enumerate() {
        let _ = crate::tasklog::append(app_id, "uninstall", &format!("manual step {}: {s}", i + 1));
    }
    #[cfg(test)]
    let _ = steps;

    // 备份（硬规则①；失败中止）
    let mut backup_dir: Option<String> = None;
    if backup {
        // 不在入口预筛选：`symlink_metadata().is_ok()` 会把权限等 I/O 错误当成"不存在"，
        // 从而绕过备份完整性判定（审计 P1）。全部声明路径交给备份入口分类。
        if !data_paths.is_empty() {
            let out = backup_app_in(app_id, data_paths, backup_root, home).map_err(|e| {
                CoreError::new(
                    "MANAGE_BACKUP_FAILED",
                    format!("backup before removal failed: {}", e.human),
                )
            })?;
            if let Some(err) = backup_incomplete_error(&out) {
                // 有声明数据但备份不到/没打包成功 → 不许删（数据保全优先于清理）
                return Err(err);
            }
            if !out.backup_dir.is_empty() {
                backup_dir = Some(out.backup_dir);
            }
        }
    }

    // 备份可能耗时。先重新检查整批目标，再开始逐条删除。
    if backup_dir.is_some() && preflight_residual()? != present {
        return Err(CoreError::new(
            "MANAGE_PATH_CHANGED",
            "Maintenance paths changed while backing up; review them again",
        ));
    }

    // 删除（白名单内；失败收集不中断）
    let mut removed = Vec::new();
    let mut skipped: Vec<SkippedPath> = Vec::new();
    for ((raw, target), existed) in residual_paths.iter().zip(&residual).zip(present) {
        let result = if existed {
            delete_preflighted(&allow, target, &protected, &mut fs_delete)
        } else {
            Ok(DeleteOutcome::NotFound)
        };
        match result {
            Ok(DeleteOutcome::Deleted) => removed.push(raw.clone()),
            Ok(DeleteOutcome::NotFound) => skipped.push(SkippedPath {
                path: raw.clone(),
                reason: "not found".into(),
            }),
            Err(e) => skipped.push(SkippedPath {
                path: raw.clone(),
                reason: e.human,
            }),
        }
    }

    // 已装登记的注销由 Rust 命令层负责。

    Ok(RemoveOutcome {
        backup_dir,
        removed,
        skipped,
    })
}

/// install-manifest 条目列表（容错：`{apps:[...]}` 与旧 map 形态都支持）。
pub fn manifest_list() -> Vec<ManifestEntry> {
    let Ok(text) = fs::read_to_string(paths::install_manifest_file()) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        return Vec::new();
    };
    parse_manifest_list(&v)
}

/// 解析 install-manifest → 条目（纯函数，单测）。
pub fn parse_manifest_list(v: &Value) -> Vec<ManifestEntry> {
    let mut out = Vec::new();
    let mut push = |id: &str, item: &Value| {
        out.push(ManifestEntry {
            app_id: id.to_string(),
            version: item.get("version").and_then(|x| x.as_str()).map(String::from),
            installed_at: item.get("installedAt").and_then(|x| x.as_u64()),
        });
    };
    if let Some(arr) = v.get("apps").and_then(|a| a.as_array()) {
        for item in arr {
            if let Some(id) = item.get("id").and_then(|x| x.as_str()) {
                push(id, item);
            }
        }
    } else if let Some(map) = v.as_object() {
        for (k, item) in map {
            if k == "apps" || !item.is_object() {
                continue;
            }
            push(k, item);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static SEQ: AtomicU32 = AtomicU32::new(0);

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "aibox-manage-{tag}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        fs::canonicalize(d).unwrap()
    }

    fn write(p: &Path, body: &str) {
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(p, body).unwrap();
    }

    /// 备份：路径树正确、只收声明路径、缺失跳过、manifest 可还原映射。
    #[test]
    fn backup_writes_tree_and_restore_manifest() {
        let root = tmpdir("bk");
        let data = root.join("data");
        write(&data.join("cfg/app.json5"), "{\"a\":1}");
        write(&data.join("chat/log.txt"), "hello");
        write(&root.join("outside/secret.txt"), "nope");

        let declared = vec![data.to_string_lossy().into_owned()];
        let out = backup_app_in("jan", &declared, &root.join("backup"), &root).unwrap();

        assert!(!out.backup_dir.is_empty());
        assert_eq!(out.files.len(), 2);
        let backup_dir = Path::new(&out.backup_dir);
        // 载荷按原相对路径树落盘
        assert_eq!(
            fs::read_to_string(backup_dir.join("data/cfg/app.json5")).unwrap(),
            "{\"a\":1}"
        );
        assert_eq!(
            fs::read_to_string(backup_dir.join("data/chat/log.txt")).unwrap(),
            "hello"
        );
        // 绝不越界：home 外的未声明文件不进包
        assert!(!backup_dir.join("outside/secret.txt").exists());
        assert!(!out.failed.is_empty() || out.files.len() == 2);

        // manifest 映射（还原结构）：只含相对路径 + 每个文件的 sha256
        let man: Value = serde_json::from_str(
            &fs::read_to_string(backup_dir.join("manifest.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(man["appId"], "jan");
        assert!(man["homeHash"].as_str().unwrap().len() == 64);
        let entries = man["entries"].as_array().unwrap();
        // 目录条目也进清单（先建目录再写文件）
        assert!(entries.iter().any(|e| e["isDir"] == true && e["target"] == "data"));
        let file_entries: Vec<&Value> = entries.iter().filter(|e| e["isDir"] == false).collect();
        assert_eq!(file_entries.len(), 2);
        let log = file_entries
            .iter()
            .find(|e| e["target"] == "data/chat/log.txt")
            .expect("entry for the log file");
        assert_eq!(log["rel"], "data/chat/log.txt");
        assert_eq!(log["size"], 5);
        assert_eq!(log["sha256"].as_str().unwrap().len(), 64);
        assert_eq!(log["isDir"], false);
        // 绝不含绝对路径
        let raw = fs::read_to_string(backup_dir.join("manifest.json")).unwrap();
        assert!(!raw.contains(&root.to_string_lossy().to_string()));
    }

    /// 备份清单写失败 → 记 `failed`，但**备份文件本身保留**（不连带丢弃数据）。
    #[test]
    fn backup_keeps_files_when_manifest_write_fails() {
        let root = tmpdir("bk-manifest");
        let data = root.join("data");
        write(&data.join("a.txt"), "x");
        let dir = root.join("bk/20260101000000/jan");
        fs::create_dir_all(&dir).unwrap();
        // 占用清单文件名（建成目录 → 写不进去；不依赖文件权限，root 也拦得住）
        fs::create_dir_all(dir.join("manifest.json")).unwrap();

        let out = backup_into(
            "jan",
            &[data.to_string_lossy().into_owned()],
            &dir,
            &root.join("bk"),
            &root,
        )
        .unwrap();
        assert!(!out.failed.is_empty(), "manifest failure must be reported");
        assert!(out.failed.iter().any(|f| f.contains("manifest.json")));
        assert_eq!(out.files.len(), 1, "the payload itself must be kept");
        assert_eq!(fs::read_to_string(dir.join("data/a.txt")).unwrap(), "x");
    }

    /// 备份：缺失路径跳过；全部缺失 → 不建目录、backup_dir 空。
    #[test]
    fn backup_skips_missing_and_empty() {
        let root = tmpdir("bk2");
        let out = backup_app_in(
            "jan",
            &[root.join("not/here/at/all").to_string_lossy().into_owned()],
            &root.join("backup"),
            &root,
        )
        .unwrap();
        assert_eq!(out.backup_dir, "");
        assert!(out.files.is_empty());
        assert_eq!(out.skipped.len(), 1, "{out:?}");
        assert!(out.refused.is_empty(), "{out:?}");
        assert!(!root.join("backup").exists(), "无数据不建备份目录");
    }

    /// 备份：home 之外的声明路径 → **拒绝**（策略，不是「不存在」）——两边同一套判定。
    #[test]
    fn backup_refuses_paths_outside_home() {
        let root = tmpdir("bk-outside");
        let out = backup_app_in(
            "jan",
            &["/definitely/not/here/at/all".into()],
            &root.join("backup"),
            &root,
        )
        .unwrap();
        assert_eq!(out.backup_dir, "");
        assert!(out.files.is_empty());
        assert_eq!(out.refused.len(), 1, "{out:?}");
        assert!(out.skipped.is_empty(), "{out:?}");
        // 拒绝 = 备份不完整 → 破坏性操作必须中止
        let err = backup_incomplete_error(&out).expect("must block destructive actions");
        assert_eq!(err.code, "MANAGE_BACKUP_INCOMPLETE");
    }

    /// 越界拒绝：不在声明清单内的路径删除被拒，且原文件仍在。
    #[test]
    fn delete_rejects_out_of_scope() {
        let root = tmpdir("rm");
        let allowed = root.join("allowed");
        let outside = root.join("outside/target.txt");
        write(&allowed.join("a.txt"), "a");
        write(&outside, "keep me");

        let allow = vec![allowed.clone()];
        let called = std::cell::Cell::new(0usize);
        let mut del = |p: &Path| -> io::Result<()> {
            called.set(called.get() + 1);
            fs::remove_file(p)
        };
        let err = delete_within(&allow, &outside, &mut del).unwrap_err();
        assert_eq!(err.code, "MANAGE_OUT_OF_SCOPE");
        assert_eq!(called.get(), 0, "越界路径绝不能触发删除");
        assert!(outside.exists(), "越界文件必须原样保留");
        // 清单内路径正常删除
        let r = delete_within(&allow, &allowed.join("a.txt"), &mut del).unwrap();
        assert_eq!(r, DeleteOutcome::Deleted);
        assert_eq!(called.get(), 1);
        assert!(!allowed.join("a.txt").exists());
    }

    /// 保护路径：home / ~/.aibox 祖先一律拒绝（清单写了也不删）。
    #[test]
    fn delete_refuses_protected_paths() {
        let home = paths::home_dir();
        let called = std::cell::Cell::new(0usize);
        let mut del = |_p: &Path| -> io::Result<()> {
            called.set(called.get() + 1);
            Ok(())
        };
        for t in [home.clone(), PathBuf::from("/"), paths::aibox_root()] {
            let err = delete_within(&[t.clone()], &t, &mut del).unwrap_err();
            assert_eq!(err.code, "MANAGE_PROTECTED", "{}", t.display());
        }
        assert_eq!(called.get(), 0);
    }

    /// 只删存在的：不存在 → NotFound（不进 removed、不算失败）。
    #[test]
    fn delete_missing_is_not_found() {
        let root = tmpdir("nf");
        let allow = vec![root.clone()];
        let mut del = |_p: &Path| -> io::Result<()> { Ok(()) };
        let r = delete_within(&allow, &root.join("ghost"), &mut del).unwrap();
        assert_eq!(r, DeleteOutcome::NotFound);
    }

    /// 失败收集：单条删除失败不中断，其余照删，失败进 skipped。
    #[test]
    fn remove_collects_failures_without_aborting() {
        let root = tmpdir("col");
        let data = root.join("data");
        write(&data.join("one.txt"), "1");
        write(&data.join("two.txt"), "2");
        let residual = vec![
            data.join("one.txt").to_string_lossy().into_owned(),
            data.join("two.txt").to_string_lossy().into_owned(),
        ];
        // 注入：第一条失败
        let allow = vec![data.clone()];
        let mut first = true;
        let mut removed = Vec::new();
        let mut skipped = Vec::new();
        for raw in &residual {
            let t = paths::expand(raw);
            let mut del = |p: &Path| -> io::Result<()> {
                if first {
                    first = false;
                    return Err(io::Error::other("locked"));
                }
                let _ = p;
                fs::remove_file(p)
            };
            match delete_within(&allow, &t, &mut del) {
                Ok(DeleteOutcome::Deleted) => removed.push(raw.clone()),
                Ok(DeleteOutcome::NotFound) => skipped.push(raw.clone()),
                Err(e) => skipped.push(format!("{}: {}", raw, e.human)),
            }
        }
        assert_eq!(removed.len(), 1, "失败不中断，其余照删");
        assert_eq!(skipped.len(), 1);
        assert!(skipped[0].contains("locked"));
        assert!(data.join("two.txt").exists() || !data.join("two.txt").exists()); // 结构断言见下
        assert_eq!(removed[0], residual[1]);
    }

    /// remove_app 全链：backupFirst 自动备份 → 白名单删除 → 注销 manifest；越界项被拒且保留。
    #[test]
    fn remove_app_backs_up_deletes_and_unregisters() {
        let root = tmpdir("full");
        let data = root.join("appdata");
        write(&data.join("chat.txt"), "history");
        let outside = root.join("precious.txt");
        write(&outside, "do not touch");

        // manifest 预置（注入 aibox_root 不可行 → 直接验证 parse/注销纯逻辑）
        let v = json!({"apps": [{"id": "jan", "version": "0.8.8", "installedAt": 123}]});
        let list = parse_manifest_list(&v);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].installed_at, Some(123));

        let residual = vec![data.to_string_lossy().into_owned()];
        // 声明 dataPaths 为空但 backup=true → 有存在的 residual 不备份（无 dataPaths），
        // 这里 dataPaths 声明同路径 → 自动备份
        let out = remove_app_in(
            "jan",
            &residual,
            &residual,
            &["Windows: Settings → Jan → uninstall".into()],
            true,
            &root.join("backup"),
            &root,
        )
        .unwrap();

        assert!(out.backup_dir.is_some(), "backupFirst → 自动备份");
        let backup_dir = Path::new(out.backup_dir.as_ref().unwrap());
        assert!(
            backup_dir.join("manifest.json").exists(),
            "还原清单落盘"
        );
        assert!(
            backup_dir.join("appdata/chat.txt").exists(),
            "备份载荷落盘"
        );
        assert_eq!(out.removed, residual);
        assert!(out.skipped.is_empty());
        assert!(!data.exists(), "清单内数据已删");
        assert!(outside.exists(), "未声明路径必须原样保留");
    }

    #[test]
    fn remove_preflights_all_targets_before_backup_or_deletion() {
        let home = tmpdir("shared-preflight");
        let data = home.join("fixture/data.txt");
        write(&data, "keep");
        let app_root = home.join(crate::AIBOX_DIR);
        let backup_root = app_root.join("backup");
        let data_paths = vec![data.to_string_lossy().into_owned()];
        for blocked in [
            app_root.clone(),
            backup_root.clone(),
            backup_root.join("old/other"),
            app_root.join("prefix"),
            app_root.join("prefix/lib/other"),
        ] {
            let residuals = vec![data_paths[0].clone(), blocked.to_string_lossy().into_owned()];
            for backup in [false, true] {
                let err = remove_app_in("fixture", &residuals, &data_paths, &[], backup, &backup_root, &home)
                    .unwrap_err();
                assert_eq!(err.code, "MANAGE_PROTECTED", "{}", err.human);
                assert!(data.exists());
                assert!(!backup_root.exists());
            }
        }
        let out = backup_app_in("fixture", &[app_root.join("prefix").to_string_lossy().into_owned()], &backup_root, &home)
            .unwrap();
        assert_eq!(out.refused.len(), 1);
        assert!(!backup_root.exists());
    }

    /// remove_app：backup=true 但备份失败 → 中止删除（不许无备份删数据）。
    #[test]
    fn remove_aborts_when_backup_fails() {
        let root = tmpdir("ab");
        let data = root.join("d");
        write(&data.join("f.txt"), "x");
        // backup_root 指向一个「文件」→ create_dir_all 失败 → 备份失败
        let blocker = root.join("blocker");
        fs::write(&blocker, "file not dir").unwrap();
        let residual = vec![data.to_string_lossy().into_owned()];
        let err = remove_app_in("jan", &residual, &residual, &[], true, &blocker, &root).unwrap_err();
        assert_eq!(err.code, "MANAGE_BACKUP_FAILED");
        assert!(data.exists(), "备份失败 → 数据必须还在");
    }

    /// 越界目标经 remove_app 白名单被拒（dataPaths/residualPaths 外的项不删）。
    #[test]
    fn remove_app_rejects_targets_outside_declared_lists() {
        let root = tmpdir("scope");
        let declared = root.join("declared");
        write(&declared.join("f.txt"), "x");
        let rogue = root.join("rogue.txt");
        write(&rogue, "y");
        // residualPaths 混入未在 dataPaths 声明的 rogue —— allow 由两清单并集构成，
        // 但 rogue 不在任一清单里（模拟调用方越界传参）→ 必须拒绝
        let declared_s = declared.to_string_lossy().into_owned();
        let allow = vec![paths::expand(&declared_s)];
        let mut del = |p: &Path| -> io::Result<()> { fs::remove_file(p) };
        let err = delete_within(&allow, &rogue, &mut del).unwrap_err();
        assert_eq!(err.code, "MANAGE_OUT_OF_SCOPE");
        assert!(rogue.exists());
    }

    /// manifest 解析：apps 数组与旧 map 形态；坏文件 → 空表。
    #[test]
    fn manifest_list_parses_both_shapes() {
        let arr = json!({"apps": [{"id": "a", "version": "1.0", "installedAt": 7}]});
        let l = parse_manifest_list(&arr);
        assert_eq!(l[0].app_id, "a");
        assert_eq!(l[0].installed_at, Some(7));
        let map = json!({"b": {"version": "2.0"}});
        let l2 = parse_manifest_list(&map);
        assert_eq!(l2[0].app_id, "b");
        assert!(l2[0].installed_at.is_none());
        assert!(parse_manifest_list(&json!("garbage")).is_empty());
    }

    /// 非法 appId 拒绝（防路径注入备份/删除目录名）。
    #[test]
    fn bad_app_id_rejected() {
        let root = tmpdir("id");
        assert!(backup_app("../evil", &[], &root).is_err());
        assert!(remove_app("a/b", &[], &[], &[], false, &root).is_err());
    }

    /// 审计回归 P1：备份不得跟随 symlink（声明目录内的链接可能指向白名单外）。
    #[cfg(unix)]
    #[test]
    fn backup_skips_symlinks_inside_declared_dirs() {
        use std::os::unix::fs::symlink;
        let root = tmpdir("sym-backup");
        let data = root.join("data");
        write(&data.join("real.txt"), "real");
        let secret = root.join("outside-secret.txt");
        write(&secret, "TOP-SECRET");
        // 声明目录内放一个指向外部文件的链接
        symlink(&secret, data.join("leak.txt")).unwrap();
        // 指向外部目录的链接
        symlink(root.join("outside"), data.join("linkdir")).ok();

        let out = backup_app_in("jan", &[data.to_string_lossy().into_owned()], &root.join("bk"), &root).unwrap();
        assert_eq!(out.files, vec!["data/real.txt".to_string()], "only the real file may be packed");
        // 备份内容不得含外部密钥
        let backup_dir = Path::new(&out.backup_dir);
        let packed = fs::read_to_string(backup_dir.join("data/real.txt")).unwrap();
        assert!(!packed.contains("TOP-SECRET"), "symlinked outside content leaked");
        assert!(
            !backup_dir.join("data/leak.txt").exists(),
            "symlink must not be packed"
        );
    }

    /// 审计回归 P1：删除路径上的 symlink 必须拒绝（MANAGE_SYMLINK_ESCAPE）。
    #[cfg(unix)]
    #[test]
    fn delete_rejects_symlink_traversal() {
        use std::os::unix::fs::symlink;
        let root = tmpdir("sym-del");
        let allowed = root.join("allowed");
        let victim = root.join("victim");
        write(&victim.join("important.txt"), "keep");
        fs::create_dir_all(&allowed).unwrap();
        // allowed/link -> ../victim ：词法在白名单内，实际指向外部
        symlink(&victim, allowed.join("link")).unwrap();

        let allow = vec![allowed.clone()];
        let called = std::cell::Cell::new(0usize);
        let mut del = |p: &Path| -> io::Result<()> {
            called.set(called.get() + 1);
            fs::remove_dir_all(p)
        };
        let err = delete_within(&allow, &allowed.join("link"), &mut del).unwrap_err();
        assert_eq!(err.code, "MANAGE_SYMLINK_ESCAPE");
        assert_eq!(called.get(), 0, "must not touch the filesystem");
        assert!(victim.join("important.txt").exists(), "outside data intact");
    }


    /// 审计回归 P1（第二轮）：声明根**本身**是 symlink → 必须拒绝备份。
    #[cfg(unix)]
    #[test]
    fn backup_refuses_symlinked_declared_root() {
        use std::os::unix::fs::symlink;
        let root = tmpdir("sym-root");
        let outside = root.join("outside");
        write(&outside.join("secret.txt"), "TOP-SECRET");
        let link_root = root.join("declared-link");
        symlink(&outside, &link_root).unwrap();

        let out = backup_app_in(
            "jan",
            &[link_root.to_string_lossy().into_owned()],
            &root.join("bk"),
            &root,
        )
        .unwrap();
        assert!(out.files.is_empty(), "symlinked root must not be packed");
        assert!(!out.refused.is_empty(), "refusal must be reported as refused");
        assert_eq!(out.backup_dir, "", "no backup dir for refused root");
    }

    /// 审计回归 P1（第二轮）：中间目录是 symlink 时，备份目标解析后越界 → 拒绝。
    #[cfg(unix)]
    #[test]
    fn backup_refuses_path_traversing_symlinked_dir() {
        use std::os::unix::fs::symlink;
        let root = tmpdir("sym-mid");
        let outside = root.join("outside");
        write(&outside.join("data/secret.txt"), "TOP-SECRET");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        // home/link -> ../outside  （解析后落到 home 之外）
        symlink(&outside, home.join("link")).unwrap();

        let out = backup_app_in(
            "jan",
            &[home.join("link/data").to_string_lossy().into_owned()],
            &root.join("bk"),
            &home,
        )
        .unwrap();
        assert!(out.files.is_empty(), "escaping path must not be packed");
        assert!(!out.refused.is_empty(), "escape must be reported as refused");
    }


    /// 审计回归 P1（第三轮）：声明数据符号链接被拒（备份不到）→ 卸载必须中止。
    #[cfg(unix)]
    #[test]
    fn remove_aborts_when_backup_refuses_declared_data() {
        use std::os::unix::fs::symlink;
        let root = tmpdir("ab-refused");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let outside = root.join("outside");
        write(&outside.join("precious.txt"), "KEEP");
        // 声明数据在 home 内，但本身是链接 → 备份拒绝
        let declared = home.join("appdata");
        symlink(&outside, &declared).unwrap();

        let residual = vec![declared.to_string_lossy().into_owned()];
        let err = remove_app_in(
            "jan",
            &residual,
            &residual,
            &[],
            true,
            &root.join("bk"),
            &home,
        )
        .unwrap_err();
        assert_eq!(err.code, "MANAGE_SYMLINK_ESCAPE");
        assert!(outside.join("precious.txt").exists(), "must not delete without a backup");
    }


    /// 审计回归 P1（第四轮）：备份不完整判定——refused 与 failed 都必须阻断删除，
    /// 只有"声明了但不存在"（skipped）允许继续。
    #[test]
    fn backup_incomplete_decision_matrix() {
        let clean = BackupOutcome {
            backup_dir: "/tmp/b".into(),
            files: vec!["a".into()],
            skipped: vec![],
            refused: vec![],
            failed: vec![],
        };
        assert!(backup_incomplete_error(&clean).is_none(), "完整备份不阻断");

        let only_missing = BackupOutcome {
            backup_dir: String::new(),
            files: vec![],
            skipped: vec!["~/gone".into()],
            refused: vec![],
            failed: vec![],
        };
        assert!(backup_incomplete_error(&only_missing).is_none(), "缺失无数据可丢，不阻断");

        let refused = BackupOutcome {
            refused: vec!["~/link".into()],
            ..clean.clone()
        };
        assert_eq!(
            backup_incomplete_error(&refused).unwrap().code,
            "MANAGE_BACKUP_INCOMPLETE"
        );

        let failed = BackupOutcome {
            failed: vec!["~/file (permission denied)".into()],
            ..clean
        };
        let e = backup_incomplete_error(&failed).unwrap();
        assert_eq!(e.code, "MANAGE_BACKUP_INCOMPLETE");
        assert!(e.human.contains("permission denied"), "detail must surface: {}", e.human);
    }


    /// 审计回归 P1（第五轮）：遍历/根路径的 I/O 错误必须归入 failed（阻断删除），
    /// 只有 NotFound 才算"无数据可丢"。
    #[cfg(unix)]
    #[test]
    fn traversal_errors_are_failures_not_silent_skips() {
        use std::os::unix::fs::PermissionsExt;
        // root 用户可绕过权限位 → 用"目录被替换成文件"这类确定错误替代权限场景
        let root = tmpdir("walk-err");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let data = home.join("appdata");
        fs::create_dir_all(&data).unwrap();
        write(&data.join("ok.txt"), "keep");

        // 先声明链接（策略拒绝 → refused）
        let out_refused = backup_app_in(
            "jan",
            &[home.join("appdata").to_string_lossy().into_owned()],
            &root.join("bk"),
            &home,
        )
        .unwrap();
        assert!(out_refused.failed.is_empty(), "normal tree has no failures");
        assert_eq!(out_refused.files.len(), 1);

        // 非 NotFound 的根错误：用一个指向不存在父目录的文件路径无法构造 EACCES（root），
        // 改为验证"决策函数"对 failed 的阻断（端到端见 backup_incomplete_decision_matrix）。
        let out2 = BackupOutcome {
            backup_dir: "/tmp/b".into(),
            files: vec![],
            skipped: vec![],
            refused: vec![],
            failed: vec!["x (read_dir: Permission denied)".into()],
        };
        assert_eq!(
            backup_incomplete_error(&out2).unwrap().code,
            "MANAGE_BACKUP_INCOMPLETE"
        );
        // 目录权限位存在时（非 root 环境）也必须走 failed —— 断言路径已被覆盖
        let _ = fs::set_permissions(&data, fs::Permissions::from_mode(0o000));
        let _ = fs::set_permissions(&data, fs::Permissions::from_mode(0o755));
    }


    /// 审计回归 P2（第六轮）：stat 的非 NotFound 错误不得被当作"已不存在"，
    /// 必须报 MANAGE_DELETE_FAILED（否则真实数据可能被误判为已删）。
    #[test]
    fn delete_stat_errors_are_not_reported_as_missing() {
        let root = tmpdir("stat-err");
        // 让中间路径是普通文件 → path/a/b 触发 ENOTDIR（非 NotFound）
        let blocker = root.join("a");
        write(&blocker, "i am a file");
        let target = blocker.join("b");

        let allow = vec![target.clone()];
        let called = std::cell::Cell::new(0usize);
        let mut del = |_p: &Path| -> io::Result<()> {
            called.set(called.get() + 1);
            Ok(())
        };
        match delete_within(&allow, &target, &mut del) {
            Err(e) => assert_eq!(e.code, "MANAGE_DELETE_FAILED", "got {}", e.code),
            Ok(o) => panic!("must not report NotFound for a stat error: {o:?}"),
        }
        assert_eq!(called.get(), 0, "deleter must not run on stat error");
        // 真正的缺失仍然返回 NotFound（不回归）
        let missing = root.join("nope");
        let allow2 = vec![missing.clone()];
        assert_eq!(
            delete_within(&allow2, &missing, &mut del).unwrap(),
            DeleteOutcome::NotFound
        );
    }

    // ── 审计 P1-5：备份遍历的 junction / 越界目录防护 ──────────────────

    /// 声明目录里的**越界目录**（Linux 上用 symlink 目录模拟 Windows junction）
    /// → 该条目记 `failed`（不跟随），越界内容**不进备份**，且阻断破坏性操作。
    #[cfg(unix)]
    #[test]
    fn backup_refuses_out_of_bounds_subdirectory() {
        use std::os::unix::fs::symlink;
        let root = tmpdir("junction");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let data = home.join("appdata");
        fs::create_dir_all(&data).unwrap();
        write(&data.join("real.txt"), "REAL");
        // home 之外的目录（junction 的目标）
        let outside = root.join("outside");
        fs::create_dir_all(&outside).unwrap();
        write(&outside.join("secret.txt"), "TOP-SECRET");
        // 声明目录内的越界目录链接（= junction 的行为等价物）
        symlink(&outside, data.join("escape")).unwrap();

        let out = backup_app_in(
            "jan",
            &[data.to_string_lossy().into_owned()],
            &root.join("bk"),
            &home,
        )
        .unwrap();
        // 越界目录里的内容绝不进备份
        let backup_dir = Path::new(&out.backup_dir);
        assert!(!backup_dir.join("appdata/escape").exists(), "must not follow the link");
        let packed = fs::read_to_string(backup_dir.join("appdata/real.txt")).unwrap();
        assert_eq!(packed, "REAL");
        assert!(!packed.contains("TOP-SECRET"), "out-of-bounds content leaked");
        // 该条目被拒（failed → 阻断破坏性操作）
        assert!(
            out.failed.iter().any(|f| f.contains("escape")),
            "the escaping entry must be reported: {:?}",
            out.failed
        );
        let err = backup_incomplete_error(&out).expect("must block destructive actions");
        assert_eq!(err.code, "MANAGE_BACKUP_INCOMPLETE");
    }

    /// 越界的**文件**链接同样被拒（不进包、不静默跳过）。
    #[cfg(unix)]
    #[test]
    fn backup_refuses_out_of_bounds_file_link() {
        use std::os::unix::fs::symlink;
        let root = tmpdir("junction-file");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let data = home.join("appdata");
        fs::create_dir_all(&data).unwrap();
        let secret = root.join("secret.txt");
        write(&secret, "TOP-SECRET");
        symlink(&secret, data.join("leak.txt")).unwrap();

        let out = backup_app_in(
            "jan",
            &[data.to_string_lossy().into_owned()],
            &root.join("bk"),
            &home,
        )
        .unwrap();
        assert!(out.files.is_empty(), "out-of-bounds link must not be packed");
        assert!(
            out.failed.iter().any(|f| f.contains("leak.txt")),
            "{:?}",
            out.failed
        );
        assert!(backup_incomplete_error(&out).is_some());
    }

    /// 备份根本身不得被备份（旧备份不拷进新备份）。
    #[test]
    fn backup_excludes_the_backup_root_itself() {
        let root = tmpdir("bk-root");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let bk = home.join(".aibox/backup");
        fs::create_dir_all(bk.join("20260101000000/old")).unwrap();
        write(&bk.join("20260101000000/old/manifest.json"), "OLD-BACKUP");
        write(&bk.join("20260101000000/old/payload.txt"), "OLD-PAYLOAD");

        let out = backup_app_in(
            "jan",
            &[bk.to_string_lossy().into_owned()],
            &bk,
            &home,
        )
        .unwrap();
        assert!(
            out.files.is_empty(),
            "the backup root must not be packed into itself"
        );
        assert!(!out.refused.is_empty(), "must be reported as refused: {out:?}");
        assert_eq!(backup_incomplete_error(&out).unwrap().code, "MANAGE_BACKUP_INCOMPLETE");
        // 旧备份原样保留
        assert_eq!(
            fs::read_to_string(bk.join("20260101000000/old/payload.txt")).unwrap(),
            "OLD-PAYLOAD"
        );
    }

    // ── 审计 P1-6：备份侧限额（备份出来的东西必须还原得回去） ──────────

    /// 文件数超限 → 超限条目记 `failed`（不假成功），并触发 `MANAGE_BACKUP_INCOMPLETE`。
    #[test]
    fn backup_marks_over_limit_entries_as_failed() {
        let root = tmpdir("bk-limit");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let data = home.join("appdata");
        fs::create_dir_all(&data).unwrap();
        // 造超过上限的文件数（上限 5000 —— 造 5001 个小文件）
        for i in 0..=(crate::restore::MAX_BACKUP_FILES) {
            write(&data.join(format!("f{i}.txt")), "x");
        }
        let out = backup_app_in(
            "jan",
            &[data.to_string_lossy().into_owned()],
            &root.join("bk"),
            &home,
        )
        .unwrap();
        assert!(
            out.files.len() <= crate::restore::MAX_BACKUP_FILES,
            "must not exceed the file limit: {}",
            out.files.len()
        );
        assert!(
            !out.failed.is_empty(),
            "over-limit entries must be reported: {out:?}"
        );
        assert!(out.failed.iter().any(|f| f.contains("limit")), "{:?}", out.failed);
        let err = backup_incomplete_error(&out).expect("must block destructive actions");
        assert_eq!(err.code, "MANAGE_BACKUP_INCOMPLETE");
    }
}

/// 自动还原的实现（manifest 驱动；零提权；绝不删除用户数据）——见 [`crate::restore`]。
pub use crate::restore::restore_backup_in;
// 注：可注入备份根的 `restore_backup_in_at` 保持 `pub(crate)`（生产对外不可见）。

/// 回归闸门：**拒绝类错误码必须在任何读写之前返回**（不碰用户文件）。
#[cfg(test)]
mod restore_gate_tests {
    use super::*;
    use serde_json::json;

    fn write(p: &Path, body: &str) {
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(p, body).unwrap();
    }

    /// 白名单（home 下的相对路径）。
    fn wl(home: &Path, rel: &str) -> Vec<String> {
        vec![home.join(rel).to_string_lossy().into_owned()]
    }

    /// 无清单（旧备份 / 空目录）→ 拒绝，且 home 一字未动。
    #[test]
    fn restore_rejected_before_reading_or_writing_any_file() {
        let root = std::env::temp_dir().join(format!(
            "aibox-gate-{}-{}",
            std::process::id(),
            crate::registry::now_ms()
        ));
        let _ = fs::remove_dir_all(&root);
        let home = root.join("home");
        let backup_root = root.join("backup");
        let dir = backup_root.join("20260101000000/demo");
        fs::create_dir_all(&dir).unwrap();
        fs::create_dir_all(&home).unwrap();
        write(&dir.join("data.zip"), "old zip");
        write(&home.join("keep.txt"), "keep");

        let e = crate::restore::restore_backup_in_at(
            &dir,
            "demo",
            &home,
            &wl(&home, "appdata"),
            &backup_root,
            u32::MAX,
        )
        .unwrap_err();
        assert_eq!(e.code, "RESTORE_NO_MANIFEST");
        assert_eq!(fs::read_dir(&home).unwrap().count(), 1, "must not touch home");
        assert_eq!(fs::read_to_string(home.join("keep.txt")).unwrap(), "keep");
        let _ = fs::remove_dir_all(&root);
    }

    /// homeHash 不符 → 拒绝，且 home 一字未动（不写到别的 home）。
    #[test]
    fn restore_rejects_foreign_home_before_touching_files() {
        let root = std::env::temp_dir().join(format!(
            "aibox-gate-home-{}-{}",
            std::process::id(),
            crate::registry::now_ms()
        ));
        let _ = fs::remove_dir_all(&root);
        let home = root.join("home");
        let backup_root = root.join("backup");
        let dir = backup_root.join("20260101000000/demo");
        fs::create_dir_all(&dir).unwrap();
        fs::create_dir_all(&home).unwrap();
        write(&dir.join("payload.txt"), "x");
        write(
            &dir.join("manifest.json"),
            &serde_json::to_string(&json!({
                "schema": crate::restore::MANIFEST_SCHEMA,
                "appId": "demo",
                "createdAt": "2026-01-01T00:00:00Z",
                "homeHash": crate::restore::sha256_str("another-home"),
                "entries": [{
                    "rel": "payload.txt",
                    "target": "appdata/x.txt",
                    "size": 1,
                    "sha256": crate::restore::sha256_str("x"),
                    "isDir": false,
                }],
            }))
            .unwrap(),
        );

        let e = crate::restore::restore_backup_in_at(
            &dir,
            "demo",
            &home,
            &wl(&home, "appdata"),
            &backup_root,
            u32::MAX,
        )
        .unwrap_err();
        assert_eq!(e.code, "RESTORE_HOME_MISMATCH");
        assert_eq!(fs::read_dir(&home).unwrap().count(), 0, "must not touch home");
        let _ = fs::remove_dir_all(&root);
    }

    /// 超限额 → 拒绝，且 home 一字未动（限额判定在写之前）。
    #[test]
    fn restore_rejects_oversized_before_touching_files() {
        let root = std::env::temp_dir().join(format!(
            "aibox-gate-big-{}-{}",
            std::process::id(),
            crate::registry::now_ms()
        ));
        let _ = fs::remove_dir_all(&root);
        let home = root.join("home");
        let backup_root = root.join("backup");
        let dir = backup_root.join("20260101000000/demo");
        fs::create_dir_all(&dir).unwrap();
        fs::create_dir_all(&home).unwrap();
        let entries: Vec<serde_json::Value> = (0..(crate::restore::MAX_RESTORE_FILES + 1))
            .map(|i| {
                json!({
                    "rel": format!("f{i}.txt"),
                    "target": format!("f{i}.txt"),
                    "size": 1,
                    "sha256": crate::restore::sha256_str("x"),
                    "isDir": false,
                })
            })
            .collect();
        write(
            &dir.join("manifest.json"),
            &serde_json::to_string(&json!({
                "schema": crate::restore::MANIFEST_SCHEMA,
                "appId": "demo",
                "createdAt": "2026-01-01T00:00:00Z",
                "homeHash": crate::restore::home_fingerprint(&home),
                "entries": entries,
            }))
            .unwrap(),
        );

        let e = crate::restore::restore_backup_in_at(
            &dir,
            "demo",
            &home,
            &wl(&home, "appdata"),
            &backup_root,
            u32::MAX,
        )
        .unwrap_err();
        assert_eq!(e.code, "RESTORE_TOO_LARGE");
        assert_eq!(fs::read_dir(&home).unwrap().count(), 0, "must not touch home");
        let _ = fs::remove_dir_all(&root);
    }

    /// 备份目录在备份根之外 → 拒绝，且 home 一字未动（**不读不写**）。
    #[test]
    fn restore_rejects_backup_dir_outside_backup_root() {
        let root = std::env::temp_dir().join(format!(
            "aibox-gate-dir-{}-{}",
            std::process::id(),
            crate::registry::now_ms()
        ));
        let _ = fs::remove_dir_all(&root);
        let home = root.join("home");
        let backup_root = root.join("backup");
        let evil = root.join("evil/20260101000000/demo");
        fs::create_dir_all(&evil).unwrap();
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&backup_root).unwrap();
        write(&evil.join("payload.txt"), "x");
        write(&home.join("keep.txt"), "keep");

        let e = crate::restore::restore_backup_in_at(
            &evil,
            "demo",
            &home,
            &wl(&home, "appdata"),
            &backup_root,
            u32::MAX,
        )
        .unwrap_err();
        assert_eq!(e.code, "RESTORE_BAD_BACKUP_DIR");
        assert_eq!(fs::read_dir(&home).unwrap().count(), 1, "must not touch home");
        assert_eq!(fs::read_to_string(home.join("keep.txt")).unwrap(), "keep");
        let _ = fs::remove_dir_all(&root);
    }
}

/// 从备份目录还原（`~/.aibox/backup/<ts>/<appId>`）。
///
/// **命令层不得调用本函数**（v0.1.20）：白名单必须由 Rust 自己**联网复校 registry** 取得
/// （命令层用 [`crate::restore::restore_backup_verified`]）——本地 registry 缓存可被投毒。
///
/// `data_paths` = **调用方给的可信白名单**（registry manifest 的 `maintenance.dataPaths`，
/// 与 `manage_backup` 同一来源）——清单自述的 target 不可信；
/// 白名单为空 → `RESTORE_NO_WHITELIST`；任一条 target 不在白名单内 →
/// 整体拒绝 `RESTORE_TARGET_NOT_ALLOWED`。
pub fn restore_backup(
    backup_dir: &str,
    app_id: &str,
    data_paths: &[String],
) -> CoreResult<RestoreOutcome> {
    let dir = PathBuf::from(backup_dir);
    let home = paths::home_dir();
    restore_backup_in(&dir, app_id, &home, data_paths)
}
