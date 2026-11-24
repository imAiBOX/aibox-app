//! macOS implementation; Linux exercises the same descriptor-based transaction in tests.
//! Paths are for display only after traversal. In particular, publication is a
//! same-filesystem rename from a private staging directory, never a write through
//! the destination name or an unlink followed by a write.

use std::ffi::OsStr;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use rustix::fd::OwnedFd;
use rustix::fs::{self, AtFlags, FileType, Mode, OFlags, RenameFlags, Stat};
use rustix::io::Errno;
use uuid::Uuid;

use super::{ConfigWriteResult, CoreError, CoreResult};

const MAX_CONFIG_BYTES: u64 = 8 * 1024 * 1024;
const STAGED_CONFIG: &str = "config";
const STAGED_BACKUP: &str = "backup";

fn unsafe_path() -> CoreError {
    CoreError::new(
        "CONFIG_FORMAT_UNSUPPORTED",
        "Configuration path is not a safe local file",
    )
}

fn io_error(e: Errno) -> CoreError {
    std::io::Error::from(e).into()
}

fn dir_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}

fn open_dir(parent: &OwnedFd, name: &OsStr, create: bool) -> CoreResult<OwnedFd> {
    let open = || fs::openat(parent, name, dir_flags(), Mode::empty());
    let result = match open() {
        Err(Errno::NOENT) if create => {
            match fs::mkdirat(parent, name, Mode::RUSR | Mode::WUSR | Mode::XUSR) {
                Ok(()) | Err(Errno::EXIST) => {}
                Err(e) => return Err(io_error(e)),
            }
            open()
        }
        result => result,
    };
    result.map_err(|e| match e {
        Errno::LOOP | Errno::NOTDIR => unsafe_path(),
        _ => io_error(e),
    })
}

fn require_private_owner(dir: &OwnedFd) -> CoreResult<()> {
    let stat = fs::fstat(dir).map_err(io_error)?;
    if stat.st_uid != rustix::process::geteuid().as_raw()
        || Mode::from_raw_mode(stat.st_mode).intersects(Mode::WGRP | Mode::WOTH)
    {
        return Err(unsafe_path());
    }
    Ok(())
}

struct Target<'a> {
    _home: OwnedFd,
    parent: OwnedFd,
    parent_display: PathBuf,
    leaf: &'a str,
}

impl<'a> Target<'a> {
    fn open(home: &Path, relative: &'a str) -> CoreResult<Self> {
        let mut components = home.components();
        if components.next() != Some(Component::RootDir) {
            return Err(unsafe_path());
        }
        let mut home_fd = fs::open("/", dir_flags(), Mode::empty()).map_err(io_error)?;
        let mut depth = 0;
        for component in components {
            let Component::Normal(name) = component else {
                return Err(unsafe_path());
            };
            home_fd = open_dir(&home_fd, name, false)?;
            depth += 1;
        }
        if depth == 0 {
            return Err(unsafe_path());
        }
        require_private_owner(&home_fd)?;

        let mut parts = relative.split('/').peekable();
        // Validate the entire relative path before creating any directory.
        if parts.clone().any(|part| matches!(part, "" | "." | "..")) {
            return Err(unsafe_path());
        }
        let mut parent = None;
        let mut parent_display = home.to_owned();
        while let Some(part) = parts.next() {
            if parts.peek().is_none() {
                return Ok(Self {
                    parent: match parent {
                        Some(fd) => fd,
                        None => rustix::io::fcntl_dupfd_cloexec(&home_fd, 0).map_err(io_error)?,
                    },
                    _home: home_fd,
                    parent_display,
                    leaf: part,
                });
            }
            let next = open_dir(parent.as_ref().unwrap_or(&home_fd), OsStr::new(part), true)?;
            require_private_owner(&next)?;
            parent = Some(next);
            parent_display.push(part);
        }
        Err(unsafe_path())
    }

    fn read(&self) -> CoreResult<Option<(File, String)>> {
        // Precheck special files without opening them; the open itself independently
        // enforces no-follow, and the actual descriptor must still match a regular file.
        let before = match fs::statat(&self.parent, self.leaf, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => stat,
            Err(Errno::NOENT) => return Ok(None),
            Err(e) => return Err(io_error(e)),
        };
        require_regular(&before)?;
        let fd = fs::openat(
            &self.parent,
            self.leaf,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(io_error)?;
        let opened = fs::fstat(&fd).map_err(io_error)?;
        require_regular(&opened)?;
        if !same_file(&before, &opened) {
            return Err(changed());
        }
        let mut file = File::from(fd);
        let mut text = String::new();
        (&mut file)
            .take(MAX_CONFIG_BYTES + 1)
            .read_to_string(&mut text)?;
        if text.len() as u64 > MAX_CONFIG_BYTES {
            return Err(unsafe_path());
        }
        Ok(Some((file, text)))
    }
}

fn require_regular(stat: &Stat) -> CoreResult<()> {
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile
        || stat.st_nlink != 1
        || stat.st_uid != rustix::process::geteuid().as_raw()
        || Mode::from_raw_mode(stat.st_mode).intersects(Mode::WGRP | Mode::WOTH)
    {
        return Err(unsafe_path());
    }
    Ok(())
}

fn same_file(a: &Stat, b: &Stat) -> bool {
    a.st_dev == b.st_dev && a.st_ino == b.st_ino
}

fn changed() -> CoreError {
    CoreError::new(
        "CONFIG_FAILED",
        "The local configuration changed during the write; try again",
    )
}

struct Stage<'a> {
    parent: &'a OwnedFd,
    dir: OwnedFd,
    name: String,
}

impl<'a> Stage<'a> {
    fn create(parent: &'a OwnedFd) -> CoreResult<Self> {
        let name = format!(".aibox-config-{}", Uuid::new_v4());
        fs::mkdirat(parent, name.as_str(), Mode::RUSR | Mode::WUSR | Mode::XUSR)
            .map_err(io_error)?;
        let dir = open_dir(parent, OsStr::new(&name), false)?;
        let stage = Self { parent, dir, name };
        require_private_owner(&stage.dir)?;
        Ok(stage)
    }

    fn write(&self, name: &str, text: &str) -> CoreResult<Stat> {
        let fd = fs::openat(
            &self.dir,
            name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(io_error)?;
        let mut file = File::from(fd);
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        fs::fstat(&file).map_err(io_error)
    }
}

impl Drop for Stage<'_> {
    fn drop(&mut self) {
        for name in [STAGED_CONFIG, STAGED_BACKUP] {
            let _ = fs::unlinkat(&self.dir, name, AtFlags::empty());
        }
        let name = self.name.as_str();
        if let (Ok(opened), Ok(named)) = (
            fs::fstat(&self.dir),
            fs::statat(self.parent, name, AtFlags::SYMLINK_NOFOLLOW),
        ) {
            if same_file(&opened, &named) {
                let _ = fs::unlinkat(self.parent, name, AtFlags::REMOVEDIR);
            }
        }
    }
}

pub(super) fn write(
    home: &Path,
    relative: &str,
    render: impl FnOnce(&str) -> CoreResult<(String, Vec<String>)>,
) -> CoreResult<ConfigWriteResult> {
    let target = Target::open(home, relative)?;
    let previous = target.read()?;
    let (text, merged_keys) = render(previous.as_ref().map_or("", |(_, old)| old))?;
    if text.len() as u64 > MAX_CONFIG_BYTES {
        return Err(unsafe_path());
    }
    let stage = Stage::create(&target.parent)?;
    let staged = stage.write(STAGED_CONFIG, &text)?;
    let backup_path = if let Some((old_file, old_text)) = previous {
        stage.write(STAGED_BACKUP, &old_text)?;
        let (current, current_text) = target.read()?.ok_or_else(changed)?;
        let old = fs::fstat(&old_file).map_err(io_error)?;
        let current = fs::fstat(&current).map_err(io_error)?;
        if !same_file(&old, &current) || current_text != old_text {
            return Err(changed());
        }
        let backup_name = format!(
            "{}.bak.{}.{}",
            target.leaf,
            crate::fsops::now_ts(),
            Uuid::new_v4()
        );
        fs::renameat_with(
            &stage.dir,
            STAGED_BACKUP,
            &target.parent,
            backup_name.as_str(),
            RenameFlags::NOREPLACE,
        )
        .map_err(io_error)?;
        // Recheck the leaf after publishing the backup. This narrows the race
        // with external editors, but POSIX has no compare-inode-and-replace.
        let named =
            fs::statat(&target.parent, target.leaf, AtFlags::SYMLINK_NOFOLLOW).map_err(io_error)?;
        require_regular(&named)?;
        if !same_file(&old, &named) {
            return Err(changed());
        }
        // renameat operates on the entry itself, never follows a symlink at the
        // leaf, and cannot overwrite a directory with this regular file.
        fs::renameat(&stage.dir, STAGED_CONFIG, &target.parent, target.leaf).map_err(io_error)?;
        target
            .parent_display
            .join(backup_name)
            .to_string_lossy()
            .into_owned()
    } else {
        // This must stay an atomic no-replace operation. Never emulate it with
        // a precheck followed by a replacing rename on unsupported filesystems.
        fs::renameat_with(
            &stage.dir,
            STAGED_CONFIG,
            &target.parent,
            target.leaf,
            RenameFlags::NOREPLACE,
        )
        .map_err(|e| {
            if e == Errno::EXIST {
                changed()
            } else {
                io_error(e)
            }
        })?;
        String::new()
    };
    let (file, actual) = target.read()?.ok_or_else(changed)?;
    let ok = same_file(&staged, &fs::fstat(&file).map_err(io_error)?) && actual == text;
    Ok(ConfigWriteResult {
        backup_path,
        ok,
        merged_keys,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{create_dir, read_to_string, symlink_metadata, write as put};
    use std::os::unix::fs::{symlink, PermissionsExt};

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let dir = std::env::temp_dir()
                .canonicalize()
                .unwrap()
                .join(Uuid::new_v4().to_string());
            create_dir(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn no_render(_: &str) -> CoreResult<(String, Vec<String>)> {
        panic!("unsafe paths must be rejected before rendering")
    }

    #[test]
    fn does_not_open_symlinks_in_home_ancestors_parent_or_leaf() {
        let root = TestDir::new();
        let home = root.0.join("home");
        create_dir(&home).unwrap();
        create_dir(home.join("dir")).unwrap();
        put(home.join("dir/actual.json"), "unchanged").unwrap();
        symlink(&home, root.0.join("alias")).unwrap();
        symlink("dir", home.join("dir-alias")).unwrap();
        symlink("actual.json", home.join("dir/alias.json")).unwrap();
        symlink("missing.json", home.join("dir/dangling.json")).unwrap();
        for (origin, relative) in [
            (root.0.join("alias"), "dir/cfg.json"),
            (root.0.join("alias/dir"), "cfg.json"),
            (home.clone(), "dir-alias/cfg.json"),
            (home.clone(), "dir/alias.json"),
            (home.clone(), "dir/dangling.json"),
        ] {
            assert_eq!(
                write(&origin, relative, no_render).unwrap_err().code,
                "CONFIG_FORMAT_UNSUPPORTED"
            );
        }
        assert_eq!(
            read_to_string(home.join("dir/actual.json")).unwrap(),
            "unchanged"
        );
        assert!(!home.join("dir/cfg.json").exists());
        assert!(!home.join("dir/missing.json").exists());
    }

    #[test]
    fn final_read_and_write_remain_bound_to_the_open_parent() {
        let root = TestDir::new();
        let parent = root.0.join("app");
        let moved = root.0.join("moved");
        let out = write(&root.0, "app/config.json", |_| {
            std::fs::rename(&parent, &moved).unwrap();
            create_dir(&parent).unwrap();
            put(parent.join("config.json"), "other directory").unwrap();
            Ok(("pinned directory".into(), vec![]))
        })
        .unwrap();
        assert!(out.ok);
        assert_eq!(
            read_to_string(parent.join("config.json")).unwrap(),
            "other directory"
        );
        assert_eq!(
            read_to_string(moved.join("config.json")).unwrap(),
            "pinned directory"
        );
    }

    #[test]
    fn first_install_never_clobbers_a_later_file() {
        let root = TestDir::new();
        let file = root.0.join("inner/config.json");
        let error = write(&root.0, "inner/config.json", |old| {
            assert!(old.is_empty());
            put(&file, "new local configuration").unwrap();
            Ok(("automatic configuration".into(), vec![]))
        })
        .unwrap_err();
        assert_eq!(error.code, "CONFIG_FAILED");
        assert_eq!(read_to_string(&file).unwrap(), "new local configuration");
        assert_eq!(
            std::fs::read_dir(file.parent().unwrap()).unwrap().count(),
            1
        );
    }

    #[test]
    fn rejects_an_external_edit_observed_before_publish() {
        let root = TestDir::new();
        let file = root.0.join("config.json");
        put(&file, "previous").unwrap();
        let error = write(&root.0, "config.json", |old| {
            assert_eq!(old, "previous");
            put(&file, "local edit").unwrap();
            Ok(("automatic configuration".into(), vec![]))
        })
        .unwrap_err();
        assert_eq!(error.code, "CONFIG_FAILED");
        assert_eq!(read_to_string(&file).unwrap(), "local edit");
        assert_eq!(std::fs::read_dir(&root.0).unwrap().count(), 1);
    }

    #[test]
    fn writes_and_backups_are_private_and_cleanup_is_local() {
        let root = TestDir::new();
        let relative = "app/config.json";
        let first = write(&root.0, relative, |_| Ok(("previous".into(), vec![]))).unwrap();
        assert!(first.ok && first.backup_path.is_empty());
        let second = write(&root.0, relative, |_| Ok(("current".into(), vec![]))).unwrap();
        assert!(second.ok);
        let backup = PathBuf::from(second.backup_path);
        let target = root.0.join(relative);
        for (path, expected) in [(backup, "previous"), (target, "current")] {
            assert_eq!(read_to_string(&path).unwrap(), expected);
            assert_eq!(
                symlink_metadata(path).unwrap().permissions().mode() & 0o077,
                0
            );
        }
        assert_eq!(std::fs::read_dir(root.0.join("app")).unwrap().count(), 2);
    }
}
