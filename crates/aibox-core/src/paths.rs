//! 路径约定与展开（aibox 管理的一切进 `~/.aibox/` 前缀）。
//!
//! - `~/...` → 用户主目录（跨平台）
//! - `%LOCALAPPDATA%` 等 Windows 环境变量 → **仅 cfg(windows)** 展开；其他平台原样保留

use std::path::{Path, PathBuf};

/// 用户主目录（`HOME` → `USERPROFILE` → `USER_HOME` 兜底）。
pub fn home_dir() -> PathBuf {
    #[cfg(windows)]
    {
        if let Some(h) = std::env::var_os("USERPROFILE").filter(|s| !s.is_empty()) {
            return PathBuf::from(h);
        }
    }
    if let Some(h) = std::env::var_os("HOME").filter(|s| !s.is_empty()) {
        return PathBuf::from(h);
    }
    PathBuf::from(".")
}

/// `~/.aibox` 数据根。
pub fn aibox_root() -> PathBuf {
    home_dir().join(crate::AIBOX_DIR)
}

/// `~/.aibox/cache/downloads/`（DownloadTask.dest 落点）。
pub fn downloads_dir() -> PathBuf {
    aibox_root().join("cache").join("downloads")
}

/// `~/.aibox/cache/registry/`（registry 12h 缓存）。
pub fn registry_cache_dir() -> PathBuf {
    aibox_root().join("cache").join("registry")
}

/// `~/.aibox/logs/`（按任务分文件）。
pub fn logs_dir() -> PathBuf {
    aibox_root().join("logs")
}

/// `~/.aibox/settings.json`（net_region_set 等用户设置）。
pub fn settings_file() -> PathBuf {
    aibox_root().join("settings.json")
}

/// `~/.aibox/install-manifest.json`（已装应用登记）。
pub fn install_manifest_file() -> PathBuf {
    aibox_root().join("install-manifest.json")
}

/// 桌面目录（诊断 zip 落点）。`~/Desktop` 不存在则回落主目录。
pub fn desktop_dir() -> PathBuf {
    let d = home_dir().join("Desktop");
    if d.is_dir() {
        return d;
    }
    // Windows 中文系统可能只有 OneDrive 桌面；不猜路径，回落 home（人话报错里给出实际路径）
    home_dir()
}

/// 展开路径：
/// - 前导 `~/` / `~` → [`home_dir`]
/// - `%VAR%` 形式 → 仅 cfg(windows) 用环境变量替换（`%LOCALAPPDATA%` / `%APPDATA%` / `%USERPROFILE%` / `%TEMP%`）
/// - 其他平台的 `%VAR%` 原样保留（不猜语义）
pub fn expand(raw: &str) -> PathBuf {
    let trimmed = raw.trim();
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut s = trimmed.to_string();

    #[cfg(windows)]
    {
        // 循环替换直到稳定（处理嵌套变量），上限 8 轮防环
        for _ in 0..8 {
            let before = s.clone();
            let mut out = String::with_capacity(s.len());
            let bytes: Vec<char> = s.chars().collect();
            let mut i = 0;
            while i < bytes.len() {
                if bytes[i] == '%' {
                    if let Some(end) = (i + 1..bytes.len()).find(|&j| bytes[j] == '%') {
                        let name: String = bytes[i + 1..end].iter().collect();
                        if !name.is_empty()
                            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                        {
                            if let Some(v) = std::env::var_os(&name) {
                                out.push_str(&v.to_string_lossy());
                                i = end + 1;
                                continue;
                            }
                        }
                    }
                }
                out.push(bytes[i]);
                i += 1;
            }
            s = out;
            if s == before {
                break;
            }
        }
    }

    if s == "~" {
        return home_dir();
    }
    if let Some(rest) = s.strip_prefix("~/").or_else(|| s.strip_prefix("~\\")) {
        return home_dir().join(rest);
    }
    PathBuf::from(s)
}

/// 展开并规范化为绝对路径（相对路径基于当前目录）。
pub fn expand_abs(raw: &str) -> PathBuf {
    let p = expand(raw);
    if p.is_absolute() {
        p
    } else {
        std::env::current_dir().map(|cwd| cwd.join(&p)).unwrap_or(p)
    }
}

/// 目录确保存在。
pub fn ensure_dir(p: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_tilde_to_home() {
        assert_eq!(expand("~"), home_dir());
        assert_eq!(expand("~/.aibox/x"), home_dir().join(".aibox").join("x"));
        assert_eq!(expand("  ~/x  "), home_dir().join("x"));
    }

    #[test]
    fn expand_plain_relative_untouched() {
        assert_eq!(expand("relative/dir"), PathBuf::from("relative/dir"));
        assert_eq!(expand("/abs/path"), PathBuf::from("/abs/path"));
    }

    #[cfg(windows)]
    #[test]
    fn expand_windows_env_vars() {
        std::env::set_var("LOCALAPPDATA", r"C:\Users\me\AppData\Local");
        assert_eq!(
            expand(r"%LOCALAPPDATA%\Aibox"),
            PathBuf::from(r"C:\Users\me\AppData\Local\Aibox")
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn windows_vars_left_alone_on_unix() {
        // 非 Windows 平台不猜 %VAR% 语义，原样保留
        assert_eq!(
            expand(r"%LOCALAPPDATA%\Aibox"),
            PathBuf::from(r"%LOCALAPPDATA%\Aibox")
        );
    }
}
