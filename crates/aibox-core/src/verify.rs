//! SHA-256 校验原语（S1：全通道产物零豁免；复用 aibox-net `sha256_file`）。

use std::path::Path;

use aibox_net::sha256_file;

use crate::err::{io_err, CoreError, CoreResult};

/// 校验文件 SHA-256：
/// - 文件不存在 → `VERIFY_FILE_MISSING` 结构化错误
/// - 不匹配 → `Ok(false)`（调用方据此中止安装，S1）
/// - 匹配 → `Ok(true)`
pub fn verify_sha256(path: &Path, expected: &str) -> CoreResult<bool> {
    if !path.is_file() {
        return Err(CoreError::new(
            "VERIFY_FILE_MISSING",
            format!("File not found: {}", path.display()),
        ));
    }
    let expected = expected.trim();
    if expected.len() != 64 || !expected.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(CoreError::new(
            "DOWNLOAD_BAD_SHA256",
            "Expected a 64-hex-char SHA-256 value",
        ));
    }
    let actual = sha256_file(path).map_err(|e| io_err(format!("hash {}", path.display()), e))?;
    Ok(actual.eq_ignore_ascii_case(expected))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn match_mismatch_missing() {
        let dir = std::env::temp_dir().join(format!("aibox-verify-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("x.bin");
        std::fs::write(&f, b"hello").unwrap();
        let expected = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
        assert!(verify_sha256(&f, expected).unwrap());
        assert!(!verify_sha256(&f, &"0".repeat(64)).unwrap());
        assert!(verify_sha256(&dir.join("missing"), expected).is_err());
        assert!(verify_sha256(&f, "nothex").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
