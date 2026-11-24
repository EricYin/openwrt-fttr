use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
};

fn save(mac: &str, payload: &[u8]) -> io::Result<()> {
    let dir = Path::new(crate::ipc::SUBDEV_DIR).join(mac);
    fs::create_dir_all(&dir)?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
    let temporary = dir.join(format!("telemetry.{}.tmp", std::process::id()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(payload)?;
        fs::rename(&temporary, dir.join("telemetry.bin"))
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

pub(super) async fn run() {
    while let Some((mac, payload, received)) = super::store::next().await {
        let work = crate::runtime::spawn_blocking(move || {
            let result = match payload {
                Some(payload) => save(&mac, &payload)
                    .map_err(|_| "private telemetry storage failed".to_owned())
                    .and_then(|_| {
                        super::parse(&payload)
                            .map_err(|_| "invalid or oversized gzip/tar telemetry".to_owned())
                    }),
                None => Err("compressed telemetry limit exceeded".to_owned()),
            };
            if let Ok(report) = &result {
                if let Some((serial, claimed_mac)) = super::identity_claim(report, &mac) {
                    let recorded = if let Ok(mut identities) = crate::mqtt_identity::shared().lock()
                    {
                        let eval_time = received.max(identities.clock());
                        identities
                            .record_verified_report(&serial, &claimed_mac, eval_time)
                            .then(|| identities.verified_reports_snapshot())
                    } else {
                        None
                    };
                    if let Some(snapshot) = recorded {
                        if let Ok(bytes) = serde_json::to_vec(&snapshot) {
                            let _ = crate::provisioning::storage::write_atomic(
                                "/var/lib/fttr/mqtt-identities.json",
                                &bytes,
                                false,
                            );
                        }
                    }
                }
            }
            super::store::complete(mac, received, result);
        })
        .await;
        if let Err(error) = work {
            eprintln!("[-] Telemetry parser failed: {error}");
            break;
        }
    }
    eprintln!("[-] Telemetry worker stopped: state unavailable");
}
