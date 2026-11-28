// Copyright (C) 2026 dekajoule
// SPDX-License-Identifier: GPL-2.0-or-later

//! Files replaced in one step: a temporary file next to the target, fsync, rename, then the directory synced. The scope
//! is switched off at the mains, so a later start finds either the old file or the new one, never a part.

use std::io::Write;
use std::path::{Path, PathBuf};

/// The helper that brings the card to a clean state before the power goes or another kernel starts: the USB stick
/// unmounted, everything synced, the data partition read only (`rootfs/initramfs/bin/oos-data-close`).
pub const DATA_CLOSE: &str = "/bin/oos-data-close";

/// Runs `DATA_CLOSE` with `what` (`close` or `reopen`): its line, or why it failed (the helper's own steps have time
/// limits).
pub fn data_close(what: &str) -> Result<String, String> {
    let out =
        std::process::Command::new(DATA_CLOSE).arg(what).output().map_err(|e| format!("{}: {}", DATA_CLOSE, e))?;
    let line = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if out.status.success() {
        Ok(line)
    } else {
        Err(if line.is_empty() { format!("{} {}: {}", DATA_CLOSE, what, out.status) } else { line })
    }
}

/// What a failed sync of the directory after the rename means.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DirSync {
    /// the directory is not synced
    Skip,
    /// synced, a failure is ignored
    #[default]
    BestEffort,
    /// synced, a failure is the caller's error (Unix only: Windows opens no directory as a file)
    Required,
}

/// How `write_atomic` writes.
#[derive(Clone, Copy, Debug, Default)]
pub struct Opts<'a> {
    /// the temporary file; None = the target's path with `.new` appended
    pub tmp: Option<&'a Path>,
    /// the old file is kept as the target's path with `.bak` appended
    pub bak: bool,
    /// the mode a new file gets (Unix), e.g. 0o600 for keys
    pub mode: Option<u32>,
    pub dir_sync: DirSync,
}

/// `path` with `ext` appended to its whole name (`a.conf` -> `a.conf.new`).
fn appended(path: &Path, ext: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(ext);
    PathBuf::from(s)
}

/// Replaces `path` with `bytes` in one step (see the module). The directory must exist.
pub fn write_atomic(path: &Path, bytes: &[u8], o: &Opts) -> std::io::Result<()> {
    let tmp = o.tmp.map_or_else(|| appended(path, ".new"), Path::to_path_buf);
    // a temporary file left by an earlier attempt goes first, and the new one is created, never opened: on a foreign
    // stick a symbolic link in its place would otherwise have the file it points to written over
    match std::fs::remove_file(&tmp) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    let mut open = std::fs::OpenOptions::new();
    open.write(true).create_new(true);
    if let Some(m) = o.mode {
        std::os::unix::fs::OpenOptionsExt::mode(&mut open, m);
    }
    {
        let mut f = open.open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    if o.bak && path.exists() {
        std::fs::rename(path, appended(path, ".bak"))?;
    }
    std::fs::rename(&tmp, path)?;
    match o.dir_sync {
        DirSync::Skip => Ok(()),
        DirSync::BestEffort => {
            let _ = sync_dir(path);
            Ok(())
        }
        DirSync::Required => sync_dir(path),
    }
}

/// Makes a rename or a removal of `path` durable: syncs the directory it is in.
pub fn sync_dir(path: &Path) -> std::io::Result<()> {
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    std::fs::File::open(dir)?.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("oos-fsutil-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    // Break it catches: a temporary file left behind, a backup of the new instead of the old content, a key file
    // readable by others
    #[test]
    fn a_file_is_replaced_with_backup_and_mode() {
        let d = dir("replace");
        let p = d.join("a.hex");
        write_atomic(&p, b"one", &Opts::default()).unwrap();
        write_atomic(
            &p,
            b"two",
            &Opts { bak: true, mode: Some(0o600), dir_sync: DirSync::Required, ..Default::default() },
        )
        .unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"two");
        assert_eq!(std::fs::read(d.join("a.hex.bak")).unwrap(), b"one");
        assert!(!d.join("a.hex.new").exists());
        let tmp = d.join("b.tmp");
        write_atomic(&d.join("b"), b"x", &Opts { tmp: Some(&tmp), mode: Some(0o600), ..Default::default() }).unwrap();
        assert!(!tmp.exists() && std::fs::read(d.join("b")).unwrap() == b"x");
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(d.join("b")).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    // Break it catches: a symbolic link in the temporary file's place (a foreign stick) followed, so the file it points
    // to is written over; a temporary file left by an earlier attempt that stops the next write
    #[test]
    fn a_link_or_a_leftover_in_the_temporary_place_is_replaced_not_followed() {
        let d = dir("link");
        let victim = d.join("victim");
        std::fs::write(&victim, b"keep").unwrap();
        let p = d.join("setup1.txt");
        std::os::unix::fs::symlink(&victim, d.join("setup1.txt.new")).unwrap();
        write_atomic(&p, b"new", &Opts { dir_sync: DirSync::Required, ..Default::default() }).unwrap();
        assert_eq!(std::fs::read(&victim).unwrap(), b"keep");
        assert_eq!(std::fs::read(&p).unwrap(), b"new");
        assert!(!std::fs::symlink_metadata(&p).unwrap().file_type().is_symlink());
        std::fs::write(d.join("setup1.txt.new"), b"half").unwrap();
        write_atomic(&p, b"newer", &Opts::default()).unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"newer");
        let _ = std::fs::remove_dir_all(&d);
    }
}
