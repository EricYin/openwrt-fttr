use base64::{engine::general_purpose::STANDARD, Engine};
use fttr_api::{
    CalibrationInfo, CalibrationSaved, CalibrationSource, FactoryCalibration, FirmwareCalibration,
};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, Read, Write},
    os::unix::fs::OpenOptionsExt,
};

const PATH: &str = "/lib/firmware/modified_UX3326.bin";
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub(super) fn get() -> io::Result<CalibrationInfo> {
    let (device, factory) = crate::bosa::factory_calibration();
    let firmware = match fs::File::open(PATH) {
        Ok(file) => {
            let mut bytes = Vec::new();
            file.take(289).read_to_end(&mut bytes)?;
            bytes
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e),
    };
    // Firmware accepted by the hardware loader is 56 or 288 bytes. Do not export
    // arbitrarily sized files into a ubus reply.
    if firmware.len() > 288 {
        return Err(io::Error::other("Calibration file exceeds 288 bytes"));
    }
    let exists = std::path::Path::new(PATH).is_file();
    let (source, path, bytes): (CalibrationSource, &str, &[u8]) = if let Some(bytes) = &factory {
        (
            CalibrationSource::Factory,
            "Factory partition (offset 0x2200)",
            bytes,
        )
    } else if firmware.len() >= 56 {
        (CalibrationSource::Firmware, PATH, &firmware)
    } else {
        (
            CalibrationSource::Builtin,
            "Builtin static default",
            &crate::bosa::DEFAULT_UX3326_CALIBRATION_DATA,
        )
    };
    Ok(CalibrationInfo {
        source,
        active_path: path.into(),
        size: bytes.len() as u32,
        sha256: hash(bytes),
        data: STANDARD.encode(bytes),
        factory: FactoryCalibration {
            detected: device.is_some(),
            device: device.unwrap_or_default(),
            has_data: factory.is_some(),
            sha256: factory.as_ref().map(|b| hash(b)).unwrap_or_default(),
            data: factory
                .as_ref()
                .map(|b| STANDARD.encode(b))
                .unwrap_or_default(),
        },
        firmware: FirmwareCalibration {
            exists,
            path: PATH.into(),
            size: firmware.len() as u32,
            sha256: if exists {
                hash(&firmware)
            } else {
                String::new()
            },
            data: STANDARD.encode(&firmware),
        },
    })
}

pub(super) fn save(data: &str) -> io::Result<CalibrationSaved> {
    save_at(data, std::path::Path::new(PATH))
}

fn save_at(data: &str, path: &std::path::Path) -> io::Result<CalibrationSaved> {
    if data.len() > 384 {
        return Err(io::Error::other("Invalid calibration length"));
    }
    let bytes = STANDARD.decode(data).map_err(io::Error::other)?;
    if !matches!(bytes.len(), 56 | 288) {
        return Err(io::Error::other(
            "Calibration payload must be 56 or 288 bytes",
        ));
    }
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("Invalid calibration path"))?;
    fs::create_dir_all(parent)?;
    let mut temporary = path.as_os_str().to_os_string();
    temporary.push(format!(".{}.tmp", std::process::id()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o644)
        .open(&temporary)?;
    let result = (|| {
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        fs::File::open(parent)?.sync_all()?;
        Ok(CalibrationSaved {
            success: true,
            path: path.to_string_lossy().into_owned(),
            size: bytes.len() as u32,
            sha256: hash(&bytes),
        })
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn calibration_replacement_validates_before_writing_and_preserves_foreign_temporary_files() {
        let directory =
            std::env::temp_dir().join(format!("fttr-calibration-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("calibration.bin");
        for length in [56, 288] {
            let bytes = vec![0x5a; length];
            let reply = save_at(&STANDARD.encode(&bytes), &path).unwrap();
            assert_eq!(reply.sha256, hash(&bytes));
            assert_eq!(fs::read(&path).unwrap(), bytes);
        }
        let expected = fs::read(&path).unwrap();
        for input in [
            "invalid-base64".to_owned(),
            STANDARD.encode([0; 57]),
            STANDARD.encode([0; 289]),
        ] {
            assert!(save_at(&input, &path).is_err());
            assert_eq!(fs::read(&path).unwrap(), expected);
        }
        let temporary = directory.join(format!("calibration.bin.{}.tmp", std::process::id()));
        fs::write(&temporary, b"existing temporary data").unwrap();
        assert!(save_at(&STANDARD.encode([0; 56]), &path).is_err());
        assert_eq!(fs::read(&temporary).unwrap(), b"existing temporary data");
        assert_eq!(fs::read(&path).unwrap(), expected);
        fs::remove_file(&temporary).unwrap();
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(save_at(&STANDARD.encode([0; 56]), &path).is_err());
        assert!(!temporary.exists());
        assert!(path.is_dir());
        fs::remove_dir_all(directory).unwrap();
    }
}
