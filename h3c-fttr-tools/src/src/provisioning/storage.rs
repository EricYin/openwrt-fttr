//! Persistence mechanics only; callers retain format and retry decisions.
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

pub(super) fn registry_path(directory: &str, id: &str) -> String {
    let hash = id.bytes().fold(0xcbf29ce484222325u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x100000001b3)
    });
    format!("{directory}/{hash:016x}.json")
}

/// Preserve the existing file-fsync/rename sequence. Ownership registries also
/// fsync their directory; controller history intentionally retains its policy.
pub(super) fn write_atomic(path: &str, data: &[u8], sync_directory: bool) -> io::Result<()> {
    let directory = Path::new(path)
        .parent()
        .expect("state path has a directory");
    fs::create_dir_all(directory)?;
    let temporary = format!("{path}.tmp");
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporary)?;
    file.write_all(data)?;
    file.sync_all()?;
    fs::rename(temporary, path)?;
    if sync_directory {
        fs::File::open(directory)?.sync_all()?;
    }
    Ok(())
}
