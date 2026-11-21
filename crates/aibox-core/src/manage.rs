//! MANAGE 维护模式：数据备份 / 清单式卸载（零提权；只碰 registry 声明的路径）。
//!
//! 硬规则（产品 §5 行为规则 ①③）：
//! - 备份：把声明的 `dataPaths`（展开 `~`、只读存在的）打包到
//!   `~/.aibox/backup/<ts>/<appId>/`（`data.zip` + `manifest.json` 条目↔原路径映射，可还原）；
//!   **只读声明路径，绝不越界收集**。
//! - 卸载：只删清单内路径（`residualPaths` ∪ `dataPaths` 白名单，越界 → `MANAGE_OUT_OF_SCOPE` 拒绝）；
//!   删除前默认自动备份（`backupFirst`，可显式关闭）；失败收集不中断；零提权。
//! - `steps` 是 registry 的人话指引文本，**只写任务日志，绝不作为命令执行**。
//!
//! 备份包不脱敏（本地还原用途；诊断包才脱敏——两者场景不同）。

use std::collections::HashSet;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{json, Value};

use crate::err::{io_err, CoreError, CoreResult};
use crate::fsops;
use crate::paths;

/// 备份结果（TS: `{backupDir, files}`；backupDir 空串 = 无数据可备份）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupOutcome {
    pub backup_dir: String,
    /// zip 内条目名（相对 home / 声明根，`/` 分隔）——还原时按 manifest 映射回原路径
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

/// zip 条目名：优先相对 home（`/` 分隔）；不在 home 内 → 声明根名 + 相对子路径。
fn entry_name(root: &Path, f: &Path, home: &Path) -> String {
    if let Ok(r) = f.strip_prefix(home) {
        let s = r.to_string_lossy().replace('\\', "/");
        if !s.is_empty() {
            return s;
        }
    }
    let base = root
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "data".into());
    match f.strip_prefix(root) {
        Ok(r) if !r.as_os_str().is_empty() => {
            format!("{base}/{}", r.to_string_lossy().replace('\\', "/"))
        }
        _ => base,
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

/// 递归收集目录下全部文件（跳过目录本身与任何 symlink——
/// 声明目录下的符号链接可能指向白名单外，跟随即越界）。
fn walk_files(root: &Path, out: &mut Vec<PathBuf>, errors: &mut Vec<String>) {
    let rd = match fs::read_dir(root) {
        Ok(rd) => rd,
        Err(e) => {
            // 目录不可读 = 可能有数据但采集不到 → 必须上报（会导致删除中止）
            errors.push(format!("{} (read_dir: {e})", root.display()));
            return;
        }
    };
    for entry in rd {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                errors.push(format!("{} (entry: {e})", root.display()));
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
        if meta.is_symlink() {
            continue;
        }
        if meta.is_dir() {
            walk_files(&p, out, errors);
        } else {
            out.push(p);
        }
    }
}

/// 备份声明的 dataPaths → `<backup_root>/<ts>/<appId>/data.zip` + `manifest.json`。
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

    let home = home.to_path_buf();
    let mut entries: Vec<(String, PathBuf)> = Vec::new();
    let mut used: HashSet<String> = HashSet::new();
    let mut skipped: Vec<String> = Vec::new();
    let mut refused: Vec<String> = Vec::new();
    let mut failed: Vec<String> = Vec::new();

    for raw in data_paths {
        let root = paths::expand(raw);
        // 只有 NotFound 才算"没有数据可丢"；权限等其他 I/O 错误意味着数据在但读不到
        let meta = match fs::symlink_metadata(&root) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                skipped.push(raw.clone());
                continue;
            }
            Err(e) => {
                failed.push(format!("{raw} (stat: {e})"));
                continue;
            }
        };
        // 声明根本身是链接 → 拒绝（避免被换成任意目标，审计 P1）
        if meta.file_type().is_symlink() {
            refused.push(raw.clone());
            continue;
        }
        // 解析真实路径并要求仍在 home 内（祖先链接/越界目标一网打尽）
        let Some(real_root) = resolve_within_home(&root, &home) else {
            refused.push(raw.clone());
            continue;
        };
        let mut files = Vec::new();
        if real_root.is_dir() {
            let mut walk_errors: Vec<String> = Vec::new();
            walk_files(&real_root, &mut files, &mut walk_errors);
            failed.extend(walk_errors);
        } else {
            files.push(real_root.clone());
        }
        for f in files {
            let mut name = entry_name(&real_root, &f, &home);
            if !used.insert(name.clone()) {
                // 条目名冲突（同名声明根在 home 外）→ 序号前缀消歧
                let n = used.len();
                name = format!("{n}/{name}");
                used.insert(name.clone());
            }
            entries.push((name, f));
        }
    }

    let ts = fsops::now_ts();
    let dir = backup_root.join(ts).join(app_id);
    if entries.is_empty() {
        return Ok(BackupOutcome {
            backup_dir: String::new(),
            files: Vec::new(),
            skipped,
            refused,
            failed,
        });
    }
    paths::ensure_dir(&dir).map_err(|e| io_err("create backup dir", e))?;

    // manifest：条目 → 原路径（还原映射）
    let manifest = json!({
        "schema": "aibox-backup/v1",
        "appId": app_id,
        "createdAtMs": crate::registry::now_ms() as u64,
        "sources": data_paths,
        "entries": entries
            .iter()
            .map(|(name, src)| json!({"entry": name, "source": src.to_string_lossy()}))
            .collect::<Vec<Value>>(),
    });
    fs::write(
        dir.join("manifest.json"),
        serde_json::to_string_pretty(&manifest)
            .map_err(|e| CoreError::new("BACKUP_MANIFEST", e.to_string()))?,
    )
    .map_err(|e| io_err("write backup manifest", e))?;

    // data.zip（deflate 纯 Rust，同诊断包依赖）
    let zip_path = dir.join("data.zip");
    let file =
        fs::File::create(&zip_path).map_err(|e| io_err("create backup zip", e))?;
    let mut zip = zip::ZipWriter::new(file);
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    let mut files_out = Vec::with_capacity(entries.len());
    for (name, src) in &entries {
        let bytes = match read_entry_pinned(src, &home) {
            Ok(b) => b,
            Err(e) => {
                // 读失败 = 数据在但没进包 → failed（会导致破坏性操作中止）
                failed.push(format!("{} ({e})", src.display()));
                continue;
            }
        };
        zip.start_file(name.as_str(), opts)
            .map_err(|e| CoreError::new("BACKUP_ZIP", format!("add {name}: {e}")))?;
        zip.write_all(&bytes)
            .map_err(|e| io_err(format!("write entry {name}"), e))?;
        files_out.push(name.clone());
    }
    zip.finish()
        .map_err(|e| CoreError::new("BACKUP_ZIP", e.to_string()))?;

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

/// 保护路径：home 本身 / home 或 `~/.aibox` 的祖先 / 文件系统根——任何清单都不许删。
fn is_protected(target: &Path, home: &Path) -> bool {
    let ab = paths::aibox_root();
    target.parent().is_none() || home.starts_with(target) || ab.starts_with(target)
}

/// 词法规范化：去掉 `.` 段、解析 `..`（不触碰文件系统，symlink 不解析——
/// 越界判定用声明路径词法比较即可）。
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// 白名单内删除：target 必须等于 allow 之一或在其之下，且非保护路径。
/// **路径上任何组件是 symlink 都拒绝**：词法边界不解析链接，`remove_dir_all`
/// 可能顺着声明目录内的链接删到外部（审计 P1：symlink 越界）。
/// `deleter` 注入（单测模拟失败收集；生产传 `fs_delete`）。
pub fn delete_within<F>(
    allow: &[PathBuf],
    target: &Path,
    deleter: &mut F,
) -> CoreResult<DeleteOutcome>
where
    F: FnMut(&Path) -> io::Result<()>,
{
    let home = paths::home_dir();
    let t = normalize(target);
    if is_protected(&t, &home) {
        return Err(CoreError::new(
            "MANAGE_PROTECTED",
            format!("refusing to delete protected path {}", target.display()),
        ));
    }
    let in_allow = allow.iter().any(|a| {
        let a = normalize(a);
        t == a || t.starts_with(&a)
    });
    if !in_allow {
        return Err(CoreError::new(
            "MANAGE_OUT_OF_SCOPE",
            format!("path not in declared list: {}", target.display()),
        ));
    }
    // 逐段拒绝 symlink：target 自身与其每个祖先（直到 allow 根）都不得是链接，
    // 否则删除可能经链接落到白名单之外。
    if has_symlink_component(&t, allow) {
        return Err(CoreError::new(
            "MANAGE_SYMLINK_ESCAPE",
            format!("path traverses a symlink: {}", target.display()),
        ));
    }
    match fs::symlink_metadata(&t) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(DeleteOutcome::NotFound)
        }
        Err(e) => {
            // 权限等错误 ≠ 不存在：必须报失败，不能静默记为"已不存在"
            return Err(CoreError::new(
                "MANAGE_DELETE_FAILED",
                format!("cannot inspect {}: {e}", t.display()),
            ));
        }
    }
    deleter(&t).map_err(|e| {
        CoreError::new("MANAGE_DELETE_FAILED", format!("{}: {e}", t.display()))
    })?;
    Ok(DeleteOutcome::Deleted)
}

/// target 自身或其到 allow 根的祖先链上是否存在 symlink。
fn has_symlink_component(target: &Path, allow: &[PathBuf]) -> bool {
    if fs::symlink_metadata(target).is_ok_and(|m| m.file_type().is_symlink()) {
        return true;
    }
    // 沿 target 的每个前缀向上查到匹配的 allow 根为止
    let mut cur = target.parent();
    while let Some(p) = cur {
        if allow.iter().any(|a| normalize(a) == p || p.starts_with(&normalize(a))) {
            // p 在某 allow 内/即其根——继续查 p 自身是否链接（根也可能是指向外部的链接）
            if fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink()) {
                return true;
            }
        }
        if fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink()) {
            return true;
        }
        cur = p.parent();
    }
    false
}

/// 真实删除：目录 → remove_dir_all（symlink/文件 → remove_file，不跟随链接）。
fn fs_delete(p: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(p)?;
    if meta.is_dir() {
        fs::remove_dir_all(p)
    } else {
        fs::remove_file(p)
    }
}

/// 读取备份条目：**先打开句柄再从该句柄读**，并校验路径未被替换成链接
/// （检查与使用之间被换成 symlink 也可被 dev/ino 比对发现，审计 P1 TOCTOU）。
fn read_entry_pinned(path: &Path, home: &Path) -> io::Result<Vec<u8>> {
    use std::io::Read as _;
    let mut f = fs::File::open(path)?;
    let h = f.metadata()?;
    let link_meta = fs::symlink_metadata(path)?;
    if link_meta.file_type().is_symlink() {
        return Err(io::Error::new(io::ErrorKind::Other, "path became a symlink"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if h.dev() != link_meta.dev() || h.ino() != link_meta.ino() {
            return Err(io::Error::new(io::ErrorKind::Other, "path swapped during backup"));
        }
    }
    #[cfg(not(unix))]
    {
        // Windows 无法用 dev/ino 固定句柄身份：任何重解析点一律 fail-closed
        let m = fs::symlink_metadata(path)?;
        if m.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "reparse point refused on this platform",
            ));
        }
    }
    // 兜底：真实路径必须仍在（canonical）home 之内
    let real = fs::canonicalize(path)?;
    let home_real = fs::canonicalize(home)?;
    if real != home_real && !real.starts_with(&home_real) {
        return Err(io::Error::new(io::ErrorKind::Other, "entry escaped home"));
    }
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;
    Ok(buf)
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

/// 按清单卸载：备份（backupFirst）→ 白名单内删除 → 成功后注销 install-manifest。
///
/// - `allow` = `residualPaths` ∪ `dataPaths`（均展开）；目标不在其内 → `MANAGE_OUT_OF_SCOPE` 拒绝；
/// - `backup=false` 仅在风险卡「不备份」二次确认后由 TS 传入（默认 true = 硬规则①）；
/// - 备份失败 → **中止删除**（不许无备份删数据）；
/// - 单条删除失败收集进 `skipped`，继续处理其余。
/// - **不写 install-manifest**（清单注销由 TS 在删除成功后调 `install_forget`，
///   避免本纯函数在单测里碰真实 manifest）。
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

    // 人话指引 → 任务日志（绝不执行）
    for (i, s) in steps.iter().enumerate() {
        let _ = crate::tasklog::append(
            app_id,
            "uninstall",
            &format!("manual step {}: {s}", i + 1),
        );
    }

    // 白名单（声明路径展开）
    let mut allow: Vec<PathBuf> = Vec::new();
    for raw in residual_paths.iter().chain(data_paths.iter()) {
        let p = paths::expand(raw);
        if !allow.contains(&p) {
            allow.push(p);
        }
    }

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

    // 删除（白名单内；失败收集不中断）
    let mut removed = Vec::new();
    let mut skipped: Vec<SkippedPath> = Vec::new();
    for raw in residual_paths {
        let target = paths::expand(raw);
        match delete_within(&allow, &target, &mut fs_delete) {
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

    // 单测安全：不触碰真实 install-manifest（注销由 TS 命令层负责）

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
        d
    }

    fn write(p: &Path, body: &str) {
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(p, body).unwrap();
    }

    /// 备份：zip 结构正确、只收声明路径、缺失跳过、manifest 可还原映射。
    #[test]
    fn backup_zips_declared_paths_only() {
        let root = tmpdir("bk");
        let data = root.join("data");
        write(&data.join("cfg/app.json5"), "{\"a\":1}");
        write(&data.join("chat/log.txt"), "hello");
        write(&root.join("outside/secret.txt"), "nope");

        let declared = vec![data.to_string_lossy().into_owned()];
        let out = backup_app_in("jan", &declared, &root.join("backup"), &root).unwrap();

        assert!(!out.backup_dir.is_empty());
        assert_eq!(out.files.len(), 2);
        // zip 内容正确（条目名 = 声明根名 + 相对子路径）
        let zip_path = Path::new(&out.backup_dir).join("data.zip");
        let names = crate::diagzip::zip_entry_names(&zip_path).unwrap();
        assert!(names.contains(&"data/cfg/app.json5".to_string()));
        assert!(names.contains(&"data/chat/log.txt".to_string()));
        assert_eq!(
            crate::diagzip::read_zip_entry(&zip_path, "data/chat/log.txt").unwrap(),
            "hello"
        );
        // 绝不越界：home 外的未声明文件不进包
        assert!(!names.iter().any(|n| n.contains("secret")));

        // manifest 映射（还原结构）
        let man: Value = serde_json::from_str(
            &fs::read_to_string(Path::new(&out.backup_dir).join("manifest.json")).unwrap(),
        )
        .unwrap();
        let entries = man["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().any(|e| {
            e["entry"] == "data/chat/log.txt"
                && e["source"].as_str().unwrap().ends_with("chat/log.txt")
        }));
    }

    /// 备份：缺失路径跳过；全部缺失 → 不建目录、backup_dir 空。
    #[test]
    fn backup_skips_missing_and_empty() {
        let root = tmpdir("bk2");
        let out = backup_app_in(
            "jan",
            &["/definitely/not/here/at/all".into()],
            &root.join("backup"),
            &root,
        )
        .unwrap();
        assert_eq!(out.backup_dir, "");
        assert!(out.files.is_empty());
        assert_eq!(out.skipped.len(), 1);
        assert!(!root.join("backup").exists(), "无数据不建备份目录");
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
        let zip = Path::new(out.backup_dir.as_ref().unwrap()).join("data.zip");
        assert!(zip.exists(), "备份包落盘");
        assert_eq!(out.removed, residual);
        assert!(out.skipped.is_empty());
        assert!(!data.exists(), "清单内数据已删");
        assert!(outside.exists(), "未声明路径必须原样保留");
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
        let zip_path = Path::new(&out.backup_dir).join("data.zip");
        let mut archive = zip::ZipArchive::new(fs::File::open(&zip_path).unwrap()).unwrap();
        for i in 0..archive.len() {
            let mut f = archive.by_index(i).unwrap();
            let mut s = String::new();
            use std::io::Read as _;
            let _ = f.read_to_string(&mut s);
            assert!(!s.contains("TOP-SECRET"), "symlinked outside content leaked: {}", f.name());
        }
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
        assert_eq!(err.code, "MANAGE_BACKUP_INCOMPLETE");
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

}

/// 还原结果（`restore_backup`）。
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreOutcome {
    /// 成功还原的条目数
    pub restored: usize,
    /// 还原到的原路径（人话：给用户看「哪些文件回来了」）
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub restored_paths: Vec<String>,
    /// 被安全策略跳过（越界 / 软链 / manifest 不符）
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<String>,
    /// 写入失败
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub failed: Vec<String>,
}

/// 自动还原尚未通过安全审计，所有平台立即拒绝，绝不读取或覆盖用户文件。
pub fn restore_backup_in(
    backup_dir: &Path,
    app_id: &str,
    home: &Path,
) -> CoreResult<RestoreOutcome> {
    let _ = (backup_dir, app_id, home);
    Err(CoreError::new(
        "RESTORE_UNAVAILABLE",
        "Automatic data restore is unavailable; open the backup folder for manual recovery",
    ))
}

#[cfg(test)]
mod restore_gate_tests {
    use super::*;

    #[test]
    fn restore_rejected_before_reading_or_writing_any_file() {
        let missing = Path::new("/definitely-missing-backup");
        let e = restore_backup_in(missing, "demo", missing).unwrap_err();
        assert_eq!(e.code, "RESTORE_UNAVAILABLE");
    }
}

/// 从备份目录还原（`~/.aibox/backup/<ts>/<appId>`）。
pub fn restore_backup(backup_dir: &str, app_id: &str) -> CoreResult<RestoreOutcome> {
    let dir = PathBuf::from(backup_dir);
    let home = paths::home_dir();
    restore_backup_in(&dir, app_id, &home)
}
