use super::*;
use std::collections::BTreeMap;

fn online() -> BTreeMap<u16, String> {
    BTreeMap::from([(2, "H3CT12345678".into())])
}

fn links() -> BTreeMap<String, String> {
    BTreeMap::from([("02AABBCCDDEE".into(), "H3CT12345678".into())])
}

#[test]
fn mqtt_identity_resolves_without_acs_and_rejects_stale_selection() {
    let mut registry = Registry::new("daemon-a".into());
    registry.observe("02AABBCCDDEE/fttr_data.tar.gz", 100);
    registry.update_topology(online(), Ok(links()), 100);
    let snapshot = registry.snapshots(100).remove(0);
    let selection = Selection {
        optical_sn: "H3CT12345678".into(),
        onu_id: 2,
        binding_generation: snapshot.binding_generation,
    };
    assert_eq!(
        registry.resolve(&selection, 100).unwrap().mqtt_mac,
        "02AABBCCDDEE"
    );
    registry.update_topology(
        BTreeMap::from([(2, "H3CT87654321".into())]),
        Ok(links()),
        101,
    );
    assert!(registry.resolve(&selection, 101).is_err());
}

#[test]
fn mqtt_identity_refuses_ambiguity_expiry_and_clock_regression() {
    let mut registry = Registry::new("daemon-a".into());
    registry.observe("02AABBCCDDEE", 100);
    registry.update_topology(online(), Ok(links()), 100);
    assert_eq!(registry.snapshots(160)[0].reason, "ready");
    assert_ne!(registry.snapshots(161)[0].reason, "ready");
    registry.observe("02AABBCCDDEF", 100);
    let mut conflicting = links();
    conflicting.insert("02AABBCCDDEF".into(), "H3CT12345678".into());
    registry.update_topology(online(), Ok(conflicting), 100);
    assert!(registry
        .snapshots(100)
        .iter()
        .all(|s| s.reason == "ambiguous"));
    registry.observe("02AABBCCDDEE", 99);
    assert!(registry.snapshots(99).iter().all(|s| s.reason != "ready"));
}

#[test]
fn mqtt_identity_reconnect_and_daemon_restart_change_generation() {
    let mut registry = Registry::new("daemon-a".into());
    registry.observe("02AABBCCDDEE", 100);
    registry.update_topology(online(), Ok(links()), 100);
    let old = registry.snapshots(100)[0].binding_generation.clone();
    registry.observe("02AABBCCDDEE", 161);
    registry.update_topology(online(), Ok(links()), 161);
    assert_ne!(registry.snapshots(161)[0].binding_generation, old);
    let mut restarted = Registry::new("daemon-b".into());
    restarted.observe("02AABBCCDDEE", 100);
    restarted.update_topology(online(), Ok(links()), 100);
    assert_ne!(restarted.snapshots(100)[0].binding_generation, old);
}

#[test]
fn mqtt_identity_invalid_topics_never_create_sources() {
    let mut registry = Registry::new("daemon-a".into());
    for topic in [
        "../../escape",
        "02AABBCCDDEE/unknown",
        "0:2AABBCCDDEE",
        "H3CT12345678",
    ] {
        registry.observe(topic, 100);
    }
    assert!(registry.snapshots(100).is_empty());
}

#[test]
fn mqtt_identity_target_switch_never_reuses_the_old_selection() {
    let mut registry = Registry::new("daemon-a".into());
    let mut mapping = links();
    mapping.insert("02AABBCCDDEF".into(), "H3CT12345678".into());
    registry.update_topology(online(), Ok(mapping.clone()), 100);
    registry.observe("02AABBCCDDEE", 100);
    let old = registry.snapshots(100).remove(0);
    let selection = Selection {
        optical_sn: "H3CT12345678".into(),
        onu_id: 2,
        binding_generation: old.binding_generation,
    };
    registry.observe("02AABBCCDDEF", 161);
    registry.update_topology(online(), Ok(mapping), 161);
    assert!(registry.resolve(&selection, 161).is_err());
}

#[test]
fn mqtt_identity_source_limit_preserves_a_bound_target() {
    let mut registry = Registry::new("daemon-a".into());
    registry.update_topology(online(), Ok(links()), 100);
    registry.observe("02AABBCCDDEE", 100);
    for index in 0..64 {
        registry.observe(&format!("02000000{index:04X}"), 100);
    }
    let snapshots = registry.snapshots(100);
    assert_eq!(snapshots.len(), 64);
    assert!(snapshots
        .iter()
        .any(|s| s.mqtt_mac == "02AABBCCDDEE" && s.reason == "ready"));
}

#[test]
fn mqtt_identity_failed_fdb_observation_invalidates_the_binding() {
    let mut registry = Registry::new("daemon-a".into());
    registry.observe("02AABBCCDDEE", 100);
    registry.update_topology(online(), Ok(links()), 100);
    let old = registry.snapshots(100).remove(0);
    registry.update_topology(online(), Err("FDB unavailable".into()), 101);
    let current = registry.snapshots(101).remove(0);
    assert_eq!(current.reason, "unmatched");
    assert_ne!(current.binding_generation, old.binding_generation);
    assert!(current.optical_sn.is_none());
}
