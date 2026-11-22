//! 文件系统原语：备份 / JSON+JSON5 合并保键 / 原子写 + 回读校验（C5 硬规则）。
//!
//! **私有权限原语**（Unix：目录 0700 / 文件 0600）——备份库与保全目录里放的是
//! **应用凭据明文**，同机别的账号不该能读（审计 P1-3）。
//! Windows 忽略（ACL 另说：Windows 上 `mode()` 无效，权限靠用户 profile 隔离）。

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::err::{io_err, CoreError, CoreResult};
use crate::paths;

/// 时间戳 `YYYYMMDDHHMMSS`（UTC，用于 `.bak.<ts>`）。
pub fn now_ts() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (y, mo, d, h, mi, s) = civil_from_unix(secs as i64);
    format!("{y:04}{mo:02}{d:02}{h:02}{mi:02}{s:02}")
}

/// ISO-8601 UTC 时间戳（manifest `createdAt`；如 `2026-11-23T01:02:03Z`）。
pub fn now_iso8601() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (y, mo, d, h, mi, s) = civil_from_unix(secs as i64);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

/// 日期 `YYYY-MM-DD`（UTC；诊断包文件名用）。
pub fn today_date() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (y, mo, d, _, _, _) = civil_from_unix(secs as i64);
    format!("{y:04}-{mo:02}-{d:02}")
}

/// Unix 秒 → UTC (y, m, d, h, min, s)（Howard Hinnant civil_from_days 算法，无第三方依赖）。
fn civil_from_unix(secs: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);

    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (
        if m <= 2 { y + 1 } else { y },
        m,
        d,
        h as u32,
        mi as u32,
        s as u32,
    )
}

/// 备份：`<src>.bak.<ts>`（复制；原文件不存在 → `IO_NOT_FOUND` 语义的结构化错误，
/// 调用方决定是否允许无备份写入）。
pub fn backup_file(src: &Path) -> CoreResult<PathBuf> {
    if !src.is_file() {
        return Err(CoreError::new(
            "BACKUP_SRC_MISSING",
            format!("Nothing to back up: {}", src.display()),
        ));
    }
    let dst = backup_path_for(src)?;
    std::fs::copy(src, &dst).map_err(|e| io_err(format!("backup to {}", dst.display()), e))?;
    Ok(dst)
}

/// 备份路径 `<src>.bak.<ts>`。
pub fn backup_path_for(src: &Path) -> CoreResult<PathBuf> {
    let mut s = src.as_os_str().to_owned();
    s.push(format!(".bak.{}", now_ts()));
    Ok(PathBuf::from(s))
}

/// 解析 JSON 或 JSON5 文本（C5：openclaw.json5 可带注释/尾逗号；空文本 → `{}`）。
pub fn parse_json5(text: &str) -> CoreResult<Value> {
    let t = text.trim();
    if t.is_empty() {
        return Ok(json!({}));
    }
    // 纯 JSON 快路径（json5 是超集，先试 serde_json 错误信息更友好）
    if let Ok(v) = serde_json::from_str::<Value>(t) {
        return Ok(v);
    }
    json5::from_str::<Value>(t)
        .map_err(|e| CoreError::new("CONFIG_PARSE", format!("Cannot parse config file: {e}")))
}

/// 读取 JSON/JSON5 文件（不存在 → `{}`）。
pub fn read_json5(path: &Path) -> CoreResult<Value> {
    if !path.exists() {
        return Ok(json!({}));
    }
    let text =
        std::fs::read_to_string(path).map_err(|e| io_err(format!("read {}", path.display()), e))?;
    parse_json5(&text)
}

/// 深合并 `patch` 进 `base`（对象递归合并；数组与标量整体替换），
/// **保留 base 中所有未知键**。返回被写入/变更的键路径列表（如
/// `models.providers.openrouter.apiKey`）。
pub fn merge_preserve(base: &mut Value, patch: &Value) -> Vec<String> {
    let mut changed = Vec::new();
    merge_into(base, patch, String::new(), &mut changed);
    changed
}

fn merge_into(base: &mut Value, patch: &Value, path: String, changed: &mut Vec<String>) {
    match (base, patch) {
        (Value::Object(bm), Value::Object(pm)) => {
            for (k, pv) in pm {
                let child_path = if path.is_empty() {
                    k.clone()
                } else {
                    format!("{path}.{k}")
                };
                match bm.get_mut(k) {
                    Some(bv) => merge_into(bv, pv, child_path, changed),
                    None => {
                        bm.insert(k.clone(), pv.clone());
                        record_leaves(pv, &child_path, changed);
                    }
                }
            }
        }
        (bv, pv) => {
            if bv != pv {
                *bv = pv.clone();
                changed.push(path);
            }
        }
    }
}

/// 新插入子树 → 记录全部叶路径（mergedKeys 要到字段级）。
fn record_leaves(v: &Value, path: &str, out: &mut Vec<String>) {
    match v {
        Value::Object(m) if !m.is_empty() => {
            for (k, c) in m {
                record_leaves(c, &format!("{path}.{k}"), out);
            }
        }
        _ => out.push(path.to_string()),
    }
}

/// 字节数组 → 小写十六进制。
pub(crate) fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// 文件 sha256（流式；**先打开句柄再从句柄读**，并比对 dev/ino：
/// 路径在读之前被换成 symlink / 被替换 → 失败，绝不读链接指向的内容）。
pub fn sha256_file_pinned(path: &Path) -> std::io::Result<String> {
    use std::io::Read as _;
    let mut f = std::fs::File::open(path)?;
    let h = f.metadata()?;
    let link_meta = std::fs::symlink_metadata(path)?;
    if link_meta.file_type().is_symlink() {
        return Err(std::io::Error::other("path is a symlink; refusing to read"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if h.dev() != link_meta.dev() || h.ino() != link_meta.ino() {
            return Err(std::io::Error::other("path was swapped while reading"));
        }
    }
    #[cfg(not(unix))]
    {
        // Windows：无法用 dev/ino 固定句柄身份 → 任何重解析点一律 fail-closed
        if link_meta.file_type().is_symlink() {
            return Err(std::io::Error::other(
                "reparse point refused on this platform",
            ));
        }
    }
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

/// 独占创建临时文件（**`create_new`**）：`<dir>/<base><suffix>`。
///
/// **同名已存在 → 直接失败**（不覆盖别人的文件；调用方自己决定是否清理残留）。
/// Unix：新建即 `0600`（临时文件常含凭据明文）。
/// 返回 (临时文件路径, 已打开的句柄)。
pub fn create_exclusive_tmp(
    dir: &Path,
    base: &str,
    suffix: &str,
) -> CoreResult<(PathBuf, std::fs::File)> {
    let p = dir.join(format!("{base}{suffix}"));
    match open_private_new(&p) {
        Ok(f) => Ok((p, f)),
        Err(e) => Err(io_err(format!("create {}", p.display()), e)),
    }
}

/// **私有**创建（Unix `0600`；Windows = 默认）。同名已存在 → 失败。
fn open_private_new(p: &Path) -> std::io::Result<std::fs::File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(p)
}

/// 私有目录：Unix `0700`（**新建即设**，不是事后再 chmod）。
///
/// 已存在且权限更宽 → **收紧**到 0700（Unix 可 chmod）。
/// 只用于**备份库 / 保全目录**——绝不改用户原始数据目录的权限。
pub fn ensure_private_dir(p: &Path) -> std::io::Result<()> {
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut b = std::fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o700);
    }
    b.create(p)?;
    tighten_dir(p)
}

/// 收紧已存在目录的权限（Unix → 0700；Windows → no-op）。
pub fn tighten_dir(p: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let m = std::fs::metadata(p)?;
        let mode = m.permissions().mode() & 0o7777;
        if mode & 0o077 != 0 {
            let mut perms = m.permissions();
            perms.set_mode(0o700);
            std::fs::set_permissions(p, perms)?;
        }
    }
    #[cfg(not(unix))]
    {
        let _ = p;
    }
    Ok(())
}

/// 新建目录（Unix `0700`，**新建即 0700**——不是事后 chmod）。
///
/// 与 [`ensure_private_dir`] 的关键区别：**绝不 chmod 已存在的目录**。
/// 还原时建的是**用户原始数据目录**（`~/.config/xxx`）→ 不能改它的权限
/// （用户自己定的 0755 可能就是有意的）；只有**我们新建**的目录才收紧。
pub fn create_dirs_private(p: &Path) -> std::io::Result<()> {
    let mut b = std::fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o700);
    }
    b.create(p)
}

/// 独占创建临时文件（**`create_new`**）——同名已存在则**加序号重试**
/// （`.1`、`.2`…）：备份侧的临时文件不因残留而彻底卡死。
pub fn create_unique_tmp(
    dir: &Path,
    base: &str,
    suffix: &str,
) -> CoreResult<(PathBuf, std::fs::File)> {
    for i in 0..64u32 {
        let name = if i == 0 {
            format!("{base}{suffix}")
        } else {
            format!("{base}.{i}{suffix}")
        };
        let p = dir.join(name);
        match open_private_new(&p) {
            Ok(f) => return Ok((p, f)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(io_err(format!("create {}", p.display()), e)),
        }
    }
    Err(CoreError::new(
        "IO_ERROR",
        format!("cannot create a unique temporary file in {}", dir.display()),
    ))
}

/// 流式复制文件到 `dst`（同目录 `.part` 临时文件 → rename），
/// 返回 (字节数, sha256 hex)——sha256 是**写入内容的**摘要（与磁盘上的文件一致）。
///
/// 源是 symlink / 在复制期间被换成链接 → 失败（绝不把链接指向的内容拷进备份）。
/// 临时文件**独占创建**（`create_new`）；任何失败路径都清掉残留的 `.part`。
pub fn copy_file_hashed(src: &Path, dst: &Path) -> CoreResult<(u64, String)> {
    copy_file_hashed_budget(src, dst, u64::MAX)
}

/// 流式复制文件到 `dst`（同目录 `.part` 临时文件 → rename），**边拷边算配额**。
///
/// `max_bytes` = 本次允许再写入的字节上限；**拷贝过程中超过即中止**
/// （源文件变大 → 不能「按拷贝前的体积」放行，审计 P2-6）。
/// 超限 → `BACKUP_TOO_LARGE`（临时文件清掉，不留残骸）。
///
/// 源是 symlink / 在复制期间被换成链接 → 失败（绝不把链接指向的内容拷进备份）。
/// 临时文件**独占创建**（`create_new`，Unix 0600）；任何失败路径都清掉残留的 `.part`。
pub fn copy_file_hashed_budget(
    src: &Path,
    dst: &Path,
    max_bytes: u64,
) -> CoreResult<(u64, String)> {
    use std::io::{Read as _, Write as _};
    let mut input =
        std::fs::File::open(src).map_err(|e| io_err(format!("open {}", src.display()), e))?;
    let handle_meta = input
        .metadata()
        .map_err(|e| io_err(format!("stat {}", src.display()), e))?;
    let link_meta =
        std::fs::symlink_metadata(src).map_err(|e| io_err(format!("stat {}", src.display()), e))?;
    if link_meta.file_type().is_symlink() {
        return Err(CoreError::new(
            "BACKUP_SRC_SYMLINK",
            format!("Refusing to back up a symlink: {}", src.display()),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if handle_meta.dev() != link_meta.dev() || handle_meta.ino() != link_meta.ino() {
            return Err(CoreError::new(
                "BACKUP_SRC_SWAPPED",
                format!("Source changed while backing up: {}", src.display()),
            ));
        }
    }
    let dir = dst.parent().ok_or_else(|| {
        CoreError::new("IO_ERROR", format!("No parent dir for {}", dst.display()))
    })?;
    paths::ensure_dir(dir).map_err(|e| io_err(format!("create {}", dir.display()), e))?;
    let base = dst
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "payload".to_string());
    let (tmp, mut output) = create_unique_tmp(dir, &base, ".part")?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut total: u64 = 0;
    let copy_result = (|| -> CoreResult<u64> {
        loop {
            let n = input.read(&mut buf)?;
            if n == 0 {
                break;
            }
            // 配额在**写入前**判定（源在拷贝中变大 → 立刻中止，不超限落盘）
            if total.saturating_add(n as u64) > max_bytes {
                return Err(CoreError::new(
                    "BACKUP_TOO_LARGE",
                    format!(
                        "{} grew beyond the allowed {} bytes while copying; stopped",
                        src.display(),
                        max_bytes
                    ),
                ));
            }
            hasher.update(&buf[..n]);
            output
                .write_all(&buf[..n])
                .map_err(|e| io_err(format!("write {}", tmp.display()), e))?;
            total += n as u64;
        }
        output
            .flush()
            .map_err(|e| io_err(format!("flush {}", tmp.display()), e))?;
        output
            .sync_all()
            .map_err(|e| io_err(format!("sync {}", tmp.display()), e))?;
        Ok(total)
    })();
    if let Err(e) = copy_result {
        // 失败路径不留残骸（`.part` 不留在用户目录里）
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    // Windows：rename 到已存在文件会失败 → 先删目标（Unix 上 rename 直接原子替换）
    #[cfg(windows)]
    if dst.exists() {
        let _ = std::fs::remove_file(dst);
    }
    if let Err(e) = std::fs::rename(&tmp, dst) {
        let _ = std::fs::remove_file(&tmp);
        return Err(io_err(format!("rename to {}", dst.display()), e));
    }
    Ok((total, hex(&hasher.finalize())))
}

/// 原子写 JSON（同目录 tmp + rename；父目录自动创建）。
/// 临时文件**独占创建**（`create_new`）；失败路径不留残骸。
pub fn write_json_atomic(path: &Path, value: &Value) -> CoreResult<()> {
    let dir = path.parent().ok_or_else(|| {
        CoreError::new("IO_ERROR", format!("No parent dir for {}", path.display()))
    })?;
    paths::ensure_dir(dir).map_err(|e| io_err(format!("create {}", dir.display()), e))?;
    let text = serde_json::to_string_pretty(value)
        .map_err(|e| CoreError::new("CONFIG_SERIALIZE", e.to_string()))?;
    write_text_atomic(path, &text)
}

/// 原子写**文本**（同目录 tmp + rename；父目录自动创建）。
/// 多格式配置（yaml/toml）用这个 —— JSON 专用写入器会把用户文件写坏。
/// 临时文件**独占创建**（`create_new`）；失败路径不留残骸。
pub fn write_text_atomic(path: &Path, text: &str) -> CoreResult<()> {
    write_text_atomic_with(path, text, false)
}

/// 原子写**私有**文本（Unix 0600）——备份库的 `manifest.json` 与还原留痕 journal。
///
/// 备份内容就是应用凭据明文 → 同机别的账号不该能读（审计 P1-3）。
pub fn write_text_atomic_private(path: &Path, text: &str) -> CoreResult<()> {
    write_text_atomic_with(path, text, true)
}

fn write_text_atomic_with(path: &Path, text: &str, private: bool) -> CoreResult<()> {
    let dir = path.parent().ok_or_else(|| {
        CoreError::new("IO_ERROR", format!("No parent dir for {}", path.display()))
    })?;
    paths::ensure_dir(dir).map_err(|e| io_err(format!("create {}", dir.display()), e))?;
    let base = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "out".to_string());
    let (tmp, mut f) = if private {
        // 私有临时文件（Unix 0600）
        let p = dir.join(format!("{base}.tmp"));
        match open_private_new(&p) {
            Ok(f) => (p, f),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // 同名残留 → 换名重试（不覆盖别人的文件）
                create_unique_tmp(dir, &base, ".tmp")?
            }
            Err(e) => return Err(io_err(format!("create {}", p.display()), e)),
        }
    } else {
        create_unique_tmp(dir, &base, ".tmp")?
    };
    if let Err(e) = f
        .write_all(text.as_bytes())
        .map_err(|e| io_err(format!("write {}", tmp.display()), e))
    {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    if let Err(e) = f
        .flush()
        .map_err(|e| io_err(format!("flush {}", tmp.display()), e))
    {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    if let Err(e) = f
        .sync_all()
        .map_err(|e| io_err(format!("sync {}", tmp.display()), e))
    {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    drop(f);
    // Windows：rename 到已存在文件会失败 → 先删目标（Unix 上 rename 直接原子替换）
    #[cfg(windows)]
    if path.exists() {
        let _ = std::fs::remove_file(path);
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(io_err(format!("rename to {}", path.display()), e));
    }
    Ok(())
}

/// 回读校验（文本）：读盘内容必须与写入内容完全一致。
pub fn readback_verify_text(path: &Path, expected: &str) -> CoreResult<bool> {
    let actual = std::fs::read_to_string(path)
        .map_err(|e| io_err(format!("read back {}", path.display()), e))?;
    Ok(actual == expected)
}

/// 回读校验：重新读盘解析，与预期值深比较。
pub fn readback_verify(path: &Path, expected: &Value) -> CoreResult<bool> {
    let actual = read_json5(path)?;
    Ok(&actual == expected)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "aibox-fsops-{tag}-{}-{}",
            std::process::id(),
            now_ts()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn merge_preserves_unknown_keys_and_records_paths() {
        let mut base: Value = serde_json::from_str(
            r#"{
            "models": {"mode": "manual", "providers": {"ollama": {"baseUrl": "http://127.0.0.1:11434"}}},
            "users": {"alice": {"role": "admin"}},
            "skills": {"entries": {"a": {"enabled": true}}}
        }"#,
        )
        .unwrap();
        let patch = json!({
            "models": {"providers": {"openrouter": {"apiKey": "sk-or-v1-secret"}}}
        });
        let changed = merge_preserve(&mut base, &patch);

        // 未知键全部保留
        assert_eq!(base["models"]["mode"], "manual");
        assert_eq!(base["users"]["alice"]["role"], "admin");
        assert_eq!(
            base["models"]["providers"]["ollama"]["baseUrl"],
            "http://127.0.0.1:11434"
        );
        assert_eq!(base["skills"]["entries"]["a"]["enabled"], true);
        // 新键写入
        assert_eq!(
            base["models"]["providers"]["openrouter"]["apiKey"],
            "sk-or-v1-secret"
        );
        assert!(changed.contains(&"models.providers.openrouter.apiKey".to_string()));
        // 重复合并同值 → 不重复计变更
        let mut again = base.clone();
        let changed2 = merge_preserve(&mut again, &patch);
        assert!(changed2.is_empty(), "same value must not be re-recorded");
    }

    #[test]
    fn backup_creates_ts_copy() {
        let d = tmpdir("bak");
        let f = d.join("cfg.json");
        std::fs::write(&f, r#"{"a":1}"#).unwrap();
        let bak = backup_file(&f).unwrap();
        let name = bak.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("cfg.json.bak."), "{name}");
        assert_eq!(std::fs::read_to_string(&bak).unwrap(), r#"{"a":1}"#);
        // 缺失源 → 结构化错误
        assert!(backup_file(&d.join("missing.json")).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn write_and_readback_json5() {
        let d = tmpdir("rw");
        let f = d.join("openclaw.json5");
        // 带注释 + 尾逗号的 JSON5 原文件
        std::fs::write(
            &f,
            "{\n  // user comment\n  \"models\": {\"mode\": \"manual\",},\n}\n",
        )
        .unwrap();
        let mut base = read_json5(&f).unwrap();
        assert_eq!(base["models"]["mode"], "manual");

        let patch = json!({"models": {"providers": {"openai": {"apiKey": "sk-test-1234567890"}}}});
        let changed = merge_preserve(&mut base, &patch);
        write_json_atomic(&f, &base).unwrap();
        assert!(readback_verify(&f, &base).unwrap());
        // 回读的文件仍是合法 JSON 且保留了合并结果
        let reread = read_json5(&f).unwrap();
        assert_eq!(
            reread["models"]["providers"]["openai"]["apiKey"],
            "sk-test-1234567890"
        );
        assert!(changed.contains(&"models.providers.openai.apiKey".to_string()));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn readback_detects_mismatch() {
        let d = tmpdir("mismatch");
        let f = d.join("x.json");
        std::fs::write(&f, r#"{"a":1}"#).unwrap();
        assert!(!readback_verify(&f, &json!({"a": 2})).unwrap());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn parse_json5_rejects_garbage() {
        assert!(parse_json5("{not json").is_err());
        assert_eq!(parse_json5("").unwrap(), json!({}));
        assert_eq!(parse_json5("  ").unwrap(), json!({}));
    }

    #[test]
    fn timestamps_are_well_formed() {
        let ts = now_ts();
        assert_eq!(ts.len(), 14, "{ts}");
        assert!(ts.chars().all(|c| c.is_ascii_digit()));
        let d = today_date();
        assert_eq!(d.len(), 10, "{d}");
        assert!(d.contains('-'));
    }

    /// 配额在**拷贝过程中**判定：超过上限即中止（审计 P2-6）。
    #[test]
    fn copy_budget_stops_when_source_grows() {
        let d = tmpdir("budget");
        let src = d.join("big.bin");
        std::fs::write(&src, vec![b'x'; 4096]).unwrap();
        let dst = d.join("out.bin");
        // 配额小于源体积 → 必须失败，且不留残骸
        let err = copy_file_hashed_budget(&src, &dst, 1024).unwrap_err();
        assert_eq!(err.code, "BACKUP_TOO_LARGE", "{err:?}");
        assert!(!dst.exists(), "no partial payload may remain");
        // 配额充足 → 正常拷贝
        let (n, sha) = copy_file_hashed_budget(&src, &dst, 1 << 20).unwrap();
        assert_eq!(n, 4096);
        assert_eq!(sha.len(), 64);
        assert!(dst.exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// 私有目录：新建即 0700；已存在且更宽 → **收紧**（审计 P1-3）。
    #[cfg(unix)]
    #[test]
    fn private_dir_is_0700_and_tightens_existing() {
        use std::os::unix::fs::PermissionsExt;
        let d = tmpdir("priv");
        // 先建一个 0755 的目录（模拟历史遗留）
        let sub = d.join("backup");
        std::fs::create_dir_all(&sub).unwrap();
        let mut perms = std::fs::metadata(&sub).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&sub, perms).unwrap();
        ensure_private_dir(&sub).unwrap();
        let mode = std::fs::metadata(&sub).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "existing dir must be tightened to 0700");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// 私有文件：临时文件 0600（备份载荷不应对同机别的账号可读）。
    #[cfg(unix)]
    #[test]
    fn temp_files_are_0600() {
        use std::os::unix::fs::PermissionsExt;
        let d = tmpdir("tmpperm");
        let (p, _f) = create_unique_tmp(&d, "payload", ".part").unwrap();
        let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "temp file must be 0600");
        let _ = std::fs::remove_dir_all(&d);
    }
}
