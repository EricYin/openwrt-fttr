//! Persistence mechanics only; callers retain format and retry decisions.
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
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

/// Preserve the existing file-fsync/rename sequence. Ownership registries also
/// fsync their directory; controller history intentionally retains its policy.
pub(crate) fn write_atomic(path: &str, data: &[u8], sync_directory: bool) -> io::Result<()> {
    let directory = Path::new(path).parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "state path has no directory")
    })?;
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
        fs::File::open(directory)?.sync_all()?;
        if let Some(parent) = directory.parent() {
            fs::File::open(parent)?.sync_all()?;
        }
    }
    Ok(())
}
