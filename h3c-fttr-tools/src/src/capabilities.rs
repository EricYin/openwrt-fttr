//! Compile-time support, deliberately independent of UCI, hardware and liveness.
use serde::Serialize;
use std::{collections::BTreeMap, fs, io, path::Path};

#[derive(Debug, Serialize)]
pub struct Capabilities {
    pub schema_version: u32,
    pub version: &'static str,
    pub features: BTreeMap<&'static str, bool>,
    pub functions: BTreeMap<&'static str, bool>,
    pub pages: BTreeMap<&'static str, bool>,
}

pub fn compiled() -> Capabilities {
    let acs = cfg!(feature = "acs");
    let mqtt = cfg!(feature = "mqtt");
    let hl202 = cfg!(feature = "device-hl202");
    Capabilities {
        schema_version: 1,
        version: env!("CARGO_PKG_VERSION"),
        features: BTreeMap::from([
            ("acs", acs),
            ("mqtt", mqtt),
            ("device-hl202", hl202),
            ("device-hwtc", cfg!(feature = "device-hwtc")),
            ("device-star", cfg!(feature = "device-star")),
        ]),
        functions: BTreeMap::from([
            ("optical", true),
            ("omci_mib", true),
            ("mqtt_telemetry", mqtt),
            ("hl202_management", hl202 && mqtt),
            ("cwmp", acs),
            ("command_provisioning", hl202 && mqtt),
        ]),
        // Configuration and synchronization are the ACS/CWMP workflow in LuCI.
        pages: BTreeMap::from([
            ("status", true),
            ("hardware", true),
            ("calibration", true),
            ("about", true),
            ("config", acs || (hl202 && mqtt)),
            ("sync", acs || (hl202 && mqtt)),
            ("hl202", hl202 && mqtt),
        ]),
    }
}

pub fn json() -> String {
    serde_json::to_string_pretty(&compiled()).expect("static capabilities serialize")
}

// LuCI's server-side menu only supports filesystem/UCI dependencies. Publish
// page markers from the same capability model, including when MQTT cannot start.
pub fn publish_menu() -> io::Result<()> {
    publish_menu_at(Path::new("/var/run/fttr-capabilities"), Path::new("/tmp"))
}

fn publish_menu_at(directory: &Path, cache_directory: &Path) -> io::Result<()> {
    fs::create_dir_all(directory)?;
    let mut changed = false;
    for (page, supported) in compiled().pages {
        let path = directory.join(page);
        if supported && !path.is_file() {
            fs::write(&path, b"")?;
            changed = true;
        } else if !supported && path.exists() {
            fs::remove_file(&path)?;
            changed = true;
        }
    }
    if changed {
        for entry in fs::read_dir(cache_directory)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("luci-indexcache.") && name.ends_with(".json") {
                fs::remove_file(entry.path())?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_refresh_removes_stale_support_and_invalidates_luci_cache() {
        let root = std::env::temp_dir().join(format!("fttr-capabilities-{}", std::process::id()));
        let markers = root.join("markers");
        fs::create_dir_all(&markers).unwrap();
        // Seed the opposite build's marker set.
        for (page, supported) in compiled().pages {
            if !supported {
                fs::write(markers.join(page), b"").unwrap();
            }
        }
        let cache = root.join("luci-indexcache.fixture.json");
        fs::write(&cache, b"{}").unwrap();
        let unrelated = root.join("keep.json");
        fs::write(&unrelated, b"{}").unwrap();
        publish_menu_at(&markers, &root).unwrap();
        for (page, supported) in compiled().pages {
            assert_eq!(markers.join(page).is_file(), supported, "{page}");
        }
        assert!(!cache.exists());
        assert!(unrelated.exists());
        // A repeat does not invalidate an already correct menu.
        fs::write(&cache, b"{}").unwrap();
        publish_menu_at(&markers, &root).unwrap();
        assert!(cache.exists());
        fs::remove_dir_all(root).unwrap();
    }
}
