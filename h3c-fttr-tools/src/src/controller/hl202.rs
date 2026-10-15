use crate::devices::hl202::api::{self, Device};
use crate::pon_engine::{ploam::OnuState, PonEngine};
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};

static WORKERS: AtomicUsize = AtomicUsize::new(0);
struct Permit;
impl Drop for Permit {
    fn drop(&mut self) {
        WORKERS.fetch_sub(1, Ordering::AcqRel);
    }
}

pub(super) fn start(
    mut stream: UnixStream,
    input: String,
    engine: Arc<Mutex<PonEngine>>,
    services: super::service::States,
) {
    if WORKERS
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
            (count < 32).then_some(count + 1)
        })
        .is_err()
    {
        let _ = stream.write_all(b"{\"schema_version\":1,\"error\":{\"code\":\"busy\",\"message\":\"HL202 workers busy\"}}");
        return;
    }
    let permit = Permit;
    let _ = std::thread::Builder::new().name("hl202-ipc".into()).spawn(move || {
        let _permit = permit;
        let response = match (engine.lock(), services.lock()) {
            (Ok(engine), Ok(services)) => {
                let devices: Vec<Device> = engine.active_onus.iter().filter_map(|onu| {
                    let model = services.get(&onu.onu_id).and_then(|service| service.identity.as_ref())
                        .and_then(|identity| identity.equipment_id.clone()).unwrap_or_else(|| onu.model.clone());
                    if onu.vendor_str() != "H3CT" || model != "HL202-DU-C" { return None; }
                    Some(Device { optical_sn: onu.full_sn_str(), onu_id:u16::from(onu.onu_id), model,
                        online: onu.state == OnuState::O5Operation })
                }).collect();
                drop(services);
                drop(engine);
                api::control(&input, &devices)
            }
            _ => serde_json::json!({"schema_version":1,"error":{"code":"unavailable","message":"optical identity state unavailable"}}),
        };
        let encoded = response.to_string();
        if encoded.len() <= crate::hl202_ipc::MAX_RESPONSE_BYTES {
            let _ = stream.write_all(encoded.as_bytes());
        } else {
            let _ = stream.write_all(b"{\"schema_version\":1,\"error\":{\"code\":\"response_limit\",\"message\":\"HL202 response exceeds byte limit\"}}");
        }
    });
}
