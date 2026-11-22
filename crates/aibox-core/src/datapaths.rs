//! 声明数据路径（registry `maintenance.dataPaths`）的**统一判定**。
//!
//! **备份侧（[`crate::manage`]）与还原侧（[`crate::restore`]）共用同一套判定**——
//! 两边各判一次必然漂移（审计 P1-1 / P1-2 的根因）。
//!
//! 判定规则（全部 fail-closed）：
//! 1. `~` / `~/.` / home 绝对路径 → **拒绝**（[`Refusal::HomeRoot`]）：
//!    解析后等于 home 根 = 放开**整个** home（`~/.ssh/authorized_keys` 会被覆写）。
//! 2. home 之外的路径 → 拒绝（[`Refusal::OutsideHome`]）。
//! 3. 路径本身是软链 → 拒绝（[`Refusal::SymlinkRoot`]）：备份侧原本就拒绝，
//!    还原侧同步 → 两边同一套判定。
//! 4. **类型显式化**（[`PathKind`]）：目录 → 其下路径都允许；文件 → 只允许精确命中；
//!    **尚不存在（类型未知）→ 按文件对待（只允许精确命中一个文件）**——
//!    绝不据清单自述把「数据文件路径」建成目录。

use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::paths;

/// 白名单路径的**显式类型**（不再靠「是否存在」隐式推断文件/目录）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathKind {
    /// 目录 → 其下所有路径都允许
    Dir,
    /// 文件 → 只允许精确命中
    File,
    /// 路径尚不存在 → **类型未知** → 只允许精确命中（保守；当作文件）
    Unknown,
}

/// 白名单条目（home 相对路径 + 显式类型）。
#[derive(Debug, Clone)]
pub struct DataPathEntry {
    /// 相对 home 的路径（**绝不为空**：home 本身被拒绝）
    pub rel: PathBuf,
    /// 类型（显式）
    pub kind: PathKind,
}

impl DataPathEntry {
    /// 该条目是否**覆盖** target（精确命中；或目录型条目的子树）。
    pub fn covers(&self, target: &Path) -> bool {
        if target == self.rel {
            return true;
        }
        self.kind == PathKind::Dir && target.starts_with(&self.rel)
    }
}

/// 策略拒绝原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// 解析后等于 home 根（`~` / `~/.` / home 绝对路径）——**禁止**
    HomeRoot,
    /// 落在 home 之外
    OutsideHome,
    /// 路径本身是软链
    SymlinkRoot,
}

impl Refusal {
    /// 人话原因（拼进错误 human / refused 列表）。
    pub fn reason(self) -> &'static str {
        match self {
            Refusal::HomeRoot => "refuses to use the home directory itself as a data path",
            Refusal::OutsideHome => "outside the home directory",
            Refusal::SymlinkRoot => "the path is a symlink",
        }
    }
}

/// 判定结果。
#[derive(Debug, Clone)]
pub enum Decision {
    /// 可用条目
    Ok(DataPathEntry),
    /// **策略拒绝**（home 本身 / home 之外 / 软链根）——破坏性操作必须中止。
    Refused(Refusal),
    /// I/O 失败（stat 出错，且不是「不存在」）——数据可能在但读不到。
    Failed(String),
}

/// 把声明路径解析成 home 相对条目（**统一判定入口**）。
///
/// - `~` / `~/…` → home 下；绝对路径 → 原样；相对路径 → 相对 home；
/// - **home 本身 → 拒绝**（[`Refusal::HomeRoot`]）；
/// - home 之外 → 拒绝；软链根 → 拒绝；
/// - 类型：存在 → 目录/文件；不存在 → [`PathKind::Unknown`]。
pub fn resolve(raw: &str, home_real: &Path) -> Decision {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Decision::Refused(Refusal::OutsideHome);
    }
    let home_lexical = paths::home_dir();
    let expanded = expand(trimmed, home_real);
    let norm = lexical_normalize(&expanded);

    // ① home 本身（`~` / `~/.` / home 绝对路径）→ 拒绝
    if norm == *home_real || norm == home_lexical {
        return Decision::Refused(Refusal::HomeRoot);
    }

    // ② 相对 home 的路径
    let rel = match norm.strip_prefix(home_real) {
        Ok(r) => r.to_path_buf(),
        Err(_) => match norm.strip_prefix(&home_lexical) {
            Ok(r) => r.to_path_buf(),
            Err(_) => return Decision::Refused(Refusal::OutsideHome),
        },
    };
    if rel.as_os_str().is_empty() {
        return Decision::Refused(Refusal::HomeRoot);
    }

    // ③ 存在性 → 显式类型
    let abs = home_real.join(&rel);
    match fs::symlink_metadata(&abs) {
        Ok(m) => {
            if m.file_type().is_symlink() {
                return Decision::Refused(Refusal::SymlinkRoot);
            }
            // 祖先是链接且解析到 home 之外 → 越界（Windows junction 只有 canonicalize 拦得住）
            if let Ok(c) = fs::canonicalize(&abs) {
                if !(c == *home_real || c.starts_with(home_real)) {
                    return Decision::Refused(Refusal::OutsideHome);
                }
            }
            let kind = if m.is_dir() {
                PathKind::Dir
            } else {
                PathKind::File
            };
            Decision::Ok(DataPathEntry { rel, kind })
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Decision::Ok(DataPathEntry {
            rel,
            kind: PathKind::Unknown,
        }),
        Err(e) => Decision::Failed(format!("{raw} (stat: {e})")),
    }
}

/// 白名单构建（备份侧 / 还原侧共用）：把声明路径展开成条目 + 记录被拒的声明。
///
/// 返回 `(条目, 被拒声明)`——被拒的声明**绝不放行**（home 本身 = 放开整个 home）。
pub fn build_entries(data_paths: &[String], home_real: &Path) -> (Vec<DataPathEntry>, Vec<String>) {
    let mut entries = Vec::new();
    let mut refused = Vec::new();
    for raw in data_paths {
        match resolve(raw, home_real) {
            Decision::Ok(e) => entries.push(e),
            Decision::Refused(r) => refused.push(format!("{raw} ({})", r.reason())),
            Decision::Failed(m) => refused.push(m),
        }
    }
    (entries, refused)
}

/// 覆盖 target 的条目类型（精确命中优先；否则取覆盖它的目录型条目）。
///
/// `None` = 没有条目覆盖该 target。
pub fn kind_at(entries: &[DataPathEntry], target: &Path) -> Option<PathKind> {
    if let Some(e) = entries.iter().find(|e| e.rel == target) {
        return Some(e.kind);
    }
    entries
        .iter()
        .find(|e| e.kind == PathKind::Dir && target.starts_with(&e.rel))
        .map(|e| e.kind)
}

/// 白名单路径展开：`~` / `~/…` 相对 **canonical home**；绝对路径原样；相对路径相对 home。
fn expand(raw: &str, home_real: &Path) -> PathBuf {
    if raw == "~" {
        return home_real.to_path_buf();
    }
    if let Some(rest) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\")) {
        return home_real.join(rest);
    }
    let p = paths::expand(raw);
    if p.is_absolute() {
        p
    } else {
        home_real.join(p)
    }
}

/// 词法规范化：去掉 `.` 段、解析 `..`（不触碰文件系统）。
pub fn lexical_normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
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
            "aibox-datapaths-{tag}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        fs::canonicalize(d).unwrap()
    }

    /// home 本身（三种写法）→ 一律拒绝。
    #[test]
    fn refuses_home_itself_in_every_spelling() {
        let root = tmpdir("home-root");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let home = fs::canonicalize(&home).unwrap();
        for raw in [
            "~".to_string(),
            "~/.".to_string(),
            home.to_string_lossy().into_owned(),
        ] {
            match resolve(&raw, &home) {
                Decision::Refused(Refusal::HomeRoot) => {}
                other => panic!("{raw} must be refused as home root, got {other:?}"),
            }
        }
    }

    /// home 之外 → 拒绝。
    #[test]
    fn refuses_outside_home() {
        let root = tmpdir("outside");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let home = fs::canonicalize(&home).unwrap();
        match resolve(&root.join("elsewhere").to_string_lossy(), &home) {
            Decision::Refused(Refusal::OutsideHome) => {}
            other => panic!("must be refused, got {other:?}"),
        }
    }

    /// 显式类型：存在的目录 → Dir；存在的文件 → File；不存在 → Unknown。
    #[test]
    fn resolves_explicit_kind() {
        let root = tmpdir("kind");
        let home = root.join("home");
        fs::create_dir_all(home.join("dir")).unwrap();
        fs::write(home.join("file.txt"), "x").unwrap();
        let home = fs::canonicalize(&home).unwrap();

        match resolve(&home.join("dir").to_string_lossy(), &home) {
            Decision::Ok(e) => assert_eq!(e.kind, PathKind::Dir),
            other => panic!("{other:?}"),
        }
        match resolve(&home.join("file.txt").to_string_lossy(), &home) {
            Decision::Ok(e) => assert_eq!(e.kind, PathKind::File),
            other => panic!("{other:?}"),
        }
        match resolve(&home.join("gone.json").to_string_lossy(), &home) {
            Decision::Ok(e) => assert_eq!(e.kind, PathKind::Unknown),
            other => panic!("{other:?}"),
        }
    }

    /// Unknown 条目只覆盖精确命中；Dir 条目覆盖子树。
    #[test]
    fn coverage_rules() {
        let dir = DataPathEntry {
            rel: PathBuf::from("appdata"),
            kind: PathKind::Dir,
        };
        let file = DataPathEntry {
            rel: PathBuf::from("cfg.json"),
            kind: PathKind::File,
        };
        let unknown = DataPathEntry {
            rel: PathBuf::from("later.json"),
            kind: PathKind::Unknown,
        };
        assert!(dir.covers(Path::new("appdata/cfg.json")));
        assert!(file.covers(Path::new("cfg.json")));
        assert!(!file.covers(Path::new("cfg.json/x")));
        assert!(unknown.covers(Path::new("later.json")));
        assert!(!unknown.covers(Path::new("later.json/x")));
    }
}
