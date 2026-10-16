use super::{Queue, MAX_PENDING_BYTES};

#[test]
fn evicted_receipt_completes_without_losing_last_good_report() {
    let mut store = super::Store::default();
    let mac = "020000000001";
    let mut state = super::State::default();
    state.apply(0, Ok(super::super::parse(&super::super::tests::fixture(&[("IP", b"192.0.2.10")])).unwrap()));
    store.states.insert(mac.into(), state);
    let size = super::super::archive::MAX_COMPRESSED;
    for received in 1..=5 {
        assert!(store.queue.put(&format!("02000000{received:04X}"), vec![1; size], received, &[]));
    }

    let next = store.take().unwrap();

    assert_eq!(next.0, "020000000002");
    let state = &store.states[mac];
    assert_eq!(state.last_processed_receipt, Some(1));
    assert_eq!(state.last_good, Some(0));
    assert_eq!(state.report.as_ref().unwrap().fields["ip"].value, "192.0.2.10");
    assert_eq!(state.error.as_deref(), Some("pending telemetry byte budget exceeded"));
}

#[test]
fn oldest_payload_becomes_error_receipt_when_pending_budget_is_exceeded() {
    let mut queue = Queue::default();
    let size = super::super::archive::MAX_COMPRESSED;
    for received in 1..=4 {
        assert!(queue.put(&format!("02000000{received:04X}"), vec![1; size], received, &[]));
    }

    assert!(queue.put("020000000005", vec![2; size], 5, &[]));

    assert_eq!(queue.pending.values().filter_map(|item| item.payload.as_ref()).map(Vec::len).sum::<usize>(), MAX_PENDING_BYTES);
    let (mac, oldest) = queue.take().unwrap();
    assert_eq!(mac, "020000000001");
    assert_eq!(oldest.received, 1);
    assert!(oldest.budget_evicted);
    assert!(oldest.payload.is_none());
    assert_eq!(queue.pending["020000000005"].payload.as_deref(), Some(vec![2; size].as_slice()));
}

#[test]
fn replacement_reuses_budget_when_source_already_has_payload() {
    let mut queue = Queue::default();
    let size = super::super::archive::MAX_COMPRESSED;
    for received in 1..=4 {
        assert!(queue.put(&format!("02000000{received:04X}"), vec![1; size], received, &[]));
    }

    assert!(queue.put("020000000001", vec![2; size], 5, &[]));

    assert_eq!(queue.pending.len(), 4);
    assert!(queue.pending.values().all(|item| !item.budget_evicted));
    assert_eq!(queue.pending["020000000001"].received, 5);
}

#[test]
fn processing_payload_is_not_evicted_when_more_pending_payloads_arrive() {
    let mut queue = Queue::default();
    let size = super::super::archive::MAX_COMPRESSED;
    assert!(queue.put("020000000001", vec![7; size], 1, &[]));
    let processing = queue.take().unwrap().1;

    for received in 2..=6 {
        assert!(queue.put(&format!("02000000{received:04X}"), vec![1; size], received, &[]));
    }

    assert_eq!(processing.payload.unwrap(), vec![7; size]);
    assert!(queue.pending["020000000002"].budget_evicted);
    assert!(!queue.pending["020000000006"].budget_evicted);
}
