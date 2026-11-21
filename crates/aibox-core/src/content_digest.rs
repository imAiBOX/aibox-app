//! **gzip 内层内容摘要**（源码归档的稳定完整性校验）。
//!
//! ## 为什么不用字节摘要
//! GitHub 自动生成的源码包（`/archive/refs/tags/*.tar.gz`）**每次 fetch 都会重新 gzip**
//! （gzip 元数据/压缩级别变化）→ 字节 sha256 **必然漂移**（实测同一天内漂 3 次）。
//! 钉字节摘要 = 用户随机撞上「下载文件校验对不上，可能被篡改了」，而实际只是上游重新打包。
//!
//! ## 方案
//! 校验 **解压后的 tar 字节流** 的 sha256：
//! - 外层 gzip 包装变了 → 字节摘要漂
//! - **内层 tar 内容不变** → 内层摘要稳定（实测 4 次跨 30 分钟抓取完全一致）
//!
//! 完整性保证不打折（覆盖整个归档内容），只是不再被 gzip 包装的随机性咬。

use std::path::Path;

use crate::err::{CoreError, CoreResult};

/// gzip 文件**解压后**内容的 sha256（十六进制，小写）。
///
/// 稳定来源：同一份源码包无论被打包多少次，内层 tar 字节完全一致。
pub fn gz_inner_sha256(path: &Path) -> CoreResult<String> {
    aibox_net::digest::gzip_content_sha256(path)
        .map_err(|e| CoreError::new("GZIP_DECODE_FAILED", format!("gzip decode failed: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 同一内容、不同 gzip 包装 → 内层摘要必须相同（这是本模块存在的理由）。
    #[test]
    fn inner_digest_is_stable_across_gzip_wrappers() {
        let dir = std::env::temp_dir().join(format!("aibox-cd-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let payload = b"the very same inner content".repeat(100);
        let a = dir.join("a.gz");
        let b = dir.join("b.gz");
        // 用不同压缩级别/文件名写入 → 外层字节不同、内层相同
        {
            use flate2::write::GzEncoder;
            use flate2::Compression;
            use std::io::Write;
            let mut e1 = GzEncoder::new(std::fs::File::create(&a).unwrap(), Compression::fast());
            e1.write_all(&payload).unwrap();
            e1.finish().unwrap();
            let mut e2 = GzEncoder::new(std::fs::File::create(&b).unwrap(), Compression::best());
            e2.write_all(&payload).unwrap();
            e2.finish().unwrap();
        }
        let ba = std::fs::read(&a).unwrap();
        let bb = std::fs::read(&b).unwrap();
        assert_ne!(ba, bb, "外层 gzip 字节应当不同（测试前提）");
        assert_eq!(
            gz_inner_sha256(&a).unwrap(),
            gz_inner_sha256(&b).unwrap(),
            "内层摘要必须一致"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn not_gzip_is_an_error_not_a_panic() {
        let p = std::env::temp_dir().join(format!("aibox-cd-bad-{}.gz", std::process::id()));
        std::fs::write(&p, b"not gzip at all").unwrap();
        assert!(gz_inner_sha256(&p).is_err());
        let _ = std::fs::remove_file(&p);
    }
}
