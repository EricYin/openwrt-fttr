use super::Report;
#[path = "store_state.rs"]
mod state;
pub use state::State;
use std::{
    collections::BTreeMap,
    sync::{Mutex, OnceLock},
};

struct Pending {
    payload: Option<Vec<u8>>,
    received: u64,
    budget_evicted: bool,
}

const MAX_PENDING_BYTES: usize = 8 * 1024 * 1024;

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
        self.pending.insert(
            mac,
            Pending {
                payload,
                received,
                budget_evicted: false,
            },
        );
        while self
            .pending
            .values()
            .filter_map(|item| item.payload.as_ref())
            .map(Vec::len)
            .sum::<usize>()
            > MAX_PENDING_BYTES
        {
            let oldest = self
                .pending
                .values_mut()
                .filter(|item| item.payload.is_some())
                .min_by_key(|item| item.received);
            if let Some(item) = oldest {
                item.payload = None;
                item.budget_evicted = true;
            }
        }
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
static SHARED: OnceLock<(Mutex<Store>, tokio::sync::Notify)> = OnceLock::new();

impl Store {
    fn take(&mut self) -> Option<(String, Pending)> {
        while let Some((mac, item)) = self.queue.take() {
            if item.budget_evicted {
                self.states.entry(mac).or_default().apply(
                    item.received,
                    Err("pending telemetry byte budget exceeded".into()),
                );
            } else {
                return Some((mac, item));
            }
        }
        None
    }
}

fn shared() -> &'static (Mutex<Store>, tokio::sync::Notify) {
    SHARED.get_or_init(|| {
        crate::runtime::spawn_service("HL202 telemetry", super::worker::run());
        (Mutex::new(Store::default()), tokio::sync::Notify::new())
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
    let Store { queue, states } = &mut *store;
    states.retain(|key, _| queue.seen.contains_key(key));
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

pub fn snapshot(mac: &str) -> Option<State> {
    let (mutex, _) = shared();
    let store = mutex.lock().ok()?;
    store.states.get(mac).cloned()
}

pub fn revision(mac: &str) -> Option<u64> {
    let (mutex, _) = shared();
    let store = mutex.lock().ok()?;
    Some(store.states.get(mac)?.revision)
}

pub(super) async fn next() -> Option<(String, Option<Vec<u8>>, u64)> {
    let (mutex, signal) = shared();
    loop {
        let notified = signal.notified();
        if let Some((mac, item)) = mutex.lock().ok()?.take() {
            return Some((mac, item.payload, item.received));
        }
        tokio::select! {
            _ = notified => {},
            _ = crate::runtime::cancelled() => return None,
        }
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
#[path = "store_budget_tests.rs"]
mod budget_tests;

#[cfg(test)]
mod tests {
    #[test]
    fn revision_is_absent_when_source_has_no_state() {
        assert_eq!(super::revision("revision-test-missing"), None);
    }

    #[test]
    fn revision_matches_snapshot_when_report_is_retained_after_error() {
        let mac = "revision-test-retained";
        let report =
            super::super::parse(&super::super::tests::fixture(&[("unknown", b"raw text")]))
                .unwrap();
        let mut state = super::State::default();
        state.apply(10, Ok(report));
        state.apply(11, Err("invalid gzip".into()));
        super::shared()
            .0
            .lock()
            .unwrap()
            .states
            .insert(mac.into(), state);
        let revision = super::revision(mac);
        let snapshot = super::snapshot(mac).unwrap();
        assert_eq!(revision, Some(2));
        assert_eq!(snapshot.revision, 2);
        assert_eq!(
            snapshot.report.unwrap().members[0].text.as_deref(),
            Some("raw text")
        );
        assert_eq!(snapshot.error.as_deref(), Some("invalid gzip"));
        super::shared().0.lock().unwrap().states.remove(mac);
    }

    #[test]
    fn revision_returns_zero_when_state_is_pending() {
        let mac = "revision-test-pending";
        super::shared()
            .0
            .lock()
            .unwrap()
            .states
            .insert(mac.into(), super::State::default());
        let revision = super::revision(mac);
        assert_eq!(revision, Some(0));
        super::shared().0.lock().unwrap().states.remove(mac);
    }

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
