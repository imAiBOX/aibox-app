//! Read-only review of folders left by previous restores. The OS file manager
//! receives one validated folder to reveal. AiBOX never traverses its contents.
//!
//! Do not add path-based permanent deletion here. A canonical-path or metadata
//! check cannot bind a later recursive delete to the same directory tree. The
//! operation would need platform-specific handle-relative traversal, including
//! Windows reparse-point handling and protection against directory replacement.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::{manage_paths, CoreError, CoreResult};

const PREFIX: &str = "pre-restore-";

// Preserver uses a 14-digit UTC timestamp, optionally followed by a numeric
// collision counter or an 8-digit hexadecimal fallback.
fn valid_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix(PREFIX) else {
        return false;
    };
    let (stamp, suffix) = rest
        .split_once('-')
        .map_or((rest, None), |(a, b)| (a, Some(b)));
    stamp.len() == 14
        && stamp.bytes().all(|b| b.is_ascii_digit())
        && suffix.is_none_or(|s| {
            ((1..=3).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit()))
                || (s.len() == 8 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        })
}

fn unavailable() -> CoreError {
    CoreError::new(
        "PRESERVED_UNAVAILABLE",
        "The preserved-data folders could not be checked",
    )
}

fn not_found() -> CoreError {
    CoreError::new(
        "PRESERVED_NOT_FOUND",
        "That preserved-data folder is no longer available",
    )
}

fn backup_root(home: &Path) -> CoreResult<Option<PathBuf>> {
    // Canonicalize the OS-provided home anchor, then reject links and Windows
    // reparse points on both app-owned components below it.
    if !home.is_absolute() {
        return Err(unavailable());
    }
    let home = fs::canonicalize(home).map_err(|_| unavailable())?;
    let app_root = home.join(crate::AIBOX_DIR);
    let backup = app_root.join("backup");
    for path in [&app_root, &backup] {
        match fs::symlink_metadata(path) {
            Ok(meta) if meta.is_dir() && !manage_paths::is_link_or_reparse(&meta) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            _ => return Err(unavailable()),
        }
    }
    if fs::canonicalize(&backup).map_err(|_| unavailable())? != backup {
        return Err(unavailable());
    }
    Ok(Some(backup))
}

/// List names only. Matching names are candidates for review, not a provenance
/// claim. Old folders may lack a journal and inspecting journals could read paths.
pub fn list_in_home(home: &Path) -> CoreResult<Vec<String>> {
    let Some(root) = backup_root(home)? else {
        return Ok(Vec::new());
    };
    let mut names = Vec::new();
    for item in fs::read_dir(root).map_err(|_| unavailable())? {
        let item = item.map_err(|_| unavailable())?;
        let Ok(name) = item.file_name().into_string() else {
            continue;
        };
        if !valid_name(&name) {
            continue;
        }
        let meta = match fs::symlink_metadata(item.path()) {
            Ok(meta) => meta,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(_) => return Err(unavailable()),
        };
        if meta.is_dir() && !manage_paths::is_link_or_reparse(&meta) {
            names.push(name);
        }
    }
    names.sort_unstable_by(|a, b| b.cmp(a));
    Ok(names)
}

/// Recheck the name and current filesystem on each open request. No IPC caller
/// can supply a path or select a normal backup via this entry point.
pub fn folder_to_reveal(home: &Path, name: &str) -> CoreResult<PathBuf> {
    if !valid_name(name) {
        return Err(not_found());
    }
    let root = backup_root(home)?.ok_or_else(not_found)?;
    let path = root.join(name);
    let meta = fs::symlink_metadata(&path).map_err(|e| {
        if e.kind() == io::ErrorKind::NotFound {
            not_found()
        } else {
            unavailable()
        }
    })?;
    if !meta.is_dir() || manage_paths::is_link_or_reparse(&meta) {
        return Err(not_found());
    }
    let real = fs::canonicalize(&path).map_err(|_| not_found())?;
    if real != path {
        return Err(not_found());
    }
    Ok(real)
}

#[cfg(test)]
mod tests {
    use super::*;

    const OLD: &str = "pre-restore-20261123000000";
    const NEW: &str = "pre-restore-20261124000000-12";

    struct Fixture(PathBuf);

    impl Fixture {
        fn new(label: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "aibox-preserved-{label}-{}-{:?}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir(&dir).unwrap();
            Self(dir)
        }

        fn backup(&self) -> PathBuf {
            self.0.join(crate::AIBOX_DIR).join("backup")
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn lists_only_preserved_directories_and_does_not_change_data() {
        let home = Fixture::new("list");
        let backup = home.backup();
        assert!(list_in_home(&home.0).unwrap().is_empty());
        let preserved = backup.join(OLD).join("old.json");
        let real_backup = backup.join("20261123000000/app/data.json");
        let live = home.0.join("data.json");
        for path in [&preserved, &real_backup, &live] {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, b"untouched").unwrap();
        }
        fs::create_dir(backup.join(NEW)).unwrap();
        fs::create_dir(backup.join("pre-restore-unrelated")).unwrap();
        fs::write(backup.join("pre-restore-20261125000000"), b"not a folder").unwrap();
        assert_eq!(list_in_home(&home.0).unwrap(), [NEW, OLD]);
        assert_eq!(
            folder_to_reveal(&home.0, OLD).unwrap(),
            fs::canonicalize(backup.join(OLD)).unwrap()
        );
        for path in [&preserved, &real_backup, &live] {
            assert_eq!(fs::read(path).unwrap(), b"untouched");
        }
        fs::remove_dir(backup.join(NEW)).unwrap();
        assert_eq!(
            folder_to_reveal(&home.0, NEW).unwrap_err().code,
            "PRESERVED_NOT_FOUND"
        );
        assert_eq!(list_in_home(&home.0).unwrap(), [OLD]);
    }

    #[test]
    fn rejects_path_arguments_and_validates_collision_names() {
        let home = Fixture::new("names");
        for bad in [
            "20261123000000",
            "pre-restore-",
            "../pre-restore-20261123000000",
            "pre-restore-20261123000000/..",
            "pre-restore-20261123000000\\abc",
            "pre-restore-20261123000000-",
            "pre-restore-20261123000000. ",
        ] {
            assert!(!valid_name(bad));
            assert_eq!(
                folder_to_reveal(&home.0, bad).unwrap_err().code,
                "PRESERVED_NOT_FOUND"
            );
        }
        assert!(valid_name("pre-restore-20261123000000-00c0ffee"));
    }

    #[test]
    fn invalid_backup_root_is_an_error_not_an_empty_list() {
        let home = Fixture::new("bad-root");
        fs::create_dir_all(home.backup().parent().unwrap()).unwrap();
        fs::write(home.backup(), b"not a folder").unwrap();
        assert_eq!(
            list_in_home(&home.0).unwrap_err().code,
            "PRESERVED_UNAVAILABLE"
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_linked_candidates_and_linked_backup_ancestors() {
        use std::os::unix::fs::symlink;

        let home = Fixture::new("links");
        let outside = Fixture::new("outside");
        let backup = home.backup();
        fs::create_dir_all(&backup).unwrap();
        symlink(&outside.0, backup.join(OLD)).unwrap();
        assert!(list_in_home(&home.0).unwrap().is_empty());
        assert_eq!(
            folder_to_reveal(&home.0, OLD).unwrap_err().code,
            "PRESERVED_NOT_FOUND"
        );
        fs::remove_file(backup.join(OLD)).unwrap();
        fs::remove_dir(&backup).unwrap();
        symlink(&outside.0, &backup).unwrap();
        assert_eq!(
            list_in_home(&home.0).unwrap_err().code,
            "PRESERVED_UNAVAILABLE"
        );
        fs::remove_file(&backup).unwrap();
        let app_root = backup.parent().unwrap();
        fs::remove_dir(app_root).unwrap();
        symlink(&outside.0, app_root).unwrap();
        assert_eq!(
            folder_to_reveal(&home.0, OLD).unwrap_err().code,
            "PRESERVED_UNAVAILABLE"
        );
    }
}
