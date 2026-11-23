//! Filesystem safety boundary shared by maintenance backup and removal.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use crate::err::{CoreError, CoreResult};

fn starts_with(path: &Path, base: &Path) -> bool {
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        path.starts_with(base)
    }
    #[cfg(any(windows, target_os = "macos"))]
    {
        let mut components = path.components();
        base.components().all(|b| {
            components.next().is_some_and(|a| {
                a.as_os_str().to_string_lossy().to_uppercase()
                    == b.as_os_str().to_string_lossy().to_uppercase()
            })
        })
    }
}

pub(crate) fn checked_path(path: &Path) -> CoreResult<PathBuf> {
    let invalid = |component| match component {
        Component::ParentDir => true,
        #[cfg(windows)]
        Component::Prefix(p) => !matches!(
            p.kind(),
            std::path::Prefix::Disk(_) | std::path::Prefix::VerbatimDisk(_)
        ),
        #[cfg(windows)]
        Component::Normal(n) => {
            let s = n.to_string_lossy();
            let stem = s
                .split('.')
                .next()
                .unwrap_or_default()
                .trim_end()
                .to_ascii_uppercase();
            s.ends_with(['.', ' '])
                || s.contains([':', '*', '?', '%', '<', '>', '|', '"'])
                || s.chars().any(char::is_control)
                || matches!(
                    stem.as_str(),
                    "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
                )
                || ["COM", "LPT"].iter().any(|prefix| {
                    stem.strip_prefix(prefix).is_some_and(|n| {
                        (n.len() == 1 && n.as_bytes()[0].is_ascii_digit())
                            || matches!(n, "¹" | "²" | "³")
                    })
                })
        }
        _ => false,
    };
    if !path.is_absolute() || path.components().any(invalid) {
        return Err(CoreError::new(
            "MANAGE_UNSAFE_PATH",
            format!("ambiguous path: {}", path.display()),
        ));
    }
    Ok(path.components().collect())
}

fn path_error(path: &Path, error: io::Error) -> CoreError {
    CoreError::new(
        "MANAGE_DELETE_FAILED",
        format!("cannot inspect {}: {error}", path.display()),
    )
}

/// Resolve aliases even when the leaf is absent, using the closest existing ancestor.
fn projected_path(path: &Path) -> CoreResult<PathBuf> {
    let mut cursor = path;
    let mut missing = Vec::new();
    loop {
        match fs::canonicalize(cursor) {
            Ok(mut existing) => {
                existing.extend(missing.into_iter().rev());
                return Ok(existing);
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                match (cursor.parent(), cursor.file_name()) {
                    (Some(parent), Some(name)) => {
                        missing.push(name);
                        cursor = parent;
                    }
                    _ => return Err(path_error(cursor, e)),
                }
            }
            Err(e) => return Err(path_error(cursor, e)),
        }
    }
}

pub(crate) struct ProtectedPaths {
    home: PathBuf,
    app_root: PathBuf,
    shared: [PathBuf; 3],
}

impl ProtectedPaths {
    pub(crate) fn new(home: &Path, backup_root: &Path) -> Self {
        let app_root = home.join(crate::AIBOX_DIR);
        Self {
            home: home.to_path_buf(),
            shared: [
                app_root.join("backup"),
                app_root.join("prefix"),
                backup_root.to_path_buf(),
            ],
            app_root,
        }
    }

    pub(crate) fn check(&self, path: &Path) -> CoreResult<()> {
        let reject = || {
            CoreError::new(
                "MANAGE_PROTECTED",
                format!("refusing protected path {}", path.display()),
            )
        };
        let overlaps = |a: &Path, b: &Path| starts_with(a, b) || starts_with(b, a);
        if path.parent().is_none()
            || starts_with(&self.home, path)
            || starts_with(&self.app_root, path)
            || self.shared.iter().any(|p| overlaps(p, path))
        {
            return Err(reject());
        }
        let resolved = projected_path(path)?;
        if starts_with(&projected_path(&self.home)?, &resolved)
            || starts_with(&projected_path(&self.app_root)?, &resolved)
        {
            return Err(reject());
        }
        for shared in &self.shared {
            if overlaps(&projected_path(shared)?, &resolved) {
                return Err(reject());
            }
        }
        Ok(())
    }
}

pub(crate) fn is_link_or_reparse(meta: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return true;
        }
    }
    meta.file_type().is_symlink()
}

fn reject_link(path: &Path) -> CoreError {
    CoreError::new(
        "MANAGE_SYMLINK_ESCAPE",
        format!("path traverses a link or reparse point: {}", path.display()),
    )
}

pub(crate) fn inspect_components(target: &Path) -> CoreResult<Option<fs::Metadata>> {
    let components: Vec<_> = target.ancestors().filter(|p| p.has_root()).collect();
    for p in components.into_iter().rev() {
        match fs::symlink_metadata(p) {
            Ok(m) if is_link_or_reparse(&m) => return Err(reject_link(p)),
            Ok(m) if p == target => return Ok(Some(m)),
            Ok(m) if !m.is_dir() => return Err(path_error(p, io::ErrorKind::NotADirectory.into())),
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(path_error(p, e)),
        }
    }
    Err(CoreError::new(
        "MANAGE_UNSAFE_PATH",
        "cannot inspect a path without a filesystem root",
    ))
}

#[cfg(windows)]
fn inspect_tree(target: &Path) -> CoreResult<()> {
    let mut dirs = vec![target.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for entry in fs::read_dir(&dir).map_err(|e| path_error(&dir, e))? {
            let p = entry.map_err(|e| path_error(&dir, e))?.path();
            let meta = fs::symlink_metadata(&p).map_err(|e| path_error(&p, e))?;
            if is_link_or_reparse(&meta) {
                return Err(reject_link(&p));
            }
            if meta.is_dir() {
                dirs.push(p);
            }
        }
    }
    Ok(())
}

pub(crate) fn preflight_target(
    allow: &[PathBuf],
    target: &Path,
    protected: &ProtectedPaths,
) -> CoreResult<bool> {
    if !allow.iter().any(|a| starts_with(target, a)) {
        return Err(CoreError::new(
            "MANAGE_OUT_OF_SCOPE",
            format!("path not in declared list: {}", target.display()),
        ));
    }
    let meta = inspect_components(target)?;
    protected.check(target)?;
    #[cfg(windows)]
    if meta.as_ref().is_some_and(|m| m.is_dir()) {
        inspect_tree(target)?;
    }
    Ok(meta.is_some())
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn rejects_ambiguous_windows_paths_before_io() {
        for raw in [
            r"C:\fixture\folder.",
            r"C:\fixture\folder ",
            r"C:\fixture\file:stream",
            r"C:\fixture\NUL.txt",
            r"C:\fixture\..\other",
            r"\\server\share\fixture",
        ] {
            assert_eq!(
                checked_path(Path::new(raw)).unwrap_err().code,
                "MANAGE_UNSAFE_PATH"
            );
        }
        assert!(checked_path(Path::new(r"C:\fixture\file.txt")).is_ok());
    }
}
