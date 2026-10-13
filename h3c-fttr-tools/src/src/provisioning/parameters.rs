//! Parameter discovery, diff application and readback. This is a child of the
//! controller so execution can mutate its operation without exposing internals.
use super::{acs, now, Item, ItemKind, Operation, OperationPhase, Parameter, ParameterKind};
use serde_json::{json, Value};
use std::collections::BTreeSet;

fn queue(op: &mut Operation, method: &str, args: Value, stage: OperationPhase) {
    let reply = acs(json!({"op":"queue","device":op.device.cwmp_id,"method":method,"args":args}));
    if let Some(id) = reply["id"].as_u64() {
        op.task = Some(id);
        op.stage = stage;
    } else {
        op.finish_item(
            "failed",
            reply["error"].as_str().unwrap_or("cannot queue RPC"),
        );
    }
}
fn roots(item: &Item) -> Vec<String> {
    let mut roots = BTreeSet::new();
    for p in &item.parameters {
        let parts: Vec<_> = p.name.split('.').collect();
        let count =
            if p.name.contains("WANConnectionDevice.") || p.name.contains("WLANConfiguration.") {
                5
            } else {
                2
            };
        roots.insert(format!("{}.", parts[..count].join(".")));
    }
    roots.into_iter().collect()
}
pub(super) fn advance(op: &mut Operation, reachable: bool) {
    if op.stage == OperationPhase::Settling {
        if reachable && now() >= op.verify_after {
            queue_read(op, OperationPhase::Verifying);
        }
        return;
    }
    if op.stage == OperationPhase::Waiting {
        if now().saturating_sub(op.started) > 900 {
            op.finish_item(
                "failed",
                "operation deadline exceeded while waiting for management session",
            );
            return;
        }
        if !reachable {
            return;
        }
        if acs(json!({"op":"busy","device":op.device.cwmp_id}))["busy"] == true {
            return;
        }
        if op.apply
            && matches!(
                op.item().kind,
                ItemKind::PortBinding | ItemKind::WanActivation
            )
        {
            if let Some(service) = &op.item().wan_ref {
                if !op
                    .results
                    .get(service)
                    .is_some_and(|r| r["state"] == "verified" && r["revision"] == op.revision)
                {
                    op.finish_item("blocked","verify the referenced WAN service at the current revision before applying bindings");
                    return;
                }
            }
            if op.item().kind == ItemKind::WanActivation
                && op
                    .device
                    .items
                    .iter()
                    .filter(|i| i.kind == ItemKind::PortBinding && i.wan_ref == op.item().wan_ref)
                    .any(|i| {
                        !op.results.get(&i.id).is_some_and(|r| {
                            r["state"] == "verified" && r["revision"] == op.revision
                        })
                    })
            {
                op.finish_item(
                    "blocked",
                    "WAN activation requires verified bindings at the current revision",
                );
                return;
            }
        }
        if let Some(reason) = op.item().blocked.clone() {
            op.finish_item("blocked", &reason);
            return;
        }
        op.roots = roots(op.item());
        let root = op.roots.remove(0);
        queue(
            op,
            "GetParameterNames",
            json!({"path":root,"next_level":false}),
            OperationPhase::Discovering,
        );
        return;
    }
    let Some(id) = op.task else {
        return;
    };
    let task = acs(json!({"op":"task","id":id}));
    let state = task["state"].as_str().unwrap_or("missing");
    if matches!(state, "queued" | "sent") {
        return;
    }
    if op.stage == OperationPhase::Disabling {
        if matches!(state, "completed" | "accepted_pending_apply" | "unknown") {
            queue_read(op, OperationPhase::DisablingRead);
        } else {
            op.finish_item("failed", "cannot disable WAN before configuration");
        }
        return;
    }
    if op.stage == OperationPhase::Writing {
        if matches!(state, "completed" | "accepted_pending_apply" | "unknown") {
            op.uncertain = state == "unknown";
            op.pending = state == "accepted_pending_apply";
            op.written = state != "unknown";
            let delay = op.device.contract().requirements.settle_seconds;
            if delay == 0 {
                queue_read(op, OperationPhase::Verifying);
            } else {
                op.verify_after = now() + delay;
                op.task = None;
                op.stage = OperationPhase::Settling;
            }
        } else {
            // HL202 rejects the standard PreSharedKey.1.KeyPassphrase on some
            // WLAN instances (fault 9007) but accepts the vendor's top-level
            // KeyPassphrase parameter on the same object.
            let parameter_faults: Vec<String> = task["result"]["parameters"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter(|p| p["fault_code"].as_str().is_some_and(|f| !f.is_empty()))
                        .filter_map(|p| p["name"].as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            if op.device.contract().wlan.passphrase_fallback
                && !op.passphrase_fallback
                && !parameter_faults.is_empty()
                && parameter_faults
                    .iter()
                    .all(|n| n.ends_with(".PreSharedKey.1.KeyPassphrase"))
            {
                op.passphrase_fallback = true;
                op.task = None;
                op.stage = OperationPhase::Waiting;
                let root = parameter_faults[0]
                    .trim_end_matches("PreSharedKey.1.KeyPassphrase")
                    .to_string();
                for p in &mut op.changes {
                    if p.name.ends_with(".PreSharedKey.1.KeyPassphrase") {
                        p.name = format!("{root}KeyPassphrase");
                    }
                }
                let parameters: Vec<_> = op
                    .changes
                    .iter()
                    .map(|p| json!({"name":p.name,"value":p.value,"kind":p.kind}))
                    .collect();
                queue(
                    op,
                    "SetParameterValues",
                    json!({"parameters":parameters}),
                    OperationPhase::Writing,
                );
                return;
            }
            op.capabilities
                .invalidate("parameter write fault invalidated the capability contract");
            op.finish_item(
                "failed",
                "SetParameterValues failed; inspect ACS task for fault",
            );
        }
        return;
    }
    if state != "completed" {
        op.capabilities
            .invalidate("parameter discovery/read fault invalidated the capability contract");
        op.finish_item(
            "failed",
            &format!(
                "{}: {state}, fault {}",
                op.stage,
                task["result"]["fault_code"].as_str().unwrap_or("")
            ),
        );
        return;
    }
    let values = task["result"]["parameters"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if op.stage == OperationPhase::Discovering {
        for p in &values {
            if matches!(p["writable"].as_str(), Some("1" | "true")) {
                if let Some(name) = p["name"].as_str() {
                    op.writable.insert(name.into());
                }
            }
        }
        if !op.roots.is_empty() {
            let root = op.roots.remove(0);
            queue(
                op,
                "GetParameterNames",
                json!({"path":root,"next_level":false}),
                OperationPhase::Discovering,
            );
            return;
        }
        let missing: Vec<_> = op
            .item()
            .parameters
            .iter()
            .filter(|p| {
                !(op.writable.contains(&p.name)
                    || op.device.contract().wlan.nat_writable_exception
                        && p.name.ends_with(".NATEnabled"))
            })
            .map(|p| p.name.clone())
            .collect();
        if !missing.is_empty() {
            op.finish_item(
                "blocked",
                &format!("parameters missing or not writable: {}", missing.join(",")),
            );
            return;
        }
        queue_read(op, OperationPhase::Reading);
        return;
    }
    let item = op.item().clone();
    if op.stage == OperationPhase::DisablingRead {
        let disabled = item
            .parameters
            .iter()
            .filter(|p| p.name.ends_with(".Enable"))
            .all(|p| {
                values.iter().any(|v| {
                    v["name"] == p.name && v["value"].as_str().is_some_and(|v| equivalent(p, v))
                })
            });
        if !disabled {
            op.finish_item(
                "unknown",
                "WAN disable did not read back; further writes withheld",
            );
            return;
        }
        op.stage = OperationPhase::Reading;
    }
    if let Some(guard) = &item.guard {
        if let Some((root, _)) = guard.rsplit_once('.') {
            if crate::devices::cwmp::observed::protected(&values, &format!("{root}.")) {
                op.finish_item(
                    "blocked",
                    "cross-contract readback identifies a management/fallback WAN",
                );
                return;
            }
        }
        let observed = values
            .iter()
            .find(|p| p["name"] == *guard)
            .and_then(|p| p["value"].as_str());
        let Some(service) = observed else {
            op.finish_item("blocked", "service list missing in readback");
            return;
        };
        if item.service_type != "tr069"
            && crate::provisioning::contains_service_token(service, "TR069")
        {
            op.finish_item(
                "blocked",
                "WAN carries TR069; dedicated business object required",
            );
            return;
        }
        if crate::provisioning::contains_service_token(service, "INTERNET")
            && guard.contains(".WANConnectionDevice.")
        {
            let contract = op.device.contract().wan;
            let enabled = contract.tagged(|name| {
                values
                    .iter()
                    .find(|p| {
                        p["name"]
                            .as_str()
                            .is_some_and(|n| n.ends_with(&format!(".{name}")))
                    })
                    .and_then(|p| p["value"].as_str())
                    .map(str::to_owned)
            });
            let allocated = item
                .wan_ref
                .as_ref()
                .and_then(|id| op.wan.owned.get(id))
                .is_some_and(|o| o.baseline.is_some());
            if enabled.is_none() || (enabled == Some(false) && !allocated) {
                op.finish_item(
                    "blocked",
                    "WAN became an untagged fallback; business writes withheld",
                );
                return;
            }
        }
        if item.service_type == "tr069" {
            op.finish_item("blocked", "TR069 is owned by OMCI");
            return;
        }
    }
    let mut changes = Vec::new();
    for p in &item.parameters {
        // Credential parameters are write-only on the supported ONUs. A
        // successful SetParameterValues is sufficient; never fail an item
        // because CWMP cannot return the password.
        if op.stage == OperationPhase::Verifying && p.secret {
            continue;
        }
        let value = values
            .iter()
            .find(|v| v["name"] == p.name)
            .and_then(|v| v["value"].as_str());
        if op.stage == OperationPhase::Reading && p.secret && value.is_none_or(str::is_empty) {
            changes.push(p.clone());
            continue;
        }
        if value.is_none() {
            op.finish_item("failed", "incomplete parameter readback");
            return;
        }
        if !equivalent(p, value.unwrap()) {
            changes.push(p.clone());
        }
    }
    if op.stage == OperationPhase::Verifying {
        if changes.is_empty() {
            op.finish_item("verified","readable CWMP parameters match; credential write was accepted (password readback is write-only)");
        } else if op.pending {
            op.finish_item(
                "accepted_pending_apply",
                "device returned Status=1; readback still differs",
            );
        } else if op.uncertain {
            op.finish_item(
                "unknown",
                "write outcome unknown; readback differs, no automatic replay",
            );
        } else {
            op.finish_item("failed", "SetParameterValues accepted but readback differs");
        }
        return;
    }
    if matches!(item.kind, ItemKind::DeviceService | ItemKind::PortBinding)
        && changes.iter().all(|p| p.name.ends_with(".Enable"))
        && op
            .device
            .items
            .iter()
            .any(|i| i.kind == ItemKind::WanActivation && i.wan_ref == item.wan_ref)
    {
        changes.clear();
    }
    if item.kind == ItemKind::DeviceService
        && op.device.contract().wlan.nat_writable_exception
        && changes.iter().any(|p| p.name.ends_with(".NATEnabled"))
    {
        if !changes
            .iter()
            .any(|p| p.name.ends_with(".ConnectionType") && p.value == "IP_Bridged")
        {
            op.finish_item(
                "blocked",
                "bridge NAT readback differs; independent NAT writes have not been verified",
            );
            return;
        }
        changes.retain(|p| !p.name.ends_with(".NATEnabled"));
    }
    op.changes = changes;
    if op.changes.is_empty() {
        op.finish_item("verified", "already matches desired state");
        return;
    }
    if !op.apply {
        op.finish_item("drift", "read-only plan: parameters differ");
        return;
    }
    if item.service_type == "tr069" {
        op.finish_item("blocked", "TR069 is owned by OMCI");
        return;
    }
    if op.device.contract().requirements.disable_wan_before_write
        && matches!(item.kind, ItemKind::DeviceService | ItemKind::PortBinding)
    {
        if let Some(enable) = op
            .changes
            .iter()
            .find(|p| p.name.ends_with(".Enable") && matches!(p.value.as_str(), "0" | "false"))
        {
            let parameter = json!({"name":enable.name,"value":enable.value,"kind":enable.kind});
            queue(
                op,
                "SetParameterValues",
                json!({"parameters":[parameter]}),
                OperationPhase::Disabling,
            );
            return;
        }
    }
    let parameters: Vec<_> = op
        .changes
        .iter()
        .map(|p| json!({"name":p.name,"value":p.value,"kind":p.kind}))
        .collect();
    queue(
        op,
        "SetParameterValues",
        json!({"parameters":parameters}),
        OperationPhase::Writing,
    );
}
fn queue_read(op: &mut Operation, stage: OperationPhase) {
    let mut names: Vec<_> = op
        .item()
        .parameters
        .iter()
        .map(|p| p.name.clone())
        .collect();
    if let Some(guard) = op
        .item()
        .guard
        .as_ref()
        .filter(|g| g.contains(".WANConnectionDevice."))
    {
        let contract = op.device.contract().wan;
        if let Some((root, _)) = guard.rsplit_once('.') {
            if let Some(extra) = op.capabilities.wan_fields().get(&format!("{root}.")) {
                names.extend(extra.iter().cloned());
            }
        }
        let field = contract.enabled.unwrap_or(contract.vlan);
        if field.location == crate::devices::cwmp::contract::Location::Parent {
            if let Some((parent, _)) = guard.split_once(".WANIPConnection.") {
                names.push(format!("{parent}.{}", field.name));
            }
        } else if let Some((root, _)) = guard.rsplit_once('.') {
            names.push(format!("{root}.{}", field.name));
        }
    }
    if let Some(g) = &op.item().guard {
        if !names.contains(g) {
            names.push(g.clone());
        }
    }
    names.sort();
    names.dedup();
    queue(op, "GetParameterValues", json!({"names":names}), stage);
}
pub(super) fn equivalent(p: &Parameter, observed: &str) -> bool {
    if p.kind == ParameterKind::Boolean {
        return matches!(
            (p.value.as_str(), observed),
            ("1", "true" | "1") | ("0", "false" | "0")
        );
    }
    for contract in crate::devices::cwmp::registry::available() {
        for field in [
            Some(contract.wan.service),
            Some(contract.wan.vlan),
            Some(contract.wan.priority),
            contract.wan.enabled,
            contract.wan.binding,
            contract.wan.cloud_cpe,
        ]
        .into_iter()
        .flatten()
        {
            if p.name.ends_with(&format!(".{}", field.name)) {
                return crate::devices::cwmp::contract::equivalent_field(field, &p.value, observed);
            }
        }
    }
    p.value == observed
}
