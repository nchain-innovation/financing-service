//! Writing a file that holds secrets (CS-526).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Replaces `path` with `bytes` so that it is never half-written and never
/// readable by anyone but its owner.
///
/// The bytes go to a new sibling file created `0600` -- so they are private
/// from the first byte, whatever the umask -- are flushed to disk, and the
/// sibling is then renamed over `path`. A crash at any point leaves either the
/// old file or the new one. The directory is synced afterwards so the rename
/// itself survives a power loss; that step is best effort, since not every
/// filesystem allows it.
pub fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let name = path
        .file_name()
        .ok_or_else(|| std::io::Error::other(format!("{} names no file", path.display())))?;
    let mut temporary_name = std::ffi::OsString::from(".");
    temporary_name.push(name);
    temporary_name.push(format!(
        ".tmp-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let temporary = path.with_file_name(temporary_name);

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let written = options.open(&temporary).and_then(|mut file| {
        file.write_all(bytes)?;
        file.sync_all()
    });
    if let Err(error) = written.and_then(|()| std::fs::rename(&temporary, path)) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error);
    }
    sync_directory(path);
    Ok(())
}

/// Makes `path` owner-only if anyone else can read or write it, returning
/// whether it had to. A file written before CS-526 took the umask, typically
/// `0644`.
#[cfg(unix)]
pub fn make_private(path: &Path) -> std::io::Result<bool> {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(path)?.permissions().mode();
    if mode & 0o077 == 0 {
        return Ok(false);
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode & 0o700))?;
    Ok(true)
}

#[cfg(not(unix))]
pub fn make_private(_path: &Path) -> std::io::Result<bool> {
    Ok(false)
}

fn sync_directory(path: &Path) {
    #[cfg(unix)]
    {
        let directory: PathBuf = match path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
            _ => PathBuf::from("."),
        };
        if let Err(error) = std::fs::File::open(&directory).and_then(|d| d.sync_all()) {
            log::debug!("could not sync {}: {error}", directory.display());
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_directory() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "fs-private-file-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn leftovers(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp-"))
            .collect()
    }

    #[test]
    fn sr_sec_022_replaces_the_file_whole_and_leaves_nothing_behind() {
        let dir = temporary_directory();
        let path = dir.join("dynamic.toml");
        write_private(&path, b"first").unwrap();
        write_private(&path, b"second").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"second");
        assert!(leftovers(&dir).is_empty());
    }

    /// Atomic replacement is a rename, so the path ends up naming a new file;
    /// rewriting in place -- what a crash can leave half-done -- keeps the old
    /// one.
    #[cfg(unix)]
    #[test]
    fn sr_sec_022_the_file_is_replaced_not_rewritten_in_place() {
        use std::os::unix::fs::MetadataExt;
        let dir = temporary_directory();
        let path = dir.join("dynamic.toml");
        write_private(&path, b"first").unwrap();
        let before = std::fs::metadata(&path).unwrap().ino();
        write_private(&path, b"second").unwrap();
        assert_ne!(std::fs::metadata(&path).unwrap().ino(), before);
    }

    #[cfg(unix)]
    #[test]
    fn sr_sec_022_the_file_is_owner_only_even_over_a_world_readable_one() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temporary_directory();
        let path = dir.join("dynamic.toml");
        std::fs::write(&path, b"old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_private(&path, b"new").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    /// A failed write leaves the old file exactly as it was.
    #[test]
    fn sr_sec_022_a_failed_write_keeps_the_old_file() {
        let dir = temporary_directory();
        let path = dir.join("dynamic.toml");
        write_private(&path, b"kept").unwrap();
        // Renaming a file over a directory fails, after the temporary file
        // has been written.
        let blocked = dir.join("blocked");
        std::fs::create_dir_all(blocked.join("inner")).unwrap();
        assert!(write_private(&blocked, b"new").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"kept");
        assert!(leftovers(&dir).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn sr_sec_022_an_existing_readable_file_is_made_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temporary_directory();
        let path = dir.join("dynamic.toml");
        std::fs::write(&path, b"x").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(make_private(&path).unwrap());
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert!(!make_private(&path).unwrap());
    }
}
