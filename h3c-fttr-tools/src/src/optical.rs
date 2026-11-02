//! Optical desired state and process ownership. Hardware transitions belong to the PLOAM owner.
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::PathBuf;

pub const OWNER_LOCK: &str = "/var/run/fttr-optical.lock";

/// Held for the daemon lifetime; an offline shutdown must never race a live owner.
pub fn claim_owner() -> io::Result<File> {
    claim_owner_at(std::path::Path::new(OWNER_LOCK))
}

fn claim_owner_at(path: &std::path::Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "optical hardware is owned by fttrd; shutdown outcome unknown",
        ));
    }
    Ok(file)
}

const CONFIG: &str = "/etc/config/fttr";

struct UciSession {
    dir: PathBuf,
    conf_dir: PathBuf,
    package: String,
    original: Vec<u8>,
    uci: openwrt_uci::Uci,
}
impl UciSession {
    fn new(config: &std::path::Path) -> io::Result<Self> {
        // A private snapshot with a unique package name cannot consume fttr's pending edits.
        let original = fs::read(config)?;
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos();
        for nonce in 0..100 {
            let path =
                std::env::temp_dir().join(format!("fttr-optical-{}-{nonce}", std::process::id()));
            match fs::DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => {
                    let conf_dir = path.join("config");
                    let delta_dir = path.join("delta");
                    fs::create_dir(&conf_dir)?;
                    fs::create_dir(&delta_dir)?;
                    let package = format!("optical_{}_{stamp}_{nonce}", std::process::id());
                    fs::write(conf_dir.join(&package), &original)?;
                    let mut uci =
                        openwrt_uci::Uci::new().map_err(|e| io::Error::other(e.to_string()))?;
                    uci.set_confdir(&conf_dir)
                        .map_err(|e| io::Error::other(e.to_string()))?;
                    uci.set_savedir(&delta_dir)
                        .map_err(|e| io::Error::other(e.to_string()))?;
                    return Ok(Self {
                        dir: path,
                        conf_dir,
                        package,
                        original,
                        uci,
                    });
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
        Err(io::Error::other("cannot allocate optical UCI savedir"))
    }

    fn load(&mut self) -> io::Result<bool> {
        let text = self
            .uci
            .export_package(&self.package)
            .map_err(|e| io::Error::other(e.to_string()))?;
        parse_export(&text)
    }

    fn set_optical(&mut self, enabled: bool) -> io::Result<()> {
        self.uci
            .ensure_section(&self.package, "optical", "optical")
            .map_err(|e| io::Error::other(e.to_string()))?;
        self.uci
            .set(
                &self.package,
                "optical",
                "enabled",
                if enabled { "1" } else { "0" },
            )
            .map_err(|e| io::Error::other(e.to_string()))?;
        self.uci
            .commit(&self.package)
            .map_err(|e| io::Error::other(e.to_string()))?;
        Ok(())
    }
}
impl Drop for UciSession {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

pub fn load_enabled() -> io::Result<bool> {
    load_enabled_from(std::path::Path::new(CONFIG))
}

pub fn load_enabled_from(config_path: &std::path::Path) -> io::Result<bool> {
    if !config_path.exists() {
        return Ok(true);
    }
    let original = fs::read(config_path)?;
    // Create an isolated temporary session without reading shared /tmp/.uci deltas
    let temp_dir = std::env::temp_dir().join(format!("fttr-optical-load-{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&temp_dir)?;
    let conf_dir = temp_dir.join("config");
    let delta_dir = temp_dir.join("delta");
    fs::create_dir_all(&conf_dir)?;
    fs::create_dir_all(&delta_dir)?;
    let package = "fttr";
    fs::write(conf_dir.join(package), &original)?;

    let res = (|| -> io::Result<bool> {
        let mut uci = openwrt_uci::Uci::new().map_err(|e| io::Error::other(e.to_string()))?;
        uci.set_confdir(&conf_dir)
            .map_err(|e| io::Error::other(e.to_string()))?;
        uci.set_savedir(&delta_dir)
            .map_err(|e| io::Error::other(e.to_string()))?;
        let text = uci
            .export_package(package)
            .map_err(|e| io::Error::other(e.to_string()))?;
        parse_export(&text)
    })();
    let _ = fs::remove_dir_all(&temp_dir);
    res
}

pub fn save_enabled(enabled: bool) -> io::Result<()> {
    save_at(std::path::Path::new(CONFIG), enabled)
}

fn save_at(config: &std::path::Path, enabled: bool) -> io::Result<()> {
    let source = OpenOptions::new().read(true).write(true).open(config)?;
    if unsafe { libc::flock(source.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "FTTR configuration is being edited; retry",
        ));
    }
    let mut session = UciSession::new(config)?;
    let current = session.load()?;
    if current == enabled {
        return Ok(());
    }
    session.set_optical(enabled)?;
    if session.load()? != enabled {
        return Err(io::Error::other("optical configuration readback mismatch"));
    }
    let bytes = fs::read(session.conf_dir.join(&session.package))?;
    replace_config(config, &source, &session.original, &bytes, &session.package)
}

fn replace_config(
    config: &std::path::Path,
    source: &File,
    original: &[u8],
    bytes: &[u8],
    token: &str,
) -> io::Result<()> {
    let staged = config.with_file_name(format!(".{token}.tmp"));
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&staged)?;
    let result = (|| {
        let meta = source.metadata()?;
        if unsafe { libc::fchown(output.as_raw_fd(), meta.uid(), meta.gid()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        output.set_permissions(meta.permissions())?;
        output.write_all(bytes)?;
        output.sync_all()?;
        if fs::read(config)? != original {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "FTTR configuration changed during optical save; retry",
            ));
        }
        fs::rename(&staged, config)?;
        File::open(
            config
                .parent()
                .ok_or_else(|| io::Error::other("configuration has no parent"))?,
        )?
        .sync_all()?;
        Ok(())
    })();
    let _ = fs::remove_file(staged);
    result?;
    Ok(())
}

pub(crate) fn parse_export(text: &str) -> io::Result<bool> {
    let mut optical = false;
    let mut seen = false;
    let mut enabled = true; // Upgrade compatibility: old configurations have no optical section.
    for line in text.lines() {
        let words = crate::provisioning::uci_words(line).map_err(io::Error::other)?;
        match words.first().map(String::as_str) {
            Some("config") => {
                let kind = words.get(1).map(String::as_str);
                let name = words.get(2).map(String::as_str);
                optical = kind == Some("optical");
                if optical {
                    if seen || name != Some("optical") {
                        return Err(io::Error::other(
                            "expected one named config optical 'optical' section",
                        ));
                    }
                    seen = true;
                } else if name == Some("optical") {
                    return Err(io::Error::other("fttr.optical has the wrong section type"));
                }
            }
            Some("option" | "list")
                if optical && words.get(1).map(String::as_str) == Some("enabled") =>
            {
                enabled = match (words[0].as_str(), words.get(2).map(String::as_str)) {
                    ("option", Some("1")) => true,
                    ("option", Some("0")) => false,
                    _ => return Err(io::Error::other("fttr.optical.enabled must be 0 or 1")),
                };
            }
            _ => (),
        }
    }
    Ok(enabled)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn old_configuration_and_strict_optical_settings() {
        assert!(parse_export("package fttr\nconfig acs 'acs'\n option enabled '0'\n").unwrap());
        assert!(!parse_export("config optical 'optical'\n option enabled '0'\n").unwrap());
        assert!(parse_export("config optical 'optical'\n option enabled '1'\n").unwrap());
        for text in [
            "config optical 'other'",
            "config acs 'optical'",
            "config optical 'optical'\noption enabled 'yes'",
            "config optical 'optical'\nlist enabled '1'",
            "config optical 'optical'\nconfig optical 'optical'",
        ] {
            assert!(parse_export(text).is_err(), "{text}");
        }
    }

    #[test]
    fn atomic_config_write_preserves_metadata_and_rejects_concurrent_edits() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("fttr-optical-atomic-{}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        let config = dir.join("fttr");
        let original = b"config acs 'acs'\n option password 'unchanged'\n";
        fs::write(&config, original).unwrap();
        fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
        let source = File::open(&config).unwrap();
        let next = b"config acs 'acs'\n option password 'unchanged'\nconfig optical 'optical'\n option enabled '0'\n";
        replace_config(&config, &source, original, next, "optical-test").unwrap();
        assert_eq!(fs::read(&config).unwrap(), next);
        assert_eq!(
            fs::metadata(&config).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(
            replace_config(&config, &source, original, b"stale writer", "optical-test").is_err()
        );
        assert_eq!(fs::read(&config).unwrap(), next);
        assert!(!dir.join(".optical-test.tmp").exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn offline_shutdown_cannot_claim_live_owner_and_drop_releases_lock() {
        let path =
            std::env::temp_dir().join(format!("fttr-optical-lock-test-{}", std::process::id()));
        let owner = claim_owner_at(&path).unwrap();
        assert_eq!(
            claim_owner_at(&path).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        drop(owner);
        drop(claim_owner_at(&path).unwrap());
        fs::remove_file(path).unwrap();
    }
}
