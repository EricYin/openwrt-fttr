use super::Report;
use serde::Serialize;
use std::{
    collections::BTreeMap,
    sync::{Condvar, Mutex, OnceLock},
};

#[derive(Default, Serialize)]
pub struct State {
    pub revision: u64,
    pub last_received: u64,
    pub last_processed_receipt: Option<u64>,
    pub last_good: Option<u64>,
    pub error: Option<String>,
    pub report: Option<Report>,
}

impl State {
    fn apply(&mut self, received: u64, result: Result<Report, String>) {
        self.revision = self.revision.wrapping_add(1);
        self.last_received = self.last_received.max(received);
        self.last_processed_receipt = Some(received);
        match result {
            Ok(report) => {
                self.last_good = Some(received);
                self.report = Some(report);
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
    }
}

struct Pending {
    payload: Option<Vec<u8>>,
    received: u64,
}

#[derive(Default)]
struct Queue {
    seen: BTreeMap<String, u64>,
    pending: BTreeMap<String, Pending>,
}

impl Queue {
    fn put(&mut self, source: &str, payload: Vec<u8>, received: u64, protected: &[String]) -> bool {
        let Some(mac) = crate::mqtt_identity::normalize_target(source) else {
            return false;
        };
        let payload = (payload.len() <= super::archive::MAX_COMPRESSED).then_some(payload);
        if !self.seen.contains_key(&mac) && self.seen.len() >= 64 {
            let oldest = self
                .seen
                .iter()
                .filter(|(key, _)| !protected.contains(key))
                .min_by_key(|(_, received)| *received)
                .map(|(key, _)| key.clone());
            let Some(oldest) = oldest else {
                return false;
            };
            self.seen.remove(&oldest);
            self.pending.remove(&oldest);
        }
        self.seen.insert(mac.clone(), received);
        self.pending.insert(mac, Pending { payload, received });
        true
    }

    fn take(&mut self) -> Option<(String, Pending)> {
        let mac = self
            .pending
            .iter()
            .min_by_key(|(_, item)| item.received)
            .map(|(mac, _)| mac.clone())?;
        self.pending.remove(&mac).map(|item| (mac, item))
    }
}

#[derive(Default)]
struct Store {
    queue: Queue,
    states: BTreeMap<String, State>,
}
static SHARED: OnceLock<(Mutex<Store>, Condvar)> = OnceLock::new();

fn shared() -> &'static (Mutex<Store>, Condvar) {
    SHARED.get_or_init(|| {
        std::thread::spawn(super::worker::run);
        (Mutex::new(Store::default()), Condvar::new())
    })
}

pub fn observe(source: &str, payload: Vec<u8>, received: u64) -> bool {
    let protected = match crate::mqtt_identity::shared().lock() {
        Ok(registry) => registry
            .snapshots(received)
            .into_iter()
            .filter(|source| source.reason == "ready")
            .map(|source| source.mqtt_mac)
            .collect::<Vec<_>>(),
        Err(_) => return false,
    };
    let (mutex, signal) = shared();
    let Ok(mut store) = mutex.lock() else {
        return false;
    };
    if !store.queue.put(source, payload, received, &protected) {
        return false;
    }
    let seen = store.queue.seen.keys().cloned().collect::<Vec<_>>();
    store.states.retain(|key, _| seen.contains(key));
    if let Some(mac) = crate::mqtt_identity::normalize_target(source) {
        let state = store.states.entry(mac).or_default();
        if received < state.last_received {
            *state = State::default();
        }
        state.last_received = received;
    }
    signal.notify_one();
    true
}

pub fn snapshot(mac: &str) -> Option<serde_json::Value> {
    let (mutex, _) = shared();
    let store = mutex.lock().ok()?;
    serde_json::to_value(store.states.get(mac)?).ok()
}

pub(super) fn next() -> Option<(String, Option<Vec<u8>>, u64)> {
    let (mutex, signal) = shared();
    let mut store = mutex.lock().ok()?;
    loop {
        if let Some((mac, item)) = store.queue.take() {
            return Some((mac, item.payload, item.received));
        }
        store = signal.wait(store).ok()?;
    }
}

pub(super) fn complete(mac: String, received: u64, result: Result<Report, String>) {
    if let Ok(mut store) = shared().0.lock() {
        if store
            .queue
            .seen
            .get(&mac)
            .is_some_and(|latest| received <= *latest)
        {
            store.states.entry(mac).or_default().apply(received, result);
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn telemetry_ingest_pending_receipt_does_not_regress_on_older_completion() {
        let mut state = super::State {
            last_received: 12,
            ..Default::default()
        };
        state.apply(11, Err("bad gzip".into()));
        assert_eq!(state.last_received, 12);
    }

    #[test]
    fn telemetry_ingest_oversized_payload_is_queued_as_error_without_retaining_bytes() {
        let mut queue = super::Queue::default();
        assert!(queue.put(
            "001122334455",
            vec![0; super::super::archive::MAX_COMPRESSED + 1],
            1,
            &[]
        ));
        assert_eq!(queue.pending.len(), 1);
        assert!(queue.pending["001122334455"].payload.is_none());
    }

    #[test]
    fn telemetry_ingest_source_limit_preserves_protected_sources() {
        let mut queue = super::Queue::default();
        for n in 0..64 {
            assert!(queue.put(&format!("02000000{n:04X}"), vec![1], n, &[]));
        }
        assert!(queue.put("020000000100", vec![1], 100, &["020000000000".into()]));
        assert_eq!(queue.seen.len(), 64);
        assert!(queue.seen.contains_key("020000000000"));
        assert!(!queue.seen.contains_key("020000000001"));
        let protected = queue.seen.keys().cloned().collect::<Vec<_>>();
        assert!(!queue.put("020000000101", vec![1], 101, &protected));
    }

    #[test]
    fn telemetry_ingest_bad_new_report_retains_distinct_old_good_state() {
        let mut state = super::State::default();
        let report = crate::devices::hl202::telemetry::parse(&super::super::tests::fixture(&[(
            "IP",
            b"192.0.2.10",
        )]))
        .unwrap();
        state.apply(10, Ok(report));
        state.apply(11, Err("invalid gzip".into()));
        assert_eq!(state.last_received, 11);
        assert_eq!(state.last_good, Some(10));
        assert_eq!(state.revision, 2);
        assert_eq!(
            state.report.as_ref().unwrap().fields["ip"].value,
            serde_json::json!("192.0.2.10")
        );
        assert_eq!(state.error.as_deref(), Some("invalid gzip"));
    }

    #[test]
    fn telemetry_ingest_invalid_topic_never_enters_queue_and_pending_is_latest_only() {
        let mut queue = super::Queue::default();
        assert!(!queue.put("../../escape", vec![1], 1, &[]));
        assert!(queue.put("001122334455", vec![1], 1, &[]));
        assert!(queue.put("001122334455", vec![2], 2, &[]));
        assert_eq!(queue.pending.len(), 1);
        assert_eq!(queue.take().unwrap().1.payload, Some(vec![2]));
    }
}
