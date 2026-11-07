//! Optical identity boundary for the independent structured MQTT adapter.
use crate::{
    devices::ty_mqtt as adapter,
    pon_engine::{ploam::OnuState, PonEngine},
};
use std::{
    collections::BTreeMap,
    io::Write,
    os::unix::net::UnixStream,
    sync::{Arc, Mutex, OnceLock},
};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
struct Binding {
    onu_id: u8,
    optical_sn: String,
    generation: u64,
}

fn bindings(
    engine: &Mutex<PonEngine>,
    services: &super::service::States,
) -> Result<BTreeMap<String, Binding>, String> {
    let engine = engine.lock().map_err(|_| "optical state unavailable")?;
    let services = services.lock().map_err(|_| "device identity unavailable")?;
    let onus: BTreeMap<_, _> = engine
        .active_onus
        .iter()
        .filter(|o| o.state == OnuState::O5Operation)
        .map(|o| (u16::from(o.onu_id), o.full_sn_str()))
        .collect();
    let supported: BTreeMap<_, _> = engine
        .active_onus
        .iter()
        .filter_map(|o| {
            let service = services.get(&o.onu_id)?;
            let identity = service.identity.as_ref()?;
            if o.state != OnuState::O5Operation
                || service.phase == crate::model::BusinessPhase::Offline
                || crate::devices::select(&o.vendor_str(), identity)
                    != crate::devices::Adapter::Scty
            {
                return None;
            }
            Some((
                o.full_sn_str(),
                Binding {
                    onu_id: o.onu_id,
                    optical_sn: o.full_sn_str(),
                    generation: service.generation,
                },
            ))
        })
        .collect();
    // Keep registration stable throughout this read-only FDB observation.
    let links = crate::provisioning::link_identity::discover(&onus)?;
    Ok(links
        .into_iter()
        .filter_map(|(mac, sn)| supported.get(&sn).cloned().map(|b| (mac, b)))
        .collect())
}

pub(super) fn start(
    mut stream: UnixStream,
    input: String,
    engine: Arc<Mutex<PonEngine>>,
    services: super::service::States,
) {
    static WORKERS: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    let Ok(permit) = WORKERS
        .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(4)))
        .clone()
        .try_acquire_owned()
    else {
        let _ = stream.write_all(b"{\"error\":\"TY MQTT workers busy\"}\n");
        return;
    };
    crate::runtime::spawn_blocking(move || {
        let _permit = permit;
        let result = control(&input, &engine, &services)
            .unwrap_or_else(|e| serde_json::json!({"schema_version":1,"error":e}));
        let _ = writeln!(stream, "{result}");
    });
}

fn control(
    input: &str,
    engine: &Mutex<PonEngine>,
    services: &super::service::States,
) -> Result<serde_json::Value, String> {
    if input.len() > 512 {
        return Err("TY MQTT request too large".into());
    }
    if !crate::mqtt_settings::Settings::load()?.ty_fs803g() {
        return Err("TY MQTT adapter disabled".into());
    }
    let query: adapter::Query =
        serde_json::from_str(input).map_err(|_| "invalid TY MQTT request")?;
    match query {
        adapter::Query::List => {
            let links = bindings(engine, services);
            let sources: Vec<_> = adapter::sources()
                .into_iter()
                .map(|s| {
                    let binding = links.as_ref().ok().and_then(|links| links.get(&s.mac));
                    serde_json::json!({"source":s,"binding":binding})
                })
                .collect();
            Ok(
                serde_json::json!({"schema_version":1,"sources":sources,"binding_error":links.err(),"read_only":true}),
            )
        }
        query => {
            let (mac, kind) = match query {
                adapter::Query::Cpu { mac } => (mac, adapter::ReadKind::Cpu),
                adapter::Query::Wifi { mac } => (mac, adapter::ReadKind::Wifi),
                adapter::Query::Radio { mac } => (mac, adapter::ReadKind::Radio),
                adapter::Query::Iptv { mac } => (mac, adapter::ReadKind::Iptv),
                adapter::Query::List => unreachable!(),
            };
            let mac = adapter::mac(&mac).ok_or("invalid TY MAC")?;
            let before = bindings(engine, services)?
                .get(&mac)
                .cloned()
                .ok_or("source is not linked to a current online TY-FS803G")?;
            crate::runtime::block_on(adapter::read(&mac, kind, || {
                if bindings(engine, services)?.get(&mac) == Some(&before) {
                    Ok(())
                } else {
                    Err("TY optical registration or source binding changed".into())
                }
            }))
        }
    }
}
