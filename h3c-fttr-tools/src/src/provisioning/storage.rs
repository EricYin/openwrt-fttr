//! Persistence mechanics only; callers retain format and retry decisions.
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

pub(super) fn registry_path(directory: &str, id: &str) -> String {
    let hash = id.bytes().fold(0xcbf29ce484222325u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x100000001b3)
    });
    format!("{directory}/{hash:016x}.json")
}

pub(crate) fn remove_registry(path: &str) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => fs::File::open(
            Path::new(path)
                .parent()
                .ok_or("state path has no directory")?,
        )
        .and_then(|directory| directory.sync_all())
        .map_err(|e| format!("persist registry removal: {e}")),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let directory = Path::new(path)
                .parent()
                .ok_or("state path has no directory")?;
            match fs::File::open(directory) {
                Ok(directory) => directory
                    .sync_all()
                    .map_err(|e| format!("persist registry removal: {e}")),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(format!("open registry directory: {e}")),
            }
        }
        Err(e) => Err(format!("remove ownership registry: {e}")),
    }
}

fn sync_directories(directory: &Path) -> io::Result<()> {
    fs::File::open(directory)?.sync_all()?;
    if let Some(parent) = directory.parent() {
        fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

/// Avoid rewriting identical snapshots. Keep durability barriers even on the
/// identical-content path: a previous attempt may have renamed successfully but
/// failed to sync the directory. Retrying must finish that commit, not assume it.
pub(crate) fn write_atomic(path: &str, data: &[u8], sync_directory: bool) -> io::Result<()> {
    let directory = Path::new(path).parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "state path has no directory")
    })?;
    if let Ok(mut existing) = fs::File::open(path) {
        let mut bytes = Vec::new();
        if existing.read_to_end(&mut bytes).is_ok() && bytes == data {
            if existing.metadata()?.permissions().mode() & 0o777 != 0o600 {
                existing.set_permissions(fs::Permissions::from_mode(0o600))?;
            }
            existing.sync_all()?;
            if sync_directory {
                sync_directories(directory)?;
            }
            return Ok(());
        }
    }
    fs::create_dir_all(directory)?;
    let temporary = format!("{path}.tmp");
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporary)?;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    file.write_all(data)?;
    file.sync_all()?;
    fs::rename(temporary, path)?;
    if sync_directory {
        sync_directories(directory)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn repeated_snapshot_keeps_inode_and_changed_snapshot_is_atomic() {
        let directory = std::env::temp_dir().join(format!(
            "fttr-storage-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(directory.clone());
        let path = directory.join("state.json");
        let path = path.to_str().unwrap();
        write_atomic(path, b"pending", true).unwrap();
        let original = fs::File::open(path).unwrap();
        let metadata = original.metadata().unwrap();
        for _ in 0..10 {
            write_atomic(path, b"pending", true).unwrap();
        }
        let unchanged = fs::metadata(path).unwrap();
        assert_eq!(unchanged.ino(), metadata.ino());
        assert_eq!(unchanged.mtime(), metadata.mtime());
        assert_eq!(unchanged.mtime_nsec(), metadata.mtime_nsec());
        assert!(!Path::new(&format!("{path}.tmp")).exists());
        write_atomic(path, b"completed", true).unwrap();
        assert_ne!(fs::metadata(path).unwrap().ino(), metadata.ino());
        assert_eq!(fs::read(path).unwrap(), b"completed");
        let mut before_commit = String::new();
        (&original).read_to_string(&mut before_commit).unwrap();
        assert_eq!(before_commit, "pending");
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
