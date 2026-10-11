//! Per-registration workers. Cancellation precedes teardown; tokens are never reused.
use std::{collections::BTreeMap, sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}}, thread, time::Duration};

#[derive(Clone, Default)]
pub(super) struct State {
    pub generation: u64,
    pub phase: String,
    pub error: Option<String>,
    pub identity: Option<crate::model::OnuIdentity>,
    pub identity_at: Option<std::time::Instant>,
}
pub(super) type States = Arc<Mutex<BTreeMap<u8, State>>>;
pub(super) struct Services { pub states: States, tokens: BTreeMap<u8, Arc<AtomicBool>> }
impl Services {
    pub fn new(states: States) -> Self { Self { states, tokens: BTreeMap::new() } }
    pub fn stop(&mut self, id: u8) {
        if let Some(token) = self.tokens.remove(&id) { token.store(true, Ordering::Release); }
        if let Some(state) = self.states.lock().unwrap().get_mut(&id) { state.phase = "Offline".into(); }
    }
    pub fn start(&mut self, id: u8, vendor: String) {
        self.stop(id);
        let generation = self.states.lock().unwrap().get(&id).map_or(1, |state| state.generation + 1);
        self.states.lock().unwrap().insert(id, State { generation, phase: "Collecting identity".into(), ..State::default() });
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
                    state.phase = "Retrying identity".into();
                }
                for _ in 0..100 {
                    if token.load(Ordering::Acquire) { return; }
                    thread::sleep(Duration::from_millis(100));
                }
            };
            if token.load(Ordering::Acquire) { return; }
            let adapter = crate::devices::select(&vendor, &identity);
            if let Some(state) = states.lock().unwrap().get_mut(&id).filter(|state| state.generation == generation) {
                state.phase = adapter.status().into();
                state.identity = Some(identity);
                state.identity_at = Some(std::time::Instant::now());
            }
            if !adapter.capabilities().business {
                println!("[!] ONU {id} generation={generation} vendor={vendor}: identity collected; business profile unsupported");
                return;
            }
            let set = |phase: &str, error: Option<String>| {
                if token.load(Ordering::Acquire) { return; }
                if let Some(state) = states.lock().unwrap().get_mut(&id).filter(|state| state.generation == generation) {
                    state.phase = phase.into(); state.error = error;
                }
            };
            while !token.load(Ordering::Acquire) {
                set("Configuring", None);
                let result = adapter.configure(id, token.clone())
                    .and_then(|()| {
                        if token.load(Ordering::Acquire) { return Ok(()); }
                        let phase = adapter.verification_status();
                        println!("[+] ONU {id} generation={generation}: OMCI profile readback complete; {phase}; starting evidence monitor");
                        set(phase, None);
                        super::health::watch(id, token.clone(), vendor.as_bytes().try_into().map_err(|_| std::io::Error::other("invalid vendor ID"))?)
                    });
                if token.load(Ordering::Acquire) { break; }
                if let Err(error) = result {
                    eprintln!("[-] ONU {id} generation={generation} service retry: {error}");
                    set("Retrying", Some(error.to_string()));
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
