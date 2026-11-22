//! 安全自动数据还原（manifest 驱动；零提权；**永不删除用户数据**）。
//!
//! 备份布局（`manage::backup_app_in` 产出）：
//!
//! ```text
//! <backup_root>/<ts>/<appId>/manifest.json   ← 还原清单（**不可信**：攻击者可篡改）
//! <backup_root>/<ts>/<appId>/<rel>...        ← 备份载荷（相对路径树）
//! <backup_root>/pre-restore-<ts>/<appId>/…   ← 覆盖前的旧数据保全目录（在备份根内）
//! ```
//!
//! 还原不变量（每条都有单测，见本文件 `tests`）：
//! 1. **白名单来自调用方**（P0）：还原目标必须落在调用方给的 `dataPaths` 白名单内
//!    （与 `manage_backup` 同一来源：registry manifest 的 `maintenance.dataPaths`）——
//!    **清单自述的 target 不可信**；白名单为空 → `RESTORE_NO_WHITELIST`；
//!    任一条 entry 的 target 不在白名单内 → **整体拒绝** `RESTORE_TARGET_NOT_ALLOWED`
//!    （fail-closed：不逐条跳过——被篡改的清单不该只丢一半）。
//!    **白名单条目本身等于 home 根（`~` / `~/.` / home 绝对路径）→ 拒绝该条目**
//!    （否则等于放开整个 home → 可覆写 `~/.ssh/authorized_keys`；审计 P1-1/P1-2）。
//!    白名单类型**显式化**：目录 → 其下路径都允许；文件 → 只允许精确命中；
//!    **尚不存在（类型未知）→ 只允许精确命中一个文件**（不据清单自述把数据文件建成目录）。
//! 2. **备份目录在备份根内**（P0）：`backup_dir` 必须位于 `~/.aibox/backup` 之下，
//!    否则整体拒绝 `RESTORE_BAD_BACKUP_DIR`；保全目录也固定写在备份根内；
//!    **还原目标永远不得落在备份库内**（否则篡改清单可写进备份库；审计 P2-4）。
//! 3. **路径防护**：target 相对 home、不含 `..`、拼回 home 后仍在 home 内；
//!    目标任一组件是软链 → 跳过；备份源是软链 → 跳过（绝不穿链读写）。
//! 4. **校验后替换**：还原前重算备份源 sha256，与清单不符 → 跳过（疑似篡改，不写）。
//! 5. **现有数据先保全**：目标已存在 → 先**移**到**唯一**保全目录
//!    （`<backup_root>/pre-restore-<ts>/<appId>/…`，不是删除）→ 再写新文件；
//!    失败时回滚，用户仍能从原位拿到旧数据；跨设备分支比 **sha256**（不只比大小）。
//! 6. **原子写 + 写后校验**：同目录临时文件（**独占创建** `create_new`）→ 写完重算
//!    sha256 → 一致才 rename 到位；失败路径清掉临时文件。
//! 7. **单条失败不中断**：逐条收集 `skipped` / `failed`，其余照旧还原。
//! 8. **限额**：≤5000 文件、≤2 GiB、≤20000 条目、清单 ≤32 MiB（**有界读取**）→
//!    超限整体拒绝（`RESTORE_TOO_LARGE` / `RESTORE_MANIFEST_TOO_LARGE`，此时未写任何文件）。
//! 9. **零提权 / 不删任何东西**：只做「移到保全目录」+「写新文件」。
//! 10. **备份库与保全目录私有**（Unix 0700 / 0600）：里面是应用凭据明文，
//!    同机别的账号不该能读（审计 P1-3）。
//!
//! 整体拒绝（抛错、**在任何读写之前**）：`RESTORE_NO_MANIFEST` / `RESTORE_HOME_MISMATCH` /
//! `RESTORE_TOO_LARGE` / `RESTORE_MANIFEST_TOO_LARGE` / `RESTORE_APP_MISMATCH` /
//! `RESTORE_NO_WHITELIST` / `RESTORE_TARGET_NOT_ALLOWED` / `RESTORE_BAD_BACKUP_DIR`；
//! 逐条问题只进 `skipped` / `failed` 列表。

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};

use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::err::{CoreError, CoreResult};
use crate::fsops;
use crate::fsops::sha256_file_pinned;
use crate::datapaths::{self, DataPathEntry, PathKind};

/// 单次还原的文件数上限（超限整体拒绝）。
pub const MAX_RESTORE_FILES: usize = 5_000;
/// 单次还原的总字节上限（2 GiB；超限整体拒绝）。
pub const MAX_RESTORE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// 单次还原的条目总数上限（含目录条目；超限整体拒绝——防止清单爆量拖死还原）。
pub const MAX_RESTORE_ENTRIES: usize = 20_000;
/// 清单文件体积上限（32 MiB；超限 → 拒绝，防恶意清单爆内存）。
pub const MAX_MANIFEST_BYTES: usize = 32 * 1024 * 1024;
/// 备份侧上限（与还原一致：**备份出来的东西必须还原得回去**）。
pub const MAX_BACKUP_FILES: usize = MAX_RESTORE_FILES;
pub const MAX_BACKUP_BYTES: u64 = MAX_RESTORE_BYTES;
pub const MAX_BACKUP_ENTRIES: usize = MAX_RESTORE_ENTRIES;
/// 还原清单格式版本（不符 → 拒绝，不猜路径）。
pub const MANIFEST_SCHEMA: &str = "aibox-restore/v1";

/// 还原结果（`restore_backup`；TS: `{restored, restoredPaths?, skipped?, failed?, preservedDir?}`）。
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreOutcome {
    /// 成功还原的文件数
    pub restored: usize,
    /// 还原到的原路径（人话：给用户看「哪些文件回来了」）
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub restored_paths: Vec<String>,
    /// 被安全策略跳过（越界 / 软链 / 校验不符 / 类型冲突）
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<String>,
    /// 写入失败（保全失败 / 写后校验不符 / 源缺失）
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub failed: Vec<String>,
    /// 旧数据保全目录（被覆盖的原文件移到这里；空 = 本次没有覆盖任何已有文件）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preserved_dir: Option<String>,
}

/// 清单单条（`rel` = 备份内相对路径；`target` = 相对 home 的原路径，**绝不含绝对路径**）。
#[derive(Debug, Clone)]
struct ManifestEntry {
    rel: String,
    target: String,
    size: u64,
    sha256: String,
    is_dir: bool,
}

/// 还原清单（只解析需要的字段；结构不符 = 清单损坏 → 整体拒绝）。
#[derive(Debug, Clone)]
struct Manifest {
    home_hash: String,
    entries: Vec<ManifestEntry>,
}

/// 白名单（**调用方给的 dataPaths**；清单自述路径不可信）。
///
/// - `entries` = home 相对条目 + **显式类型**（目录/文件/未知——见 [`datapaths`]）
/// - `refused` = 被策略拒绝的声明（home 本身 / home 之外 / 软链根）
struct Allowlist {
    entries: Vec<DataPathEntry>,
    refused: Vec<String>,
}

/// 单条还原结果。
enum EntryResult {
    /// 文件已还原（绝对路径）
    Restored(String),
    /// 目录已就绪
    Ready,
    /// 跳过（原因）——安全策略或校验不符，**未写任何东西**
    Skip(String),
    /// 失败（原因）——尽力保全后仍未还原
    Fail(String),
}

/// home 指纹：canonical home 路径的 sha256（备份与还原必须一致）。
///
/// 作用：备份被搬到另一台机器 / 另一个 home 后**拒绝还原**（绝不写到别的 home）。
pub fn home_fingerprint(home: &Path) -> String {
    let canon = fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
    sha256_str(&canon.to_string_lossy())
}

/// 从备份目录还原数据到 `home` 下的原路径（备份根固定为 `~/.aibox/backup`）。
///
/// `data_paths` = **调用方给的可信白名单**（registry manifest 的 `maintenance.dataPaths`，
/// 与 `manage_backup` 同一来源）。清单自述的 target 一律不可信。
///
/// 整体拒绝（**任何读写之前**返回）：
/// - `RESTORE_BAD_BACKUP_DIR`：备份目录不在备份根之内
/// - `RESTORE_NO_WHITELIST`：白名单为空 / 没有可用条目（含「白名单 = home 本身」被拒）
/// - `RESTORE_NO_MANIFEST`：备份里没有清单 / 清单损坏（旧备份格式 → 明确拒绝，不猜路径）
/// - `RESTORE_MANIFEST_TOO_LARGE`：清单超过 32 MiB
/// - `RESTORE_HOME_MISMATCH`：备份来自别的 home（homeHash 对不上）
/// - `RESTORE_TOO_LARGE`：超过文件数 / 字节限额
/// - `RESTORE_APP_MISMATCH`：清单属于别的应用
/// - `RESTORE_TARGET_NOT_ALLOWED`：清单里任一条 target 不在白名单内 / 类型冲突 /
///   目标落在备份库内
///
/// 逐条问题只进 `skipped` / `failed`（不抛异常、不中断其余条目）。
pub fn restore_backup_in(
    backup_dir: &Path,
    app_id: &str,
    home: &Path,
    data_paths: &[String],
) -> CoreResult<RestoreOutcome> {
    restore_backup_in_at(
        backup_dir,
        app_id,
        home,
        data_paths,
        &crate::manage::default_backup_root(),
    )
}

/// 可注入备份根的实现（**`pub(crate)`**：生产对外不允许换根——审计 P2-1；
/// 只有 crate 内（测试）能指定别的备份根）。
pub(crate) fn restore_backup_in_at(
    backup_dir: &Path,
    app_id: &str,
    home: &Path,
    data_paths: &[String],
    backup_root: &Path,
) -> CoreResult<RestoreOutcome> {
    crate::registry::validate_id(app_id)?;

    // ① home 规范化（解析不了 → fail-closed，绝不猜）
    let home_real = fs::canonicalize(home).map_err(|e| {
        CoreError::new(
            "RESTORE_HOME_MISMATCH",
            format!(
                "Home directory could not be resolved ({}): {e}; refusing to restore",
                home.display()
            ),
        )
    })?;

    // ② 备份目录必须在备份根之内（P0：不能借还原在 home 之外写数据）
    let backup_root_real = fs::canonicalize(backup_root).map_err(|e| {
        CoreError::new(
            "RESTORE_BAD_BACKUP_DIR",
            format!(
                "Backup root could not be resolved ({}): {e}; refusing to restore",
                backup_root.display()
            ),
        )
    })?;
    let backup_dir_real = fs::canonicalize(backup_dir).map_err(|e| {
        CoreError::new(
            "RESTORE_BAD_BACKUP_DIR",
            format!(
                "Backup folder could not be resolved ({}): {e}; refusing to restore",
                backup_dir.display()
            ),
        )
    })?;
    if !backup_dir_real.starts_with(&backup_root_real) {
        return Err(CoreError::new(
            "RESTORE_BAD_BACKUP_DIR",
            format!(
                "Backup folder {} is outside the backup root {}; refusing to restore",
                backup_dir_real.display(),
                backup_root_real.display()
            ),
        ));
    }

    // ③ 白名单（调用方给的 dataPaths；空 → 整体拒绝，不读不写）
    if data_paths.is_empty() {
        return Err(CoreError::new(
            "RESTORE_NO_WHITELIST",
            "No data path whitelist was supplied; refusing to restore",
        ));
    }
    let allow = build_allowlist(data_paths, &home_real);
    if allow.entries.is_empty() {
        // 常见原因：白名单就是 home 本身（`~` / `~/.` / home 绝对路径）→ 拒绝
        return Err(CoreError::new(
            "RESTORE_NO_WHITELIST",
            format!(
                "The supplied data path whitelist does not contain any usable path inside the home directory{}; refusing to restore",
                if allow.refused.is_empty() {
                    String::new()
                } else {
                    format!(" (refused: {})", allow.refused.join(", "))
                }
            ),
        ));
    }

    // ④ 定位清单（备份目录本身或其 <appId> 子目录）
    let manifest_path = locate_manifest(&backup_dir_real, app_id).ok_or_else(|| {
        CoreError::new(
            "RESTORE_NO_MANIFEST",
            format!(
                "No restorable manifest found in {}; open the backup folder for manual recovery",
                backup_dir_real.display()
            ),
        )
    })?;
    let text = read_manifest_bounded(&manifest_path)?;
    let value: Value = serde_json::from_str(&text).map_err(|e| {
        CoreError::new(
            "RESTORE_NO_MANIFEST",
            format!(
                "Backup manifest is damaged ({}): {e}",
                manifest_path.display()
            ),
        )
    })?;
    let manifest = parse_manifest(&value, app_id)?;

    // ⑤ home 指纹比对（备份是给别的 home 做的 → 拒绝）
    if !home_fingerprint(&home_real).eq_ignore_ascii_case(&manifest.home_hash) {
        return Err(CoreError::new(
            "RESTORE_HOME_MISMATCH",
            "This backup was taken for a different home directory; refusing to restore",
        ));
    }

    // ⑥ 限额（在任何写之前判定）
    if manifest.entries.len() > MAX_RESTORE_ENTRIES {
        return Err(CoreError::new(
            "RESTORE_TOO_LARGE",
            format!(
                "Backup manifest lists {} entries (limit {MAX_RESTORE_ENTRIES}); refusing to restore",
                manifest.entries.len()
            ),
        ));
    }
    let file_count = manifest.entries.iter().filter(|e| !e.is_dir).count();
    if file_count > MAX_RESTORE_FILES {
        return Err(CoreError::new(
            "RESTORE_TOO_LARGE",
            format!(
                "Backup contains {file_count} files (limit {MAX_RESTORE_FILES}); refusing to restore"
            ),
        ));
    }
    let total_bytes = manifest
        .entries
        .iter()
        .filter(|e| !e.is_dir)
        .fold(0u64, |acc, e| acc.saturating_add(e.size));
    if total_bytes > MAX_RESTORE_BYTES {
        return Err(CoreError::new(
            "RESTORE_TOO_LARGE",
            format!(
                "Backup contains {total_bytes} bytes (limit {MAX_RESTORE_BYTES}); refusing to restore"
            ),
        ));
    }

    // ⑦ 白名单逐条校验：**任一条不通过 → 整体拒绝**（不做「逐条跳过」）
    for entry in &manifest.entries {
        let Some(target_rel) = safe_rel(&entry.target) else {
            return Err(CoreError::new(
                "RESTORE_TARGET_NOT_ALLOWED",
                format!(
                    "Backup manifest targets '{}', which is not inside the declared data paths; refusing to restore",
                    entry.target
                ),
            ));
        };
        if !allow_allows(&allow, &target_rel) {
            return Err(CoreError::new(
                "RESTORE_TARGET_NOT_ALLOWED",
                format!(
                    "Backup manifest targets '{}', which is outside the declared data paths; refusing to restore",
                    entry.target
                ),
            ));
        }
        // 类型一致性：白名单条目是「文件」或「类型未知」时，清单不得在该路径上建目录
        //（否则数据文件路径会被建成目录；审计 P2-3）。
        if entry.is_dir && datapaths::kind_at(&allow.entries, &target_rel) != Some(PathKind::Dir) {
            return Err(CoreError::new(
                "RESTORE_TARGET_NOT_ALLOWED",
                format!(
                    "Backup manifest turns '{}' into a folder, but that data path is not known to be a folder; refusing to restore",
                    entry.target
                ),
            ));
        }
        // 备份库排除：还原目标永远不得落在备份库内（否则篡改清单可写进备份库；审计 P2-4）。
        if target_inside_backup_root(&target_rel, &home_real, &backup_root_real) {
            return Err(CoreError::new(
                "RESTORE_TARGET_NOT_ALLOWED",
                format!(
                    "Backup manifest targets '{}', which is inside the backup library; refusing to restore",
                    entry.target
                ),
            ));
        }
    }

    // ⑧ 逐条还原（载荷根 = 清单所在目录；保全根 = 备份根）
    let payload_root = manifest_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| backup_dir_real.clone());

    let mut outcome = RestoreOutcome::default();
    let mut preserver = Preserver::new(&backup_root_real, app_id);

    for entry in &manifest.entries {
        let res = restore_entry(entry, &payload_root, &home_real, &allow, &mut preserver);
        match res {
            EntryResult::Restored(p) => {
                outcome.restored += 1;
                outcome.restored_paths.push(p);
            }
            EntryResult::Ready => {}
            EntryResult::Skip(reason) => {
                outcome.skipped.push(format!("{}: {reason}", entry.target));
            }
            EntryResult::Fail(reason) => {
                outcome.failed.push(format!("{}: {reason}", entry.target));
            }
        }
    }
    outcome.preserved_dir = preserver.current().map(|p| p.to_string_lossy().into_owned());
    Ok(outcome)
}

/// 定位清单：`<backup_dir>/manifest.json` 或 `<backup_dir>/<appId>/manifest.json`。
fn locate_manifest(backup_dir: &Path, app_id: &str) -> Option<PathBuf> {
    let direct = backup_dir.join("manifest.json");
    if direct.is_file() {
        return Some(direct);
    }
    let nested = backup_dir.join(app_id).join("manifest.json");
    if nested.is_file() {
        return Some(nested);
    }
    None
}

/// **有界读取**清单：先 `metadata().len()` 预检（超限直接拒绝，**不读一个字节**），
/// 再用 `take(MAX+1)` 读入并复核长度（防止文件在预检之后被撑大）。
fn read_manifest_bounded(path: &Path) -> CoreResult<String> {
    let meta = fs::metadata(path).map_err(|e| {
        CoreError::new(
            "RESTORE_NO_MANIFEST",
            format!(
                "Backup manifest could not be inspected ({}): {e}",
                path.display()
            ),
        )
    })?;
    if meta.len() > MAX_MANIFEST_BYTES as u64 {
        return Err(CoreError::new(
            "RESTORE_MANIFEST_TOO_LARGE",
            format!(
                "Backup manifest is too large ({} bytes, limit {MAX_MANIFEST_BYTES}); refusing to restore",
                meta.len()
            ),
        ));
    }
    let file = fs::File::open(path).map_err(|e| {
        CoreError::new(
            "RESTORE_NO_MANIFEST",
            format!("Backup manifest could not be read ({}): {e}", path.display()),
        )
    })?;
    let mut buf: Vec<u8> = Vec::new();
    let mut limited = file.take(MAX_MANIFEST_BYTES as u64 + 1);
    limited.read_to_end(&mut buf).map_err(|e| {
        CoreError::new(
            "RESTORE_NO_MANIFEST",
            format!("Backup manifest could not be read ({}): {e}", path.display()),
        )
    })?;
    if buf.len() > MAX_MANIFEST_BYTES {
        return Err(CoreError::new(
            "RESTORE_MANIFEST_TOO_LARGE",
            format!(
                "Backup manifest is too large (limit {MAX_MANIFEST_BYTES} bytes); refusing to restore"
            ),
        ));
    }
    String::from_utf8(buf).map_err(|_| {
        CoreError::new(
            "RESTORE_NO_MANIFEST",
            format!("Backup manifest is not valid UTF-8 ({}); refusing to restore", path.display()),
        )
    })
}

/// 解析清单（结构不符 = 损坏 → `RESTORE_NO_MANIFEST`）。
fn parse_manifest(value: &Value, app_id: &str) -> CoreResult<Manifest> {
    let damaged = |what: &str| {
        CoreError::new(
            "RESTORE_NO_MANIFEST",
            format!("Backup manifest is damaged ({what}); refusing to restore"),
        )
    };
    let obj = value
        .as_object()
        .ok_or_else(|| damaged("expected a JSON object"))?;
    // 格式版本必须显式匹配（旧格式 `aibox-backup/v1` 一律拒绝，不猜路径）
    match obj.get("schema").and_then(|v| v.as_str()) {
        Some(s) if s == MANIFEST_SCHEMA => {}
        Some(s) => {
            return Err(damaged(&format!(
                "unsupported schema '{s}' (expected '{MANIFEST_SCHEMA}')"
            )))
        }
        None => return Err(damaged("missing schema")),
    }
    let m_app = obj
        .get("appId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| damaged("missing appId"))?;
    if m_app != app_id {
        return Err(CoreError::new(
            "RESTORE_APP_MISMATCH",
            format!("Backup manifest belongs to app '{m_app}', not '{app_id}'"),
        ));
    }
    let home_hash = obj
        .get("homeHash")
        .and_then(|v| v.as_str())
        .ok_or_else(|| damaged("missing homeHash"))?;
    if !is_hex64(home_hash) {
        return Err(damaged("homeHash is not a 64-hex sha256"));
    }
    let arr = obj
        .get("entries")
        .and_then(|v| v.as_array())
        .ok_or_else(|| damaged("missing entries"))?;

    let mut entries = Vec::with_capacity(arr.len());
    for item in arr {
        let rel = item
            .get("rel")
            .and_then(|v| v.as_str())
            .ok_or_else(|| damaged("entry without rel"))?;
        let target = item
            .get("target")
            .and_then(|v| v.as_str())
            .ok_or_else(|| damaged("entry without target"))?;
        if rel.is_empty() || target.is_empty() {
            return Err(damaged("entry with empty rel/target"));
        }
        let is_dir = item.get("isDir").and_then(|v| v.as_bool()).unwrap_or(false);
        // 文件条目的 size **缺失即视为损坏**（默认 0 会让限额与校验失效）
        let size = match item.get("size").and_then(|v| v.as_u64()) {
            Some(s) => s,
            None => {
                if is_dir {
                    0
                } else {
                    return Err(damaged("file entry without size"));
                }
            }
        };
        let sha256 = item
            .get("sha256")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        if !is_dir && !is_hex64(sha256) {
            return Err(damaged("file entry without a 64-hex sha256"));
        }
        entries.push(ManifestEntry {
            rel: rel.to_string(),
            target: target.to_string(),
            size,
            sha256: sha256.to_ascii_lowercase(),
            is_dir,
        });
    }
    Ok(Manifest {
        home_hash: home_hash.to_ascii_lowercase(),
        entries,
    })
}

/// 白名单构建（**与备份侧共用同一套判定**——[`datapaths`]）。
///
/// - `~` / `~/.` / home 绝对路径 → **拒绝该条目**（`refused`）：解析后等于 home 根
///   = 放开整个 home（可覆写 `~/.ssh/authorized_keys`；审计 P1-1）。
/// - home 之外 / 软链根 → 拒绝该条目。
/// - 显式类型：目录 → 其下路径都允许；文件 → 只允许精确命中；
///   **尚不存在（类型未知）→ 只允许精确命中**（不据清单自述把数据文件路径建成目录）。
fn build_allowlist(data_paths: &[String], home_real: &Path) -> Allowlist {
    let (entries, refused) = datapaths::build_entries(data_paths, home_real);
    Allowlist { entries, refused }
}

/// target 是否落在白名单内（精确命中，或位于某个目录型白名单条目之下）。
fn allow_allows(allow: &Allowlist, target: &Path) -> bool {
    allow.entries.iter().any(|e| e.covers(target))
}

/// 还原目标是否落在**备份库**内（备份库 = `~/.aibox/backup`）——必须拒绝：
/// 否则篡改清单可以把文件写进备份库（审计 P2-4）。
fn target_inside_backup_root(target_rel: &Path, home_real: &Path, backup_root_real: &Path) -> bool {
    let abs = home_real.join(target_rel);
    if abs.starts_with(backup_root_real) {
        return true;
    }
    // 祖先目录是软链时，词法比较不足 → 再按解析后的路径比一次
    match fs::canonicalize(&abs) {
        Ok(c) => c.starts_with(backup_root_real),
        Err(_) => false,
    }
}

/// 保全器：一次还原**只建一个**保全目录（`<backup_root>/pre-restore-<ts>/<appId>`），
/// 所有被覆盖的旧文件都进这里；同时写一份 `restore-journal.json` 留痕（原路径 → 保全路径）。
struct Preserver<'a> {
    backup_root: &'a Path,
    app_id: &'a str,
    base: Option<PathBuf>,
    dir: Option<PathBuf>,
    journal: Vec<(String, String)>,
}

impl<'a> Preserver<'a> {
    fn new(backup_root: &'a Path, app_id: &'a str) -> Self {
        Self {
            backup_root,
            app_id,
            base: None,
            dir: None,
            journal: Vec::new(),
        }
    }

    /// 保全目录（一次还原只创建一次并复用）。
    fn dir(&mut self) -> Option<PathBuf> {
        if self.dir.is_some() {
            return self.dir.clone();
        }
        let ts = fsops::now_ts();
        // **独占创建**（同名已存在 → 换名；序号用尽 → 随机后缀；再失败 → 拒绝）
        // → 新旧保全目录**绝不会混写**（审计 P2-5）。
        let base = unique_dir(self.backup_root, &format!("pre-restore-{ts}"))?;
        let dir = base.join(self.app_id);
        // 保全目录私有（Unix 0700）——里面是被覆盖的旧凭据
        if fsops::ensure_private_dir(&dir).is_err() {
            return None;
        }
        self.base = Some(base);
        self.dir = Some(dir.clone());
        Some(dir)
    }

    /// 已创建的保全目录（**不创建**——没发生覆盖时不该凭空多一个目录）。
    fn current(&self) -> Option<PathBuf> {
        self.dir.clone()
    }

    /// 留痕文件：`<pre-restore-<ts>>/restore-journal-<appId>.json`（在备份根内，不碰用户数据）。
    fn journal_path(&self) -> Option<PathBuf> {
        self.base
            .as_ref()
            .map(|b| b.join(format!("restore-journal-{}.json", self.app_id)))
    }

    /// 写留痕（崩溃无痕：每保全一个文件就落一次盘）。
    fn write_journal(&self) {
        let Some(path) = self.journal_path() else {
            return;
        };
        let entries: Vec<Value> = self
            .journal
            .iter()
            .map(|(target, preserved)| json!({ "target": target, "preserved": preserved }))
            .collect();
        let payload = json!({
            "appId": self.app_id,
            "updatedAt": fsops::now_iso8601(),
            "entries": entries,
        });
        let Ok(text) = serde_json::to_string_pretty(&payload) else {
            return;
        };
        // 留痕含原路径与保全路径 → **私有**（Unix 0600）
        let _ = fsops::write_text_atomic_private(&path, &text);
    }
}

/// 单条还原。
fn restore_entry(
    entry: &ManifestEntry,
    payload_root: &Path,
    home_real: &Path,
    allow: &Allowlist,
    preserver: &mut Preserver,
) -> EntryResult {
    // ── 不变式 1：白名单（调用方给的 dataPaths）——清单自述路径不可信 ──
    let Some(target_rel) = safe_rel(&entry.target) else {
        return EntryResult::Skip(format!(
            "target path '{}' is not a safe relative path",
            entry.target
        ));
    };
    if !allow_allows(allow, &target_rel) {
        return EntryResult::Skip("target is outside the declared data paths".into());
    }
    let Some(rel) = safe_rel(&entry.rel) else {
        return EntryResult::Skip(format!(
            "backup path '{}' is not a safe relative path",
            entry.rel
        ));
    };
    let target_abs = home_real.join(&target_rel);
    let source = payload_root.join(&rel);

    // ── 不变式 3：拼回 home 后必须仍在 home 内（软链逃逸兜底） ──
    if !canonical_within(&target_abs, home_real) {
        return EntryResult::Skip("resolved target escapes the home directory".into());
    }
    // ── 不变式 3（续）：链接防护（目标侧：任一组件是软链 → 跳过，绝不穿链写） ──
    if let Some(link) = symlink_component(&target_abs, home_real) {
        return EntryResult::Skip(format!(
            "target path traverses a symlink ({})",
            link.display()
        ));
    }
    // ── 不变式 3（续）：链接防护（备份源侧） ──
    if let Some(link) = symlink_component(&source, payload_root) {
        return EntryResult::Skip(format!(
            "backup source traverses a symlink ({})",
            link.display()
        ));
    }

    // 类型冲突：清单说文件但原位是目录（或反之）→ 跳过，不动原数据
    let target_meta = fs::symlink_metadata(&target_abs);
    match &target_meta {
        Ok(m) if entry.is_dir && !m.is_dir() => {
            return EntryResult::Skip("target exists as a file".into())
        }
        Ok(m) if !entry.is_dir && m.is_dir() => {
            return EntryResult::Skip("target exists as a directory".into())
        }
        _ => {}
    }

    if entry.is_dir {
        return restore_dir(&target_abs);
    }
    restore_file(entry, &source, &target_abs, &target_rel, preserver)
}

/// 目录条目：缺失则创建（父目录一并建）。
fn restore_dir(target_abs: &Path) -> EntryResult {
    match fs::symlink_metadata(target_abs) {
        Ok(m) if m.is_dir() => EntryResult::Ready,
        Ok(_) => EntryResult::Skip("target exists as a file".into()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            if let Err(e) = fs::create_dir_all(target_abs) {
                return EntryResult::Fail(format!("cannot create directory: {e}"));
            }
            // 建完再确认不是软链（fail-closed）
            match fs::symlink_metadata(target_abs) {
                Ok(m) if m.file_type().is_symlink() => {
                    let _ = fs::remove_dir(target_abs);
                    EntryResult::Fail("created path turned out to be a symlink".into())
                }
                Ok(_) => EntryResult::Ready,
                Err(e) => EntryResult::Fail(format!("cannot inspect created directory: {e}")),
            }
        }
        Err(e) => EntryResult::Fail(format!("cannot inspect target: {e}")),
    }
}

/// 文件条目：校验 → 保全旧数据 → 原子写 + 写后校验。
fn restore_file(
    entry: &ManifestEntry,
    source: &Path,
    target_abs: &Path,
    target_rel: &Path,
    preserver: &mut Preserver,
) -> EntryResult {
    // 源必须是普通文件（缺失 = 备份不完整；软链已在上面拦掉）
    let meta = match fs::symlink_metadata(source) {
        Ok(m) => m,
        Err(e) => return EntryResult::Fail(format!("backup source unreadable: {e}")),
    };
    if !meta.is_file() {
        return EntryResult::Skip("backup source is not a regular file".into());
    }
    if meta.len() != entry.size {
        return EntryResult::Skip(format!(
            "backup source size {} does not match the manifest ({})",
            meta.len(),
            entry.size
        ));
    }

    // ── 不变式 4：校验后替换（备份源被改过 → 跳过，绝不写） ──
    let actual = match sha256_file_pinned(source) {
        Ok(h) => h,
        Err(e) => return EntryResult::Fail(format!("cannot hash backup source: {e}")),
    };
    if !actual.eq_ignore_ascii_case(&entry.sha256) {
        return EntryResult::Skip(format!(
            "checksum mismatch (backup file was modified: expected {}, got {actual})",
            entry.sha256
        ));
    }

    // ── 不变式 5：现有数据先保全（移走，不是删除） ──
    let mut preserved: Option<PathBuf> = None;
    if fs::symlink_metadata(target_abs)
        .map(|m| m.is_file())
        .unwrap_or(false)
    {
        match preserve_existing(target_abs, target_rel, preserver) {
            Ok(Some(dest)) => preserved = Some(dest),
            Ok(None) => {}
            Err(e) => {
                return EntryResult::Fail(format!("cannot preserve existing data: {e}"));
            }
        }
    }

    // ── 不变式 6：原子写 + 写后校验 ──
    match write_verified(source, target_abs, &entry.sha256) {
        Ok(()) => EntryResult::Restored(target_abs.to_string_lossy().into_owned()),
        Err(e) => {
            // 写失败 → 把保全的旧数据放回原位（用户仍拿到旧内容）
            let mut note = e.to_string();
            if let Some(dest) = preserved {
                if let Err(rb) = move_back(&dest, target_abs) {
                    note.push_str(&format!("; rollback failed: {rb}"));
                }
            }
            EntryResult::Fail(note)
        }
    }
}

/// 把已存在的目标文件**移**到保全目录（`<backup_root>/pre-restore-<ts>/<appId>/<target>`）。
///
/// 只移动、绝不删除：跨设备时先复制 + **校验 sha256**（不只比大小），复制成功才删原文件。
fn preserve_existing(
    target_abs: &Path,
    target_rel: &Path,
    preserver: &mut Preserver,
) -> io::Result<Option<PathBuf>> {
    let Some(dir) = preserver.dir() else {
        return Err(io::Error::other("cannot create the preservation folder"));
    };
    let dest = unique_path(&dir.join(target_rel));
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    match fs::rename(target_abs, &dest) {
        Ok(()) => {}
        Err(_) => {
            // 跨设备（EXDEV）：复制 → 比 sha256 → 到位 → 才删原文件
            let src_hash = sha256_file_pinned(target_abs)?;
            let (_, copied_hash) = fsops::copy_file_hashed(target_abs, &dest)
                .map_err(|e| io::Error::other(e.human))?;
            if !copied_hash.eq_ignore_ascii_case(&src_hash) {
                // 内容对不上 → **不删原文件**（等长但损坏的情况也要挡住）
                let _ = fs::remove_file(&dest);
                return Err(io::Error::other(
                    "copy of the existing file does not match the original (checksum mismatch)",
                ));
            }
            fs::remove_file(target_abs)?;
        }
    }
    preserver.journal.push((
        target_rel.to_string_lossy().replace('\\', "/"),
        dest.to_string_lossy().into_owned(),
    ));
    preserver.write_journal();
    Ok(Some(dest))
}

/// 还原失败时的回滚：把保全的文件移回原位。
fn move_back(preserved: &Path, target_abs: &Path) -> io::Result<()> {
    if fs::rename(preserved, target_abs).is_ok() {
        return Ok(());
    }
    fs::copy(preserved, target_abs)?;
    let _ = fs::remove_file(preserved);
    Ok(())
}

/// 原子写 + 写后校验：同目录临时文件 → 写完重算 sha256 → 一致才 rename 到位。
///
/// 临时文件名固定（`.<name>.aibox-restore.<pid>.tmp`）且**独占创建**
/// （`create_new`）——同名已存在 → 换名重试（不覆盖别人的文件，也不静默复用）。
fn write_verified(source: &Path, target_abs: &Path, expected_sha: &str) -> io::Result<()> {
    let dir = target_abs
        .parent()
        .ok_or_else(|| io::Error::other("target has no parent directory"))?;
    fs::create_dir_all(dir)?;
    let name = target_abs
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "restore".into());
    let base = format!(".{name}.aibox-restore.{}", std::process::id());
    // 清掉**本进程**上次留下的同名临时文件（只删普通文件；目录/其他一律不碰），
    // 避免残留临时文件把还原永久卡住。
    let stale = dir.join(format!("{base}.tmp"));
    if let Ok(m) = fs::symlink_metadata(&stale) {
        if m.is_file() {
            let _ = fs::remove_file(&stale);
        }
    }
    let (tmp, mut output) = fsops::create_exclusive_tmp(dir, &base, ".tmp")
        .map_err(|e| io::Error::other(e.human))?;
    // 写（流式；同时算出写入内容的 sha256）
    let write_result = (|| -> io::Result<()> {
        let mut input = fs::File::open(source)?;
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = input.read(&mut buf)?;
            if n == 0 {
                break;
            }
            output.write_all(&buf[..n])?;
        }
        output.flush()?;
        output.sync_all()?;
        Ok(())
    })();
    if let Err(e) = write_result {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    drop(output);
    // 写后校验：重新从磁盘读回重算 sha256
    let actual = match sha256_file_pinned(&tmp) {
        Ok(h) => h,
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }
    };
    if !actual.eq_ignore_ascii_case(expected_sha) {
        let _ = fs::remove_file(&tmp);
        return Err(io::Error::other(format!(
            "write-back checksum mismatch (expected {expected_sha}, got {actual})"
        )));
    }
    // 到位（目标此刻应为空——旧数据已被移入保全目录）
    if let Err(e) = fs::rename(&tmp, target_abs) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// 字符串的 sha256（home 指纹用）。
pub fn sha256_str(s: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    hex(&hasher.finalize())
}

/// 十六进制（小写）。
fn hex(bytes: &[u8]) -> String {
    fsops::hex(bytes)
}

/// 是否 64 位十六进制。
fn is_hex64(s: &str) -> bool {
    s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// 相对路径白名单：必须相对、无 `..`、无根/盘符前缀、无空结果。
fn safe_rel(raw: &str) -> Option<PathBuf> {
    if raw.is_empty() {
        return None;
    }
    let mut out = PathBuf::new();
    for c in Path::new(raw).components() {
        match c {
            Component::Normal(s) => out.push(s),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    if out.as_os_str().is_empty() {
        None
    } else {
        Some(out)
    }
}

/// path（相对 base 的部分）上任一组件是软链 → 返回该组件路径。
fn symlink_component(path: &Path, base: &Path) -> Option<PathBuf> {
    let rel = path.strip_prefix(base).ok()?;
    let mut cur = base.to_path_buf();
    for c in rel.components() {
        cur.push(c.as_os_str());
        match fs::symlink_metadata(&cur) {
            Ok(m) => {
                if m.file_type().is_symlink() {
                    return Some(cur);
                }
            }
            // 不存在 → 更深的也不存在
            Err(_) => return None,
        }
    }
    None
}

/// 最深的已存在祖先解析后仍须在 home 内（软链逃逸兜底）。
fn canonical_within(path: &Path, home_real: &Path) -> bool {
    let mut p = path.to_path_buf();
    loop {
        match fs::canonicalize(&p) {
            Ok(c) => return c == *home_real || c.starts_with(home_real),
            Err(_) => {
                if !p.pop() {
                    return false;
                }
            }
        }
    }
}

/// **唯一**的保全目录名（**独占创建**）：同名已存在 → 换名重试；
/// 序号用尽（>1000）→ 随机后缀；仍失败 → `None`（拒绝，绝不返回已存在的路径）。
///
/// 旧实现用 `p.exists()` 轮询 → 同名超过 1000 个时**返回已存在的目录** →
/// 新旧保全混写（审计 P2-5）。
fn unique_dir(root: &Path, name: &str) -> Option<PathBuf> {
    // 序号候选：`name`、`name-2` … `name-1000`
    for i in 0..1000u32 {
        let candidate = if i == 0 {
            root.join(name)
        } else {
            root.join(format!("{name}-{i}"))
        };
        match fs::create_dir(&candidate) {
            Ok(()) => {
                // 收紧权限（Unix 0700；新建即 0700，这里兼容旧目录）
                let _ = fsops::tighten_dir(&candidate);
                return Some(candidate);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(_) => return None,
        }
    }
    // 序号用尽 → 随机后缀（最多 32 次）
    for _ in 0..32u32 {
        let candidate = root.join(format!("{name}-{}", random_suffix()));
        match fs::create_dir(&candidate) {
            Ok(()) => {
                let _ = fsops::tighten_dir(&candidate);
                return Some(candidate);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(_) => return None,
        }
    }
    None
}

/// 随机后缀（8 位十六进制；不依赖外部 crate）。
fn random_suffix() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let pid = std::process::id() as u64;
    let mut v = nanos ^ (pid << 32);
    // xorshift 变体：同一纳秒内也不重复
    v ^= v << 13;
    v ^= v >> 7;
    v ^= v << 17;
    format!("{v:016x}")[..8].to_string()
}

/// 不存在的文件路径 → 加序号，避免覆盖既有保全文件。
fn unique_path(p: &Path) -> PathBuf {
    if !p.exists() {
        return p.to_path_buf();
    }
    let name = p
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".into());
    let dir = p.parent().unwrap_or(Path::new("."));
    let mut i = 2;
    loop {
        let cand = dir.join(format!("{name}.{i}"));
        if !cand.exists() {
            return cand;
        }
        i += 1;
        if i > 10_000 {
            return cand;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static SEQ: AtomicU32 = AtomicU32::new(0);

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "aibox-restore-{tag}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn write(p: &Path, body: &str) {
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(p, body).unwrap();
    }

    /// 造一个备份（清单 + 载荷），返回 (备份目录, home, manifest 路径)。
    fn make_backup(
        root: &Path,
        home: &Path,
        app_id: &str,
        files: &[(&str, &str)],
    ) -> (PathBuf, PathBuf) {
        let dir = root.join("20260101000000").join(app_id);
        fs::create_dir_all(&dir).unwrap();
        let mut entries = Vec::new();
        for (rel, body) in files {
            let dest = dir.join(rel);
            write(&dest, body);
            entries.push(json_entry(
                rel,
                rel,
                body.len() as u64,
                &sha256_str(body),
                false,
            ));
        }
        let manifest = serde_json::json!({
            "schema": MANIFEST_SCHEMA,
            "appId": app_id,
            "createdAt": "2026-01-01T00:00:00Z",
            "homeHash": home_fingerprint(home),
            "entries": entries,
        });
        write(
            &dir.join("manifest.json"),
            &serde_json::to_string(&manifest).unwrap(),
        );
        (dir, home.to_path_buf())
    }

    fn json_entry(rel: &str, target: &str, size: u64, sha: &str, is_dir: bool) -> Value {
        serde_json::json!({
            "rel": rel,
            "target": target,
            "size": size,
            "sha256": sha,
            "isDir": is_dir,
        })
    }

    /// 还原用的白名单（home 下的相对路径）。
    fn wl(home: &Path, rel: &str) -> Vec<String> {
        vec![home.join(rel).to_string_lossy().into_owned()]
    }

    /// 正常还原：目录 + 文件回到原路径，内容一致。
    #[test]
    fn restores_files_to_original_paths() {
        let root = tmpdir("ok");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let home = fs::canonicalize(&home).unwrap();
        // 数据目录存在（升级流程的真实情形）→ 白名单类型 = 目录 → 其下路径都允许
        fs::create_dir_all(home.join("appdata")).unwrap();
        let backup_root = root.join("backup");
        fs::create_dir_all(&backup_root).unwrap();
        let (dir, _) = make_backup(
            &backup_root,
            &home,
            "demo",
            &[
                ("appdata/cfg.json", "{\"a\":1}"),
                ("appdata/chat.log", "hello"),
            ],
        );

        let out = restore_backup_in_at(
            &dir,
            "demo",
            &home,
            &wl(&home, "appdata"),
            &backup_root,
        )
        .unwrap();
        assert_eq!(out.restored, 2, "{out:?}");
        assert!(out.failed.is_empty(), "{out:?}");
        assert!(out.skipped.is_empty(), "{out:?}");
        assert_eq!(
            fs::read_to_string(home.join("appdata/cfg.json")).unwrap(),
            "{\"a\":1}"
        );
        assert_eq!(
            fs::read_to_string(home.join("appdata/chat.log")).unwrap(),
            "hello"
        );
    }

    /// 无关文件不受影响（只动清单登记的路径）。
    #[test]
    fn leaves_unrelated_files_untouched() {
        let root = tmpdir("unrelated");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let home = fs::canonicalize(&home).unwrap();
        fs::create_dir_all(home.join("appdata")).unwrap();
        let backup_root = root.join("backup");
        fs::create_dir_all(&backup_root).unwrap();
        write(&home.join("keep-me.txt"), "keep");
        write(&home.join("other/thing.bin"), "thing");
        let (dir, _) = make_backup(
            &backup_root,
            &home,
            "demo",
            &[("appdata/cfg.json", "{}")],
        );

        restore_backup_in_at(
            &dir,
            "demo",
            &home,
            &wl(&home, "appdata"),
            &backup_root,
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(home.join("keep-me.txt")).unwrap(),
            "keep"
        );
        assert_eq!(
            fs::read_to_string(home.join("other/thing.bin")).unwrap(),
            "thing"
        );
        assert!(!home.join("other/thing.bin~").exists());
    }

    /// 越界 target（`../` 与绝对路径）→ **整体拒绝**（fail-closed），目标文件**没被创建**。
    #[test]
    fn refuses_escaping_targets() {
        let root = tmpdir("escape");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let home = fs::canonicalize(&home).unwrap();
        let backup_root = root.join("backup");
        fs::create_dir_all(&backup_root).unwrap();
        let dir = backup_root.join("20260101000000/demo");
        fs::create_dir_all(&dir).unwrap();
        let body = "pwn";
        write(&dir.join("payload.txt"), body);
        let manifest = serde_json::json!({
            "schema": MANIFEST_SCHEMA,
            "appId": "demo",
            "createdAt": "2026-01-01T00:00:00Z",
            "homeHash": home_fingerprint(&home),
            "entries": [
                json_entry("payload.txt", "../escaped.txt", body.len() as u64, &sha256_str(body), false),
                json_entry("payload.txt", "/etc/aibox-escape.txt", body.len() as u64, &sha256_str(body), false),
                json_entry("payload.txt", "appdata/../../escaped2.txt", body.len() as u64, &sha256_str(body), false),
            ],
        });
        write(
            &dir.join("manifest.json"),
            &serde_json::to_string(&manifest).unwrap(),
        );

        let err = restore_backup_in_at(
            &dir,
            "demo",
            &home,
            &wl(&home, "appdata"),
            &backup_root,
        )
        .unwrap_err();
        assert_eq!(err.code, "RESTORE_TARGET_NOT_ALLOWED", "{err:?}");
        assert!(!root.join("escaped.txt").exists(), "must not escape home");
        assert!(!root.join("escaped2.txt").exists(), "must not escape home");
        assert!(!home.join("escaped.txt").exists());
        assert!(!Path::new("/etc/aibox-escape.txt").exists());
        assert_eq!(
            fs::read_dir(&home).unwrap().count(),
            0,
            "a rejected manifest must not touch home"
        );
    }

    /// 目标路径含软链 → 跳过，且**没有穿过软链写到别处**。
    #[cfg(unix)]
    #[test]
    fn skips_symlinked_target_path() {
        use std::os::unix::fs::symlink;
        let root = tmpdir("sym-target");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let home = fs::canonicalize(&home).unwrap();
        let backup_root = root.join("backup");
        fs::create_dir_all(&backup_root).unwrap();
        let outside = root.join("outside");
        fs::create_dir_all(&outside).unwrap();
        let body = "leak";
        let (dir, _) = make_backup(&backup_root, &home, "demo", &[("payload.txt", body)]);
        // 白名单目录内放一个指向 home 之外的软链
        fs::create_dir_all(home.join("appdata")).unwrap();
        symlink(&outside, home.join("appdata/link")).unwrap();
        // 清单：目标 = appdata/link/leak.txt（在白名单内 → 但路径上有软链）
        let manifest = serde_json::json!({
            "schema": MANIFEST_SCHEMA,
            "appId": "demo",
            "createdAt": "2026-01-01T00:00:00Z",
            "homeHash": home_fingerprint(&home),
            "entries": [json_entry("payload.txt", "appdata/link/leak.txt", body.len() as u64, &sha256_str(body), false)],
        });
        write(
            &dir.join("manifest.json"),
            &serde_json::to_string(&manifest).unwrap(),
        );

        let out = restore_backup_in_at(
            &dir,
            "demo",
            &home,
            &wl(&home, "appdata"),
            &backup_root,
        )
        .unwrap();
        assert_eq!(out.restored, 0, "{out:?}");
        assert_eq!(out.skipped.len(), 1, "{out:?}");
        assert!(
            !outside.join("leak.txt").exists(),
            "must not write through the symlink"
        );
    }

    /// 备份源被改（sha 不符）→ skipped，原目标文件保持不变。
    #[test]
    fn skips_modified_backup_source() {
        let root = tmpdir("tampered");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let home = fs::canonicalize(&home).unwrap();
        let backup_root = root.join("backup");
        fs::create_dir_all(&backup_root).unwrap();
        write(&home.join("appdata/cfg.json"), "ORIGINAL");
        let (dir, _) = make_backup(
            &backup_root,
            &home,
            "demo",
            &[("appdata/cfg.json", "BACKED-UP")],
        );
        // 篡改备份载荷
        write(&dir.join("appdata/cfg.json"), "TAMPERED");

        let out = restore_backup_in_at(
            &dir,
            "demo",
            &home,
            &wl(&home, "appdata"),
            &backup_root,
        )
        .unwrap();
        assert_eq!(out.restored, 0, "{out:?}");
        assert_eq!(out.skipped.len(), 1, "{out:?}");
        assert_eq!(
            fs::read_to_string(home.join("appdata/cfg.json")).unwrap(),
            "ORIGINAL",
            "tampered backup must not overwrite the target"
        );
    }

    /// 备份源是软链 → skipped（绝不读链接指向的内容）。
    #[cfg(unix)]
    #[test]
    fn skips_symlinked_backup_source() {
        use std::os::unix::fs::symlink;
        let root = tmpdir("sym-source");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let home = fs::canonicalize(&home).unwrap();
        fs::create_dir_all(home.join("appdata")).unwrap();
        let backup_root = root.join("backup");
        fs::create_dir_all(&backup_root).unwrap();
        let (dir, _) = make_backup(
            &backup_root,
            &home,
            "demo",
            &[("appdata/cfg.json", "REAL")],
        );
        let secret = root.join("secret.txt");
        write(&secret, "SECRET");
        fs::remove_file(dir.join("appdata/cfg.json")).unwrap();
        symlink(&secret, dir.join("appdata/cfg.json")).unwrap();

        let out = restore_backup_in_at(
            &dir,
            "demo",
            &home,
            &wl(&home, "appdata"),
            &backup_root,
        )
        .unwrap();
        assert_eq!(out.restored, 0, "{out:?}");
        assert_eq!(out.skipped.len(), 1, "{out:?}");
        assert!(!home.join("appdata/cfg.json").exists());
    }

    /// 目标已存在 → 原文件出现在保全目录，新文件写入成功。
    #[test]
    fn preserves_existing_target_in_pre_restore_dir() {
        let root = tmpdir("preserve");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let home = fs::canonicalize(&home).unwrap();
        let backup_root = root.join("backup");
        fs::create_dir_all(&backup_root).unwrap();
        write(&home.join("appdata/cfg.json"), "OLD");
        let (dir, _) = make_backup(
            &backup_root,
            &home,
            "demo",
            &[("appdata/cfg.json", "NEW")],
        );

        let out = restore_backup_in_at(
            &dir,
            "demo",
            &home,
            &wl(&home, "appdata"),
            &backup_root,
        )
        .unwrap();
        assert_eq!(out.restored, 1, "{out:?}");
        assert_eq!(
            fs::read_to_string(home.join("appdata/cfg.json")).unwrap(),
            "NEW"
        );
        let preserved = out.preserved_dir.expect("preserved dir must be reported");
        let preserved = PathBuf::from(preserved);
        assert!(preserved.exists(), "preserved dir must exist");
        assert_eq!(
            fs::read_to_string(preserved.join("appdata/cfg.json")).unwrap(),
            "OLD",
            "the previous file must be recoverable"
        );
    }

    /// 写失败 → failed，且目标保持旧内容（不被清空）。
    #[test]
    fn reports_failure_and_keeps_old_content() {
        let root = tmpdir("write-fail");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let home = fs::canonicalize(&home).unwrap();
        let backup_root = root.join("backup");
        fs::create_dir_all(&backup_root).unwrap();
        write(&home.join("appdata/cfg.json"), "OLD");
        let (dir, _) = make_backup(
            &backup_root,
            &home,
            "demo",
            &[("appdata/cfg.json", "NEW")],
        );
        // 让临时文件路径被目录占住 → 写入必然失败（不依赖文件权限，root 也拦得住）
        let parent = home.join("appdata");
        let blocker = parent.join(format!(
            ".cfg.json.aibox-restore.{}.tmp",
            std::process::id()
        ));
        fs::create_dir_all(&blocker).unwrap();

        let out = restore_backup_in_at(
            &dir,
            "demo",
            &home,
            &wl(&home, "appdata"),
            &backup_root,
        )
        .unwrap();
        assert_eq!(out.restored, 0, "{out:?}");
        assert_eq!(out.failed.len(), 1, "{out:?}");
        assert_eq!(
            fs::read_to_string(home.join("appdata/cfg.json")).unwrap(),
            "OLD",
            "a failed restore must not lose the existing file"
        );
    }

    /// 没有清单 → RESTORE_NO_MANIFEST（拒绝，不猜路径）。
    #[test]
    fn refuses_without_manifest() {
        let root = tmpdir("no-manifest");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let home = fs::canonicalize(&home).unwrap();
        let backup_root = root.join("backup");
        fs::create_dir_all(&backup_root).unwrap();
        let dir = backup_root.join("20260101000000/demo");
        fs::create_dir_all(&dir).unwrap();
        write(&dir.join("data.zip"), "old zip");

        let err = restore_backup_in_at(
            &dir,
            "demo",
            &home,
            &wl(&home, "appdata"),
            &backup_root,
        )
        .unwrap_err();
        assert_eq!(err.code, "RESTORE_NO_MANIFEST");
        assert_eq!(
            fs::read_dir(&home).unwrap().count(),
            0,
            "must not touch home"
        );
    }

    /// 清单损坏（旧格式 / 坏 JSON）→ RESTORE_NO_MANIFEST。
    #[test]
    fn refuses_damaged_manifest() {
        let root = tmpdir("damaged");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let home = fs::canonicalize(&home).unwrap();
        let backup_root = root.join("backup");
        fs::create_dir_all(&backup_root).unwrap();
        let dir = backup_root.join("20260101000000/demo");
        fs::create_dir_all(&dir).unwrap();
        // 旧格式清单（有 entry/source、无 homeHash、schema 是旧版）
        write(
            &dir.join("manifest.json"),
            r#"{"schema":"aibox-backup/v1","appId":"demo","entries":[{"entry":"a","source":"/etc/passwd"}]}"#,
        );
        let err = restore_backup_in_at(
            &dir,
            "demo",
            &home,
            &wl(&home, "appdata"),
            &backup_root,
        )
        .unwrap_err();
        assert_eq!(err.code, "RESTORE_NO_MANIFEST");

        // 坏 JSON
        write(&dir.join("manifest.json"), "{not json");
        let err = restore_backup_in_at(
            &dir,
            "demo",
            &home,
            &wl(&home, "appdata"),
            &backup_root,
        )
        .unwrap_err();
        assert_eq!(err.code, "RESTORE_NO_MANIFEST");
    }

    /// 清单缺 schema → 拒绝（格式必须显式匹配）。
    #[test]
    fn refuses_manifest_without_schema() {
        let root = tmpdir("no-schema");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let home = fs::canonicalize(&home).unwrap();
        let backup_root = root.join("backup");
        fs::create_dir_all(&backup_root).unwrap();
        let dir = backup_root.join("20260101000000/demo");
        fs::create_dir_all(&dir).unwrap();
        let body = "x";
        write(&dir.join("payload.txt"), body);
        let manifest = serde_json::json!({
            "appId": "demo",
            "homeHash": home_fingerprint(&home),
            "entries": [json_entry("payload.txt", "appdata/x.txt", 1, &sha256_str(body), false)],
        });
        write(
            &dir.join("manifest.json"),
            &serde_json::to_string(&manifest).unwrap(),
        );
        let err = restore_backup_in_at(
            &dir,
            "demo",
            &home,
            &wl(&home, "appdata"),
            &backup_root,
        )
        .unwrap_err();
        assert_eq!(err.code, "RESTORE_NO_MANIFEST");
        assert_eq!(fs::read_dir(&home).unwrap().count(), 0);
    }

    /// 文件条目缺 size → 清单损坏（不默认 0）。
    #[test]
    fn refuses_file_entry_without_size() {
        let root = tmpdir("no-size");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let home = fs::canonicalize(&home).unwrap();
        let backup_root = root.join("backup");
        fs::create_dir_all(&backup_root).unwrap();
        let dir = backup_root.join("20260101000000/demo");
        fs::create_dir_all(&dir).unwrap();
        let body = "x";
        write(&dir.join("payload.txt"), body);
        let manifest = serde_json::json!({
            "schema": MANIFEST_SCHEMA,
            "appId": "demo",
            "homeHash": home_fingerprint(&home),
            "entries": [{
                "rel": "payload.txt",
                "target": "appdata/x.txt",
                "sha256": sha256_str(body),
                "isDir": false,
            }],
        });
        write(
            &dir.join("manifest.json"),
            &serde_json::to_string(&manifest).unwrap(),
        );
        let err = restore_backup_in_at(
            &dir,
            "demo",
            &home,
            &wl(&home, "appdata"),
            &backup_root,
        )
        .unwrap_err();
        assert_eq!(err.code, "RESTORE_NO_MANIFEST");
    }

    /// homeHash 不符 → RESTORE_HOME_MISMATCH（拒绝，不写到别的 home）。
    #[test]
    fn refuses_home_mismatch() {
        let root = tmpdir("home-mismatch");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let home = fs::canonicalize(&home).unwrap();
        let backup_root = root.join("backup");
        fs::create_dir_all(&backup_root).unwrap();
        let dir = backup_root.join("20260101000000/demo");
        fs::create_dir_all(&dir).unwrap();
        let body = "x";
        write(&dir.join("payload.txt"), body);
        let manifest = serde_json::json!({
            "schema": MANIFEST_SCHEMA,
            "appId": "demo",
            "createdAt": "2026-01-01T00:00:00Z",
            "homeHash": sha256_str("some-other-home"),
            "entries": [json_entry("payload.txt", "appdata/x.txt", 1, &sha256_str(body), false)],
        });
        write(
            &dir.join("manifest.json"),
            &serde_json::to_string(&manifest).unwrap(),
        );

        let err = restore_backup_in_at(
            &dir,
            "demo",
            &home,
            &wl(&home, "appdata"),
            &backup_root,
        )
        .unwrap_err();
        assert_eq!(err.code, "RESTORE_HOME_MISMATCH");
        assert_eq!(
            fs::read_dir(&home).unwrap().count(),
            0,
            "must not touch home"
        );
    }

    /// 超限额 → RESTORE_TOO_LARGE（拒绝，且未写任何文件）。
    #[test]
    fn refuses_oversized_backup() {
        let root = tmpdir("too-large");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let home = fs::canonicalize(&home).unwrap();
        let backup_root = root.join("backup");
        fs::create_dir_all(&backup_root).unwrap();
        let dir = backup_root.join("20260101000000/demo");
        fs::create_dir_all(&dir).unwrap();
        // 文件数超限
        let mut entries = Vec::new();
        for i in 0..(MAX_RESTORE_FILES + 1) {
            entries.push(json_entry(
                &format!("f{i}.txt"),
                &format!("f{i}.txt"),
                1,
                &sha256_str("x"),
                false,
            ));
        }
        let manifest = serde_json::json!({
            "schema": MANIFEST_SCHEMA,
            "appId": "demo",
            "createdAt": "2026-01-01T00:00:00Z",
            "homeHash": home_fingerprint(&home),
            "entries": entries,
        });
        write(
            &dir.join("manifest.json"),
            &serde_json::to_string(&manifest).unwrap(),
        );
        let err = restore_backup_in_at(
            &dir,
            "demo",
            &home,
            &wl(&home, "appdata"),
            &backup_root,
        )
        .unwrap_err();
        assert_eq!(err.code, "RESTORE_TOO_LARGE");
        assert_eq!(
            fs::read_dir(&home).unwrap().count(),
            0,
            "must not touch home"
        );

        // 字节数超限
        let manifest = serde_json::json!({
            "schema": MANIFEST_SCHEMA,
            "appId": "demo",
            "createdAt": "2026-01-01T00:00:00Z",
            "homeHash": home_fingerprint(&home),
            "entries": [json_entry("big.bin", "appdata/big.bin", MAX_RESTORE_BYTES + 1, &sha256_str("x"), false)],
        });
        write(
            &dir.join("manifest.json"),
            &serde_json::to_string(&manifest).unwrap(),
        );
        let err = restore_backup_in_at(
            &dir,
            "demo",
            &home,
            &wl(&home, "appdata"),
            &backup_root,
        )
        .unwrap_err();
        assert_eq!(err.code, "RESTORE_TOO_LARGE");
        assert_eq!(
            fs::read_dir(&home).unwrap().count(),
            0,
            "must not touch home"
        );
    }

    /// 清单属于别的应用 → RESTORE_APP_MISMATCH。
    #[test]
    fn refuses_manifest_of_another_app() {
        let root = tmpdir("app-mismatch");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let home = fs::canonicalize(&home).unwrap();
        let backup_root = root.join("backup");
        fs::create_dir_all(&backup_root).unwrap();
        let (dir, _) = make_backup(&backup_root, &home, "demo", &[("a.txt", "a")]);
        let err = restore_backup_in_at(
            &dir,
            "other",
            &home,
            &wl(&home, "appdata"),
            &backup_root,
        )
        .unwrap_err();
        assert_eq!(err.code, "RESTORE_APP_MISMATCH");
    }

    /// 单条失败不中断其余（好的条目照旧还原）。
    #[test]
    fn one_bad_entry_does_not_stop_the_rest() {
        let root = tmpdir("partial");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let home = fs::canonicalize(&home).unwrap();
        fs::create_dir_all(home.join("appdata")).unwrap();
        let backup_root = root.join("backup");
        fs::create_dir_all(&backup_root).unwrap();
        let dir = backup_root.join("20260101000000/demo");
        fs::create_dir_all(&dir).unwrap();
        let good = "GOOD";
        write(&dir.join("good.txt"), good);
        let manifest = serde_json::json!({
            "schema": MANIFEST_SCHEMA,
            "appId": "demo",
            "createdAt": "2026-01-01T00:00:00Z",
            "homeHash": home_fingerprint(&home),
            "entries": [
                json_entry("good.txt", "appdata/good.txt", good.len() as u64, &sha256_str(good), false),
                json_entry("missing.txt", "appdata/missing.txt", 3, &sha256_str("abc"), false),
            ],
        });
        write(
            &dir.join("manifest.json"),
            &serde_json::to_string(&manifest).unwrap(),
        );

        let out = restore_backup_in_at(
            &dir,
            "demo",
            &home,
            &wl(&home, "appdata"),
            &backup_root,
        )
        .unwrap();
        assert_eq!(out.restored, 1, "{out:?}");
        assert_eq!(out.failed.len(), 1, "{out:?}");
        assert_eq!(
            fs::read_to_string(home.join("appdata/good.txt")).unwrap(),
            good
        );
    }

    /// 端到端：备份 → 还原（清单与还原实现对得上；原路径回来，内容一致）。
    #[test]
    fn backup_then_restore_roundtrip() {
        let root = tmpdir("roundtrip");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let home = fs::canonicalize(&home).unwrap();
        write(&home.join("appdata/cfg.json"), "{\"a\":1}");
        write(&home.join("appdata/sub/log.txt"), "hello");

        let backup_root = root.join("backup");
        fs::create_dir_all(&backup_root).unwrap();
        let out = crate::manage::backup_app_in(
            "demo",
            &[home.join("appdata").to_string_lossy().into_owned()],
            &backup_root,
            &home,
        )
        .unwrap();
        assert!(out.failed.is_empty(), "{out:?}");
        assert_eq!(out.files.len(), 2, "{out:?}");

        // 数据内容被改坏（模拟升级写坏了配置）→ 还原回原样
        write(&home.join("appdata/cfg.json"), "BROKEN");
        fs::remove_file(home.join("appdata/sub/log.txt")).unwrap();

        let r = restore_backup_in_at(
            Path::new(&out.backup_dir),
            "demo",
            &home,
            &wl(&home, "appdata"),
            &backup_root,
        )
        .unwrap();
        assert_eq!(r.restored, 2, "{r:?}");
        assert!(r.failed.is_empty(), "{r:?}");
        assert!(r.skipped.is_empty(), "{r:?}");
        assert_eq!(
            fs::read_to_string(home.join("appdata/cfg.json")).unwrap(),
            "{\"a\":1}"
        );
        assert_eq!(
            fs::read_to_string(home.join("appdata/sub/log.txt")).unwrap(),
            "hello"
        );
    }

    /// 数据目录**已被删除**（卸载后）→ 白名单类型未知 → 整体拒绝（什么都没改动），
    /// 备份仍在备份库里供人工取回。
    #[test]
    fn roundtrip_refuses_when_data_dir_vanished() {
        let root = tmpdir("roundtrip-gone");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let home = fs::canonicalize(&home).unwrap();
        write(&home.join("appdata/cfg.json"), "{\"a\":1}");

        let backup_root = root.join("backup");
        fs::create_dir_all(&backup_root).unwrap();
        let out = crate::manage::backup_app_in(
            "demo",
            &[home.join("appdata").to_string_lossy().into_owned()],
            &backup_root,
            &home,
        )
        .unwrap();
        assert_eq!(out.files.len(), 1, "{out:?}");

        // 应用已卸载 → 数据目录没了
        fs::remove_dir_all(home.join("appdata")).unwrap();

        let err = restore_backup_in_at(
            Path::new(&out.backup_dir),
            "demo",
            &home,
            &wl(&home, "appdata"),
            &backup_root,
        )
        .unwrap_err();
        assert_eq!(err.code, "RESTORE_TARGET_NOT_ALLOWED", "{err:?}");
        assert_eq!(fs::read_dir(&home).unwrap().count(), 0, "must not touch home");
        // 备份仍在（人工取回的入口）
        assert!(Path::new(&out.backup_dir).join("manifest.json").exists());
    }

    /// 目录条目：缺失则创建（先建目录再写文件）。
    #[test]
    fn creates_missing_directories() {
        let root = tmpdir("dirs");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let home = fs::canonicalize(&home).unwrap();
        // 白名单目录存在（升级流程的真实情形）；清单里的子目录尚未存在 → 还原时重建
        fs::create_dir_all(home.join("appdata")).unwrap();
        let backup_root = root.join("backup");
        fs::create_dir_all(&backup_root).unwrap();
        let dir = backup_root.join("20260101000000/demo");
        fs::create_dir_all(&dir).unwrap();
        let body = "x";
        write(&dir.join("payload.txt"), body);
        let manifest = serde_json::json!({
            "schema": MANIFEST_SCHEMA,
            "appId": "demo",
            "createdAt": "2026-01-01T00:00:00Z",
            "homeHash": home_fingerprint(&home),
            "entries": [
                json_entry("appdata", "appdata", 0, "", true),
                json_entry("payload.txt", "appdata/x.txt", 1, &sha256_str(body), false),
            ],
        });
        write(
            &dir.join("manifest.json"),
            &serde_json::to_string(&manifest).unwrap(),
        );

        let out = restore_backup_in_at(
            &dir,
            "demo",
            &home,
            &wl(&home, "appdata"),
            &backup_root,
        )
        .unwrap();
        assert_eq!(out.restored, 1, "{out:?}");
        assert!(home.join("appdata").is_dir());
        assert_eq!(
            fs::read_to_string(home.join("appdata/x.txt")).unwrap(),
            body
        );
    }

    // ── 审计 P0-1：白名单 ────────────────────────────────────────────────

    /// **P0-1 PoC**：篡改清单把 target 指向 `~/.ssh/authorized_keys` →
    /// 整体拒绝（RESTORE_TARGET_NOT_ALLOWED），原文件字节不变，home 内无新增/修改。
    #[test]
    fn refuses_tampered_target_outside_whitelist() {
        let root = tmpdir("poc-target");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let home = fs::canonicalize(&home).unwrap();
        let backup_root = root.join("backup");
        fs::create_dir_all(&backup_root).unwrap();
        // 用户真实的 ssh 公钥
        let ssh = home.join(".ssh");
        fs::create_dir_all(&ssh).unwrap();
        write(&ssh.join("authorized_keys"), "ssh-rsa REAL-KEY\n");
        let before = fs::read_to_string(ssh.join("authorized_keys")).unwrap();

        // 攻击者把备份载荷和清单都改成指向 authorized_keys
        let dir = backup_root.join("20260101000000/demo");
        fs::create_dir_all(&dir).unwrap();
        let evil = "ssh-rsa ATTACKER-KEY\n";
        write(&dir.join("payload.txt"), evil);
        let manifest = serde_json::json!({
            "schema": MANIFEST_SCHEMA,
            "appId": "demo",
            "createdAt": "2026-01-01T00:00:00Z",
            "homeHash": home_fingerprint(&home),
            "entries": [json_entry("payload.txt", ".ssh/authorized_keys", evil.len() as u64, &sha256_str(evil), false)],
        });
        write(
            &dir.join("manifest.json"),
            &serde_json::to_string(&manifest).unwrap(),
        );

        let err = restore_backup_in_at(
            &dir,
            "demo",
            &home,
            &wl(&home, "appdata"),
            &backup_root,
        )
        .unwrap_err();
        assert_eq!(err.code, "RESTORE_TARGET_NOT_ALLOWED", "{err:?}");
        assert_eq!(
            fs::read_to_string(ssh.join("authorized_keys")).unwrap(),
            before,
            "the real key file must be byte-identical"
        );
        // home 内没有新增/修改文件（除攻击者本来就有的 .ssh/authorized_keys）
        let mut names: Vec<String> = fs::read_dir(&home)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, vec![".ssh".to_string()], "{names:?}");
        assert_eq!(
            fs::read_dir(ssh).unwrap().count(),
            1,
            "no extra file may appear next to the key"
        );
    }

    /// 白名单为空 → RESTORE_NO_WHITELIST，不读不写。
    #[test]
    fn refuses_without_whitelist() {
        let root = tmpdir("no-wl");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let home = fs::canonicalize(&home).unwrap();
        let backup_root = root.join("backup");
        fs::create_dir_all(&backup_root).unwrap();
        let (dir, _) = make_backup(&backup_root, &home, "demo", &[("appdata/x.txt", "x")]);
        write(&home.join("keep.txt"), "keep");

        let err = restore_backup_in_at(&dir, "demo", &home, &[], &backup_root).unwrap_err();
        assert_eq!(err.code, "RESTORE_NO_WHITELIST", "{err:?}");
        assert_eq!(fs::read_dir(&home).unwrap().count(), 1, "must not touch home");
        assert_eq!(fs::read_to_string(home.join("keep.txt")).unwrap(), "keep");
    }

    /// 白名单全在 home 之外 → 同样拒绝（没有可用白名单）。
    #[test]
    fn refuses_whitelist_outside_home() {
        let root = tmpdir("wl-outside");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let home = fs::canonicalize(&home).unwrap();
        let backup_root = root.join("backup");
        fs::create_dir_all(&backup_root).unwrap();
        let (dir, _) = make_backup(&backup_root, &home, "demo", &[("appdata/x.txt", "x")]);

        let outside = root.join("outside");
        fs::create_dir_all(&outside).unwrap();
        let err = restore_backup_in_at(
            &dir,
            "demo",
            &home,
            &[outside.to_string_lossy().into_owned()],
            &backup_root,
        )
        .unwrap_err();
        assert_eq!(err.code, "RESTORE_NO_WHITELIST", "{err:?}");
        assert_eq!(fs::read_dir(&home).unwrap().count(), 0);
    }

    /// 白名单是**文件**时 → 只允许精确命中（其下的路径不算）。
    #[test]
    fn whitelist_file_entry_allows_exact_match_only() {
        let root = tmpdir("wl-file");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let home = fs::canonicalize(&home).unwrap();
        let backup_root = root.join("backup");
        fs::create_dir_all(&backup_root).unwrap();
        write(&home.join("cfg.json"), "OLD");

        let dir = backup_root.join("20260101000000/demo");
        fs::create_dir_all(&dir).unwrap();
        let body = "NEW";
        write(&dir.join("payload.txt"), body);
        let manifest = serde_json::json!({
            "schema": MANIFEST_SCHEMA,
            "appId": "demo",
            "homeHash": home_fingerprint(&home),
            "entries": [
                json_entry("payload.txt", "cfg.json", body.len() as u64, &sha256_str(body), false),
                json_entry("payload.txt", "cfg.json/evil.txt", body.len() as u64, &sha256_str(body), false),
            ],
        });
        write(
            &dir.join("manifest.json"),
            &serde_json::to_string(&manifest).unwrap(),
        );

        // 清单里有一条落在「文件型白名单」之下 → 整体拒绝
        let err = restore_backup_in_at(
            &dir,
            "demo",
            &home,
            &wl(&home, "cfg.json"),
            &backup_root,
        )
        .unwrap_err();
        assert_eq!(err.code, "RESTORE_TARGET_NOT_ALLOWED", "{err:?}");
        assert_eq!(
            fs::read_to_string(home.join("cfg.json")).unwrap(),
            "OLD",
            "the file must be untouched"
        );
    }

    /// 生产形态的白名单（registry 的 `~/...` 写法）→ 同样生效。
    #[test]
    fn whitelist_accepts_tilde_paths() {
        let root = tmpdir("wl-tilde");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let home = fs::canonicalize(&home).unwrap();
        // 数据目录存在 → 白名单类型 = 目录
        fs::create_dir_all(home.join(".config/openclaw")).unwrap();
        let backup_root = root.join("backup");
        fs::create_dir_all(&backup_root).unwrap();
        let (dir, _) = make_backup(
            &backup_root,
            &home,
            "demo",
            &[(".config/openclaw/cfg.json", "{}")],
        );

        let out = restore_backup_in_at(
            &dir,
            "demo",
            &home,
            &["~/.config/openclaw".to_string()],
            &backup_root,
        )
        .unwrap();
        assert_eq!(out.restored, 1, "{out:?}");
        assert_eq!(
            fs::read_to_string(home.join(".config/openclaw/cfg.json")).unwrap(),
            "{}"
        );
    }

    /// 卸载后数据目录已不存在（白名单路径不存在）→ **类型未知** → 只允许精确命中，
    /// 其下路径一律拒绝（审计 P2-3：绝不据清单自述在数据文件路径上建目录树）。
    #[test]
    fn whitelist_dir_absent_allows_exact_match_only() {
        let root = tmpdir("wl-absent");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let home = fs::canonicalize(&home).unwrap();
        let backup_root = root.join("backup");
        fs::create_dir_all(&backup_root).unwrap();
        let (dir, _) = make_backup(
            &backup_root,
            &home,
            "demo",
            &[("appdata/cfg.json", "{\"a\":1}")],
        );
        // 应用已卸载 → 数据目录没了（白名单条目类型未知）
        assert!(!home.join("appdata").exists());

        // 其下的路径 → 整体拒绝（什么都没改动）
        let err = restore_backup_in_at(
            &dir,
            "demo",
            &home,
            &wl(&home, "appdata"),
            &backup_root,
        )
        .unwrap_err();
        assert_eq!(err.code, "RESTORE_TARGET_NOT_ALLOWED", "{err:?}");
        assert_eq!(fs::read_dir(&home).unwrap().count(), 0, "must not touch home");
    }

    // ── 审计 P1-1：白名单 = home 根 → 拒绝（否则放开整个 home）─────────

    /// **P1-1 PoC（还原侧）**：白名单是 `~` / `~/` / home 绝对路径 → 整体拒绝
    /// `RESTORE_NO_WHITELIST`，**home 一字未动**（`~/.ssh/authorized_keys` 不会被覆写）。
    #[test]
    fn refuses_whitelist_equal_to_home_root() {
        for spelling in ["~", "~/", "~/.", "\u{0}"] {
            let root = tmpdir("wl-home");
            let home = root.join("home");
            fs::create_dir_all(&home).unwrap();
            let home = fs::canonicalize(&home).unwrap();
            let backup_root = root.join("backup");
            fs::create_dir_all(&backup_root).unwrap();
            // 用户真实的 ssh 公钥
            fs::create_dir_all(home.join(".ssh")).unwrap();
            write(&home.join(".ssh/authorized_keys"), "ssh-rsa REAL-KEY\n");
            // 篡改清单：把载荷写到 authorized_keys
            let dir = backup_root.join("20260101000000/demo");
            fs::create_dir_all(&dir).unwrap();
            let evil = "ssh-rsa ATTACKER-KEY\n";
            write(&dir.join("payload.txt"), evil);
            let manifest = serde_json::json!({
                "schema": MANIFEST_SCHEMA,
                "appId": "demo",
                "createdAt": "2026-01-01T00:00:00Z",
                "homeHash": home_fingerprint(&home),
                "entries": [json_entry("payload.txt", ".ssh/authorized_keys", evil.len() as u64, &sha256_str(evil), false)],
            });
            write(
                &dir.join("manifest.json"),
                &serde_json::to_string(&manifest).unwrap(),
            );

            let raw = if spelling == "\u{0}" {
                home.to_string_lossy().into_owned()
            } else {
                spelling.to_string()
            };
            let err = restore_backup_in_at(&dir, "demo", &home, &[raw.clone()], &backup_root)
                .unwrap_err();
            assert_eq!(err.code, "RESTORE_NO_WHITELIST", "{raw}: {err:?}");
            assert_eq!(
                fs::read_to_string(home.join(".ssh/authorized_keys")).unwrap(),
                "ssh-rsa REAL-KEY\n",
                "the real key file must be byte-identical ({raw})"
            );
            assert_eq!(
                fs::read_dir(&home).unwrap().count(),
                1,
                "nothing may be written into home ({raw})"
            );
        }
    }

    /// **P1-1（续）**：白名单里混进 home 根时，**只丢那一条**，
    /// 其余合法条目照常生效（但 home 根依然被拒绝）。
    #[test]
    fn home_root_entry_is_dropped_but_others_still_work() {
        let root = tmpdir("wl-home-mixed");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let home = fs::canonicalize(&home).unwrap();
        fs::create_dir_all(home.join("appdata")).unwrap();
        let backup_root = root.join("backup");
        fs::create_dir_all(&backup_root).unwrap();
        let dir = backup_root.join("20260101000000/demo");
        fs::create_dir_all(&dir).unwrap();
        let body = "OK";
        write(&dir.join("payload.txt"), body);
        let manifest = serde_json::json!({
            "schema": MANIFEST_SCHEMA,
            "appId": "demo",
            "homeHash": home_fingerprint(&home),
            "entries": [json_entry("payload.txt", "appdata/x.txt", body.len() as u64, &sha256_str(body), false)],
        });
        write(
            &dir.join("manifest.json"),
            &serde_json::to_string(&manifest).unwrap(),
        );

        let out = restore_backup_in_at(
            &dir,
            "demo",
            &home,
            &[
                "~".to_string(),
                home.join("appdata").to_string_lossy().into_owned(),
            ],
            &backup_root,
        )
        .unwrap();
        assert_eq!(out.restored, 1, "{out:?}");
        assert_eq!(fs::read_to_string(home.join("appdata/x.txt")).unwrap(), body);
    }

    /// **P1-1（备份侧）**：白名单 = home 根 → 该 dataPath 被 **refused**，
    /// 且**整个 home 都没被拷进备份**（哨兵文件不在备份里）。
    #[test]
    fn backup_refuses_home_root_and_copies_nothing() {
        let root = tmpdir("bk-home-root");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let home = fs::canonicalize(&home).unwrap();
        write(&home.join(".ssh/authorized_keys"), "ssh-rsa REAL-KEY\n");
        write(&home.join("sentinel.txt"), "SENTINEL");
        let backup_root = root.join("backup");
        fs::create_dir_all(&backup_root).unwrap();

        let out = crate::manage::backup_app_in("demo", &["~".to_string()], &backup_root, &home)
            .unwrap();
        assert_eq!(out.refused.len(), 1, "{out:?}");
        assert!(out.files.is_empty(), "{out:?}");
        assert_eq!(out.backup_dir, "", "nothing must be backed up");
        // 备份库里不得出现 home 里的哨兵文件
        let mut leaked = Vec::new();
        for name in walk(&backup_root) {
            if name.contains("authorized_keys") || name.contains("sentinel") {
                leaked.push(name);
            }
        }
        assert!(leaked.is_empty(), "home must not be copied: {leaked:?}");
        // 拒绝 = 备份不完整 → 破坏性操作（卸载）必须中止
        let err = crate::manage::backup_incomplete_error(&out).expect("must block destructive actions");
        assert_eq!(err.code, "MANAGE_BACKUP_INCOMPLETE");
    }

    /// 递归列出目录下的所有文件名（测试用）。
    fn walk(dir: &Path) -> Vec<String> {
        let mut out = Vec::new();
        let Ok(rd) = fs::read_dir(dir) else {
            return out;
        };
        for e in rd.flatten() {
            let p = e.path();
            out.push(p.file_name().unwrap_or_default().to_string_lossy().into_owned());
            if p.is_dir() {
                out.extend(walk(&p));
            }
        }
        out
    }

    // ── 审计 P1-3：备份库 / 保全目录私有（0700 / 0600）─────────────────

    /// **P1-3**：备份根 0700、备份载荷 0600、保全目录 0700、留痕 0600。
    #[cfg(unix)]
    #[test]
    fn backup_and_preservation_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let root = tmpdir("perm");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let home = fs::canonicalize(&home).unwrap();
        write(&home.join("appdata/secret.json"), "{\"apiKey\":\"SECRET\"}");
        let backup_root = root.join("backup");
        fs::create_dir_all(&backup_root).unwrap();

        let out = crate::manage::backup_app_in(
            "demo",
            &[home.join("appdata").to_string_lossy().into_owned()],
            &backup_root,
            &home,
        )
        .unwrap();
        assert!(out.failed.is_empty(), "{out:?}");

        // 备份根 0700
        let root_mode = fs::metadata(&backup_root).unwrap().permissions().mode() & 0o777;
        assert_eq!(root_mode, 0o700, "backup root must be 0700");
        // 备份载荷 0600
        let payload = Path::new(&out.backup_dir).join("appdata/secret.json");
        let mode = fs::metadata(&payload).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "backup payload must be 0600");
        // 清单 0600（里面有路径与摘要）
        let m = fs::metadata(Path::new(&out.backup_dir).join("manifest.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(m, 0o600, "manifest must be 0600");

        // 还原时覆盖旧文件 → 产生保全目录（0700）+ 留痕（0600）
        let r = restore_backup_in_at(
            Path::new(&out.backup_dir),
            "demo",
            &home,
            &[home.join("appdata").to_string_lossy().into_owned()],
            &backup_root,
        )
        .unwrap();
        assert_eq!(r.restored, 1, "{r:?}");
        let preserved = PathBuf::from(r.preserved_dir.expect("preserved dir"));
        let pmode = fs::metadata(&preserved).unwrap().permissions().mode() & 0o777;
        assert_eq!(pmode, 0o700, "preservation dir must be 0700");
        let journal: Vec<PathBuf> = fs::read_dir(preserved.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| {
                p.file_name()
                    .map(|n| n.to_string_lossy().starts_with("restore-journal-"))
                    .unwrap_or(false)
            })
            .collect();
        assert_eq!(journal.len(), 1);
        let jmode = fs::metadata(&journal[0]).unwrap().permissions().mode() & 0o777;
        assert_eq!(jmode, 0o600, "journal must be 0600");
    }

    // ── 审计 P2-3：白名单类型显式（不存在 → 只允许精确命中）────────────

    /// **P2-3**：白名单是不存在的**文件** → 其下的路径**拒绝**；精确命中**允许**。
    #[test]
    fn whitelist_absent_file_allows_exact_match_only() {
        let root = tmpdir("wl-absent-file");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let home = fs::canonicalize(&home).unwrap();
        let backup_root = root.join("backup");
        fs::create_dir_all(&backup_root).unwrap();
        let dir = backup_root.join("20260101000000/demo");
        fs::create_dir_all(&dir).unwrap();
        let body = "NEW";
        write(&dir.join("payload.txt"), body);

        // ① target 落在白名单路径之下 → 整体拒绝
        let manifest = serde_json::json!({
            "schema": MANIFEST_SCHEMA,
            "appId": "demo",
            "homeHash": home_fingerprint(&home),
            "entries": [json_entry("payload.txt", "cfg.json/evil.txt", body.len() as u64, &sha256_str(body), false)],
        });
        write(
            &dir.join("manifest.json"),
            &serde_json::to_string(&manifest).unwrap(),
        );
        let err = restore_backup_in_at(
            &dir,
            "demo",
            &home,
            &wl(&home, "cfg.json"),
            &backup_root,
        )
        .unwrap_err();
        assert_eq!(err.code, "RESTORE_TARGET_NOT_ALLOWED", "{err:?}");
        assert!(!home.join("cfg.json").exists(), "must not touch home");

        // ② target 精确等于白名单路径 → 允许（写成一个文件）
        let manifest = serde_json::json!({
            "schema": MANIFEST_SCHEMA,
            "appId": "demo",
            "homeHash": home_fingerprint(&home),
            "entries": [json_entry("payload.txt", "cfg.json", body.len() as u64, &sha256_str(body), false)],
        });
        write(
            &dir.join("manifest.json"),
            &serde_json::to_string(&manifest).unwrap(),
        );
        let out = restore_backup_in_at(
            &dir,
            "demo",
            &home,
            &wl(&home, "cfg.json"),
            &backup_root,
        )
        .unwrap();
        assert_eq!(out.restored, 1, "{out:?}");
        assert_eq!(fs::read_to_string(home.join("cfg.json")).unwrap(), body);
        // 不许把数据文件路径建成目录
        assert!(home.join("cfg.json").is_file());
    }

    /// **P2-3（续）**：白名单路径不存在时，清单不得把它变成**目录**。
    #[test]
    fn whitelist_absent_path_refuses_directory_entry() {
        let root = tmpdir("wl-absent-dir");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let home = fs::canonicalize(&home).unwrap();
        let backup_root = root.join("backup");
        fs::create_dir_all(&backup_root).unwrap();
        let dir = backup_root.join("20260101000000/demo");
        fs::create_dir_all(&dir).unwrap();
        let manifest = serde_json::json!({
            "schema": MANIFEST_SCHEMA,
            "appId": "demo",
            "homeHash": home_fingerprint(&home),
            "entries": [json_entry("appdata", "cfg.json", 0, "", true)],
        });
        write(
            &dir.join("manifest.json"),
            &serde_json::to_string(&manifest).unwrap(),
        );
        let err = restore_backup_in_at(
            &dir,
            "demo",
            &home,
            &wl(&home, "cfg.json"),
            &backup_root,
        )
        .unwrap_err();
        assert_eq!(err.code, "RESTORE_TARGET_NOT_ALLOWED", "{err:?}");
        assert!(!home.join("cfg.json").exists(), "must not create a folder there");
    }

    // ── 审计 P2-4：还原目标不得落在备份库内 ────────────────────────────

    /// **P2-4**：白名单含 `~/.aibox`（备份库的父目录）→ 备份库本身**不可被写入**。
    #[test]
    fn refuses_writing_into_the_backup_library() {
        let root = tmpdir("wl-backup-lib");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let home = fs::canonicalize(&home).unwrap();
        // 备份库在 home 之内（生产的真实布局：~/.aibox/backup）
        let backup_root = home.join(".aibox/backup");
        fs::create_dir_all(&backup_root).unwrap();
        let dir = backup_root.join("20260101000000/demo");
        fs::create_dir_all(&dir).unwrap();
        let evil = "pwn";
        write(&dir.join("payload.txt"), evil);
        let manifest = serde_json::json!({
            "schema": MANIFEST_SCHEMA,
            "appId": "demo",
            "homeHash": home_fingerprint(&home),
            "entries": [json_entry("payload.txt", ".aibox/backup/evil.txt", evil.len() as u64, &sha256_str(evil), false)],
        });
        write(
            &dir.join("manifest.json"),
            &serde_json::to_string(&manifest).unwrap(),
        );

        let err = restore_backup_in_at(
            &dir,
            "demo",
            &home,
            &[home.join(".aibox").to_string_lossy().into_owned()],
            &backup_root,
        )
        .unwrap_err();
        assert_eq!(err.code, "RESTORE_TARGET_NOT_ALLOWED", "{err:?}");
        assert!(
            !backup_root.join("evil.txt").exists(),
            "must not write into the backup library"
        );
    }

    // ── 审计 P2-5：保全目录必须唯一（同名 >1000 也不混写）──────────────

    /// **P2-5**：同名保全目录超过 1000 个 → 仍必须**新建一个唯一目录**（不混写）。
    #[test]
    fn preservation_dir_stays_unique_beyond_1000_collisions() {
        let root = tmpdir("uniq");
        let base = "pre-restore-20260101000000";
        // 预置 1000 个同名目录（序号位全占满）
        fs::create_dir_all(root.join(base)).unwrap();
        for i in 2..=1000 {
            fs::create_dir_all(root.join(format!("{base}-{i}"))).unwrap();
        }
        let got = unique_dir(&root, base).expect("must still find a unique dir");
        assert!(got.exists(), "the returned dir must exist");
        assert_eq!(fs::read_dir(&got).unwrap().count(), 0, "must not mix old content");
        // 再取一次 → 又是一个新的唯一目录
        let again = unique_dir(&root, base).expect("must still find a unique dir");
        assert_ne!(again, got);
    }

    // ── 审计 P0-2：备份目录必须在备份根内 ───────────────────────────────

    /// backup_dir 指向备份根之外 → RESTORE_BAD_BACKUP_DIR，不读不写。
    #[test]
    fn refuses_backup_dir_outside_backup_root() {
        let root = tmpdir("poc-dir");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let home = fs::canonicalize(&home).unwrap();
        let backup_root = root.join("backup");
        fs::create_dir_all(&backup_root).unwrap();
        // 攻击者把「备份」放在 home 之外（或备份根之外的子目录）
        let evil = root.join("evil-backup/20260101000000/demo");
        fs::create_dir_all(&evil).unwrap();
        let body = "x";
        write(&evil.join("payload.txt"), body);
        let manifest = serde_json::json!({
            "schema": MANIFEST_SCHEMA,
            "appId": "demo",
            "homeHash": home_fingerprint(&home),
            "entries": [json_entry("payload.txt", "appdata/x.txt", 1, &sha256_str(body), false)],
        });
        write(
            &evil.join("manifest.json"),
            &serde_json::to_string(&manifest).unwrap(),
        );
        write(&home.join("keep.txt"), "keep");

        let err = restore_backup_in_at(
            &evil,
            "demo",
            &home,
            &wl(&home, "appdata"),
            &backup_root,
        )
        .unwrap_err();
        assert_eq!(err.code, "RESTORE_BAD_BACKUP_DIR", "{err:?}");
        assert_eq!(fs::read_dir(&home).unwrap().count(), 1, "must not touch home");
        assert_eq!(fs::read_to_string(home.join("keep.txt")).unwrap(), "keep");
    }

    /// 保全目录必须落在备份根内（不写到用户数据目录里）。
    #[test]
    fn preservation_lives_inside_backup_root() {
        let root = tmpdir("preserve-root");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let home = fs::canonicalize(&home).unwrap();
        let backup_root = root.join("backup");
        fs::create_dir_all(&backup_root).unwrap();
        write(&home.join("appdata/cfg.json"), "OLD");
        let (dir, _) = make_backup(
            &backup_root,
            &home,
            "demo",
            &[("appdata/cfg.json", "NEW")],
        );

        let out = restore_backup_in_at(
            &dir,
            "demo",
            &home,
            &wl(&home, "appdata"),
            &backup_root,
        )
        .unwrap();
        let preserved = PathBuf::from(out.preserved_dir.expect("preserved dir"));
        assert!(
            preserved.starts_with(&backup_root),
            "preservation must stay inside the backup root: {}",
            preserved.display()
        );
        assert!(
            !preserved.starts_with(&home.join("appdata")),
            "preservation must not land in the user data directory: {}",
            preserved.display()
        );
        assert_eq!(
            fs::read_to_string(preserved.join("appdata/cfg.json")).unwrap(),
            "OLD"
        );
    }

    // ── 审计 P1-3：一次还原只有一个保全目录 ─────────────────────────────

    /// 多文件被覆盖 → **只有一个**保全目录，里面能看到全部旧文件；preservedDir 指向它。
    #[test]
    fn one_preservation_dir_for_all_overwritten_files() {
        let root = tmpdir("one-preserve");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let home = fs::canonicalize(&home).unwrap();
        let backup_root = root.join("backup");
        fs::create_dir_all(&backup_root).unwrap();
        write(&home.join("appdata/a.txt"), "OLD-A");
        write(&home.join("appdata/sub/b.txt"), "OLD-B");
        let (dir, _) = make_backup(
            &backup_root,
            &home,
            "demo",
            &[("appdata/a.txt", "NEW-A"), ("appdata/sub/b.txt", "NEW-B")],
        );

        let out = restore_backup_in_at(
            &dir,
            "demo",
            &home,
            &wl(&home, "appdata"),
            &backup_root,
        )
        .unwrap();
        assert_eq!(out.restored, 2, "{out:?}");
        let preserved = PathBuf::from(out.preserved_dir.expect("preserved dir"));
        assert_eq!(
            fs::read_to_string(preserved.join("appdata/a.txt")).unwrap(),
            "OLD-A"
        );
        assert_eq!(
            fs::read_to_string(preserved.join("appdata/sub/b.txt")).unwrap(),
            "OLD-B"
        );
        // 备份根里只有一个 pre-restore-* 目录
        let pres: Vec<String> = fs::read_dir(&backup_root)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("pre-restore-"))
            .collect();
        assert_eq!(pres.len(), 1, "exactly one preservation dir: {pres:?}");
        // 留痕文件存在且记录了原路径 → 保全路径
        let journal: Vec<PathBuf> = fs::read_dir(preserved.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| {
                p.file_name()
                    .map(|n| n.to_string_lossy().starts_with("restore-journal-"))
                    .unwrap_or(false)
            })
            .collect();
        assert_eq!(journal.len(), 1, "exactly one journal file");
        let text = fs::read_to_string(&journal[0]).unwrap();
        assert!(text.contains("appdata/a.txt"), "{text}");
        assert!(text.contains("appdata/sub/b.txt"), "{text}");
    }

    // ── 审计 P1-4：清单有界读取 ────────────────────────────────────────

    /// 清单超限 → 拒绝（且**读之前**就拒——用稀疏文件造一个巨大的清单）。
    #[test]
    fn refuses_oversized_manifest_before_reading() {
        let root = tmpdir("big-manifest");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let home = fs::canonicalize(&home).unwrap();
        let backup_root = root.join("backup");
        fs::create_dir_all(&backup_root).unwrap();
        let dir = backup_root.join("20260101000000/demo");
        fs::create_dir_all(&dir).unwrap();
        let manifest_path = dir.join("manifest.json");
        // 稀疏文件：metadata 体积超限，磁盘占用≈0（证明没有整读进内存）
        let f = fs::File::create(&manifest_path).unwrap();
        f.set_len(MAX_MANIFEST_BYTES as u64 + 4096).unwrap();
        drop(f);

        let err = restore_backup_in_at(
            &dir,
            "demo",
            &home,
            &wl(&home, "appdata"),
            &backup_root,
        )
        .unwrap_err();
        assert_eq!(err.code, "RESTORE_MANIFEST_TOO_LARGE", "{err:?}");
        assert!(err.human.contains("too large"), "{err:?}");
        assert_eq!(fs::read_dir(&home).unwrap().count(), 0);
    }

    // ── 备份侧：junction / 越界目录 + 限额（manage.rs 里断言） ──────────
}
