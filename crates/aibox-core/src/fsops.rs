//! 文件系统原语：备份 / JSON+JSON5 合并保键 / 原子写 + 回读校验（C5 硬规则）。

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

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

/// 原子写 JSON（同目录 tmp + rename；父目录自动创建）。
pub fn write_json_atomic(path: &Path, value: &Value) -> CoreResult<()> {
    let dir = path.parent().ok_or_else(|| {
        CoreError::new("IO_ERROR", format!("No parent dir for {}", path.display()))
    })?;
    paths::ensure_dir(dir).map_err(|e| io_err(format!("create {}", dir.display()), e))?;
    let text = serde_json::to_string_pretty(value)
        .map_err(|e| CoreError::new("CONFIG_SERIALIZE", e.to_string()))?;
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, text).map_err(|e| io_err(format!("write {}", tmp.display()), e))?;
    // Windows 上 rename 到已存在文件会失败 → 先删目标（备份已在前序步骤完成）
    if path.exists() {
        let _ = std::fs::remove_file(path);
    }
    std::fs::rename(&tmp, path).map_err(|e| io_err(format!("rename to {}", path.display()), e))
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
}
