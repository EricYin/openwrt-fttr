//! Per-registration workers. Cancellation precedes teardown; tokens are never reused.
use std::{collections::BTreeMap, sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}}, thread, time::Duration};
use crate::model::{BusinessPhase, BusinessStatus};

#[derive(Clone, Default)]
pub(super) struct State {
    pub generation: u64,
    pub phase: BusinessPhase,
    pub error: Option<String>,
    pub identity: Option<crate::model::OnuIdentity>,
    pub identity_at: Option<std::time::Instant>,
    pub mib: Option<std::sync::mpsc::SyncSender<super::inventory::MibRequest>>,
}
impl State {
    pub fn business_status(&self, carrier: bool, management_ready: bool) -> BusinessStatus {
        let phase = match self.phase {
            BusinessPhase::WanBootstrapPending => match (management_ready, carrier) {
                (true, true) => BusinessPhase::Verified,
                (true, false) => BusinessPhase::Verifying,
                (false, _) => BusinessPhase::WanBootstrapPending,
            },
            BusinessPhase::Verifying => if carrier { BusinessPhase::Verified } else { BusinessPhase::Verifying },
            BusinessPhase::NotStarted | BusinessPhase::CollectingIdentity | BusinessPhase::RetryingIdentity
            | BusinessPhase::Unsupported | BusinessPhase::Configuring | BusinessPhase::Verified
            | BusinessPhase::Retrying | BusinessPhase::Offline => self.phase,
        };
        BusinessStatus { phase, generation: self.generation, error: self.error.clone() }
    }
}
pub(super) type States = Arc<Mutex<BTreeMap<u8, State>>>;
pub(super) struct Services { pub states: States, tokens: BTreeMap<u8, Arc<AtomicBool>> }
impl Services {
    pub fn new(states: States) -> Self { Self { states, tokens: BTreeMap::new() } }
    pub fn stop(&mut self, id: u8) {
        if let Some(token) = self.tokens.remove(&id) { token.store(true, Ordering::Release); }
        if let Some(state) = self.states.lock().unwrap().get_mut(&id) { state.phase = BusinessPhase::Offline; }
    }
    pub fn start(&mut self, id: u8, vendor: String) {
        self.stop(id);
        let generation = self.states.lock().unwrap().get(&id).map_or(1, |state| state.generation + 1);
        let (mib, requests) = std::sync::mpsc::sync_channel(1);
        self.states.lock().unwrap().insert(id, State { generation, phase: BusinessPhase::CollectingIdentity, mib: Some(mib), ..State::default() });
        let token = Arc::new(AtomicBool::new(false));
        self.tokens.insert(id, token.clone());
        let states = self.states.clone();
        thread::spawn(move || {
            // A transient equipment-ID GET failure must not permanently classify
            // an otherwise supported device as unsupported for this generation.
            let identity = loop {
                let identity = super::inventory::collect(id, token.clone()).unwrap_or_else(|error| crate::model::OnuIdentity {
                    errors: vec![error.to_string()], ..Default::default()
                });
                if token.load(Ordering::Acquire) { return; }
                if identity.equipment_id.is_some() { break identity; }
                if let Some(state) = states.lock().unwrap().get_mut(&id).filter(|state| state.generation == generation) {
                    state.identity = Some(identity);
                    state.identity_at = Some(std::time::Instant::now());
                    state.phase = BusinessPhase::RetryingIdentity;
                }
                for _ in 0..100 {
                    if token.load(Ordering::Acquire) { return; }
                    thread::sleep(Duration::from_millis(100));
                }
            };
            if token.load(Ordering::Acquire) { return; }
            let adapter = crate::devices::select(&vendor, &identity);
            if let Some(state) = states.lock().unwrap().get_mut(&id).filter(|state| state.generation == generation) {
                state.phase = adapter.status();
                state.identity = Some(identity);
                state.identity_at = Some(std::time::Instant::now());
            }
            if !adapter.capabilities().business {
                println!("[!] ONU {id} generation={generation} vendor={vendor}: identity collected; business profile unsupported");
                return;
            }
            let set = |phase: BusinessPhase, error: Option<String>| {
                if token.load(Ordering::Acquire) { return; }
                if let Some(state) = states.lock().unwrap().get_mut(&id).filter(|state| state.generation == generation) {
                    state.phase = phase; state.error = error;
                }
            };
            while !token.load(Ordering::Acquire) {
                set(BusinessPhase::Configuring, None);
                let result = adapter.configure(id, token.clone())
                    .and_then(|()| {
                        if token.load(Ordering::Acquire) { return Ok(()); }
                        let phase = adapter.verification_status();
                        println!("[+] ONU {id} generation={generation}: OMCI profile readback complete; {phase}; starting evidence monitor");
                        set(phase, None);
                        super::health::watch(id, token.clone(), vendor.as_bytes().try_into().map_err(|_| std::io::Error::other("invalid vendor ID"))?, &requests)
                    });
                if token.load(Ordering::Acquire) { break; }
                if let Err(error) = result {
                    eprintln!("[-] ONU {id} generation={generation} service retry: {error}");
                    set(BusinessPhase::Retrying, Some(error.to_string()));
                }
                for _ in 0..100 {
                    if token.load(Ordering::Acquire) { break; }
                    thread::sleep(Duration::from_millis(100));
                }
            }
            println!("[*] ONU {id} generation={generation} worker resources released");
        });
    }
}
static PON_ENGINE: std::sync::OnceLock<Arc<Mutex<crate::pon_engine::PonEngine>>> =
    std::sync::OnceLock::new();

pub(super) fn init_pon_engine(engine: Arc<Mutex<crate::pon_engine::PonEngine>>) {
    let _ = PON_ENGINE.set(engine);
}

pub(crate) fn operation_confirmed(id: u8) -> bool {
    if let Some(engine) = PON_ENGINE.get() {
        if let Ok(guard) = engine.lock() {
            return guard
                .active_onus
                .iter()
                .any(|onu| onu.onu_id == id && onu.state == crate::pon_engine::ploam::OnuState::O5Operation);
        }
    }
    false
}

pub(crate) fn onu_serial(id: u8) -> Option<String> {
    if let Some(engine) = PON_ENGINE.get() {
        if let Ok(guard) = engine.lock() {
            return guard
                .active_onus
                .iter()
                .find(|onu| onu.onu_id == id)
                .map(|onu| onu.full_sn_str());
        }
    }
    None
}


#[cfg(test)]
mod tests {
    use super::State;
    use crate::model::{BusinessPhase, BusinessStatus};

    #[test]
    fn business_status_tracks_management_and_carrier_readiness() {
        // Given: a configured V152 waiting for management bootstrap.
        let state = State { phase: BusinessPhase::WanBootstrapPending, generation: 1, ..State::default() };
        for (carrier, management_ready, expected) in [
            (true, true, BusinessPhase::Verified),
            (false, true, BusinessPhase::Verifying),
            (true, false, BusinessPhase::WanBootstrapPending),
            (false, false, BusinessPhase::WanBootstrapPending),
        ] {
            // When: status is requested with current readiness evidence.
            let status = state.business_status(carrier, management_ready);
            // Then: neither CWMP nor carrier alone certifies V152 business readiness.
            assert_eq!(status, BusinessStatus { phase: expected, generation: 1, error: None }, "carrier={carrier}, management_ready={management_ready}");
        }
    }

    #[test]
    fn business_status_preserves_other_worker_phases_and_errors() {
        // Given: worker states outside the V152 bootstrap gate.
        for (phase, carrier, error, expected) in [
            (BusinessPhase::Verifying, true, None, BusinessPhase::Verified),
            (BusinessPhase::Verifying, false, None, BusinessPhase::Verifying),
            (BusinessPhase::Configuring, true, None, BusinessPhase::Configuring),
            (BusinessPhase::Retrying, true, Some("readback failed"), BusinessPhase::Retrying),
            (BusinessPhase::Offline, true, None, BusinessPhase::Offline),
            (BusinessPhase::NotStarted, true, None, BusinessPhase::NotStarted),
            (BusinessPhase::CollectingIdentity, true, None, BusinessPhase::CollectingIdentity),
            (BusinessPhase::RetryingIdentity, true, None, BusinessPhase::RetryingIdentity),
            (BusinessPhase::Unsupported, true, None, BusinessPhase::Unsupported),
            (BusinessPhase::Verified, false, None, BusinessPhase::Verified),
        ] {
            let state = State { phase, generation: 2, error: error.map(str::to_owned), ..State::default() };
            // When: current CWMP readiness is available.
            let status = state.business_status(carrier, true);
            // Then: readiness does not hide worker transitions or errors.
            assert_eq!(status, BusinessStatus { phase: expected, generation: 2, error: error.map(str::to_owned) });
        }
    }

    #[test]
    fn stopping_worker_preserves_generation_and_error() {
        let states = std::sync::Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::from([
            (1, State { phase: BusinessPhase::Retrying, generation: 7, error: Some("readback failed".into()), ..State::default() }),
        ])));
        let mut services = super::Services::new(states.clone());
        services.stop(1);
        let status = states.lock().unwrap()[&1].business_status(true, true);
        assert_eq!(serde_json::to_value(status).unwrap(), serde_json::json!({
            "phase": "offline", "generation": 7, "error": "readback failed"
        }));
    }

    #[test]
    fn absent_worker_serializes_as_not_started_without_error() {
        let status = State::default().business_status(true, true);
        assert_eq!(serde_json::to_value(status).unwrap(), serde_json::json!({
            "phase": "not_started", "generation": 0, "error": null
        }));
    }
}
