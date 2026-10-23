//! One bounded reconciliation transaction per scheduler tick.
use super::*;
use crate::provisioning::{bootstrap, link_identity, management_network, storage};

pub(super) fn tick(engine: &Arc<Mutex<crate::pon_engine::PonEngine>>) {
    // Release the optical lock before consulting ACS or the controller.
    let online_onus: BTreeMap<_, _> = engine
        .lock()
        .unwrap()
        .active_onus
        .iter()
        .filter(|o| o.state == crate::pon_engine::ploam::OnuState::O5Operation)
        .map(|o| (u16::from(o.onu_id), o.full_sn_str()))
        .collect();
    let online: BTreeSet<_> = online_onus.values().cloned().collect();
    let source_links = link_identity::discover(&online_onus);
    if let Ok(mut registry) = crate::mqtt_identity::shared().lock() {
        registry.update_topology(online_onus.clone(), source_links, now());
    }
    let devices = acs(json!({"op":"devices"}));
    let acs_status = acs(json!({"op":"status"}));
    let desired = controller()
        .lock()
        .unwrap()
        .config
        .management_network
        .clone();
    management_network::tick(desired.as_ref());
    let active_management = management_network::active();
    let mut c = controller().lock().unwrap();
    c.acs_status = acs_status;
    c.resolve_pending_identities(&devices);
    let config = c.config.clone();
    bootstrap::retain_devices(&config.devices.iter().map(|d| d.id.clone()).collect());
    let time = now();
    c.optical_epochs.retain(|serial, _| {
        online.contains(serial) && config.devices.iter().any(|d| &d.optical_sn == serial)
    });
    for (id, serial) in &online_onus {
        if !config.devices.iter().any(|d| &d.optical_sn == serial) {
            continue;
        }
        if c.optical_epochs
            .get(serial)
            .is_none_or(|(previous, _)| previous != id)
        {
            let epoch = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u64;
            c.optical_epochs.insert(serial.clone(), (*id, epoch));
        }
    }
    let mut fresh = BTreeSet::new();
    c.begin_history_tick();
    let mut history_changed = false;
    for d in &config.devices {
        if c.resets.blocks(&d.optical_sn) {
            continue;
        }
        let observed = &devices["devices"][&d.cwmp_id];
        let at = (observed["endpoint_id"] == c.acs_status["endpoint_id"]
            && c.acs_status["endpoint_id"]
                .as_str()
                .is_some_and(|id| !id.is_empty()))
        .then(|| observed["last_inform"].as_u64())
        .flatten();
        let reachable = online.contains(&d.optical_sn)
            && at.is_some_and(|at| {
                time.saturating_sub(at)
                    <= d.connection_request.effective_freshness(config.freshness)
            });
        c.management.insert(d.id.clone(), json!({"cwmp_id":d.cwmp_id,"state":if reachable {"online"} else if at.is_none() {"waiting_inform"} else {"offline_or_stale"},"optical_online":online.contains(&d.optical_sn),"last_inform":at,"age_seconds":at.map(|at| time.saturating_sub(at))}));
        if reachable {
            fresh.insert(d.id.clone());
        }
        let boot = observed["inform"]["events"].as_array().is_some_and(|a| {
            a.iter()
                .any(|e| matches!(e.as_str(), Some("0 BOOTSTRAP" | "1 BOOT")))
        });
        let new_boot = boot && at != c.last_boot.get(&d.id).copied();
        if boot {
            if let Some(at) = at {
                c.last_boot.insert(d.id.clone(), at);
            }
        }
        bootstrap::tick(bootstrap::Observation {
            device: d,
            management: active_management.as_ref(),
            last_inform: at,
            online: online.contains(&d.optical_sn),
            new_boot,
            freshness: d.connection_request.effective_freshness(config.freshness),
            time,
            acs_available: active_management.is_some()
                && c.acs_status["enabled"] == true
                && c.acs_status["error"].is_null(),
            authorization: management_network::grant(&d.id, &d.optical_sn),
        });
        let host_matches = config
            .management_network
            .as_ref()
            .is_none_or(management_network::matches_desired);
        let reachable = host_matches && reachable && bootstrap::is_ready(&d.id);
        if let Some(m) = c.management.get_mut(&d.id) {
            m["state"] = json!(if reachable {
                "online"
            } else if at.is_none() {
                "waiting_inform"
            } else {
                "offline_or_stale"
            });
        }
        if !reachable {
            fresh.remove(&d.id);
        }
        schedule(&mut c, d, reachable, new_boot, time);
        let capability_identity = identity(d, observed);
        use std::hash::{Hash, Hasher};
        let mut epoch_hash = std::collections::hash_map::DefaultHasher::new();
        (
            c.last_boot.get(&d.id).copied().unwrap_or(0),
            c.optical_epochs
                .get(&d.optical_sn)
                .map(|(_, epoch)| *epoch)
                .unwrap_or(0),
        )
            .hash(&mut epoch_hash);
        let capability_epoch = epoch_hash.finish();
        if let Some(op) = c.operations.get_mut(&d.id) {
            if op.stage == OperationPhase::Completed
                && (new_boot
                    || op
                        .capabilities
                        .observation_changed(&capability_identity, capability_epoch))
            {
                op.capabilities.invalidate("identity, firmware, optical registration or BOOTSTRAP changed; evidence is stale");
                history_changed = true;
            }
            if op.device.cwmp_id.starts_with("pending:") && !d.cwmp_id.starts_with("pending:") {
                history_changed = true;
                op.device.cwmp_id = d.cwmp_id.clone();
                match (wan::load(&d.cwmp_id), wifi::load(&d.cwmp_id)) {
                    (Ok(owned), Ok(registry)) => {
                        let cleanup = op.wan.full_sync();
                        op.wan =
                            wan::Reconciler::new(&op.device, owned.unwrap_or_default(), cleanup);
                        op.wifi = wifi::Reconciler::new(&op.device, registry, cleanup);
                    }
                    _ => {
                        op.complete(now());
                        op.wan.results.insert(
                            "identity".into(),
                            json!({"state":"blocked","reason":"cannot load device ownership"}),
                        );
                    }
                }
            }
            if op.stage != OperationPhase::Completed {
                if config.management_network.is_some() && !host_matches {
                    continue;
                }
                if !reconcile(op, capability_identity, capability_epoch, reachable) {
                    continue;
                }
            }
            if let Err(error) = op.wan.save(&op.device.cwmp_id) {
                history_changed = true;
                op.wan.results.insert(
                    "persistence".into(),
                    json!({"state":"blocked","reason":error}),
                );
            }
            if let Err(error) = op.wifi.save(&op.device.cwmp_id) {
                history_changed = true;
                op.wifi.results.insert(
                    "persistence".into(),
                    json!({"state":"blocked","reason":error}),
                );
                op.complete(now());
            }
            let wifi_owned = op.wifi.registry.clone();
            c.wifi_owned.insert(d.cwmp_id.clone(), wifi_owned);
            let owned = c.operations.get(&d.id).unwrap().wan.owned.clone();
            c.owned.insert(d.cwmp_id.clone(), owned);
        }
    }
    c.last_online = fresh;
    c.dirty |= history_changed;
    c.persist_history(|data| {
        storage::write_atomic("/var/lib/fttr/provisioning-state.json", data, false)
    });
}

fn schedule(c: &mut Controller, d: &Device, reachable: bool, new_boot: bool, time: u64) {
    let previous = c
        .operations
        .get(&d.id)
        .map(Operation::view)
        .or_else(|| c.recovered.get(&d.id).cloned());
    let needs_reconcile = previous.as_ref().is_some_and(|p| {
        p["reconcile_required"] == true
            || p["items"].as_array().is_some_and(|a| {
                a.iter()
                    .any(|i| ItemOutcome::from_result(i) == Some(ItemOutcome::ReconcileRequired))
            })
    });
    let needs_manual = previous.as_ref().is_some_and(|p| {
        (p["state"] == "interrupted" && p["reconcile_required"] != true)
            || ["wan_lifecycle", "wifi_lifecycle"].iter().any(|key| {
                p[*key].as_object().is_some_and(|m| {
                    m.values()
                        .any(|i| ItemOutcome::from_result(i) == Some(ItemOutcome::Unknown))
                })
            })
            || p["items"].as_array().is_some_and(|a| {
                a.iter().any(|i| {
                    matches!(
                        ItemOutcome::from_result(i),
                        Some(ItemOutcome::Unknown | ItemOutcome::AcceptedPendingApply)
                    )
                })
            })
    });
    let all_verified = previous.as_ref().is_some_and(|p| {
        p["state"] == "completed"
            && p["items"].as_array().is_some_and(|a| {
                !a.is_empty()
                    && a.iter()
                        .all(|i| ItemOutcome::from_result(i) == Some(ItemOutcome::Verified))
            })
    });
    if all_verified {
        c.attempts.remove(&d.id);
        c.next_retry_at.remove(&d.id);
    }
    let active_op = c.operations.get(&d.id).is_some_and(|o| {
        o.stage != OperationPhase::Completed && o.stage != OperationPhase::Interrupted
    });
    let startup_trigger = c.startup_trigger_pending(&d.id);
    let periodic = c.periodic_due(&d.id, time);
    if !active_op && reachable && c.config.enabled && c.config.automatic {
        if needs_reconcile {
            let _ = c.start_selected(&d.id, false, false, None);
        } else if !needs_manual
            && (startup_trigger
                || periodic
                || (c.config.triggers.contains("device_online") && !c.last_online.contains(&d.id))
                || (new_boot && c.config.triggers.contains("cwmp_boot")))
        {
            if c.start(&d.id, true, false).is_ok() && startup_trigger {
                c.startup_pending.remove(&d.id);
            }
        } else if !needs_manual
            && previous.as_ref().is_some_and(|p| {
                p["items"].as_array().is_some_and(|a| {
                    a.iter()
                        .any(|i| ItemOutcome::from_result(i) == Some(ItemOutcome::Drift))
                })
            })
        {
            let attempts = c.attempts.get(&d.id).copied().unwrap_or(0);
            let next = c.next_retry_at.get(&d.id).copied().unwrap_or(0);
            if attempts < 5 && now() >= next {
                let next_attempts = attempts + 1;
                c.attempts.insert(d.id.clone(), next_attempts);
                let delay = (15 * (1 << (next_attempts - 1))).min(300);
                c.next_retry_at.insert(d.id.clone(), now() + delay);
                let _ = c.start(&d.id, true, false);
            }
        }
    }
}

fn identity(d: &Device, observed: &Value) -> crate::devices::cwmp::identity::Identity {
    let version = |suffix: &str| {
        observed["inform"]["parameters"]
            .as_array()
            .and_then(|p| {
                p.iter()
                    .find(|p| p["name"].as_str().is_some_and(|n| n.ends_with(suffix)))
            })
            .and_then(|p| p["value"].as_str())
            .unwrap_or("")
            .to_owned()
    };
    crate::devices::cwmp::identity::Identity {
        optical_vendor: d.optical_sn[..4].into(),
        optical_serial: d.optical_sn.clone(),
        cwmp_serial: observed["serial"].as_str().unwrap_or("").into(),
        product_class: observed["product_class"].as_str().unwrap_or("").into(),
        software_version: version(".SoftwareVersion"),
        hardware_version: version(".HardwareVersion"),
    }
}

/// False retains the original tick's early-continue boundary (no ownership publication).
fn reconcile(
    op: &mut Operation,
    identity: crate::devices::cwmp::identity::Identity,
    epoch: u64,
    reachable: bool,
) -> bool {
    op.capabilities
        .tick(&op.device.cwmp_id, identity, epoch, reachable, acs);
    if op.capabilities.blocked() {
        let reason = op.capabilities.view()["reason"]
            .as_str()
            .unwrap_or("capability contract unresolved")
            .to_owned();
        for item in &op.device.items {
            op.results.entry(item.id.clone()).or_insert_with(|| json!({"id":item.id,"kind":item.kind,"service_type":item.service_type,"state":"blocked","reason":reason,"revision":op.revision}));
        }
        op.complete(now());
        return false;
    }
    if !op.capabilities.ready() {
        return false;
    }
    op.device.cwmp_contract = op.capabilities.selected().unwrap().to_owned();
    if !op.compiled {
        match op.capabilities.endpoints(&op.device) {
            Ok(endpoints) => {
                crate::devices::cwmp::compiler::compile_device(&mut op.device, &endpoints);
                let mut prepared = Operation::new(
                    op.id,
                    op.device.clone(),
                    op.revision.clone(),
                    op.apply,
                    op.management,
                );
                prepared.wan = wan::Reconciler::new(
                    &prepared.device,
                    op.wan.owned.clone(),
                    op.wan.full_sync(),
                );
                prepared.wifi = wifi::Reconciler::new(
                    &prepared.device,
                    op.wifi.registry.clone(),
                    op.wan.full_sync(),
                );
                op.device = prepared.device;
                op.wan = prepared.wan;
                op.wifi = prepared.wifi;
                op.compiled = true;
            }
            Err(reason) => {
                op.capabilities.invalidate(&reason);
                return false;
            }
        }
    }
    op.wan.protected_roots = op.capabilities.protected_wan();
    op.wan.evidence_paths = op.capabilities.wan_fields();
    let ready = op
        .wan
        .tick(&mut op.device, op.apply, op.management, reachable);
    if let Err(error) = op.wan.save(&op.device.cwmp_id) {
        op.wan.results.insert(
            "persistence".into(),
            json!({"state":"blocked","reason":error}),
        );
        op.complete(now());
    } else if ready {
        op.wan.block_unresolved(&mut op.device);
        let finish = op.index >= op.device.items.len();
        if finish
            && op
                .wan
                .results
                .get("final_inventory")
                .is_some_and(|r| ItemOutcome::from_result(r) == Some(ItemOutcome::Blocked))
        {
            for item in op.device.items.iter().filter(|i| i.wan_ref.is_some()) {
                if let Some(result) = op.results.get_mut(&item.id) {
                    result["state"] = json!("blocked");
                    result["reason"] =
                        json!("final full WAN inventory does not match ordered configuration");
                }
            }
        }
        let verified = op
            .device
            .items
            .iter()
            .filter(|i| i.kind == ItemKind::Ssid)
            .all(|i| {
                op.results
                    .get(&i.id)
                    .is_some_and(|r| ItemOutcome::from_result(r) == Some(ItemOutcome::Verified))
            });
        if finish && op.wan.begin_final_verification(op.apply) {
            op.stage = OperationPhase::WanVerifying;
        } else if op
            .wifi
            .tick(&op.device, op.apply, reachable, finish, verified)
        {
            if finish {
                op.complete(now());
            } else {
                op.wifi.block_unresolved(&mut op.device);
                if matches!(
                    op.stage,
                    OperationPhase::WanReconciling | OperationPhase::WifiReconciling
                ) {
                    op.stage = OperationPhase::Waiting;
                }
                advance(op, reachable);
            }
        } else {
            op.stage = OperationPhase::WifiReconciling;
        }
        for item in &op.device.items {
            if op.results.get(&item.id).is_some_and(|r| {
                ItemOutcome::from_result(r) == Some(ItemOutcome::Verified)
                    && r["revision"] == op.revision
            }) {
                if let Some(owned) = op.wan.owned.get_mut(&item.id) {
                    if let Some(p) = item.parameters.iter().find(|p| {
                        p.name.ends_with("ServiceList") || p.name.ends_with("SERVICELIST")
                    }) {
                        owned.service_list = p.value.clone();
                    }
                }
            }
        }
    } else {
        op.stage = OperationPhase::WanReconciling;
    }
    true
}
