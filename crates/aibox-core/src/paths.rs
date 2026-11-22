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

/// 路径是否落在给定的 home 之内（**两边都 canonicalize** 后比较）。
///
/// 用于「打开文件夹」这类**路径由前端传**的命令：前端传什么就开什么 =
/// 前端被注入 / 被改就能让系统打开任意目录（审计 P2-6）→ Rust 侧必须兜底。
///
/// - `canonicalize` 会解析软链 → 「home 里的软链指向 home 外」也能挡住；
/// - Windows 上 `canonicalize` 会加 `\\?\` 前缀 → **两边都 canonicalize** → 前缀一致
///   → 可以直接比（`Path::starts_with` 是按路径段比的，不会把 `C:\Users\me2`
///   误判成在 `C:\Users\me` 里）；
/// - 解析不了（路径不存在 / home 不可达）→ `false`（fail-closed：拿不准就不放行）。
pub fn is_within_home(path: &Path, home: &Path) -> bool {
    let Ok(real) = std::fs::canonicalize(path) else {
        return false;
    };
    let Ok(home_real) = std::fs::canonicalize(home) else {
        return false;
    };
    real == home_real || real.starts_with(&home_real)
}

/// 路径是否落在**本机 home** 之内（[`is_within_home`] + 本机 home）。
pub fn is_inside_home(path: &Path) -> bool {
    is_within_home(path, &home_dir())
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

    /// home 之内 → 放行；home 之外 → 拒绝（审计 P2-6：前端不能指哪开哪）。
    #[test]
    fn inside_home_paths_are_allowed_others_are_not() {
        let root = std::env::temp_dir().join(format!("aibox-paths-home-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home");
        let inside = home.join("data").join("app");
        let outside = root.join("outside");
        std::fs::create_dir_all(&inside).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let home_real = std::fs::canonicalize(&home).unwrap();

        assert!(is_within_home(&inside, &home_real), "home 之内必须放行");
        assert!(
            is_within_home(&home_real, &home_real),
            "home 本身也算之内"
        );
        assert!(
            !is_within_home(&outside, &home_real),
            "home 之外必须拒绝"
        );
        // 解析不了的路径 → 拒绝（fail-closed）
        assert!(
            !is_within_home(&root.join("does-not-exist"), &home_real),
            "不存在的路径必须拒绝"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// home 里的**软链**指向 home 外 → 拒绝（canonicalize 解析软链后才比较）。
    #[cfg(unix)]
    #[test]
    fn symlink_out_of_home_is_rejected() {
        use std::os::unix::fs::symlink;
        let root = std::env::temp_dir().join(format!("aibox-paths-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home");
        let outside = root.join("outside");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        symlink(&outside, home.join("escape")).unwrap();
        let home_real = std::fs::canonicalize(&home).unwrap();
        assert!(
            !is_within_home(&home.join("escape"), &home_real),
            "指向 home 外的软链必须拒绝"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
