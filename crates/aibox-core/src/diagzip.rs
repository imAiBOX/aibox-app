//! 诊断打包：日志 + ScanReport + 计划 + 版本 + 系统信息 → zip；
//! **打包前对全部文本跑正则扫描**（anthropic|openai|sk-|api[_-]?key …），命中脱敏
//! `sk-****1234` 并计入 `_redaction-report.json`；输出 `aibox-diagnostic-<date>.zip`。

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::err::{io_err, CoreError, CoreResult};
use crate::paths;
use crate::redact::scan_and_mask;

/// 单文件上限 4MB（超出跳过并记入报告）。
const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
/// 全部日志累计上限 40MB。
const MAX_TOTAL_BYTES: u64 = 40 * 1024 * 1024;

/// 打包输入（可注入，单测离线）。
#[derive(Debug, Default)]
pub struct DiagInputs {
    /// 输出 zip 完整路径（含文件名）
    pub out_path: PathBuf,
    /// 日志根目录（递归收集）
    pub logs_dir: PathBuf,
    /// 附加文件：(zip 内条目名, 源路径)
    pub extra_files: Vec<(String, PathBuf)>,
    /// 生成内容：(zip 内条目名, 文本)
    pub generated: Vec<(String, String)>,
}

/// 打包结果。
#[derive(Debug, Clone)]
pub struct DiagOutcome {
    pub path: PathBuf,
    pub entries: usize,
    pub redactions: usize,
    pub skipped: usize,
}

/// 诊断包文件名 `aibox-diagnostic-<date>.zip`。
pub fn diag_zip_name(date: &str) -> String {
    format!("aibox-diagnostic-{date}.zip")
}

/// 递归收集目录下全部文件（跳过超大与目录）。
fn walk_files(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = fs::read_dir(root) else {
        return;
    };
    for entry in rd.flatten() {
        let p = entry.path();
        if p.is_dir() {
            walk_files(&p, out);
        } else if p.is_file() {
            out.push(p);
        }
    }
}

/// 构建诊断 zip（同步；调用方放入 spawn_blocking）。
pub fn build_diag_zip(inputs: &DiagInputs) -> CoreResult<DiagOutcome> {
    if let Some(parent) = inputs.out_path.parent() {
        paths::ensure_dir(parent).map_err(|e| io_err("create desktop dir", e))?;
    }

    let mut redaction_hits: Vec<Value> = Vec::new();
    let mut skipped: Vec<Value> = Vec::new();
    let mut files_scanned = 0usize;
    let mut total_redactions = 0usize;
    let mut total_logged: u64 = 0;

    // 条目 = 日志文件 + 附加文件 + 生成内容
    let mut entries: Vec<(String, Vec<u8>, bool)> = Vec::new(); // (name, bytes, is_text)

    let mut logs = Vec::new();
    walk_files(&inputs.logs_dir, &mut logs);
    logs.sort();
    for p in logs {
        let Ok(meta) = fs::metadata(&p) else { continue };
        if meta.len() > MAX_FILE_BYTES {
            skipped.push(json!({
                "entry": p.display().to_string(),
                "reason": format!("file too large ({} bytes)", meta.len()),
            }));
            continue;
        }
        if total_logged + meta.len() > MAX_TOTAL_BYTES {
            skipped.push(json!({
                "entry": p.display().to_string(),
                "reason": "log budget exceeded (40MB)",
            }));
            continue;
        }
        total_logged += meta.len();
        let rel = p
            .strip_prefix(&inputs.logs_dir)
            .map(|r| r.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| {
                p.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            });
        let bytes = match fs::read(&p) {
            Ok(b) => b,
            Err(e) => {
                skipped.push(json!({"entry": rel, "reason": format!("read error: {e}")}));
                continue;
            }
        };
        entries.push((format!("logs/{rel}"), bytes, true));
    }

    for (name, src) in &inputs.extra_files {
        if !src.is_file() {
            skipped.push(json!({"entry": name, "reason": "source file not found"}));
            continue;
        }
        let Ok(bytes) = fs::read(src) else {
            skipped.push(json!({"entry": name, "reason": "read error"}));
            continue;
        };
        entries.push((name.clone(), bytes, true));
    }

    for (name, text) in &inputs.generated {
        entries.push((name.clone(), text.as_bytes().to_vec(), true));
    }

    // ── 强制脱敏扫描（硬规则：命中 → sk-****1234 并计入报告）──
    let mut zipped: Vec<(String, Vec<u8>)> = Vec::new();
    for (name, bytes, _) in entries {
        match String::from_utf8(bytes) {
            Ok(text) => {
                files_scanned += 1;
                let (safe, hits) = scan_and_mask(&text);
                if hits > 0 {
                    total_redactions += hits;
                    redaction_hits.push(json!({"entry": name, "hits": hits}));
                }
                zipped.push((name, safe.into_bytes()));
            }
            Err(e) => {
                skipped.push(json!({
                    "entry": name,
                    "reason": format!("not UTF-8 text ({})", e.utf8_error().valid_up_to()),
                }));
            }
        }
    }

    // 脱敏报告（本身不含任何原文）
    let report = json!({
        "schema": "aibox-diag-redaction/v1",
        "generatedAtMs": crate::registry::now_ms(),
        "filesScanned": files_scanned,
        "totalRedactions": total_redactions,
        "redactions": redaction_hits,
        "skipped": skipped,
    });
    zipped.push((
        "_redaction-report.json".to_string(),
        serde_json::to_string_pretty(&report)
            .unwrap_or_else(|_| "{}".into())
            .into_bytes(),
    ));

    // ── 写 zip（deflate 纯 Rust：flate2 → miniz_oxide/zlib-rs）──
    let file = fs::File::create(&inputs.out_path)
        .map_err(|e| io_err(format!("create {}", inputs.out_path.display()), e))?;
    let mut zip = zip::ZipWriter::new(file);
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (name, bytes) in &zipped {
        zip.start_file(name.as_str(), opts)
            .map_err(|e| CoreError::new("DIAG_ZIP_WRITE", format!("add {name}: {e}")))?;
        zip.write_all(bytes)
            .map_err(|e| io_err(format!("write entry {name}"), e))?;
    }
    zip.finish()
        .map_err(|e| CoreError::new("DIAG_ZIP_WRITE", e.to_string()))?;

    Ok(DiagOutcome {
        path: inputs.out_path.clone(),
        entries: zipped.len(),
        redactions: total_redactions,
        skipped: report["skipped"].as_array().map(|a| a.len()).unwrap_or(0),
    })
}

/// 默认输入（壳侧 `diag_export` 直接用）：日志 + ScanReport + 计划 + 版本 + 系统信息。
pub fn default_inputs(date: &str) -> DiagInputs {
    let mut extra: Vec<(String, PathBuf)> = Vec::new();
    for (name, p) in [
        ("settings.json", paths::settings_file()),
        ("install-manifest.json", paths::install_manifest_file()),
        (
            "last-plan.json",
            paths::aibox_root().join("cache").join("last-plan.json"),
        ),
        (
            "registry-index.json",
            paths::registry_cache_dir().join("index.json"),
        ),
    ] {
        if p.is_file() {
            extra.push((name.to_string(), p));
        }
    }

    let scan = crate::scan::scan_report_value();
    let system = json!({
        "os": scan.get("os"),
        "arch": scan.pointer("/os/arch"),
        "diskFreeGb": scan.get("diskFreeGb"),
        "memTotalGb": scan.get("memTotalGb"),
        "node": scan.get("node"),
        "git": scan.get("git"),
        "wsl": scan.get("wsl"),
        "port18789": scan.get("port18789"),
        "env": {
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
        },
    });
    let version = json!({
        "installer": env!("CARGO_PKG_VERSION"),
        "dataRoot": paths::aibox_root().display().to_string(),
    });

    let mut generated = vec![
        (
            "scan-report.json".to_string(),
            serde_json::to_string_pretty(&scan).unwrap_or_default(),
        ),
        (
            "system.json".to_string(),
            serde_json::to_string_pretty(&system).unwrap_or_default(),
        ),
        (
            "version.json".to_string(),
            serde_json::to_string_pretty(&version).unwrap_or_default(),
        ),
    ];
    // 诊断包自身的元信息（无敏感内容）
    generated.push((
        "about.json".to_string(),
        serde_json::to_string_pretty(&json!({
            "note": "AiBOX diagnostic export — keys are redacted before packing",
            "redactionPattern": "(anthropic|openai|sk-|api[_-]?key)",
        }))
        .unwrap_or_default(),
    ));

    DiagInputs {
        out_path: paths::desktop_dir().join(diag_zip_name(date)),
        logs_dir: paths::logs_dir(),
        extra_files: extra,
        generated,
    }
}

/// 读 zip 单条目内容（测试与诊断回读用）。
pub fn read_zip_entry(zip_path: &Path, entry: &str) -> CoreResult<String> {
    let file =
        fs::File::open(zip_path).map_err(|e| io_err(format!("open {}", zip_path.display()), e))?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| CoreError::new("DIAG_ZIP_READ", e.to_string()))?;
    let mut found = archive
        .by_name(entry)
        .map_err(|e| CoreError::new("DIAG_ZIP_READ", format!("{entry}: {e}")))?;
    let mut s = String::new();
    found
        .read_to_string(&mut s)
        .map_err(|e| io_err(format!("read entry {entry}"), e))?;
    Ok(s)
}

/// 列出 zip 条目名（测试用）。
pub fn zip_entry_names(zip_path: &Path) -> CoreResult<Vec<String>> {
    let file =
        fs::File::open(zip_path).map_err(|e| io_err(format!("open {}", zip_path.display()), e))?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| CoreError::new("DIAG_ZIP_READ", e.to_string()))?;
    Ok((0..archive.len())
        .filter_map(|i| archive.by_index(i).ok().map(|f| f.name().to_string()))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zip_name_format() {
        assert_eq!(
            diag_zip_name("2026-11-09"),
            "aibox-diagnostic-2026-11-09.zip"
        );
    }

    #[test]
    fn build_zip_redacts_every_text_entry() {
        let dir = std::env::temp_dir().join(format!("aibox-diag-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("logs/openclaw")).unwrap();

        let fake_key = "sk-ant-api03-ZZZSECRET1234567890abcdef";
        fs::write(
            dir.join("logs/openclaw/2026-11-09-install.log"),
            format!("starting\nAuthorization: Bearer {fake_key}\nprovider=openai done\n"),
        )
        .unwrap();
        // 二进制文件应被跳过（不 panic、不打包）
        fs::write(dir.join("logs/blob.bin"), [0u8, 159, 146, 150]).unwrap();

        let out = dir.join("out").join("aibox-diagnostic-2026-11-09.zip");
        let inputs = DiagInputs {
            out_path: out.clone(),
            logs_dir: dir.join("logs"),
            extra_files: vec![("generated-note.txt".into(), {
                let p = dir.join("note.txt");
                fs::write(&p, "openai key material example").unwrap();
                p
            })],
            generated: vec![(
                "scan-report.json".into(),
                r#"{"provider":"anthropic","ok":true}"#.to_string(),
            )],
        };

        let outcome = build_diag_zip(&inputs).unwrap();
        assert!(out.is_file());
        assert!(outcome.redactions >= 3, "hits={}", outcome.redactions);
        assert!(outcome.skipped >= 1, "binary must be skipped");

        // 产物内绝无原文
        let names = zip_entry_names(&out).unwrap();
        assert!(names.iter().any(|n| n == "_redaction-report.json"));
        assert!(names
            .iter()
            .any(|n| n == "logs/openclaw/2026-11-09-install.log"));
        assert!(names.iter().any(|n| n == "scan-report.json"));
        assert!(
            !names.iter().any(|n| n == "logs/blob.bin"),
            "binary skipped"
        );

        for name in &names {
            if name.ends_with(".bin") {
                continue;
            }
            let content = read_zip_entry(&out, name).unwrap();
            assert!(
                !content.contains("ZZZSECRET1234567890abcdef"),
                "raw key leaked in {name}"
            );
            assert!(!content.contains("api03-ZZZ"), "raw key fragment in {name}");
        }
        let log = read_zip_entry(&out, "logs/openclaw/2026-11-09-install.log").unwrap();
        assert!(log.contains("sk-****"), "mask must be present: {log}");

        let report = read_zip_entry(&out, "_redaction-report.json").unwrap();
        let v: Value = serde_json::from_str(&report).unwrap();
        assert!(v["totalRedactions"].as_u64().unwrap() >= 3);
        assert!(v["filesScanned"].as_u64().unwrap() >= 3);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_logs_dir_still_produces_zip() {
        let dir = std::env::temp_dir().join(format!("aibox-diag2-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let out = dir.join("aibox-diagnostic-2026-11-09.zip");
        let inputs = DiagInputs {
            out_path: out.clone(),
            logs_dir: dir.join("no-such-logs"),
            extra_files: vec![],
            generated: vec![("version.json".into(), "{}".into())],
        };
        let outcome = build_diag_zip(&inputs).unwrap();
        assert!(out.is_file());
        assert_eq!(outcome.entries, 2, "generated + report");
        let _ = fs::remove_dir_all(&dir);
    }
}
