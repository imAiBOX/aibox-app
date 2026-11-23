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
//! 11. **只还原本机刚做的备份**（v0.1.20 灰度）：备份年龄超过 `max_age_hours`
//!    （**Rust 硬上限 24h**，registry 不能放宽）→ 整体拒绝 `RESTORE_BACKUP_TOO_OLD`；
//!    清单没写创建时间 → 年龄未知 → **同样拒绝**（fail-closed）；
//!    **双口径**：清单自述 `createdAt` **或**备份目录 mtime 任一超龄 → 拒绝；
//!    清单自称「未来的备份」→ 拒绝（审计 P1-2）。
//! 12. **目录私有**（Unix 0700）：还原新建的目录与文件（0600）权限齐平（审计 P3-5）。
//!
//! 整体拒绝（抛错、**在任何读写之前**）：`RESTORE_NO_MANIFEST` / `RESTORE_HOME_MISMATCH` /
//! `RESTORE_TOO_LARGE` / `RESTORE_MANIFEST_TOO_LARGE` / `RESTORE_APP_MISMATCH` /
//! `RESTORE_NO_WHITELIST` / `RESTORE_TARGET_NOT_ALLOWED` / `RESTORE_BAD_BACKUP_DIR` /
//! `RESTORE_BACKUP_TOO_OLD`；逐条问题只进 `skipped` / `failed` 列表。
//!
//! **预览 = 执行**（v0.1.20）：[`plan_restore_in_at`]（预览，**不写任何东西**）与
//! [`restore_backup_in_at`]（执行）走**同一份判定代码**（`prepare_restore`）→ 用户看到的
//! 清单与真正写下去的完全一致；计划摘要（`RestorePlan::digest`）在执行前再比一次
//! （清单被改过 → `RESTORE_PLAN_CHANGED`，**不写任何东西**）。

use std::collections::HashSet;
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
/// 缺省的最大可还原时长（小时）——registry 可在 1..=168 内声明自己的值。
///
/// **注意**：真正的上限是 [`MAX_RESTORE_AGE_HOURS`]（Rust 硬上限 24h）——
/// registry 只能往小里收，不能放宽（见 [`effective_max_age_hours`]）。
pub const DEFAULT_MAX_AGE_HOURS: u32 = 24;
/// registry 可声明的最大可还原时长下界（小时）。
pub const MIN_MAX_AGE_HOURS: u32 = 1;
/// registry 可声明的最大可还原时长上界（小时）。
pub const MAX_MAX_AGE_HOURS: u32 = 168;
/// **Rust 侧硬上限**：可还原备份的最大年龄（小时）——**registry 不能放宽**。
///
/// 灰度规则是「只还原本机刚做的备份」；registry 文档虽然走多源网络交叉校验，
/// 但它仍然是**外部文档**（能被改）→ **放宽窗口的权限只留在 Rust 手里**。
/// registry 声明值 > 本值 → 一律压回本值（见 [`effective_max_age_hours`]）。
pub const MAX_RESTORE_AGE_HOURS: u32 = 24;
/// 年龄闸门的**未来容忍度**（毫秒）：清单自述时间最多比「现在」晚这么多。
///
/// 只容忍极小的时钟漂移（备份就在本机做的，误差毫秒级）；
/// **不容忍**清单自称「几年后做的」——那会让年龄算成 0 → 备份永远「新鲜」（审计 P1-2）。
const FUTURE_SKEW_MS: u64 = 5 * 60 * 1000;

// ── 逐条原因码（固定枚举；UI 必须逐个映射人话文案）────────────────────
/// 路径上有软链 → 为安全跳过。
pub const ISSUE_SYMLINK: &str = "symlink";
/// 载荷内容摘要与清单不符（备份损坏或被改）。
pub const ISSUE_SHA_MISMATCH: &str = "sha_mismatch";
/// 目标类型与清单声明不一致（文件 vs 目录）。
pub const ISSUE_TYPE_CONFLICT: &str = "type_conflict";
/// 目标落在允许范围之外（白名单外 / 备份库内）→ 禁止写。
pub const ISSUE_OUTSIDE_BACKUP: &str = "outside_backup";
/// 备份载荷里找不到这个条目。
pub const ISSUE_SOURCE_MISSING: &str = "source_missing";
/// 旧文件挪到保全目录失败 → 为安全跳过（不覆盖）。
pub const ISSUE_PRESERVE_FAILED: &str = "preserve_failed";
/// 写入失败。
pub const ISSUE_WRITE_FAILED: &str = "write_failed";
/// 写入后校验不一致 → 已回滚。
pub const ISSUE_VERIFY_FAILED: &str = "verify_failed";
/// 父路径类型冲突（文件占了目录的位置）。
pub const ISSUE_PARENT_CONFLICT: &str = "parent_conflict";

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
    pub skipped: Vec<RestoreIssue>,
    /// 写入失败（保全失败 / 写后校验不符 / 源缺失）
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub failed: Vec<RestoreIssue>,
    /// 旧数据保全目录（被覆盖的原文件移到这里；空 = 本次没有覆盖任何已有文件）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preserved_dir: Option<String>,
}

/// 逐条问题（结构化 → UI 才能给「人话原因」）。
///
/// `code` 是固定枚举（见本文件的 `ISSUE_*` 常量）；`detail` 只进「技术详情」折叠区。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreIssue {
    /// 目标绝对路径（与计划里的 `target` 同一口径）
    pub target: String,
    /// 原因码（固定枚举）
    pub code: String,
    /// 技术细节
    pub detail: String,
}

/// 还原计划单条（预览与执行共用同一份判定 → 显示 = 执行）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlannedEntry {
    /// 绝对路径（还原到的位置）
    pub target: String,
    /// 相对 home 的路径
    pub rel: String,
    /// 备份载荷内的相对路径（决定读备份里的哪个文件 → 进摘要）
    pub payload_rel: String,
    /// 清单声明的大小（字节）
    pub size: u64,
    /// 是目录
    pub is_dir: bool,
    /// 目标已存在 → 会被挪到保全目录
    pub exists: bool,
    /// 备份载荷是否在（只做 metadata 预检，不读内容）
    pub payload_present: bool,
    /// 清单声明的内容摘要
    pub sha256: String,
}

/// 还原计划（预览 = 执行前的同一份判定结果）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestorePlan {
    /// 应用 id
    pub app_id: String,
    /// 备份目录（绝对路径）
    pub backup_dir: String,
    /// 清单 createdAt（ISO-8601；空 = 清单没写）
    pub created_at: String,
    /// 清单 createdAt（unix 毫秒；0 = 未知）
    pub created_at_ms: u64,
    /// 逐条计划
    pub entries: Vec<PlannedEntry>,
    /// 文件条数
    pub file_count: usize,
    /// 目录条数
    pub dir_count: usize,
    /// 总字节
    pub total_bytes: u64,
    /// 会被挪到保全目录的条数
    pub would_preserve: usize,
    /// 计划摘要（sha256：按 target 排序的 "target|size|sha256" 拼接）
    pub digest: String,
    /// 允许的最大可还原时长（小时）
    pub max_age_hours: u32,
}

/// 备份库里的一个备份（列表页）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupRecord {
    /// 应用 id（备份目录名）
    pub app_id: String,
    /// 备份目录（绝对路径）
    pub dir: String,
    /// 创建时间（ISO-8601；空 = 未知）
    pub created_at: String,
    /// 创建时间（unix 毫秒；0 = 未知）
    pub created_at_ms: u64,
    /// 文件条数（仅 v1 有意义）
    pub file_count: usize,
    /// 总字节（仅 v1 有意义）
    pub total_bytes: u64,
    /// 备份格式：`"v1"` | `"legacy"` | `"unknown"`
    pub format: String,
    /// 是否可还原（v1 + home 指纹一致 + 未超龄）
    pub restorable: bool,
    /// 备份年龄（小时，向下取整）
    pub age_hours: u32,
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
    app_id: String,
    home_hash: String,
    /// 清单创建时间（ISO-8601 原值；空 = 清单没写）
    created_at: String,
    /// 清单创建时间（unix 毫秒；0 = 未知 / 解析不了）
    created_at_ms: u64,
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
    /// 跳过（原因码 + 技术细节）——安全策略或校验不符，**未写任何东西**
    Skip { code: &'static str, detail: String },
    /// 失败（原因码 + 技术细节）——尽力保全后仍未还原
    Fail { code: &'static str, detail: String },
}

/// home 指纹：canonical home 路径的 sha256（备份与还原必须一致）。
///
/// 作用：备份被搬到另一台机器 / 另一个 home 后**拒绝还原**（绝不写到别的 home）。
pub fn home_fingerprint(home: &Path) -> String {
    let canon = fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
    sha256_str(&canon.to_string_lossy())
}

/// 从备份目录还原数据到 `home` 下的原路径（备份根固定为 `~/.aibox/backup`）。
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
/// - `RESTORE_BACKUP_TOO_OLD`：备份超过 `max_age_hours`（或清单没写创建时间 → 年龄未知 → 拒绝）
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
        MAX_RESTORE_AGE_HOURS,
    )
}

/// 生产入口（**备份根固定**为 `~/.aibox/backup`；调用方不能换根——审计 P2-1）：
/// 带「最大可还原时长」与「计划摘要校验」。
///
/// `expected_digest` = 预览时的计划摘要；**不一致 → `RESTORE_PLAN_CHANGED`（不写任何东西）**
/// （预览之后清单被改过 → 必须让用户重新确认一次）。
pub fn restore_backup_verified(
    backup_dir: &Path,
    app_id: &str,
    home: &Path,
    data_paths: &[String],
    max_age_hours: u32,
    expected_digest: &str,
) -> CoreResult<RestoreOutcome> {
    // **内核自己夹紧上限**（复核 2026-11-23）：调用方不能放宽灰度硬规则
    // （不夹紧时，传 168 就能放行 7 天前的备份）。
    let max_age_hours = effective_max_age_hours(Some(max_age_hours as u64));
    run_restore(
        backup_dir,
        app_id,
        home,
        data_paths,
        &crate::manage::default_backup_root(),
        max_age_hours,
        Some(expected_digest),
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
    max_age_hours: u32,
) -> CoreResult<RestoreOutcome> {
    run_restore(
        backup_dir,
        app_id,
        home,
        data_paths,
        backup_root,
        max_age_hours,
        None,
    )
}

/// 执行还原：**先算计划 → 校验摘要 → 再逐条还原**。
///
/// 预览（[`plan_restore_in_at`]）与执行走**同一份判定代码** → 显示 = 执行。
fn run_restore(
    backup_dir: &Path,
    app_id: &str,
    home: &Path,
    data_paths: &[String],
    backup_root: &Path,
    max_age_hours: u32,
    expected_digest: Option<&str>,
) -> CoreResult<RestoreOutcome> {
    let ctx = prepare_restore(
        backup_dir,
        app_id,
        home,
        data_paths,
        backup_root,
        max_age_hours,
    )?;
    let plan = build_plan(&ctx, app_id, max_age_hours);
    if let Some(expected) = expected_digest {
        if !expected.trim().eq_ignore_ascii_case(plan.digest.trim()) {
            return Err(CoreError::new(
                "RESTORE_PLAN_CHANGED",
                "The backup changed after it was previewed; refusing to restore (preview it again)",
            ));
        }
    }
    execute_restore(&ctx, &plan)
}

/// 还原判定上下文（①–⑦ 全部通过后的状态；执行阶段复用 → 预览与执行同一份判定）。
struct RestoreContext {
    /// canonical home
    home_real: PathBuf,
    /// canonical 备份根
    backup_root_real: PathBuf,
    /// canonical 备份目录
    backup_dir_real: PathBuf,
    /// 载荷根（清单所在目录）
    payload_root: PathBuf,
    /// 已解析的清单
    manifest: Manifest,
    /// 白名单
    allow: Allowlist,
}

/// ①–⑦ 判定（**只读不写**）：路径、备份根、白名单、清单、home 指纹、限额、逐条越界、超龄。
///
/// 任一条不通过 → 整体拒绝（抛错，**在任何读写之前**）；通过 → 返回上下文（执行阶段复用）。
fn prepare_restore(
    backup_dir: &Path,
    app_id: &str,
    home: &Path,
    data_paths: &[String],
    backup_root: &Path,
    max_age_hours: u32,
) -> CoreResult<RestoreContext> {
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

    // ⑧ 超龄（**在 ①–⑦ 之后、任何写入之前**）：越界/限额比「太旧」更严重，先报它们。
    // **双口径**（清单自述 `createdAt` + 备份目录 mtime；任一超龄 → 拒绝；
    // 清单自称「未来」→ 也拒绝；详见 [`check_backup_age`]）。
    //
    // 载荷根 = 清单所在目录；保全根 = 备份根
    let payload_root = manifest_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| backup_dir_real.clone());
    let dir_mtime_ms = backup_dir_mtime_ms(&payload_root, &backup_root_real);
    let age = backup_age(manifest.created_at_ms, dir_mtime_ms);
    check_backup_age(&age, max_age_hours)?;

    Ok(RestoreContext {
        home_real,
        backup_root_real,
        backup_dir_real,
        payload_root,
        manifest,
        allow,
    })
}

/// 生成还原计划（**不写任何东西**）：走与执行完全相同的 ①–⑦ 判定。
///
/// 预览与执行共用同一份判定代码 → **显示 = 执行**（杜绝「预览说能还原、执行却不是那样」）。
pub fn plan_restore_in_at(
    backup_dir: &Path,
    app_id: &str,
    home: &Path,
    data_paths: &[String],
    backup_root: &Path,
    max_age_hours: u32,
) -> CoreResult<RestorePlan> {
    // **内核自己夹紧上限**（调用方不能放宽灰度硬规则；预览与执行必须用同一个窗口）
    let max_age_hours = effective_max_age_hours(Some(max_age_hours as u64));
    let ctx = prepare_restore(
        backup_dir,
        app_id,
        home,
        data_paths,
        backup_root,
        max_age_hours,
    )?;
    Ok(build_plan(&ctx, app_id, max_age_hours))
}

/// 由判定上下文生成计划（**只做 metadata 预检，不读载荷内容**）。
fn build_plan(ctx: &RestoreContext, app_id: &str, max_age_hours: u32) -> RestorePlan {
    let mut entries = Vec::with_capacity(ctx.manifest.entries.len());
    let mut file_count = 0usize;
    let mut dir_count = 0usize;
    let mut total_bytes = 0u64;
    let mut would_preserve = 0usize;
    for e in &ctx.manifest.entries {
        let target_rel = safe_rel(&e.target).unwrap_or_else(|| PathBuf::from(&e.target));
        let target_abs = ctx.home_real.join(&target_rel);
        let target_meta = fs::symlink_metadata(&target_abs).ok();
        let exists = target_meta.is_some();
        // 只有「目标是已存在的普通文件」才会被挪到保全目录（目录不挪）
        let will_preserve = !e.is_dir && target_meta.as_ref().map(|m| m.is_file()).unwrap_or(false);
        let payload_present = match safe_rel(&e.rel) {
            Some(rel) => {
                let p = ctx.payload_root.join(&rel);
                match fs::symlink_metadata(&p) {
                    Ok(m) => {
                        if e.is_dir {
                            m.is_dir()
                        } else {
                            m.is_file()
                        }
                    }
                    Err(_) => false,
                }
            }
            None => false,
        };
        if e.is_dir {
            dir_count += 1;
        } else {
            file_count += 1;
            total_bytes = total_bytes.saturating_add(e.size);
        }
        if will_preserve {
            would_preserve += 1;
        }
        entries.push(PlannedEntry {
            target: target_abs.to_string_lossy().into_owned(),
            rel: target_rel.to_string_lossy().replace('\\', "/"),
            payload_rel: e.rel.replace('\\', "/"),
            size: e.size,
            is_dir: e.is_dir,
            exists,
            payload_present,
            sha256: e.sha256.clone(),
        });
    }
    let digest = plan_digest(&entries);
    RestorePlan {
        app_id: app_id.to_string(),
        backup_dir: ctx.backup_dir_real.to_string_lossy().into_owned(),
        created_at: ctx.manifest.created_at.clone(),
        created_at_ms: ctx.manifest.created_at_ms,
        entries,
        file_count,
        dir_count,
        total_bytes,
        would_preserve,
        digest,
        max_age_hours,
    }
}

/// 计划摘要：**按清单原顺序**逐条**无歧义编码** → sha256。
///
/// **为什么改成无歧义编码**（审计 P1-1：摘要可碰撞 → 「预览 = 执行」不变式被破）：
/// 旧格式 `"{target}|{size}|{sha256}\n"` 有两处可碰撞：
/// ① **不含 `isDir`** → 同一个 target 从「目录」翻成「文件」→ 真正写下去的内容完全不同，
///    摘要却一模一样（预览看到 A，执行却写了 A 的文件）；
/// ② **分隔符不转义** → 清单里的 `sha256` 可以含换行 → 「条目数不同」的两份清单
///    能拼出同一段摘要输入（换行注入）。
///
/// 新格式：**每个字段都用字节长度前缀**（`len:value`）→ 任何字段内容都无法伪造边界；
/// 且覆盖**所有影响写入的字段**：`target` / `rel` / `isDir` / `size` / `sha256` / 载荷路径。
///
/// **顺序也进摘要**（复核 2026-11-23）：计划条目与清单条目是**按序号对齐**的
/// （执行阶段 `plan.entries[i]` ↔ `manifest.entries[i]`）→ 顺序变了就是「另一个计划」。
/// 旧实现先排序再算摘要 → 清单换顺序但摘要不变 → 用户看到的清单与实际写入顺序不一致。
/// （同 target 重复条目已在 `parse_manifest` 拒绝 → 不会静默「后者覆盖前者」。）
fn plan_digest(entries: &[PlannedEntry]) -> String {
    let mut buf = String::new();
    for (i, e) in entries.iter().enumerate() {
        // 序号进摘要（顺序敏感）
        buf.push_str(&i.to_string());
        buf.push(':');
        buf.push_str(&e.target.len().to_string());
        buf.push(':');
        buf.push_str(&e.target);
        buf.push('|');
        buf.push_str(&e.rel.len().to_string());
        buf.push(':');
        buf.push_str(&e.rel);
        buf.push('|');
        buf.push_str(if e.is_dir { "1" } else { "0" });
        buf.push('|');
        buf.push_str(&e.size.to_string());
        buf.push('|');
        buf.push_str(&e.payload_rel.len().to_string());
        buf.push(':');
        buf.push_str(&e.payload_rel);
        buf.push('|');
        buf.push_str(&e.sha256.len().to_string());
        buf.push(':');
        buf.push_str(&e.sha256);
        buf.push('\n');
    }
    sha256_str(&buf)
}

/// 逐条还原（⑧）：用**计划里的同一份判定**执行 → 预览与执行一致。
fn execute_restore(ctx: &RestoreContext, plan: &RestorePlan) -> CoreResult<RestoreOutcome> {
    let mut outcome = RestoreOutcome::default();
    let mut preserver = Preserver::new(&ctx.backup_root_real, &ctx.manifest.app_id);

    for (i, entry) in ctx.manifest.entries.iter().enumerate() {
        let planned = &plan.entries[i];
        let res = restore_entry(entry, ctx, &mut preserver);
        match res {
            EntryResult::Restored(p) => {
                outcome.restored += 1;
                outcome.restored_paths.push(p);
            }
            EntryResult::Ready => {}
            EntryResult::Skip { code, detail } => {
                outcome.skipped.push(RestoreIssue {
                    target: planned.target.clone(),
                    code: code.to_string(),
                    detail,
                });
            }
            EntryResult::Fail { code, detail } => {
                outcome.failed.push(RestoreIssue {
                    target: planned.target.clone(),
                    code: code.to_string(),
                    detail,
                });
            }
        }
    }
    outcome.preserved_dir = preserver
        .current()
        .map(|p| p.to_string_lossy().into_owned());
    Ok(outcome)
}

/// 列出备份库里的备份（`<backup_root>/<ts>/<appId>/`；**跳过 `pre-restore-*` 保全目录**）。
///
/// - `format = "v1"`：清单 schema 匹配 → 统计文件数/字节、读创建时间；
/// - `format = "legacy"`：有清单但 schema 不符（旧格式 `data.zip`）→ **不可还原**；
/// - `format = "unknown"`：没有清单 → **不可还原**。
///
/// `restorable` = v1 + 清单 appId 与目录名一致 + home 指纹一致 + 创建时间已知 + 未超龄。
pub fn list_backups_in(
    backup_root: &Path,
    home: &Path,
    max_age_hours: u32,
) -> CoreResult<Vec<BackupRecord>> {
    let home_real = fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
    let home_hash = home_fingerprint(&home_real);
    let Ok(rd) = fs::read_dir(backup_root) else {
        // 备份库还不存在 → 空列表（不是错误）
        return Ok(Vec::new());
    };
    // 备份根的真实路径（包含性检查的基准）
    let backup_root_real =
        fs::canonicalize(backup_root).unwrap_or_else(|_| backup_root.to_path_buf());
    let mut out: Vec<BackupRecord> = Vec::new();
    for ts_entry in rd.flatten() {
        let ts_dir = ts_entry.path();
        if !ts_dir.is_dir() {
            continue;
        }
        let ts_name = ts_entry.file_name().to_string_lossy().into_owned();
        // 保全目录不是备份（那是「被覆盖的旧数据」，不能拿去还原）
        if ts_name.starts_with("pre-restore-") {
            continue;
        }
        // **包含性检查**（审计 P2-2）：`<ts>` 目录解析软链后必须仍在备份根之内
        // （备份库里放一个指向库外的软链 → 列表不得把它交给 UI）。
        let Ok(ts_real) = fs::canonicalize(&ts_dir) else {
            continue;
        };
        if !ts_real.starts_with(&backup_root_real) {
            continue;
        }
        let Ok(app_rd) = fs::read_dir(&ts_dir) else {
            continue;
        };
        for app_entry in app_rd.flatten() {
            let app_dir = app_entry.path();
            if !app_dir.is_dir() {
                continue;
            }
            // **包含性检查**（同上）：`<ts>/<appId>` 目录也必须在备份根之内。
            let Ok(app_real) = fs::canonicalize(&app_dir) else {
                continue;
            };
            if !app_real.starts_with(&backup_root_real) {
                continue;
            }
            let app_id = app_entry.file_name().to_string_lossy().into_owned();
            out.push(read_backup_record(
                &app_dir,
                &app_id,
                &home_hash,
                max_age_hours,
                &backup_root_real,
            ));
        }
    }
    // 新的在前（同毫秒则按 app id 稳定排序）
    out.sort_by(|a, b| {
        b.created_at_ms
            .cmp(&a.created_at_ms)
            .then_with(|| a.app_id.cmp(&b.app_id))
    });
    Ok(out)
}

/// 读一个备份目录 → 列表条目（**只读元数据**；不碰载荷内容）。
fn read_backup_record(
    app_dir: &Path,
    app_id: &str,
    home_hash: &str,
    max_age_hours: u32,
    backup_root_real: &Path,
) -> BackupRecord {
    let mut rec = BackupRecord {
        app_id: app_id.to_string(),
        dir: app_dir.to_string_lossy().into_owned(),
        created_at: String::new(),
        created_at_ms: 0,
        file_count: 0,
        total_bytes: 0,
        format: "unknown".to_string(),
        restorable: false,
        age_hours: 0,
    };
    let manifest_path = app_dir.join("manifest.json");
    if !manifest_path.is_file() {
        // 没有清单 → unknown（旧备份是 data.zip，没有可核对的映射表）
        return rec;
    }
    let Ok(text) = read_manifest_bounded(&manifest_path) else {
        rec.format = "legacy".to_string();
        return rec;
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        rec.format = "legacy".to_string();
        return rec;
    };
    let created_at = value
        .get("createdAt")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let created_at_ms = parse_iso8601_ms(&created_at).unwrap_or(0);
    // **与执行同一条年龄口径**（清单自述 + 目录 mtime；任一超龄 → 不可还原）
    let dir_mtime_ms = backup_dir_mtime_ms(app_dir, backup_root_real);
    let age = backup_age(created_at_ms, dir_mtime_ms);
    rec.created_at = created_at;
    rec.created_at_ms = created_at_ms;
    rec.age_hours = age.worst();
    let schema = value.get("schema").and_then(|v| v.as_str()).unwrap_or("");
    if schema != MANIFEST_SCHEMA {
        // 有清单但不是本格式（旧备份）→ 明确标出来，UI 提示「请手动恢复」
        rec.format = "legacy".to_string();
        return rec;
    }
    let (file_count, total_bytes) = value
        .get("entries")
        .and_then(|v| v.as_array())
        .map(|arr| {
            let mut files = 0usize;
            let mut bytes = 0u64;
            for e in arr {
                let is_dir = e.get("isDir").and_then(|v| v.as_bool()).unwrap_or(false);
                if is_dir {
                    continue;
                }
                files += 1;
                bytes = bytes.saturating_add(e.get("size").and_then(|v| v.as_u64()).unwrap_or(0));
            }
            (files, bytes)
        })
        .unwrap_or((0, 0));
    let home_ok = value
        .get("homeHash")
        .and_then(|v| v.as_str())
        .map(|h| h.eq_ignore_ascii_case(home_hash))
        .unwrap_or(false);
    // 清单的 appId 必须与**目录名**一致（审计 P2-3 的一部分）：
    // 清单被挪到别的应用目录下（或目录被改名）→ 张冠李戴 → 不可还原。
    let app_ok = value
        .get("appId")
        .and_then(|v| v.as_str())
        .map(|a| a == app_id)
        .unwrap_or(false);
    rec.format = "v1".to_string();
    rec.file_count = file_count;
    rec.total_bytes = total_bytes;
    // 可还原 = v1 + appId 一致 + home 指纹一致 + 年龄合格（双口径，与执行同一条规则）
    rec.restorable = app_ok && home_ok && check_backup_age(&age, max_age_hours).is_ok();
    rec
}

/// 当前 unix 毫秒。
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 备份年龄（小时，向下取整）；`created_at_ms == 0`（时间未知）→ 0
/// （调用方按「年龄未知 → 拒绝」另行处理）。
fn age_hours_since(created_at_ms: u64) -> u32 {
    if created_at_ms == 0 {
        return 0;
    }
    let ms = now_ms().saturating_sub(created_at_ms);
    (ms / 3_600_000).min(u32::MAX as u64) as u32
}

/// registry 声明的「最大可还原时长」→ **实际生效值**（小时）。
///
/// 规则（全部 fail-closed）：
/// - 缺失 / 非数字 / 越界（不在 `MIN_MAX_AGE_HOURS..=MAX_MAX_AGE_HOURS`）→ [`MAX_RESTORE_AGE_HOURS`]；
/// - 声明值 **大于 [`MAX_RESTORE_AGE_HOURS`] → 压回 [`MAX_RESTORE_AGE_HOURS`]**：
///   **registry 不能放宽**灰度硬规则（否则一份远端文档就能把「只还原 24h 内的备份」
///   悄悄变成 7 天——审计 P1-4）。
pub fn effective_max_age_hours(declared: Option<u64>) -> u32 {
    let raw = match declared {
        Some(v) if (MIN_MAX_AGE_HOURS as u64) <= v && v <= (MAX_MAX_AGE_HOURS as u64) => v,
        _ => return MAX_RESTORE_AGE_HOURS,
    };
    raw.min(MAX_RESTORE_AGE_HOURS as u64) as u32
}

/// 备份目录的**文件系统时间**（unix 毫秒）→ 取「载荷所在目录」与「它的上一层目录」
/// 两个 mtime 里**较旧**的那个。
///
/// **为什么取较旧者**（取舍，审计 P1-2 的第二口径）：
/// - 正常备份里这两个 mtime 只差几秒（`<ts>` 先建、`<appId>` 后建、文件最后写）；
/// - **改写 `manifest.json` 会刷新 `<appId>` 目录的 mtime，但不会刷新父 `<ts>` 目录的**
///   → 取较旧者（= `<ts>` 的 mtime）才能挡住「改清单伪造年龄」这种半篡改；
///   取较新者则会被改清单顺手刷新 → 第二口径形同虚设；
/// - **拷贝 / 移动会同时刷新两个 mtime** → 取较旧者不会因为「搬过家」而误拒真备份
///   （可用性上没有代价）。
/// - 备份根本身**不计入**：保全目录（`pre-restore-*`）的创建会刷新它 → 与备份新旧无关。
///
/// 局限（写在这里避免误以为它很强）：能写备份目录的人也能改这两个 mtime →
/// 第二口径挡的是「只能改清单」的篡改与损坏，不是同机全权限攻击者。
fn backup_dir_mtime_ms(payload_root: &Path, backup_root_real: &Path) -> Option<u64> {
    let mut dirs: Vec<&Path> = vec![payload_root];
    if let Some(parent) = payload_root.parent() {
        dirs.push(parent);
    }
    let mut best: Option<u64> = None;
    for p in dirs {
        if p == backup_root_real {
            continue; // 备份根：保全目录会刷新它 → 与备份新旧无关
        }
        let Ok(meta) = fs::metadata(p) else { continue };
        let Ok(t) = meta.modified() else { continue };
        let Ok(d) = t.duration_since(std::time::UNIX_EPOCH) else {
            continue;
        };
        let ms = d.as_millis() as u64;
        best = Some(match best {
            Some(b) => b.min(ms),
            None => ms,
        });
    }
    best
}

/// 备份年龄的**两个口径**（清单自述 + 目录 mtime）。
#[derive(Debug, Clone, Copy)]
struct BackupAge {
    /// 清单自述创建时间（unix 毫秒；0 = 清单没写/解析不了 → 年龄未知）
    created_at_ms: u64,
    /// 清单自述年龄（小时；`None` = 年龄未知）
    declared: Option<u32>,
    /// 备份目录 mtime（unix 毫秒；`None` = 取不到）
    dir_mtime_ms: Option<u64>,
    /// 目录年龄（小时；`None` = 取不到）
    dir: Option<u32>,
}

impl BackupAge {
    /// **最坏估计**年龄（小时）：两个口径里**较旧**的那个（未知算 0）。
    ///
    /// 报告值与闸门取同一口径 → UI 显示与判定永远一致
    /// （不会出现「0 小时前但不可还原」这种自相矛盾）。
    fn worst(&self) -> u32 {
        self.declared.unwrap_or(0).max(self.dir.unwrap_or(0))
    }
}

/// 采集两个口径的年龄（清单时间 + 目录 mtime）。
fn backup_age(created_at_ms: u64, dir_mtime_ms: Option<u64>) -> BackupAge {
    BackupAge {
        created_at_ms,
        declared: if created_at_ms == 0 {
            None
        } else {
            Some(age_hours_since(created_at_ms))
        },
        dir_mtime_ms,
        dir: dir_mtime_ms.map(age_hours_since),
    }
}

/// 备份年龄闸门（**列表与执行共用同一套口径**——否则会出现
/// 「列表说可还原、执行说太旧」的自相矛盾）。
///
/// **双口径**（**任一超龄即拒绝**）：
/// 1. **清单自述年龄**（`createdAt`）：**可被篡改** → 只能当参考，不能当唯一依据；
/// 2. **目录年龄**（备份目录 mtime）：改清单改不了它 → 更可信。
///
/// 返回 `Ok(最坏估计年龄)`；超龄 / 时间非法 → `Err(RESTORE_BACKUP_TOO_OLD)`。
fn check_backup_age(age: &BackupAge, max_age_hours: u32) -> CoreResult<u32> {
    let too_old = |what: String| {
        CoreError::new(
            "RESTORE_BACKUP_TOO_OLD",
            format!("{what}; refusing to restore"),
        )
    };
    let now = now_ms();
    // ① 时间未知 → 拒绝（fail-closed：拿不准 → 不放行）
    if age.created_at_ms == 0 {
        return Err(too_old(
            "The backup does not state when it was created (age unknown)".to_string(),
        ));
    }
    // ② 清单自称「未来的备份」→ 清单被改过 → 按非法处理（fail-closed）
    if age.created_at_ms > now.saturating_add(FUTURE_SKEW_MS) {
        return Err(too_old(
            "The backup claims to be created in the future (its creation time was tampered with)"
                .to_string(),
        ));
    }
    // ③ 目录年龄（第二口径）：取不到 → 不可信 → 拒绝
    let Some(dir_mtime) = age.dir_mtime_ms else {
        return Err(too_old(
            "The backup folder has no readable timestamp (age unknown)".to_string(),
        ));
    };
    if dir_mtime > now.saturating_add(FUTURE_SKEW_MS) {
        return Err(too_old(
            "The backup folder timestamp is in the future (it was tampered with)".to_string(),
        ));
    }
    let declared_age = age.declared.unwrap_or(0);
    let dir_age = age.dir.unwrap_or(0);
    // ④ 任一口径超龄 → 拒绝（宁可拒绝，也不放行可疑备份）
    if declared_age > max_age_hours {
        return Err(too_old(format!(
            "This backup is {declared_age} hour(s) old (limit {max_age_hours})"
        )));
    }
    if dir_age > max_age_hours {
        return Err(too_old(format!(
            "This backup folder is {dir_age} hour(s) old (limit {max_age_hours})"
        )));
    }
    Ok(age.worst())
}

/// ISO-8601（`YYYY-MM-DDTHH:MM:SS[.fff]Z`，UTC）→ unix 毫秒；解析不了 → `None`。
///
/// **只认 UTC**（`Z`）：别的时区不猜（猜错年龄 = 放行过期备份）→ fail-closed。
fn parse_iso8601_ms(raw: &str) -> Option<u64> {
    let s = raw.trim();
    let b = s.as_bytes();
    if b.len() < 20 {
        return None;
    }
    let year: i64 = s.get(0..4)?.parse().ok()?;
    if b[4] != b'-' {
        return None;
    }
    let month: i64 = s.get(5..7)?.parse().ok()?;
    if b[7] != b'-' {
        return None;
    }
    let day: i64 = s.get(8..10)?.parse().ok()?;
    if b[10] != b'T' && b[10] != b' ' {
        return None;
    }
    let hour: i64 = s.get(11..13)?.parse().ok()?;
    if b[13] != b':' {
        return None;
    }
    let minute: i64 = s.get(14..16)?.parse().ok()?;
    if b[16] != b':' {
        return None;
    }
    let second: i64 = s.get(17..19)?.parse().ok()?;
    // 尾部：可选小数秒 + 'Z'（其它时区 → 不认）
    let mut rest = &s[19..];
    let mut millis: u64 = 0;
    if let Some(stripped) = rest.strip_prefix('.') {
        let digits: String = stripped
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        // 只取前三位（毫秒）；多余的位数丢弃（备份清单的时间精度只到毫秒）
        let mut three = digits.clone();
        three.truncate(3);
        while three.len() < 3 {
            three.push('0');
        }
        millis = three.parse().unwrap_or(0);
        rest = &stripped[digits.len()..];
    }
    if rest != "Z" && rest != "z" {
        return None;
    }
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    if hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let days = days_from_civil(year, month, day);
    let secs = days * 86_400 + hour * 3600 + minute * 60 + second;
    if secs < 0 {
        return None;
    }
    Some(secs as u64 * 1000 + millis)
}

/// 公历 → unix 天数（Howard Hinnett 算法；不依赖外部 crate）。
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
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
    // 创建时间：v1 清单必写；缺失/解析不了 → `created_at_ms = 0`（年龄未知 → 还原时拒绝）
    let created_at = obj
        .get("createdAt")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if has_control_chars(&created_at) {
        return Err(damaged("createdAt contains control characters"));
    }
    let created_at_ms = parse_iso8601_ms(&created_at).unwrap_or(0);
    let arr = obj
        .get("entries")
        .and_then(|v| v.as_array())
        .ok_or_else(|| damaged("missing entries"))?;

    let mut entries = Vec::with_capacity(arr.len());
    // 同一 target 只允许出现一次（见下方 duplicate entry target）
    let mut seen_targets: HashSet<String> = HashSet::with_capacity(arr.len());
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
        // **所有字符串字段拒绝控制字符**（U+0000–U+001F，含 `\n` `\r`）：
        // 清单里塞一个换行就能伪造摘要编码的字段边界（审计 P1-1）→ 当作损坏清单。
        if has_control_chars(rel) {
            return Err(damaged("entry rel contains control characters"));
        }
        if has_control_chars(target) {
            return Err(damaged("entry target contains control characters"));
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
        if has_control_chars(sha256) {
            return Err(damaged("entry sha256 contains control characters"));
        }
        if !is_dir && !is_hex64(sha256) {
            return Err(damaged("file entry without a 64-hex sha256"));
        }
        // 目录条目的摘要：**只允许空串或 64 位 hex**（与文件条目同规则）。
        // 旧实现对目录条目的 `sha256` 完全不校验 → 可以塞换行伪造摘要输入（审计 P1-1）。
        if is_dir && !sha256.is_empty() && !is_hex64(sha256) {
            return Err(damaged("directory entry with an invalid sha256"));
        }
        // **同一个 target 不许出现两次**（复核 2026-11-23）：
        // 两条目同 target 时，谁先写谁后写会改变最终落盘内容 → 顺序影响写入结果；
        // 清单是攻击者可改的 → 同 target 多条 = 清单自相矛盾 → 当作损坏清单。
        if !seen_targets.insert(target.to_string()) {
            return Err(damaged("duplicate entry target"));
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
        app_id: app_id.to_string(),
        home_hash: home_hash.to_ascii_lowercase(),
        created_at,
        created_at_ms,
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

/// 单条还原（执行期复核：软链 / 摘要 / 类型 / 父路径 —— 计划阶段只做 metadata 预检）。
fn restore_entry(
    entry: &ManifestEntry,
    ctx: &RestoreContext,
    preserver: &mut Preserver,
) -> EntryResult {
    // ── 不变式 1：白名单（调用方给的 dataPaths）——清单自述路径不可信 ──
    let Some(target_rel) = safe_rel(&entry.target) else {
        return EntryResult::Skip {
            code: ISSUE_OUTSIDE_BACKUP,
            detail: format!("target path '{}' is not a safe relative path", entry.target),
        };
    };
    if !allow_allows(&ctx.allow, &target_rel) {
        return EntryResult::Skip {
            code: ISSUE_OUTSIDE_BACKUP,
            detail: "target is outside the declared data paths".into(),
        };
    }
    let Some(rel) = safe_rel(&entry.rel) else {
        return EntryResult::Skip {
            code: ISSUE_SOURCE_MISSING,
            detail: format!("backup path '{}' is not a safe relative path", entry.rel),
        };
    };
    let home_real = &ctx.home_real;
    let target_abs = home_real.join(&target_rel);
    let source = ctx.payload_root.join(&rel);

    // ── 不变式 2（续）：还原目标不得落在备份库内（整体判定已拦，这里纵深防御）──
    if target_inside_backup_root(&target_rel, home_real, &ctx.backup_root_real) {
        return EntryResult::Skip {
            code: ISSUE_OUTSIDE_BACKUP,
            detail: "target is inside the backup library".into(),
        };
    }
    // ── 不变式 3：拼回 home 后必须仍在 home 内（软链逃逸兜底） ──
    // 链接防护**先于**越界判定：路径上有软链时，原因码报 `symlink`（更贴近人话）
    //（两者都是「跳过、不写」，先判谁不影响安全性）。
    if let Some(link) = symlink_component(&target_abs, home_real) {
        return EntryResult::Skip {
            code: ISSUE_SYMLINK,
            detail: format!("target path traverses a symlink ({})", link.display()),
        };
    }
    // ── 不变式 3（续）：链接防护（备份源侧） ──
    if let Some(link) = symlink_component(&source, &ctx.payload_root) {
        return EntryResult::Skip {
            code: ISSUE_SYMLINK,
            detail: format!("backup source traverses a symlink ({})", link.display()),
        };
    }
    if !canonical_within(&target_abs, home_real) {
        return EntryResult::Skip {
            code: ISSUE_OUTSIDE_BACKUP,
            detail: "resolved target escapes the home directory".into(),
        };
    }

    // 类型冲突：清单说文件但原位是目录（或反之）→ 跳过，不动原数据
    let target_meta = fs::symlink_metadata(&target_abs);
    match &target_meta {
        Ok(m) if entry.is_dir && !m.is_dir() => {
            return EntryResult::Skip {
                code: ISSUE_TYPE_CONFLICT,
                detail: "target exists as a file".into(),
            }
        }
        Ok(m) if !entry.is_dir && m.is_dir() => {
            return EntryResult::Skip {
                code: ISSUE_TYPE_CONFLICT,
                detail: "target exists as a directory".into(),
            }
        }
        _ => {}
    }

    if entry.is_dir {
        return restore_dir(&target_abs);
    }
    // 父路径类型冲突（文件占了目录的位置）→ 跳过（不硬写、不覆盖）
    if let Some(blocker) = parent_conflict(&target_abs, home_real) {
        return EntryResult::Skip {
            code: ISSUE_PARENT_CONFLICT,
            detail: format!(
                "a file already occupies the parent path ({})",
                blocker.display()
            ),
        };
    }
    restore_file(entry, &source, &target_abs, &target_rel, preserver)
}

/// 目录条目：缺失则创建（父目录一并建，**0700 私有**）。
///
/// **权限**（审计 P3-5）：新建目录用 **0700**（Unix 新建即 0700，不靠事后 chmod）
/// → 与文件 0600、备份库 0700 齐平；**已存在的目录不动权限**
/// （绝不改用户原始数据目录的权限）。
fn restore_dir(target_abs: &Path) -> EntryResult {
    match fs::symlink_metadata(target_abs) {
        Ok(m) if m.is_dir() => EntryResult::Ready,
        Ok(_) => EntryResult::Skip {
            code: ISSUE_TYPE_CONFLICT,
            detail: "target exists as a file".into(),
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            if let Err(e) = fsops::create_dirs_private(target_abs) {
                return EntryResult::Fail {
                    code: ISSUE_WRITE_FAILED,
                    detail: format!("cannot create directory: {e}"),
                };
            }
            // 建完再确认不是软链（fail-closed）
            match fs::symlink_metadata(target_abs) {
                Ok(m) if m.file_type().is_symlink() => {
                    let _ = fs::remove_dir(target_abs);
                    EntryResult::Skip {
                        code: ISSUE_SYMLINK,
                        detail: "created path turned out to be a symlink".into(),
                    }
                }
                Ok(_) => EntryResult::Ready,
                Err(e) => EntryResult::Fail {
                    code: ISSUE_WRITE_FAILED,
                    detail: format!("cannot inspect created directory: {e}"),
                },
            }
        }
        Err(e) => EntryResult::Fail {
            code: ISSUE_WRITE_FAILED,
            detail: format!("cannot inspect target: {e}"),
        },
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
        Err(e) => {
            return EntryResult::Fail {
                code: ISSUE_SOURCE_MISSING,
                detail: format!("backup source is missing or unreadable: {e}"),
            }
        }
    };
    if meta.file_type().is_symlink() {
        return EntryResult::Skip {
            code: ISSUE_SYMLINK,
            detail: "backup source is a symlink".into(),
        };
    }
    if !meta.is_file() {
        return EntryResult::Skip {
            code: ISSUE_TYPE_CONFLICT,
            detail: "backup source is not a regular file".into(),
        };
    }
    if meta.len() != entry.size {
        return EntryResult::Skip {
            code: ISSUE_SHA_MISMATCH,
            detail: format!(
                "backup source size {} does not match the manifest ({})",
                meta.len(),
                entry.size
            ),
        };
    }

    // ── 不变式 4：校验后替换（备份源被改过 → 跳过，绝不写） ──
    let actual = match sha256_file_pinned(source) {
        Ok(h) => h,
        Err(e) => {
            return EntryResult::Fail {
                code: ISSUE_SOURCE_MISSING,
                detail: format!("cannot hash backup source: {e}"),
            }
        }
    };
    if !actual.eq_ignore_ascii_case(&entry.sha256) {
        return EntryResult::Skip {
            code: ISSUE_SHA_MISMATCH,
            detail: format!(
                "checksum mismatch (backup file was modified: expected {}, got {actual})",
                entry.sha256
            ),
        };
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
                // 保全失败 → **为安全跳过**（不覆盖旧数据；规格：preserve_failed → skipped）
                return EntryResult::Skip {
                    code: ISSUE_PRESERVE_FAILED,
                    detail: format!("cannot preserve existing data: {e}"),
                };
            }
        }
    }

    // ── 不变式 6：原子写 + 写后校验 ──
    match write_verified(source, target_abs, &entry.sha256) {
        Ok(()) => EntryResult::Restored(target_abs.to_string_lossy().into_owned()),
        Err(f) => {
            // 写失败 → 把保全的旧数据放回原位（用户仍拿到旧内容）
            let mut detail = f.detail;
            if let Some(dest) = preserved {
                if let Err(rb) = move_back(&dest, target_abs) {
                    detail.push_str(&format!("; rollback failed: {rb}"));
                }
            }
            EntryResult::Fail {
                code: if f.verify {
                    ISSUE_VERIFY_FAILED
                } else {
                    ISSUE_WRITE_FAILED
                },
                detail,
            }
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
        // 保全目录里的子目录也是私有（0700）
        fsops::create_dirs_private(parent)?;
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

/// 写入失败（区分「写后校验不符」与普通写失败 → UI 给不同的人话原因）。
struct WriteFailure {
    /// true = 写后校验不符（已回滚）
    verify: bool,
    detail: String,
}

/// 原子写 + 写后校验：同目录临时文件 → 写完重算 sha256 → 一致才 rename 到位。
///
/// 临时文件名固定（`.<name>.aibox-restore.<pid>.tmp`）且**独占创建**
/// （`create_new`）——同名已存在 → 换名重试（不覆盖别人的文件，也不静默复用）。
fn write_verified(
    source: &Path,
    target_abs: &Path,
    expected_sha: &str,
) -> Result<(), WriteFailure> {
    let dir = target_abs.parent().ok_or_else(|| WriteFailure {
        verify: false,
        detail: "target has no parent directory".into(),
    })?;
    // 目标父目录：新建即 0700（与文件 0600 齐平；审计 P3-5）
    fsops::create_dirs_private(dir).map_err(|e| WriteFailure {
        verify: false,
        detail: format!("cannot create the parent directory: {e}"),
    })?;
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
    let (tmp, mut output) =
        fsops::create_exclusive_tmp(dir, &base, ".tmp").map_err(|e| WriteFailure {
            verify: false,
            detail: e.human,
        })?;
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
        return Err(WriteFailure {
            verify: false,
            detail: format!("cannot write the restored file: {e}"),
        });
    }
    drop(output);
    // 写后校验：重新从磁盘读回重算 sha256
    let actual = match sha256_file_pinned(&tmp) {
        Ok(h) => h,
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            return Err(WriteFailure {
                verify: false,
                detail: format!("cannot verify the restored file: {e}"),
            });
        }
    };
    if !actual.eq_ignore_ascii_case(expected_sha) {
        let _ = fs::remove_file(&tmp);
        return Err(WriteFailure {
            verify: true,
            detail: format!("write-back checksum mismatch (expected {expected_sha}, got {actual})"),
        });
    }
    // 到位（目标此刻应为空——旧数据已被移入保全目录）
    if let Err(e) = fs::rename(&tmp, target_abs) {
        let _ = fs::remove_file(&tmp);
        return Err(WriteFailure {
            verify: false,
            detail: format!("cannot place the restored file: {e}"),
        });
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

/// 字符串里是否含**控制字符**（U+0000–U+001F，含 `\n` `\r`）。
///
/// 清单里的路径 / 摘要字段一旦含控制字符 → 就能伪造摘要编码的字段边界
/// （审计 P1-1）→ 一律当作**损坏清单**（整体拒绝，不猜、不跳过）。
fn has_control_chars(s: &str) -> bool {
    s.chars().any(|c| (c as u32) < 0x20)
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

/// 父路径类型冲突：目标的父链上有个**文件**占了目录的位置
/// （`appdata` 是文件，却要写 `appdata/x.txt`）→ 返回那个文件路径。
///
/// 找不到冲突 → `None`（正常路径）。
fn parent_conflict(target_abs: &Path, home_real: &Path) -> Option<PathBuf> {
    let mut p = target_abs.parent()?.to_path_buf();
    loop {
        if let Ok(m) = fs::symlink_metadata(&p) {
            // 软链不算冲突（上面已经按软链跳过）；只认「普通文件占位」
            if m.is_file() {
                return Some(p);
            }
        }
        if p == *home_real {
            return None;
        }
        match p.parent() {
            Some(parent) if parent.starts_with(home_real) => p = parent.to_path_buf(),
            _ => return None,
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

    /// 测试夹具的备份是「固定时间戳的旧备份」→ 这些用例测的不是年龄 → 放开年龄闸门。
    const AGE_UNLIMITED: u32 = u32::MAX;

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
            AGE_UNLIMITED,
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
            AGE_UNLIMITED,
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
            AGE_UNLIMITED,
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
            AGE_UNLIMITED,
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
            AGE_UNLIMITED,
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
            AGE_UNLIMITED,
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
            AGE_UNLIMITED,
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
            AGE_UNLIMITED,
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
            AGE_UNLIMITED,
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
            AGE_UNLIMITED,
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
            AGE_UNLIMITED,
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
            AGE_UNLIMITED,
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
            AGE_UNLIMITED,
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
            AGE_UNLIMITED,
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
            AGE_UNLIMITED,
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
            AGE_UNLIMITED,
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
            AGE_UNLIMITED,
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
            AGE_UNLIMITED,
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
            AGE_UNLIMITED,
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
            AGE_UNLIMITED,
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
            AGE_UNLIMITED,
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
            AGE_UNLIMITED,
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

        let err = restore_backup_in_at(&dir, "demo", &home, &[], &backup_root, AGE_UNLIMITED)
            .unwrap_err();
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
            AGE_UNLIMITED,
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
            AGE_UNLIMITED,
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
            AGE_UNLIMITED,
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
            AGE_UNLIMITED,
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
            let err = restore_backup_in_at(
                &dir,
                "demo",
                &home,
                &[raw.clone()],
                &backup_root,
                AGE_UNLIMITED,
            )
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
            "createdAt": "2026-01-01T00:00:00Z",
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
            AGE_UNLIMITED,
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
            AGE_UNLIMITED,
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
            "createdAt": "2026-01-01T00:00:00Z",
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
            AGE_UNLIMITED,
        )
        .unwrap_err();
        assert_eq!(err.code, "RESTORE_TARGET_NOT_ALLOWED", "{err:?}");
        assert!(!home.join("cfg.json").exists(), "must not touch home");

        // ② target 精确等于白名单路径 → 允许（写成一个文件）
        let manifest = serde_json::json!({
            "schema": MANIFEST_SCHEMA,
            "appId": "demo",
            "createdAt": "2026-01-01T00:00:00Z",
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
            AGE_UNLIMITED,
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
            AGE_UNLIMITED,
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
            AGE_UNLIMITED,
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
            AGE_UNLIMITED,
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
            AGE_UNLIMITED,
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
            AGE_UNLIMITED,
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
            AGE_UNLIMITED,
        )
        .unwrap_err();
        assert_eq!(err.code, "RESTORE_MANIFEST_TOO_LARGE", "{err:?}");
        assert!(err.human.contains("too large"), "{err:?}");
        assert_eq!(fs::read_dir(&home).unwrap().count(), 0);
    }

    /// ISO-8601 解析：UTC 准；非 UTC / 坏格式 → `None`（年龄未知 → 拒绝）。
    #[test]
    fn parses_iso8601_utc_only() {
        assert_eq!(
            parse_iso8601_ms("2026-01-01T00:00:00Z"),
            Some(1_767_225_600_000)
        );
        // 小数秒也算 UTC
        assert_eq!(
            parse_iso8601_ms("2026-01-01T00:00:00.250Z"),
            Some(1_767_225_600_250)
        );
        // 非 UTC → 不猜（fail-closed：猜错年龄 = 放行过期备份）
        assert_eq!(parse_iso8601_ms("2026-01-01T00:00:00+08:00"), None);
        assert_eq!(parse_iso8601_ms("2026-01-01T00:00:00"), None);
        assert_eq!(parse_iso8601_ms("not-a-time"), None);
        assert_eq!(parse_iso8601_ms(""), None);
        assert_eq!(parse_iso8601_ms("2026-13-01T00:00:00Z"), None);
    }

    /// 备份年龄：小时向下取整；未知时间 → 0。
    #[test]
    fn computes_backup_age_in_hours() {
        let two_hours_ago = now_ms().saturating_sub(2 * 3_600_000);
        assert_eq!(age_hours_since(two_hours_ago), 2);
        assert_eq!(age_hours_since(0), 0);
        // 未来时间（时钟回拨）→ 不为负
        assert_eq!(age_hours_since(now_ms() + 3_600_000), 0);
    }

    // ── v0.1.20：预览（计划）+ 执行共用同一份判定 ────────────────────────

    /// 造一个**指定创建时间**的备份（清单 + 载荷）→ 返回备份目录。
    fn make_backup_at(
        root: &Path,
        home: &Path,
        app_id: &str,
        created_at: &str,
        files: &[(&str, &str)],
    ) -> PathBuf {
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
            "createdAt": created_at,
            "homeHash": home_fingerprint(home),
            "entries": entries,
        });
        write(
            &dir.join("manifest.json"),
            &serde_json::to_string(&manifest).unwrap(),
        );
        dir
    }

    /// ① 白名单为空 → 拒绝（预览与执行都是 `RESTORE_NO_WHITELIST`，且不读不写）。
    #[test]
    fn plan_and_restore_refuse_empty_whitelist() {
        let root = tmpdir("plan-no-wl");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let home = fs::canonicalize(&home).unwrap();
        fs::create_dir_all(home.join("appdata")).unwrap();
        let backup_root = root.join("backup");
        fs::create_dir_all(&backup_root).unwrap();
        let dir = make_backup_at(
            &backup_root,
            &home,
            "demo",
            &crate::fsops::now_iso8601(),
            &[("appdata/x.txt", "x")],
        );

        let err = plan_restore_in_at(&dir, "demo", &home, &[], &backup_root, 24).unwrap_err();
        assert_eq!(err.code, "RESTORE_NO_WHITELIST", "{err:?}");
        let err = restore_backup_in_at(&dir, "demo", &home, &[], &backup_root, 24).unwrap_err();
        assert_eq!(err.code, "RESTORE_NO_WHITELIST", "{err:?}");
        assert_eq!(
            fs::read_dir(&home).unwrap().count(),
            1,
            "must not touch home"
        );
    }

    /// ② 清单 target 越界 → **整体拒绝**（预览与执行一致；不写任何东西）。
    #[test]
    fn plan_and_restore_refuse_escaping_target() {
        let root = tmpdir("plan-escape");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let home = fs::canonicalize(&home).unwrap();
        fs::create_dir_all(home.join("appdata")).unwrap();
        let backup_root = root.join("backup");
        fs::create_dir_all(&backup_root).unwrap();
        let dir = backup_root.join("20260101000000/demo");
        fs::create_dir_all(&dir).unwrap();
        let body = "pwn";
        write(&dir.join("payload.txt"), body);
        let manifest = serde_json::json!({
            "schema": MANIFEST_SCHEMA,
            "appId": "demo",
            "createdAt": crate::fsops::now_iso8601(),
            "homeHash": home_fingerprint(&home),
            "entries": [json_entry("payload.txt", "../escaped.txt", body.len() as u64, &sha256_str(body), false)],
        });
        write(
            &dir.join("manifest.json"),
            &serde_json::to_string(&manifest).unwrap(),
        );

        let err = plan_restore_in_at(&dir, "demo", &home, &wl(&home, "appdata"), &backup_root, 24)
            .unwrap_err();
        assert_eq!(err.code, "RESTORE_TARGET_NOT_ALLOWED", "{err:?}");
        let err =
            restore_backup_in_at(&dir, "demo", &home, &wl(&home, "appdata"), &backup_root, 24)
                .unwrap_err();
        assert_eq!(err.code, "RESTORE_TARGET_NOT_ALLOWED", "{err:?}");
        assert!(!root.join("escaped.txt").exists(), "must not escape home");
        assert_eq!(
            fs::read_dir(&home).unwrap().count(),
            1,
            "a rejected manifest must not touch home"
        );
    }

    /// ③ home 指纹不符 → 拒绝（预览与执行一致；不写任何东西）。
    #[test]
    fn plan_and_restore_refuse_foreign_home() {
        let root = tmpdir("plan-home");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let home = fs::canonicalize(&home).unwrap();
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
            "createdAt": crate::fsops::now_iso8601(),
            "homeHash": sha256_str("some-other-home"),
            "entries": [json_entry("payload.txt", "appdata/x.txt", body.len() as u64, &sha256_str(body), false)],
        });
        write(
            &dir.join("manifest.json"),
            &serde_json::to_string(&manifest).unwrap(),
        );

        let err = plan_restore_in_at(&dir, "demo", &home, &wl(&home, "appdata"), &backup_root, 24)
            .unwrap_err();
        assert_eq!(err.code, "RESTORE_HOME_MISMATCH", "{err:?}");
        let err =
            restore_backup_in_at(&dir, "demo", &home, &wl(&home, "appdata"), &backup_root, 24)
                .unwrap_err();
        assert_eq!(err.code, "RESTORE_HOME_MISMATCH", "{err:?}");
        assert_eq!(
            fs::read_dir(&home).unwrap().count(),
            1,
            "must not touch home"
        );
    }

    /// ④ 超龄备份 → `RESTORE_BACKUP_TOO_OLD`（预览与执行一致；不写任何东西）。
    #[test]
    fn refuses_backup_older_than_max_age() {
        let root = tmpdir("plan-too-old");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let home = fs::canonicalize(&home).unwrap();
        fs::create_dir_all(home.join("appdata")).unwrap();
        let backup_root = root.join("backup");
        fs::create_dir_all(&backup_root).unwrap();
        // 3 天前的备份
        let dir = make_backup_at(
            &backup_root,
            &home,
            "demo",
            "2026-01-01T00:00:00Z",
            &[("appdata/x.txt", "OLD")],
        );

        let err = plan_restore_in_at(&dir, "demo", &home, &wl(&home, "appdata"), &backup_root, 24)
            .unwrap_err();
        assert_eq!(err.code, "RESTORE_BACKUP_TOO_OLD", "{err:?}");
        let err =
            restore_backup_in_at(&dir, "demo", &home, &wl(&home, "appdata"), &backup_root, 24)
                .unwrap_err();
        assert_eq!(err.code, "RESTORE_BACKUP_TOO_OLD", "{err:?}");
        assert!(!home.join("appdata/x.txt").exists(), "must not touch home");

        // 刚做的备份 → 允许（24h 窗口内）
        let fresh = make_backup_at(
            &backup_root,
            &home,
            "fresh",
            &crate::fsops::now_iso8601(),
            &[("appdata/y.txt", "NEW")],
        );
        let plan = plan_restore_in_at(
            &fresh,
            "fresh",
            &home,
            &wl(&home, "appdata"),
            &backup_root,
            24,
        )
        .unwrap();
        assert_eq!(plan.entries.len(), 1, "{plan:?}");
        assert_eq!(plan.max_age_hours, 24);
    }

    /// 清单没写创建时间 → 年龄未知 → **按超龄拒绝**（fail-closed）。
    #[test]
    fn refuses_backup_without_creation_time() {
        let root = tmpdir("plan-no-time");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let home = fs::canonicalize(&home).unwrap();
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
            "homeHash": home_fingerprint(&home),
            "entries": [json_entry("payload.txt", "appdata/x.txt", body.len() as u64, &sha256_str(body), false)],
        });
        write(
            &dir.join("manifest.json"),
            &serde_json::to_string(&manifest).unwrap(),
        );
        let err =
            restore_backup_in_at(&dir, "demo", &home, &wl(&home, "appdata"), &backup_root, 24)
                .unwrap_err();
        assert_eq!(err.code, "RESTORE_BACKUP_TOO_OLD", "{err:?}");
        assert!(!home.join("appdata/x.txt").exists());
    }

    /// ⑥ 计划摘要稳定：同输入 → 同 digest；输入变了 → digest 变了。
    #[test]
    fn plan_digest_is_stable_and_input_sensitive() {
        let root = tmpdir("plan-digest");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let home = fs::canonicalize(&home).unwrap();
        fs::create_dir_all(home.join("appdata")).unwrap();
        let backup_root = root.join("backup");
        fs::create_dir_all(&backup_root).unwrap();
        let dir = make_backup_at(
            &backup_root,
            &home,
            "demo",
            &crate::fsops::now_iso8601(),
            &[("appdata/x.txt", "x")],
        );

        let p1 = plan_restore_in_at(&dir, "demo", &home, &wl(&home, "appdata"), &backup_root, 24)
            .unwrap();
        let p2 = plan_restore_in_at(&dir, "demo", &home, &wl(&home, "appdata"), &backup_root, 24)
            .unwrap();
        assert_eq!(
            p1.digest, p2.digest,
            "same input must yield the same digest"
        );
        assert_eq!(p1.digest.len(), 64, "digest must be a sha256 hex");

        // 载荷换了 → 清单摘要变了 → digest 变了
        write(&dir.join("appdata/x.txt"), "y");
        let manifest = serde_json::json!({
            "schema": MANIFEST_SCHEMA,
            "appId": "demo",
            "createdAt": crate::fsops::now_iso8601(),
            "homeHash": home_fingerprint(&home),
            "entries": [json_entry("appdata/x.txt", "appdata/x.txt", 1, &sha256_str("y"), false)],
        });
        write(
            &dir.join("manifest.json"),
            &serde_json::to_string(&manifest).unwrap(),
        );
        let p3 = plan_restore_in_at(&dir, "demo", &home, &wl(&home, "appdata"), &backup_root, 24)
            .unwrap();
        assert_ne!(
            p1.digest, p3.digest,
            "a changed manifest must change the digest"
        );
    }

    /// 摘要不符 → `RESTORE_PLAN_CHANGED`（**不写任何东西**；预览后清单被改过）。
    #[test]
    fn refuses_when_plan_digest_changed() {
        let root = tmpdir("plan-changed");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let home = fs::canonicalize(&home).unwrap();
        fs::create_dir_all(home.join("appdata")).unwrap();
        let backup_root = root.join("backup");
        fs::create_dir_all(&backup_root).unwrap();
        let dir = make_backup_at(
            &backup_root,
            &home,
            "demo",
            &crate::fsops::now_iso8601(),
            &[("appdata/x.txt", "x")],
        );
        let err = run_restore(
            &dir,
            "demo",
            &home,
            &wl(&home, "appdata"),
            &backup_root,
            24,
            Some("0000000000000000000000000000000000000000000000000000000000000000"),
        )
        .unwrap_err();
        assert_eq!(err.code, "RESTORE_PLAN_CHANGED", "{err:?}");
        assert!(
            !home.join("appdata/x.txt").exists(),
            "a changed plan must not write anything"
        );
    }

    /// ⑦ 备份列表：v1 / legacy / unknown 三种格式 + `restorable` 判定。
    #[test]
    fn lists_backups_with_format_and_restorable_flag() {
        let root = tmpdir("plan-list");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let home = fs::canonicalize(&home).unwrap();
        fs::create_dir_all(home.join("appdata")).unwrap();
        let backup_root = root.join("backup");
        fs::create_dir_all(&backup_root).unwrap();

        // v1（新鲜 + home 指纹一致）→ 可还原
        let v1 = make_backup_at(
            &backup_root,
            &home,
            "v1app",
            &crate::fsops::now_iso8601(),
            &[("appdata/x.txt", "x")],
        );
        // v1（别的 home）→ 不可还原
        let other = backup_root.join("20260101000000/otherhome");
        fs::create_dir_all(&other).unwrap();
        write(&other.join("payload.txt"), "x");
        let manifest = serde_json::json!({
            "schema": MANIFEST_SCHEMA,
            "appId": "otherhome",
            "createdAt": crate::fsops::now_iso8601(),
            "homeHash": sha256_str("another-home"),
            "entries": [json_entry("payload.txt", "appdata/x.txt", 1, &sha256_str("x"), false)],
        });
        write(
            &other.join("manifest.json"),
            &serde_json::to_string(&manifest).unwrap(),
        );
        // legacy（旧 schema 的清单）→ 不可还原
        let legacy = backup_root.join("20260101000000/legacyapp");
        fs::create_dir_all(&legacy).unwrap();
        write(&legacy.join("data.zip"), "old zip");
        write(
            &legacy.join("manifest.json"),
            r#"{"schema":"aibox-backup/v1","appId":"legacyapp","entries":[]}"#,
        );
        // unknown（没有清单）→ 不可还原
        let unknown = backup_root.join("20260101000000/unknownapp");
        fs::create_dir_all(&unknown).unwrap();
        write(&unknown.join("data.zip"), "old zip");
        // 保全目录不是备份 → 不该出现在列表里
        let pres = backup_root.join("pre-restore-20260101000000/v1app");
        fs::create_dir_all(&pres).unwrap();

        let list = list_backups_in(&backup_root, &home, 24).unwrap();
        let by_app = |id: &str| list.iter().find(|r| r.app_id == id).cloned();
        let v1rec = by_app("v1app").expect("v1 backup must be listed");
        assert_eq!(v1rec.format, "v1");
        assert!(v1rec.restorable, "{v1rec:?}");
        assert_eq!(v1rec.file_count, 1);
        assert_eq!(v1rec.total_bytes, 1);
        assert_eq!(v1rec.dir, v1.to_string_lossy());
        assert!(v1rec.age_hours < 24, "{v1rec:?}");

        let otherrec = by_app("otherhome").expect("foreign-home backup must be listed");
        assert_eq!(otherrec.format, "v1");
        assert!(
            !otherrec.restorable,
            "a foreign home must not be restorable"
        );

        let legacyrec = by_app("legacyapp").expect("legacy backup must be listed");
        assert_eq!(legacyrec.format, "legacy");
        assert!(
            !legacyrec.restorable,
            "a legacy backup must not be restorable"
        );
        assert_eq!(legacyrec.file_count, 0);

        let unknownrec = by_app("unknownapp").expect("unknown backup must be listed");
        assert_eq!(unknownrec.format, "unknown");
        assert!(
            !unknownrec.restorable,
            "an unknown backup must not be restorable"
        );

        // 保全目录不得出现在列表里（它不是备份）
        assert!(
            !list.iter().any(|r| r.dir.contains("pre-restore-")),
            "{list:?}"
        );
    }

    /// ⑦（续）超龄的 v1 备份 → `restorable = false`。
    #[test]
    fn list_marks_outdated_backup_as_not_restorable() {
        let root = tmpdir("plan-list-old");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let home = fs::canonicalize(&home).unwrap();
        let backup_root = root.join("backup");
        fs::create_dir_all(&backup_root).unwrap();
        make_backup_at(
            &backup_root,
            &home,
            "oldapp",
            "2026-01-01T00:00:00Z",
            &[("appdata/x.txt", "x")],
        );
        let list = list_backups_in(&backup_root, &home, 24).unwrap();
        assert_eq!(list.len(), 1, "{list:?}");
        assert_eq!(list[0].format, "v1");
        assert!(!list[0].restorable, "{list:?}");
        assert!(list[0].age_hours > 24, "{list:?}");
    }

    /// ⑧ 预览与执行**同一份判定**：计划里的 target 集合 == 执行处理到的 target 集合。
    #[test]
    fn plan_and_restore_agree_on_targets() {
        let root = tmpdir("plan-same");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let home = fs::canonicalize(&home).unwrap();
        fs::create_dir_all(home.join("appdata")).unwrap();
        let backup_root = root.join("backup");
        fs::create_dir_all(&backup_root).unwrap();
        // 一个好文件 + 一个载荷缺失的文件（执行期会被跳过）
        let dir = backup_root.join("20260101000000/demo");
        fs::create_dir_all(&dir).unwrap();
        let good = "GOOD";
        write(&dir.join("good.txt"), good);
        let manifest = serde_json::json!({
            "schema": MANIFEST_SCHEMA,
            "appId": "demo",
            "createdAt": crate::fsops::now_iso8601(),
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

        let plan = plan_restore_in_at(&dir, "demo", &home, &wl(&home, "appdata"), &backup_root, 24)
            .unwrap();
        let out =
            restore_backup_in_at(&dir, "demo", &home, &wl(&home, "appdata"), &backup_root, 24)
                .unwrap();

        let planned: std::collections::BTreeSet<String> = plan
            .entries
            .iter()
            .filter(|e| !e.is_dir)
            .map(|e| e.target.clone())
            .collect();
        let mut touched: std::collections::BTreeSet<String> =
            out.restored_paths.iter().cloned().collect();
        for issue in out.skipped.iter().chain(out.failed.iter()) {
            touched.insert(issue.target.clone());
        }
        assert_eq!(
            planned, touched,
            "preview and execution must see the same targets"
        );
        assert_eq!(out.restored, 1, "{out:?}");
        assert_eq!(out.failed.len(), 1, "{out:?}");
        assert_eq!(out.failed[0].code, ISSUE_SOURCE_MISSING, "{out:?}");
        assert_eq!(
            out.failed[0].target,
            home.join("appdata/missing.txt").to_string_lossy()
        );
    }

    /// 逐条原因码：软链 / 摘要不符 / 类型冲突 / 父路径冲突（固定枚举，UI 逐个映射）。
    #[test]
    fn per_entry_issue_codes_are_fixed() {
        use std::os::unix::fs::symlink;

        let root = tmpdir("plan-codes");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let home = fs::canonicalize(&home).unwrap();
        fs::create_dir_all(home.join("appdata")).unwrap();
        let backup_root = root.join("backup");
        fs::create_dir_all(&backup_root).unwrap();
        let dir = backup_root.join("20260101000000/demo");
        fs::create_dir_all(&dir).unwrap();

        // ① 摘要不符（载荷被改）
        write(&dir.join("tampered.txt"), "TAMPERED");
        // ② 父路径冲突（appdata/conflict 是文件，却要写 appdata/conflict/x.txt）
        write(&home.join("appdata/conflict"), "I AM A FILE");
        // ③ 目标路径上有软链
        let outside = root.join("outside");
        fs::create_dir_all(&outside).unwrap();
        symlink(&outside, home.join("appdata/link")).unwrap();

        let manifest = serde_json::json!({
            "schema": MANIFEST_SCHEMA,
            "appId": "demo",
            "createdAt": crate::fsops::now_iso8601(),
            "homeHash": home_fingerprint(&home),
            "entries": [
                // 清单声明的摘要是「原始内容」的 → 载荷被改过 → sha_mismatch
                json_entry("tampered.txt", "appdata/tampered.txt", 8, &sha256_str("ORIGINAL"), false),
                json_entry("tampered.txt", "appdata/conflict/x.txt", 8, &sha256_str("ORIGINAL"), false),
                json_entry("tampered.txt", "appdata/link/leak.txt", 8, &sha256_str("ORIGINAL"), false),
            ],
        });
        write(
            &dir.join("manifest.json"),
            &serde_json::to_string(&manifest).unwrap(),
        );

        let out =
            restore_backup_in_at(&dir, "demo", &home, &wl(&home, "appdata"), &backup_root, 24)
                .unwrap();
        assert_eq!(out.restored, 0, "{out:?}");
        let mut codes: Vec<&str> = out
            .skipped
            .iter()
            .chain(out.failed.iter())
            .map(|i| i.code.as_str())
            .collect();
        codes.sort();
        assert_eq!(
            codes,
            vec![ISSUE_PARENT_CONFLICT, ISSUE_SHA_MISMATCH, ISSUE_SYMLINK,],
            "{out:?}"
        );
        assert!(
            !outside.join("leak.txt").exists(),
            "must not write through the symlink"
        );
    }
    // ── 备份侧：junction / 越界目录 + 限额（manage.rs 里断言） ──────────

    // ══ 第三轮独立安全审计的回归用例 ══════════════════════════════════

    /// 造一条计划条目（摘要单测用）。
    fn planned(target: &str, rel: &str, size: u64, sha: &str, is_dir: bool) -> PlannedEntry {
        PlannedEntry {
            target: target.to_string(),
            rel: rel.to_string(),
            payload_rel: rel.to_string(),
            size,
            is_dir,
            exists: false,
            payload_present: true,
            sha256: sha.to_string(),
        }
    }

    /// ① P1-1：`isDir` 翻转 → 摘要**必须**不同（否则「预览 = 执行」不变式被破）。
    #[test]
    fn plan_digest_changes_when_a_directory_becomes_a_file() {
        let dir = planned("/home/appdata/A", "appdata/A", 7, "aa", true);
        let file = planned("/home/appdata/A", "appdata/A", 7, "aa", false);
        assert_ne!(
            plan_digest(&[dir.clone()]),
            plan_digest(&[file.clone()]),
            "isDir must be part of the digest"
        );
        // 其它字段同样必须进摘要（逐个翻转都要变）
        let other_rel = planned("/home/appdata/A", "appdata/B", 7, "aa", true);
        assert_ne!(plan_digest(&[dir.clone()]), plan_digest(&[other_rel]));
        let other_size = planned("/home/appdata/A", "appdata/A", 8, "aa", true);
        assert_ne!(plan_digest(&[dir.clone()]), plan_digest(&[other_size]));
        let other_sha = planned("/home/appdata/A", "appdata/A", 7, "bb", true);
        assert_ne!(plan_digest(&[dir.clone()]), plan_digest(&[other_sha]));
        let other_target = planned("/home/appdata/B", "appdata/A", 7, "aa", true);
        assert_ne!(plan_digest(&[dir.clone()]), plan_digest(&[other_target]));
        // 相同输入 → 相同摘要（不能变成随机）
        assert_eq!(plan_digest(&[dir.clone()]), plan_digest(&[dir]));
    }

    /// ② P1-1：条目数不同的两份清单 → 摘要**必须**不同（换行注入的老坑）。
    #[test]
    fn plan_digest_differs_when_the_entry_count_differs() {
        let one = planned("/home/appdata/A", "appdata/A", 1, "aa", false);
        let two = planned("/home/appdata/B", "appdata/B", 2, "bb", false);
        assert_ne!(
            plan_digest(&[one.clone()]),
            plan_digest(&[one.clone(), two.clone()])
        );
        // 端到端：清单多一条条目 → 预览摘要必须变
        let root = tmpdir("digest-count");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let home = fs::canonicalize(&home).unwrap();
        fs::create_dir_all(home.join("appdata")).unwrap();
        let backup_root = root.join("backup");
        fs::create_dir_all(&backup_root).unwrap();
        let dir = backup_root.join("20260101000000/demo");
        fs::create_dir_all(&dir).unwrap();
        write(&dir.join("a.txt"), "a");
        write(&dir.join("b.txt"), "b");
        let manifest = serde_json::json!({
            "schema": MANIFEST_SCHEMA,
            "appId": "demo",
            "createdAt": crate::fsops::now_iso8601(),
            "homeHash": home_fingerprint(&home),
            "entries": [json_entry("a.txt", "appdata/a.txt", 1, &sha256_str("a"), false)],
        });
        write(
            &dir.join("manifest.json"),
            &serde_json::to_string(&manifest).unwrap(),
        );
        let p1 = plan_restore_in_at(&dir, "demo", &home, &wl(&home, "appdata"), &backup_root, 24)
            .unwrap();
        let manifest2 = serde_json::json!({
            "schema": MANIFEST_SCHEMA,
            "appId": "demo",
            "createdAt": crate::fsops::now_iso8601(),
            "homeHash": home_fingerprint(&home),
            "entries": [
                json_entry("a.txt", "appdata/a.txt", 1, &sha256_str("a"), false),
                json_entry("b.txt", "appdata/b.txt", 1, &sha256_str("b"), false),
            ],
        });
        write(
            &dir.join("manifest.json"),
            &serde_json::to_string(&manifest2).unwrap(),
        );
        let p2 = plan_restore_in_at(&dir, "demo", &home, &wl(&home, "appdata"), &backup_root, 24)
            .unwrap();
        assert_ne!(p1.digest, p2.digest, "条目数不同 → 摘要必须不同");
    }

    /// 复核 2026-11-23：计划条目与清单条目**按序号对齐** → 顺序变了就是另一个计划。
    /// 旧实现先排序再算摘要 → 清单换顺序但摘要不变 → 用户看到的清单与实际写入顺序不一致。
    #[test]
    fn plan_digest_changes_when_the_entry_order_changes() {
        let a = planned("/home/appdata/a.txt", "appdata/a.txt", 1, "aa", false);
        let b = planned("/home/appdata/b.txt", "appdata/b.txt", 1, "bb", false);
        assert_ne!(
            plan_digest(&[a.clone(), b.clone()]),
            plan_digest(&[b.clone(), a.clone()]),
            "条目顺序不同 → 摘要必须不同"
        );
        // 载荷路径（读备份里的哪个文件）也必须进摘要
        let mut other_payload = a.clone();
        other_payload.payload_rel = "other.txt".to_string();
        assert_ne!(
            plan_digest(&[a.clone()]),
            plan_digest(&[other_payload]),
            "载荷路径不同 → 摘要必须不同"
        );
    }

    /// 复核 2026-11-23：同一 target 出现两次 → 清单自相矛盾（谁先写谁后写会改变落盘内容）
    /// → 当作损坏清单 → 整体拒绝。
    #[test]
    fn rejects_manifest_with_duplicate_targets() {
        let root = tmpdir("dup-target");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let home = fs::canonicalize(&home).unwrap();
        fs::create_dir_all(home.join("appdata")).unwrap();
        let backup_root = root.join("backup");
        fs::create_dir_all(&backup_root).unwrap();
        let dir = backup_root.join("20260101000000/demo");
        fs::create_dir_all(&dir).unwrap();
        write(&dir.join("a.txt"), "a");
        write(&dir.join("b.txt"), "b");
        let manifest = serde_json::json!({
            "schema": MANIFEST_SCHEMA,
            "appId": "demo",
            "createdAt": crate::fsops::now_iso8601(),
            "homeHash": home_fingerprint(&home),
            "entries": [
                json_entry("a.txt", "appdata/same.txt", 1, &sha256_str("a"), false),
                json_entry("b.txt", "appdata/same.txt", 1, &sha256_str("b"), false),
            ],
        });
        write(
            &dir.join("manifest.json"),
            &serde_json::to_string(&manifest).unwrap(),
        );
        let err = plan_restore_in_at(&dir, "demo", &home, &wl(&home, "appdata"), &backup_root, 24)
            .unwrap_err();
        assert_eq!(err.code, "RESTORE_NO_MANIFEST");
        assert!(
            !home.join("appdata/same.txt").exists(),
            "重复 target 的清单必须一个字都不写"
        );
    }

    /// 复核 2026-11-23：**内核自己夹紧**最大可还原时长 → 调用方（即使传 168）也不能放宽。
    #[test]
    fn production_entry_clamps_the_max_age() {
        let root = tmpdir("clamp-age");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let home = fs::canonicalize(&home).unwrap();
        fs::create_dir_all(home.join("appdata")).unwrap();
        let backup_root = root.join("backup");
        fs::create_dir_all(&backup_root).unwrap();
        let dir = backup_root.join("20260101000000/demo");
        fs::create_dir_all(&dir).unwrap();
        write(&dir.join("a.txt"), "a");
        // 清单里的创建时间很旧（远早于 24 小时窗口）
        let old = "2020-01-01T00:00:00Z".to_string();
        let manifest = serde_json::json!({
            "schema": MANIFEST_SCHEMA,
            "appId": "demo",
            "createdAt": old,
            "homeHash": home_fingerprint(&home),
            "entries": [json_entry("a.txt", "appdata/a.txt", 1, &sha256_str("a"), false)],
        });
        write(
            &dir.join("manifest.json"),
            &serde_json::to_string(&manifest).unwrap(),
        );
        // 调用方传 168 小时（7 天）→ 内核必须夹回 24 → 旧备份被拒
        let err = plan_restore_in_at(&dir, "demo", &home, &wl(&home, "appdata"), &backup_root, 168)
            .unwrap_err();
        assert_eq!(err.code, "RESTORE_BACKUP_TOO_OLD");
        assert!(
            !home.join("appdata/a.txt").exists(),
            "超龄备份必须一个字都不写"
        );
        // 生产入口（备份根固定）走的是同一个夹紧函数
        assert_eq!(effective_max_age_hours(Some(168)), MAX_RESTORE_AGE_HOURS);
        assert_eq!(effective_max_age_hours(Some(24)), MAX_RESTORE_AGE_HOURS);
        assert_eq!(effective_max_age_hours(Some(6)), 6);
    }

    /// ③⑤ P1-1：清单里含控制字符（`\n` / `\r` / NUL）→ 清单损坏 → 整体拒绝。
    #[test]
    fn rejects_manifest_fields_with_control_characters() {
        let root = tmpdir("ctrl-chars");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let home = fs::canonicalize(&home).unwrap();
        fs::create_dir_all(home.join("appdata")).unwrap();
        let backup_root = root.join("backup");
        fs::create_dir_all(&backup_root).unwrap();
        let dir = backup_root.join("20260101000000/demo");
        fs::create_dir_all(&dir).unwrap();
        write(&dir.join("payload.txt"), "x");

        let cases: Vec<(&str, Value)> = vec![
            (
                "target with a newline",
                json_entry("payload.txt", "appdata/a\nb.txt", 1, &sha256_str("x"), false),
            ),
            (
                "rel with a newline",
                json_entry("pay\nload.txt", "appdata/a.txt", 1, &sha256_str("x"), false),
            ),
            (
                "sha256 with a newline",
                json_entry("payload.txt", "appdata/a.txt", 1, "aa\nbb", false),
            ),
            (
                "target with NUL",
                json_entry("payload.txt", "appdata/a\u{0}b.txt", 1, &sha256_str("x"), false),
            ),
        ];
        for (what, entry) in cases {
            let manifest = serde_json::json!({
                "schema": MANIFEST_SCHEMA,
                "appId": "demo",
                "createdAt": crate::fsops::now_iso8601(),
                "homeHash": home_fingerprint(&home),
                "entries": [entry],
            });
            write(
                &dir.join("manifest.json"),
                &serde_json::to_string(&manifest).unwrap(),
            );
            let err = plan_restore_in_at(
                &dir,
                "demo",
                &home,
                &wl(&home, "appdata"),
                &backup_root,
                24,
            )
            .unwrap_err();
            assert_eq!(err.code, "RESTORE_NO_MANIFEST", "{what}: {err:?}");
            let err = restore_backup_in_at(
                &dir,
                "demo",
                &home,
                &wl(&home, "appdata"),
                &backup_root,
                24,
            )
            .unwrap_err();
            assert_eq!(err.code, "RESTORE_NO_MANIFEST", "{what}: {err:?}");
        }
        assert!(!home.join("appdata/a.txt").exists(), "must not touch home");
    }

    /// ④ P1-1：目录条目的 `sha256` 非 64-hex（或含换行）→ 清单损坏 → 整体拒绝。
    #[test]
    fn rejects_directory_entries_with_an_invalid_sha256() {
        let root = tmpdir("dir-sha");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let home = fs::canonicalize(&home).unwrap();
        fs::create_dir_all(home.join("appdata")).unwrap();
        let backup_root = root.join("backup");
        fs::create_dir_all(&backup_root).unwrap();
        let dir = backup_root.join("20260101000000/demo");
        fs::create_dir_all(&dir).unwrap();

        for bad in ["B", "not-a-sha", "aa\nbb", &"a".repeat(63), &"z".repeat(64)] {
            let manifest = serde_json::json!({
                "schema": MANIFEST_SCHEMA,
                "appId": "demo",
                "createdAt": crate::fsops::now_iso8601(),
                "homeHash": home_fingerprint(&home),
                "entries": [json_entry("payload.txt", "appdata/sub", 0, bad, true)],
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
                24,
            )
            .unwrap_err();
            assert_eq!(err.code, "RESTORE_NO_MANIFEST", "sha={bad:?}: {err:?}");
        }
        // 合法形态：空串（我们自己的备份就是这么写的）与 64-hex 都放行
        for good in ["", &"a".repeat(64)] {
            let manifest = serde_json::json!({
                "schema": MANIFEST_SCHEMA,
                "appId": "demo",
                "createdAt": crate::fsops::now_iso8601(),
                "homeHash": home_fingerprint(&home),
                "entries": [json_entry("payload.txt", "appdata/sub", 0, good, true)],
            });
            write(
                &dir.join("manifest.json"),
                &serde_json::to_string(&manifest).unwrap(),
            );
            let plan = plan_restore_in_at(
                &dir,
                "demo",
                &home,
                &wl(&home, "appdata"),
                &backup_root,
                24,
            );
            assert!(plan.is_ok(), "sha={good:?} must be accepted: {plan:?}");
        }
    }

    /// P1-2：年龄闸门是 fail-closed 的（纯函数级：未来时间 / 目录很旧 / 两者都新）。
    #[test]
    fn age_gate_is_fail_closed() {
        let now = now_ms();
        let hour = 3_600_000u64;
        // ① 清单自称「未来的备份」→ 拒绝（年龄会算成 0 → 永远新鲜）
        let future = backup_age(now + 60 * 60 * 1000, Some(now));
        let err = check_backup_age(&future, 24).unwrap_err();
        assert_eq!(err.code, "RESTORE_BACKUP_TOO_OLD", "{err:?}");
        // ② 清单很旧 → 拒绝
        let old = backup_age(now - 100 * hour, Some(now));
        let err = check_backup_age(&old, 24).unwrap_err();
        assert_eq!(err.code, "RESTORE_BACKUP_TOO_OLD", "{err:?}");
        // ③ 清单说新、但目录很旧 → 拒绝（第二口径）
        let forged = backup_age(now - hour, Some(now - 100 * hour));
        let err = check_backup_age(&forged, 24).unwrap_err();
        assert_eq!(err.code, "RESTORE_BACKUP_TOO_OLD", "{err:?}");
        // ④ 清单说未来、目录也新 → 仍然拒绝（清单自述不可信）
        let forged2 = backup_age(now + 60 * 60 * 1000, Some(now - 100 * hour));
        assert!(check_backup_age(&forged2, 24).is_err());
        // ⑤ 时间未知 → 拒绝
        let unknown = backup_age(0, Some(now));
        let err = check_backup_age(&unknown, 24).unwrap_err();
        assert_eq!(err.code, "RESTORE_BACKUP_TOO_OLD", "{err:?}");
        // ⑥ 目录 mtime 取不到 → 拒绝
        let no_dir = backup_age(now - hour, None);
        assert!(check_backup_age(&no_dir, 24).is_err());
        // ⑦ 两者都新 → 放行
        let fresh = backup_age(now - hour, Some(now - 2 * hour));
        assert!(check_backup_age(&fresh, 24).is_ok());
        // ⑧ 刚好在窗口内 → 放行；超出一小时 → 拒绝
        let edge = backup_age(now - 24 * hour, Some(now - 24 * hour));
        assert!(check_backup_age(&edge, 24).is_ok());
        let over = backup_age(now - 25 * hour, Some(now - 25 * hour));
        assert!(check_backup_age(&over, 24).is_err());
    }

    /// P1-2（端到端）：清单自称「未来的备份」→ 整体拒绝（预览与执行一致）。
    #[test]
    fn refuses_backup_with_a_future_creation_time() {
        let root = tmpdir("future-time");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let home = fs::canonicalize(&home).unwrap();
        fs::create_dir_all(home.join("appdata")).unwrap();
        let backup_root = root.join("backup");
        fs::create_dir_all(&backup_root).unwrap();
        let dir = make_backup_at(
            &backup_root,
            &home,
            "demo",
            "2999-01-01T00:00:00Z",
            &[("appdata/x.txt", "x")],
        );
        let err = plan_restore_in_at(&dir, "demo", &home, &wl(&home, "appdata"), &backup_root, 24)
            .unwrap_err();
        assert_eq!(err.code, "RESTORE_BACKUP_TOO_OLD", "{err:?}");
        let err =
            restore_backup_in_at(&dir, "demo", &home, &wl(&home, "appdata"), &backup_root, 24)
                .unwrap_err();
        assert_eq!(err.code, "RESTORE_BACKUP_TOO_OLD", "{err:?}");
        assert!(!home.join("appdata/x.txt").exists(), "must not touch home");
        // 列表侧同口径 → 不可还原
        let list = list_backups_in(&backup_root, &home, 24).unwrap();
        assert_eq!(list.len(), 1, "{list:?}");
        assert!(!list[0].restorable, "{list:?}");
    }

    /// P1-2（端到端，Unix）：清单说新、但备份目录很旧 → 拒绝（第二口径生效）。
    #[cfg(unix)]
    #[test]
    fn refuses_backup_whose_folder_is_older_than_the_manifest() {
        let root = tmpdir("old-folder");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let home = fs::canonicalize(&home).unwrap();
        fs::create_dir_all(home.join("appdata")).unwrap();
        let backup_root = root.join("backup");
        fs::create_dir_all(&backup_root).unwrap();
        let dir = make_backup_at(
            &backup_root,
            &home,
            "demo",
            &crate::fsops::now_iso8601(),
            &[("appdata/x.txt", "x")],
        );
        // 把备份目录的 mtime 改成 100 天前（模拟「清单被改成新的、目录还是旧的」）
        let old = std::time::SystemTime::UNIX_EPOCH
            + std::time::Duration::from_millis(now_ms().saturating_sub(100 * 24 * 3_600_000));
        let f = fs::File::open(&dir).unwrap();
        f.set_times(fs::FileTimes::new().set_modified(old)).unwrap();
        drop(f);

        let err = restore_backup_in_at(&dir, "demo", &home, &wl(&home, "appdata"), &backup_root, 24)
            .unwrap_err();
        assert_eq!(err.code, "RESTORE_BACKUP_TOO_OLD", "{err:?}");
        assert!(!home.join("appdata/x.txt").exists(), "must not touch home");
        // 列表侧同口径 → 不可还原
        let list = list_backups_in(&backup_root, &home, 24).unwrap();
        assert_eq!(list.len(), 1, "{list:?}");
        assert!(!list[0].restorable, "{list:?}");
    }

    /// P1-4：registry **不能放宽** Rust 的 24h 硬上限。
    #[test]
    fn registry_cannot_widen_the_restore_window() {
        assert_eq!(MAX_RESTORE_AGE_HOURS, 24);
        // 声明 168h（registry 允许的最大值）→ 实际生效 24h
        assert_eq!(effective_max_age_hours(Some(168)), 24);
        assert_eq!(effective_max_age_hours(Some(25)), 24);
        assert_eq!(effective_max_age_hours(Some(48)), 24);
        // 往小里收是允许的
        assert_eq!(effective_max_age_hours(Some(1)), 1);
        assert_eq!(effective_max_age_hours(Some(12)), 12);
        // 缺失 / 越界 → 缺省 24
        assert_eq!(effective_max_age_hours(None), 24);
        assert_eq!(effective_max_age_hours(Some(0)), 24);
        assert_eq!(effective_max_age_hours(Some(9999)), 24);
    }

    /// P2-2：备份库里放一个指向库外的软链 → **列表不得把它交给 UI**。
    #[cfg(unix)]
    #[test]
    fn list_skips_backup_dirs_outside_the_backup_root() {
        use std::os::unix::fs::symlink;
        let root = tmpdir("list-escape");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let home = fs::canonicalize(&home).unwrap();
        let backup_root = root.join("backup");
        fs::create_dir_all(&backup_root).unwrap();

        // 库外做一个「备份」
        let outside = root.join("outside");
        let evil = outside.join("20260101000000/evilapp");
        fs::create_dir_all(&evil).unwrap();
        let manifest = serde_json::json!({
            "schema": MANIFEST_SCHEMA,
            "appId": "evilapp",
            "createdAt": crate::fsops::now_iso8601(),
            "homeHash": home_fingerprint(&home),
            "entries": [],
        });
        write(
            &evil.join("manifest.json"),
            &serde_json::to_string(&manifest).unwrap(),
        );
        // 库里放一个指向它的软链（模拟攻击者把库外目录挂进备份库）
        symlink(&outside.join("20260101000000"), backup_root.join("20260101000000")).unwrap();

        let list = list_backups_in(&backup_root, &home, 24).unwrap();
        assert!(
            !list.iter().any(|r| r.app_id == "evilapp"),
            "a backup outside the library must not be listed: {list:?}"
        );
        // 正常备份仍然要列出来（别把整个功能打死）
        //（用**另一个**时间戳目录：上面那个名字已被软链占用了）
        let ok = backup_root.join("20260101000001/goodapp");
        fs::create_dir_all(&ok).unwrap();
        let manifest = serde_json::json!({
            "schema": MANIFEST_SCHEMA,
            "appId": "goodapp",
            "createdAt": crate::fsops::now_iso8601(),
            "homeHash": home_fingerprint(&home),
            "entries": [],
        });
        write(
            &ok.join("manifest.json"),
            &serde_json::to_string(&manifest).unwrap(),
        );
        let list = list_backups_in(&backup_root, &home, 24).unwrap();
        assert!(list.iter().any(|r| r.app_id == "goodapp"), "{list:?}");
    }

    /// P2-3（一部分）：清单 appId 与目录名不一致 → 不可还原。
    #[test]
    fn list_marks_backup_with_mismatched_app_id_as_not_restorable() {
        let root = tmpdir("app-mismatch");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let home = fs::canonicalize(&home).unwrap();
        let backup_root = root.join("backup");
        fs::create_dir_all(&backup_root).unwrap();
        // 目录叫 "dirname"，清单却说自己是 "otherapp"
        let dir = backup_root.join("20260101000000/dirname");
        fs::create_dir_all(&dir).unwrap();
        let manifest = serde_json::json!({
            "schema": MANIFEST_SCHEMA,
            "appId": "otherapp",
            "createdAt": crate::fsops::now_iso8601(),
            "homeHash": home_fingerprint(&home),
            "entries": [],
        });
        write(
            &dir.join("manifest.json"),
            &serde_json::to_string(&manifest).unwrap(),
        );
        let list = list_backups_in(&backup_root, &home, 24).unwrap();
        assert_eq!(list.len(), 1, "{list:?}");
        assert_eq!(list[0].format, "v1", "{list:?}");
        assert!(
            !list[0].restorable,
            "a manifest from another app must not be restorable: {list:?}"
        );
    }

    /// P3-5：还原**新建**的目录是 0700（与文件 0600 齐平）；既有目录权限不动。
    #[cfg(unix)]
    #[test]
    fn restored_directories_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let root = tmpdir("dir-mode");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let home = fs::canonicalize(&home).unwrap();
        fs::create_dir_all(home.join("appdata")).unwrap();
        let backup_root = root.join("backup");
        fs::create_dir_all(&backup_root).unwrap();
        let dir = backup_root.join("20260101000000/demo");
        fs::create_dir_all(&dir).unwrap();
        write(&dir.join("payload.txt"), "x");
        let manifest = serde_json::json!({
            "schema": MANIFEST_SCHEMA,
            "appId": "demo",
            "createdAt": crate::fsops::now_iso8601(),
            "homeHash": home_fingerprint(&home),
            "entries": [json_entry("payload.txt", "appdata/sub", 0, "", true)],
        });
        write(
            &dir.join("manifest.json"),
            &serde_json::to_string(&manifest).unwrap(),
        );
        let out =
            restore_backup_in_at(&dir, "demo", &home, &wl(&home, "appdata"), &backup_root, 24)
                .unwrap();
        assert_eq!(out.restored, 0, "{out:?}");
        assert!(out.skipped.is_empty(), "{out:?}");
        let sub = home.join("appdata/sub");
        assert!(sub.is_dir(), "the directory must be created");
        let mode = fs::metadata(&sub).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "restored directories must be private");
    }
}
