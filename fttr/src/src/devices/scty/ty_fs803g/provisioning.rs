//! Native 8-WLAN CMIOT/Andlink IF5 MQTT provisioning contract for TY-FS803G.
//!
//! Maps logical `(radio, slot)` intents to IF5 `Index 1..=8`, executes
//! single-entry `XControl_WiFiParameterSync` calls in `2.4G -> 5G` order,
//! verifies odd slots `{1, 3, 5, 7}` via `XQuery_ApConfigInfo`, and reports
//! even slots `{2, 4, 6, 8}` honestly as `accepted_unverified` (due to
//! `managePlugin` filtering `(Index & 0xFFFFFFF9) == 1`).
use super::{
    boot_mock,
    if5::{self, ApConfigSnapshot, WlanConfigEntry},
};
use crate::provisioning::{
    model::{Device, EndpointRef, WifiSecurity},
    storage,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    net::SocketAddr,
    sync::{Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

static ACTIVE_TRANSACTIONS: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

struct TransactionGuard(String);
impl TransactionGuard {
    fn acquire(mac: &str) -> Result<Self, String> {
        let mut set = ACTIVE_TRANSACTIONS
            .get_or_init(|| Mutex::new(HashSet::new()))
            .lock()
            .unwrap();
        if !set.insert(mac.to_owned()) {
            return Err("device transaction already active".into());
        }
        Ok(Self(mac.to_owned()))
    }
}
impl Drop for TransactionGuard {
    fn drop(&mut self) {
        if let Some(lock) = ACTIVE_TRANSACTIONS.get() {
            let _ = lock.lock().map(|mut set| set.remove(&self.0));
        }
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct OwnershipJournal {
    #[serde(default)]
    revision: String,
    #[serde(default)]
    operation_id: u64,
    #[serde(default)]
    binding_generation: String,
    #[serde(default)]
    owned_even_slots: BTreeSet<u32>,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn odd_index(index: u32) -> bool {
    (index & 0xffff_fff9) == 1
}

fn slot_to_if5_index(endpoint: &EndpointRef) -> Result<(u32, &'static str), String> {
    match endpoint {
        EndpointRef::Ssid { radio: 2, slot } if (1..=4).contains(slot) => Ok((*slot, "2.4G")),
        EndpointRef::Ssid { radio: 5, slot } if (1..=4).contains(slot) => Ok((*slot + 4, "5G")),
        _ => Err("unsupported TY-FS803G Wi-Fi endpoint; expected radio 2g/5g slot 1..4".into()),
    }
}

fn map_security(
    security: Option<WifiSecurity>,
    key: Option<&str>,
    index: u32,
    existing: Option<&WlanConfigEntry>,
) -> Result<(String, u8, String), String> {
    match security {
        Some(WifiSecurity::None) => Ok(("None".into(), 0, String::new())),
        Some(WifiSecurity::Wpa2Psk) => Ok((
            "WPA2-Personal".into(),
            1,
            key.unwrap_or_default().to_owned(),
        )),
        Some(WifiSecurity::WpaPskWpa2Psk) => Ok((
            "MIXED-WPAPSK2".into(),
            3,
            key.unwrap_or_default().to_owned(),
        )),
        Some(WifiSecurity::Wpa3Sae) => {
            Ok(("WPA3-SAE".into(), 1, key.unwrap_or_default().to_owned()))
        }
        Some(WifiSecurity::Wpa2PskWpa3Sae) => Ok((
            "MIXED-WPA2WPA3".into(),
            1,
            key.unwrap_or_default().to_owned(),
        )),
        None => {
            if let Some(prev) = existing {
                Ok((prev.security_mode.clone(), prev.encrypt, prev.pwd.clone()))
            } else {
                Err(format!(
                    "even or unreadable WLAN index {index} requires explicit wifi security"
                ))
            }
        }
    }
}

fn entry_matches(desired: &WlanConfigEntry, actual: &WlanConfigEntry) -> bool {
    desired.index == actual.index
        && desired.enable == actual.enable
        && desired.ssid == actual.ssid
        && desired.security_mode == actual.security_mode
        && desired.encrypt == actual.encrypt
        && desired.ssid_advertisement_enabled == actual.ssid_advertisement_enabled
        && (desired.security_mode == "None" || desired.pwd == actual.pwd)
}

#[derive(Clone)]
struct CompiledItem {
    item_id: String,
    kind: crate::provisioning::model::ItemKind,
    service_type: String,
    entry: WlanConfigEntry,
    secret_redacted: bool,
}

struct CompiledPlan {
    items: Vec<CompiledItem>,
    cleanup: Vec<WlanConfigEntry>,
    next_owned_even: BTreeSet<u32>,
}

fn compile_plan(
    device: &Device,
    snapshot: &ApConfigSnapshot,
    journal: &OwnershipJournal,
    full: bool,
) -> Result<CompiledPlan, String> {
    if full && device.wifi_cleanup == "delete" {
        return Err(
            "native fixed Wi-Fi slots cannot be deleted; select disable or preserve".into(),
        );
    }

    let queried: BTreeMap<u32, &WlanConfigEntry> = snapshot
        .configurations
        .iter()
        .map(|c| (c.index, c))
        .collect();
    let mut used_indices = BTreeSet::new();
    let mut items = Vec::new();
    let mut next_owned_even = if full {
        BTreeSet::new()
    } else {
        journal.owned_even_slots.clone()
    };

    for item in &device.items {
        if let Some(reason) = &item.blocked {
            return Err(reason.clone());
        }
        if item.wan_intent.is_some() || item.binding_intent.is_some() {
            return Err(
                "TY-FS803G IF5 adapter supports Wi-Fi provisioning only; WAN/port-binding is unsupported"
                    .into(),
            );
        }
        let Some(wifi) = &item.wlan_intent else {
            return Err(format!("{}: item has no supported Wi-Fi intent", item.id));
        };
        let (index, radio) = slot_to_if5_index(&wifi.endpoint)?;
        if !used_indices.insert(index) {
            return Err(format!("duplicate desired Wi-Fi endpoint at index {index}"));
        }
        let existing = queried.get(&index).copied();
        let (security_mode, encrypt, pwd) =
            map_security(wifi.security, wifi.key.as_deref(), index, existing)?;
        let max_associate_num = existing.map_or(32, |c| c.max_associate_num.max(1));
        let entry = WlanConfigEntry {
            radio: radio.into(),
            index,
            enable: u8::from(wifi.enabled.unwrap_or(true)),
            ssid: wifi.ssid.clone(),
            security_mode,
            pwd,
            encrypt,
            max_associate_num,
            ssid_advertisement_enabled: u8::from(wifi.broadcast.unwrap_or(true)),
        };
        if !odd_index(index) {
            next_owned_even.insert(index);
        }
        items.push(CompiledItem {
            item_id: item.id.clone(),
            kind: item.kind,
            service_type: item.service_type.clone(),
            entry,
            secret_redacted: wifi.key.is_some(),
        });
    }

    let mut cleanup = Vec::new();
    if full && device.wifi_cleanup == "disable" {
        for index in 1u32..=8 {
            if used_indices.contains(&index) || device.wifi_preserve.contains(&index) {
                continue;
            }
            if odd_index(index) {
                if let Some(existing) = queried.get(&index).copied() {
                    if existing.enable != 0 {
                        let mut disabled = existing.clone();
                        disabled.enable = 0;
                        cleanup.push(disabled);
                    }
                }
            } else if journal.owned_even_slots.contains(&index) {
                cleanup.push(WlanConfigEntry {
                    radio: if index <= 4 { "2.4G" } else { "5G" }.into(),
                    index,
                    enable: 0,
                    ssid: format!("FTTR-Disabled-{index}"),
                    security_mode: "None".into(),
                    pwd: String::new(),
                    encrypt: 0,
                    max_associate_num: 32,
                    ssid_advertisement_enabled: 0,
                });
            }
        }
    }

    Ok(CompiledPlan {
        items,
        cleanup,
        next_owned_even,
    })
}

pub async fn run(
    device: Device,
    apply: bool,
    full: bool,
    revision: String,
    operation_id: u64,
) -> Result<Value, String> {
    let broker_addr: SocketAddr = "127.0.0.1:1883".parse().unwrap();
    let (target_mac, binding_generation) = {
        let registry = crate::mqtt_identity::shared().lock().unwrap();
        let targets: Vec<_> = registry
            .snapshots(now())
            .into_iter()
            .filter(|s| s.optical_sn.as_deref() == Some(&device.optical_sn) && s.reason == "ready")
            .collect();
        let [target] = targets.as_slice() else {
            return Err("fresh unique optical/MQTT identity required".into());
        };
        (target.mqtt_mac.clone(), target.binding_generation.clone())
    };
    let device_id = boot_mock::shared_registry()
        .lock()
        .unwrap()
        .lookup_device_id(&target_mac)
        .ok_or_else(|| {
            "TY-FS803G IF5 deviceId unavailable; waiting for Boot Mock / MBoot registration"
                .to_owned()
        })?;
    let _tx = TransactionGuard::acquire(&target_mac)?;

    let journal_path = format!("/etc/fttr/command-owned/{}.json", device.optical_sn);
    run_with_transport(
        device,
        apply,
        full,
        revision,
        operation_id,
        &device_id,
        &binding_generation,
        &journal_path,
        || if5::query_ap_config_info(broker_addr, &device_id),
        |entry| {
            let entry = entry.clone();
            let device_id = device_id.clone();
            async move { if5::sync_single_wlan(broker_addr, &device_id, &entry).await }
        },
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn run_with_transport<QF, QFut, WF, WFut>(
    device: Device,
    apply: bool,
    full: bool,
    revision: String,
    operation_id: u64,
    device_id: &str,
    binding_generation: &str,
    journal_path: &str,
    mut query_fn: QF,
    mut write_fn: WF,
) -> Result<Value, String>
where
    QF: FnMut() -> QFut,
    QFut: std::future::Future<Output = Result<ApConfigSnapshot, String>>,
    WF: FnMut(&WlanConfigEntry) -> WFut,
    WFut: std::future::Future<Output = Result<(), String>>,
{
    let mut journal: OwnershipJournal = match std::fs::read(journal_path) {
        Ok(bytes) => {
            serde_json::from_slice(&bytes).map_err(|_| "invalid SCTY command ownership journal")?
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => OwnershipJournal::default(),
        Err(_) => return Err("cannot read SCTY command ownership journal".into()),
    };

    let initial = query_fn().await?;
    let plan = compile_plan(&device, &initial, &journal, full)?;
    let initial_map: BTreeMap<u32, &WlanConfigEntry> = initial
        .configurations
        .iter()
        .map(|c| (c.index, c))
        .collect();

    let mut writes: Vec<&WlanConfigEntry> = Vec::new();
    for item in &plan.items {
        let needs_write = match initial_map.get(&item.entry.index) {
            Some(actual) => !entry_matches(&item.entry, actual),
            None => true,
        };
        if needs_write {
            writes.push(&item.entry);
        }
    }
    for clean in &plan.cleanup {
        writes.push(clean);
    }
    // Always apply 2.4G (Index 1..=4) before 5G (Index 5..=8) to keep BandSteering deterministic.
    writes.sort_by_key(|e| e.index);

    let mut accepted_even = BTreeSet::new();
    let mut write_performed = false;
    if apply && !writes.is_empty() {
        journal.revision = revision.clone();
        journal.operation_id = operation_id;
        journal.binding_generation = binding_generation.to_owned();
        storage::write_atomic(
            journal_path,
            &serde_json::to_vec(&journal).map_err(|_| "serialize SCTY command journal")?,
            true,
        )
        .map_err(|_| "persist SCTY command write intent")?;

        for entry in writes {
            write_fn(entry).await?;
            write_performed = true;
            if !odd_index(entry.index) {
                accepted_even.insert(entry.index);
            }
        }
        journal.owned_even_slots = plan.next_owned_even.clone();
        storage::write_atomic(
            journal_path,
            &serde_json::to_vec(&journal).map_err(|_| "serialize SCTY command journal")?,
            true,
        )
        .map_err(|_| "persist SCTY command ownership")?;
    }

    let final_snapshot = if write_performed {
        query_fn().await?
    } else {
        initial
    };
    let final_map: BTreeMap<u32, &WlanConfigEntry> = final_snapshot
        .configurations
        .iter()
        .map(|c| (c.index, c))
        .collect();

    let mut items_out = Vec::with_capacity(plan.items.len());
    let mut all_verified = true;
    for item in &plan.items {
        let idx = item.entry.index;
        let (state, reason) = if odd_index(idx) {
            match final_map.get(&idx) {
                Some(actual) if entry_matches(&item.entry, actual) => (
                    "verified",
                    "IF5 XQuery_ApConfigInfo readback matches desired WLAN state".to_owned(),
                ),
                Some(_) if apply => {
                    all_verified = false;
                    (
                        "drift",
                        format!(
                            "IF5 XQuery_ApConfigInfo readback differs after apply on Index {idx}"
                        ),
                    )
                }
                Some(_) => {
                    all_verified = false;
                    (
                        "drift",
                        format!("read-only plan: IF5 Index {idx} differs from desired WLAN state"),
                    )
                }
                None => {
                    all_verified = false;
                    (
                        "unknown",
                        format!("IF5 XQuery_ApConfigInfo missing expected odd Index {idx}"),
                    )
                }
            }
        } else {
            all_verified = false;
            if apply && accepted_even.contains(&idx) {
                (
                    "accepted_unverified",
                    format!(
                        "IF5 XControl_WiFiParameterSync accepted for even Index {idx}; XQuery_ApConfigInfo only reports odd indices {{1,3,5,7}}"
                    ),
                )
            } else {
                (
                    "unknown",
                    format!(
                        "IF5 XQuery_ApConfigInfo cannot inspect even Index {idx}; apply required"
                    ),
                )
            }
        };
        let desired_security = if item.secret_redacted {
            "<redacted>"
        } else {
            &item.entry.security_mode
        };
        items_out.push(json!({
            "id": item.item_id,
            "kind": item.kind,
            "service_type": item.service_type,
            "if5_index": idx,
            "radio": item.entry.radio,
            "state": state,
            "reason": reason,
            "updated_at": now(),
            "changes": [
                {"name": format!("WLAN.{idx}.SSID"), "desired": item.entry.ssid},
                {"name": format!("WLAN.{idx}.Enable"), "desired": item.entry.enable.to_string()},
                {"name": format!("WLAN.{idx}.SecurityMode"), "desired": desired_security},
            ]
        }));
    }

    Ok(json!({
        "items": items_out,
        "if5_device_id": device_id,
        "band_steering_status": final_snapshot.band_steering_status,
        "write_performed": write_performed,
        "readback_verified": all_verified,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provisioning::model::{Item, ItemKind, WlanIntent};
    use std::cell::RefCell;

    fn test_device(count_per_band: u32) -> Device {
        let mut items = Vec::new();
        for radio in [2u8, 5u8] {
            for slot in 1..=count_per_band {
                items.push(Item {
                    id: format!("wlan_{radio}g_{slot}"),
                    kind: ItemKind::Ssid,
                    service_type: "wifi".into(),
                    parameters: Vec::new(),
                    guard: None,
                    blocked: None,
                    wan_ref: None,
                    auto_instance: None,
                    wan_intent: None,
                    binding_intent: None,
                    wlan_intent: Some(WlanIntent {
                        endpoint: EndpointRef::Ssid { radio, slot },
                        ssid: format!("FTTR_{radio}G_{slot}"),
                        enabled: Some(true),
                        broadcast: Some(true),
                        security: Some(WifiSecurity::Wpa2Psk),
                        key: Some("TestPass123!".into()),
                    }),
                });
            }
        }
        Device {
            id: "scty_dev".into(),
            optical_sn: "SCTY00000001".into(),
            cwmp_id: "pending:SCTY00000001".into(),
            omci_adapter: "scty".into(),
            bootstrap_adapter: "none".into(),
            provisioning_adapter: "scty-if5-mqtt".into(),
            resolved_contract: None,
            connection_request: Default::default(),
            items,
            wifi_cleanup: "disable".into(),
            wifi_preserve: BTreeSet::new(),
        }
    }

    #[test]
    fn eight_wlan_provisioning_writes_2g_then_5g_verifies_odd_and_marks_even_accepted_unverified() {
        let dir = std::env::temp_dir().join(format!("fttr-scty-8wlan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let journal = dir.join("journal.json");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let store: RefCell<BTreeMap<u32, WlanConfigEntry>> = RefCell::new(BTreeMap::new());
        let write_order: RefCell<Vec<u32>> = RefCell::new(Vec::new());

        let result = rt
            .block_on(run_with_transport(
                test_device(4),
                true,
                true,
                "rev-1".into(),
                42,
                "IF5021122334455",
                "gen-1",
                journal.to_str().unwrap(),
                || {
                    let configs = store
                        .borrow()
                        .values()
                        .filter(|e| odd_index(e.index))
                        .cloned()
                        .collect();
                    async move {
                        Ok(ApConfigSnapshot {
                            band_steering_status: Some(0),
                            configurations: configs,
                        })
                    }
                },
                |entry| {
                    let entry = entry.clone();
                    write_order.borrow_mut().push(entry.index);
                    store.borrow_mut().insert(entry.index, entry);
                    async { Ok(()) }
                },
            ))
            .unwrap();

        assert_eq!(*write_order.borrow(), vec![1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(result["write_performed"], true);
        assert_eq!(result["readback_verified"], false);
        let items = result["items"].as_array().unwrap();
        assert_eq!(items.len(), 8);
        for item in items {
            let idx = item["if5_index"].as_u64().unwrap();
            let expected = if matches!(idx, 1 | 3 | 5 | 7) {
                "verified"
            } else {
                "accepted_unverified"
            };
            assert_eq!(item["state"], expected, "index {idx}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn two_wlan_odd_slots_verify_100_percent() {
        let dir = std::env::temp_dir().join(format!("fttr-scty-2wlan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let journal = dir.join("journal.json");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let mut device = test_device(1);
        device.optical_sn = "SCTY00000002".into();
        let store: RefCell<BTreeMap<u32, WlanConfigEntry>> = RefCell::new(BTreeMap::new());

        let result = rt
            .block_on(run_with_transport(
                device,
                true,
                true,
                "rev-2".into(),
                43,
                "IF5021122334456",
                "gen-1",
                journal.to_str().unwrap(),
                || {
                    let configs = store
                        .borrow()
                        .values()
                        .filter(|e| odd_index(e.index))
                        .cloned()
                        .collect();
                    async move {
                        Ok(ApConfigSnapshot {
                            band_steering_status: Some(0),
                            configurations: configs,
                        })
                    }
                },
                |entry| {
                    store.borrow_mut().insert(entry.index, entry.clone());
                    async { Ok(()) }
                },
            ))
            .unwrap();

        assert_eq!(result["readback_verified"], true);
        for item in result["items"].as_array().unwrap() {
            assert_eq!(item["state"], "verified");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
