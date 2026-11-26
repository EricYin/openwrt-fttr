//! HL202 native tcapi contract. No CWMP paths or instance numbers cross this boundary.
use super::{command::CommandStatus, command_binding};
use crate::{
    mqtt_identity::Selection,
    provisioning::{model::*, storage},
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

type Fields = BTreeMap<(String, String), String>;
#[derive(Clone, Default)]
struct Inventory {
    fields: Fields,
    parents: BTreeSet<u32>,
    children: BTreeSet<(u32, u32)>,
}
#[derive(Clone, Default, Serialize, Deserialize)]
struct Ownership {
    #[serde(default)]
    revision: String,
    #[serde(default)]
    operation_id: u64,
    #[serde(default)]
    binding_generation: String,
    #[serde(default)]
    wan: BTreeMap<String, Owned>,
    #[serde(default)]
    pending: bool,
}
#[derive(Clone, Serialize, Deserialize)]
struct Owned {
    parent: u32,
    child: u32,
    service: String,
    vlan: String,
}
struct Desired {
    fields: Fields,
    items: BTreeMap<String, BTreeSet<(String, String)>>,
    owned: Ownership,
    removed: BTreeSet<(u32, u32)>,
}

fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

async fn execute(selection: &Selection, script: &str) -> Result<String, String> {
    execute_once(selection, script)
        .await
        .map_err(|(reason, _)| reason)
}

async fn execute_once(selection: &Selection, script: &str) -> Result<String, (String, bool)> {
    let reply = command_binding::execute_async(selection, script, Duration::from_secs(90))
        .await
        .map_err(|_| {
            (
                "command transport failed; rediscovery required".into(),
                false,
            )
        })?;
    if !reply.binding_current {
        return Err((
            "command identity changed; rediscovery required".into(),
            false,
        ));
    }
    if reply.result.status == CommandStatus::Timeout {
        return Err((
            "command response timed out; execution outcome unknown".into(),
            true,
        ));
    }
    if reply.result.status != CommandStatus::Executed || reply.result.exit_code != Some(0) {
        return Err((format!("command did not complete successfully (status={:?}, exit={:?}); rediscovery required", reply.result.status, reply.result.exit_code), false));
    }
    if reply.result.output_truncated {
        return Err((
            "command output truncated; rediscovery required".into(),
            false,
        ));
    }
    reply
        .result
        .output
        .ok_or(("command returned no framed output".into(), false))
}

async fn initial_inventory_with_retry<F, Fut, Check>(
    mut read: F,
    check_auth: Check,
    retry_delay: Duration,
) -> Result<String, String>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<String, (String, bool)>>,
    Check: Fn() -> Result<(), String>,
{
    match read().await {
        Err((_, true)) => {
            tokio::time::sleep(retry_delay).await;
            check_auth()?;
            read().await.map_err(|(reason, _)| reason)
        }
        Err((reason, _)) => Err(reason),
        Ok(text) => Ok(text),
    }
}

const INVENTORY: &str = r#"read_fields() {
 n=$1; shift
 for k in "$@"; do
  v=$(tcapi get "$n" "$k") || exit 1
  printf 'V|%s|%s|' "$n" "$k"
  printf '%s' "$v" | od -An -v -tx1 | tr -d ' \n'
  printf '\n'
 done
}
root=$(tcapi show Wan) || exit 1
printf '%s\n' "$root" | grep -q '\[Wan\]:' || exit 1
parents=$(printf '%s\n' "$root" | awk '/\[PVC\]:/ { split($2,a,":"); gsub(/[^0-9]/,"",a[2]); print a[2]-1 }')
for p in $parents; do
 parent=Wan_PVC${p}
 printf 'P|%s\n' "$p"
 read_fields "$parent" VLANMode VLANID DOT1P EPONEnable GPONEnable
 tree=$(tcapi show "$parent") || exit 1
 children=$(printf '%s\n' "$tree" | awk '/\[Entry\]:/ { split($2,a,":"); gsub(/[^0-9]/,"",a[2]); print a[2]-1 }')
 for c in $children; do
  printf 'C|%s|%s\n' "$p" "$c"
   read_fields "${parent}_Entry${c}" ServiceList Active WanMode ISP VLANMode VLANID dot1q dot1p dot1pData IPVERSION LinkMode BridgeMode NATENABLE LAN1 LAN2 LAN3 LAN4 SSID1 SSID2 SSID3 SSID4 SSIDAC1 SSIDAC2 SSIDAC3 SSIDAC4
   interface=$(tcapi get "${parent}_Entry${c}" IFName)
   runtime=0
   case "$interface" in nas[0-7]_[0-7]) [ -d "/sys/class/net/$interface" ] && runtime=1 ;; esac
   printf 'V|%s|__runtime_present|3%s\n' "${parent}_Entry${c}" "$runtime"
 done
done
for radio in WLan WLan11ac; do
 read_fields "${radio}_Common" APOn BssidNum
 for slot in 0 1 2 3; do
   read_fields "${radio}_Entry${slot}" SSID EnableSSID HideSSID AuthMode EncrypType WPAPSK PMFMFPC PMFMFPR PMFSHA256
 done
done
printf 'FTTR_INVENTORY_COMPLETE\n'"#;

fn parse(text: &str) -> Result<Inventory, String> {
    let mut inventory = Inventory::default();
    let mut complete = false;
    for line in text.lines() {
        if complete {
            return Err("data after inventory completion".into());
        }
        let parts: Vec<_> = line.split('|').collect();
        match parts.as_slice() {
            ["P", p] => {
                let p = p.parse::<u32>().map_err(|_| "invalid parent")?;
                if p > 7 || !inventory.parents.insert(p) {
                    return Err("duplicate/out of range parent".into());
                }
            }
            ["C", p, c] => {
                let pair = (
                    p.parse::<u32>().map_err(|_| "invalid parent")?,
                    c.parse::<u32>().map_err(|_| "invalid child")?,
                );
                if pair.0 > 7 || pair.1 > 7 || !inventory.children.insert(pair) {
                    return Err("duplicate/out of range child".into());
                }
            }
            ["V", node, key, hex] => {
                if hex.len() % 2 != 0 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err("invalid field encoding".into());
                }
                let bytes = (0..hex.len())
                    .step_by(2)
                    .map(|i| u8::from_str_radix(&hex[i..i + 2], 16))
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|_| "invalid field encoding")?;
                let value = String::from_utf8(bytes).map_err(|_| "invalid field text")?;
                if inventory
                    .fields
                    .insert((node.to_string(), key.to_string()), value)
                    .is_some()
                {
                    return Err("duplicate field".into());
                }
            }
            ["FTTR_INVENTORY_COMPLETE"] if !complete => complete = true,
            _ => return Err("unexpected or incomplete inventory".into()),
        }
    }
    if !complete
        || inventory
            .children
            .iter()
            .any(|(p, _)| !inventory.parents.contains(p))
    {
        return Err("incomplete inventory".into());
    }
    for radio in ["WLan", "WLan11ac"] {
        if get(&inventory, &format!("{radio}_Common"), "BssidNum") != "4" {
            return Err("unverified Wi-Fi slot layout".into());
        }
    }
    // A terminator alone does not prove every declared object was read. Missing
    // fields must never be interpreted as an absent WAN or an empty credential.
    let mut expected = BTreeSet::new();
    let mut require = |node: String, keys: &str| {
        expected.extend(
            keys.split_whitespace()
                .map(|key| (node.clone(), key.into())),
        );
    };
    for p in &inventory.parents {
        require(
            format!("Wan_PVC{p}"),
            "VLANMode VLANID DOT1P EPONEnable GPONEnable",
        );
    }
    for (p, c) in &inventory.children {
        require(format!("Wan_PVC{p}_Entry{c}"), "ServiceList Active WanMode ISP VLANMode VLANID dot1q dot1p dot1pData IPVERSION LinkMode BridgeMode NATENABLE LAN1 LAN2 LAN3 LAN4 SSID1 SSID2 SSID3 SSID4 SSIDAC1 SSIDAC2 SSIDAC3 SSIDAC4 __runtime_present");
    }
    for radio in ["WLan", "WLan11ac"] {
        require(format!("{radio}_Common"), "APOn BssidNum");
        for slot in 0..4 {
            require(
                format!("{radio}_Entry{slot}"),
                "SSID EnableSSID HideSSID AuthMode EncrypType WPAPSK PMFMFPC PMFMFPR PMFSHA256",
            );
        }
    }
    if expected.len() != inventory.fields.len()
        || expected
            .iter()
            .any(|key| !inventory.fields.contains_key(key))
    {
        return Err("incomplete or out of scope inventory fields".into());
    }
    if inventory.children.iter().any(|(p, c)| {
        !matches!(
            get(
                &inventory,
                &format!("Wan_PVC{p}_Entry{c}"),
                "__runtime_present"
            ),
            "0" | "1"
        )
    }) {
        return Err("invalid runtime presence".into());
    }
    Ok(inventory)
}
fn get<'a>(inv: &'a Inventory, node: &str, key: &str) -> &'a str {
    inv.fields
        .get(&(node.into(), key.into()))
        .map(String::as_str)
        .unwrap_or("")
}
fn protected(service: &str) -> bool {
    service.split(['_', ',', ' ']).any(|s| s == "TR069")
}
fn wlan(endpoint: &EndpointRef) -> Result<String, String> {
    match endpoint {
        EndpointRef::Ssid { radio, slot } if matches!(radio, 2 | 5) && (1..=4).contains(slot) => {
            Ok(format!(
                "{}_Entry{}",
                if *radio == 2 { "WLan" } else { "WLan11ac" },
                slot - 1
            ))
        }
        _ => Err("unsupported native Wi-Fi endpoint".into()),
    }
}
fn binding(endpoint: &EndpointRef) -> Result<String, String> {
    match endpoint {
        EndpointRef::Lan(n) if (1..=4).contains(n) => Ok(format!("LAN{n}")),
        EndpointRef::Ssid { radio, slot } if matches!(radio, 2 | 5) && (1..=4).contains(slot) => {
            Ok(format!("SSID{}{slot}", if *radio == 2 { "" } else { "AC" }))
        }
        _ => Err("unsupported native binding endpoint".into()),
    }
}
impl Desired {
    fn set(&mut self, id: &str, node: &str, key: &str, value: impl ToString) {
        let field = (node.to_string(), key.to_string());
        self.items
            .entry(id.into())
            .or_default()
            .insert(field.clone());
        self.fields.insert(field, value.to_string());
    }
}

fn compile(
    device: &Device,
    inv: &Inventory,
    owned: Ownership,
    full: bool,
) -> Result<Desired, String> {
    let mut desired = Desired {
        fields: Fields::new(),
        items: BTreeMap::new(),
        owned,
        removed: BTreeSet::new(),
    };
    let mut wifi_nodes = BTreeSet::new();
    let mut desired_wan = BTreeSet::new();
    let mut selected_parents = BTreeSet::new();
    for item in &device.items {
        if let Some(reason) = &item.blocked {
            return Err(reason.clone());
        }
        desired.items.entry(item.id.clone()).or_default();
        if let Some(wifi) = &item.wlan_intent {
            let node = wlan(&wifi.endpoint)?;
            if !wifi_nodes.insert(node.clone()) {
                return Err("duplicate desired Wi-Fi endpoint".into());
            }
            desired.set(&item.id, &node, "SSID", &wifi.ssid);
            if let Some(enabled) = wifi.enabled {
                desired.set(&item.id, &node, "EnableSSID", u8::from(enabled));
            }
            if wifi.enabled != Some(false) {
                desired.set(
                    &item.id,
                    &format!("{}_Common", node.split('_').next().unwrap()),
                    "APOn",
                    "1",
                );
            }
            if let Some(broadcast) = wifi.broadcast {
                desired.set(&item.id, &node, "HideSSID", u8::from(!broadcast));
            }
            if let Some(security) = wifi.security {
                let (auth, encrypt, capable, required) = match security {
                    WifiSecurity::None => ("OPEN", "NONE", "0", "0"),
                    WifiSecurity::Wpa2Psk => ("WPA2PSK", "AES", "0", "0"),
                    WifiSecurity::WpaPskWpa2Psk => ("WPAPSKWPA2PSK", "TKIPAES", "0", "0"),
                    WifiSecurity::Wpa3Sae => ("WPA3PSK", "AES", "1", "1"),
                    WifiSecurity::Wpa2PskWpa3Sae => ("WPA2PSKWPA3PSK", "AES", "1", "0"),
                };
                desired.set(&item.id, &node, "AuthMode", auth);
                desired.set(&item.id, &node, "EncrypType", encrypt);
                desired.set(&item.id, &node, "PMFMFPC", capable);
                desired.set(&item.id, &node, "PMFMFPR", required);
                desired.set(&item.id, &node, "PMFSHA256", "0");
            }
            if let Some(key) = &wifi.key {
                desired.set(&item.id, &node, "WPAPSK", key);
            }
        } else if let Some(wan) = &item.wan_intent {
            if wan.services.is_empty()
                || wan
                    .services
                    .iter()
                    .any(|s| protected(s) || !s.bytes().all(|b| b.is_ascii_alphanumeric()))
            {
                return Err("invalid or protected WAN service".into());
            }
            if wan.mode != ConnectionMode::Bridge {
                return Err("native routed business WAN contract not verified".into());
            }
            desired_wan.insert(item.id.clone());
            let service = wan.services.join("_");
            let (vlan, mode) = match wan.vlan {
                VlanMode::Untagged => ("4097".to_string(), "TRANSPARENT"),
                VlanMode::Tagged(v) => (v.to_string(), "TAG"),
            };
            let previous = desired.owned.wan.get(&item.id).cloned();
            let candidates: Vec<_> = inv
                .children
                .iter()
                .copied()
                .filter(|(p, c)| {
                    let node = format!("Wan_PVC{p}_Entry{c}");
                    get(inv, &node, "ServiceList") == service
                        && get(inv, &format!("Wan_PVC{p}"), "VLANID") == vlan
                        && get(inv, &node, "WanMode") == "Bridge"
                })
                .collect();
            let pair = if let Some(previous) =
                previous.filter(|p| inv.children.contains(&(p.parent, p.child)))
            {
                let node = format!("Wan_PVC{}_Entry{}", previous.parent, previous.child);
                if get(inv, &node, "ServiceList") != previous.service
                    || get(inv, &format!("Wan_PVC{}", previous.parent), "VLANID") != previous.vlan
                {
                    return Err("owned WAN identity changed; refusing overwrite".into());
                }
                (previous.parent, previous.child)
            } else {
                match candidates.as_slice() {
                    [pair] => *pair,
                    [] => (
                        (0..8)
                            .find(|p| !inv.parents.contains(p) && !selected_parents.contains(p))
                            .ok_or("no free dedicated WAN parent")?,
                        0,
                    ),
                    _ => return Err("multiple matching business WANs; ambiguous ownership".into()),
                }
            };
            let (p, c) = pair;
            if !selected_parents.insert(p)
                || inv
                    .children
                    .iter()
                    .any(|(other, child)| *other == p && *child != c)
            {
                return Err("shared WAN parent is protected from business writes".into());
            }
            let parent = format!("Wan_PVC{p}");
            let node = format!("{parent}_Entry{c}");
            if protected(get(inv, &node, "ServiceList")) {
                return Err("management WAN is protected".into());
            }
            desired.set(&item.id, &parent, "VLANMode", mode);
            desired.set(&item.id, &parent, "VLANID", &vlan);
            desired.set(&item.id, &parent, "DOT1P", wan.priority);
            desired.set(&item.id, &parent, "EPONEnable", "Yes");
            desired.set(&item.id, &parent, "GPONEnable", "Yes");
            for (key, value) in [
                ("ServiceList", service.as_str()),
                ("WanMode", "Bridge"),
                ("LinkMode", "linkIP"),
                ("ISP", "3"),
                ("BridgeMode", "IP_Bridged"),
                ("NATENABLE", "Disabled"),
                ("IPVERSION", "IPv4/IPv6"),
                ("VLANMode", if mode == "TAG" { "TAG" } else { "UNTAG" }),
                ("dot1q", if mode == "TAG" { "Yes" } else { "No" }),
                ("dot1p", if mode == "TAG" { "Yes" } else { "No" }),
            ] {
                desired.set(&item.id, &node, key, value);
            }
            if mode == "TAG" {
                desired.set(&item.id, &node, "VLANID", &vlan);
                desired.set(&item.id, &node, "dot1pData", wan.priority);
            }
            desired.set(
                &item.id,
                &node,
                "Active",
                if wan.enabled { "Yes" } else { "No" },
            );
            let mut bindings = wan
                .bindings
                .iter()
                .map(binding)
                .collect::<Result<BTreeSet<_>, _>>()?;
            for related in device.items.iter().filter(|related| {
                related.binding_intent.is_some() && related.wan_ref == item.wan_ref
            }) {
                for endpoint in &related.binding_intent.as_ref().unwrap().endpoints {
                    bindings.insert(binding(endpoint)?);
                }
            }
            for prefix in ["LAN", "SSID", "SSIDAC"] {
                for n in 1..=4 {
                    let key = format!("{prefix}{n}");
                    desired.set(
                        &item.id,
                        &node,
                        &key,
                        if bindings.contains(&key) { "Yes" } else { "No" },
                    );
                }
            }
            desired.owned.wan.insert(
                item.id.clone(),
                Owned {
                    parent: p,
                    child: c,
                    service,
                    vlan,
                },
            );
        } else if item.binding_intent.is_some() {
            // Native bindings are part of the WAN transaction, never a second write.
        } else {
            return Err("item has no supported native business intent".into());
        }
    }
    for item in device.items.iter().filter(|i| i.binding_intent.is_some()) {
        let service = device
            .items
            .iter()
            .find(|s| s.wan_intent.is_some() && s.wan_ref == item.wan_ref)
            .ok_or("native binding has no matching desired WAN")?;
        desired
            .items
            .insert(item.id.clone(), desired.items[&service.id].clone());
    }
    if full {
        if device.wifi_cleanup == "delete" {
            return Err(
                "native fixed Wi-Fi slots cannot be deleted; select disable or preserve".into(),
            );
        }
        if device.wifi_cleanup == "disable" {
            for (band, radio) in [2, 5].into_iter().enumerate() {
                for slot in 1..=4 {
                    let node = wlan(&EndpointRef::Ssid { radio, slot })?;
                    if !wifi_nodes.contains(&node)
                        && !device.wifi_preserve.contains(&(band as u32 * 4 + slot))
                    {
                        desired.set("wifi_cleanup", &node, "EnableSSID", "0");
                    }
                }
            }
        }
        for (id, old) in desired.owned.wan.clone() {
            if desired_wan.contains(&id) {
                continue;
            }
            if inv.children.contains(&(old.parent, old.child)) {
                let node = format!("Wan_PVC{}_Entry{}", old.parent, old.child);
                if protected(get(inv, &node, "ServiceList"))
                    || get(inv, &node, "ServiceList") != old.service
                    || get(inv, &format!("Wan_PVC{}", old.parent), "VLANID") != old.vlan
                    || inv
                        .children
                        .iter()
                        .any(|(p, c)| *p == old.parent && *c != old.child)
                {
                    return Err(
                        "stale owned WAN changed or shares a parent; refusing removal".into(),
                    );
                }
                desired.removed.insert((old.parent, old.child));
            } else {
                desired.owned.wan.remove(&id);
            }
        }
    }
    Ok(desired)
}

fn differences(desired: &Desired, actual: &Inventory) -> Fields {
    desired
        .fields
        .iter()
        .filter(|(key, value)| actual.fields.get(*key) != Some(*value))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}
fn write_script(diff: &Fields, removed: &BTreeSet<(u32, u32)>) -> String {
    let mut script = String::from("set -e\n");
    let mut roots = BTreeSet::new();
    let wan_nodes: BTreeSet<_> = diff
        .keys()
        .filter(|(n, _)| n.starts_with("Wan_") && n.contains("_Entry"))
        .map(|(n, _)| n.clone())
        .collect();
    for node in &wan_nodes {
        // Create the service identity before any remaining child attributes.
        if let Some(service) = diff.get(&(node.clone(), "ServiceList".into())) {
            script.push_str(&format!(
                "tcapi set {} ServiceList {} >/dev/null\n",
                quote(node),
                quote(service)
            ));
        }
        script.push_str(&format!("tcapi set {} Active No >/dev/null\n", quote(node)));
    }
    for ((node, key), value) in diff {
        roots.insert(node.split('_').next().unwrap().to_string());
        if key == "Active" {
            continue;
        }
        script.push_str(&format!(
            "tcapi set {} {} {} >/dev/null\n",
            quote(node),
            quote(key),
            quote(value)
        ));
    }
    for node in &wan_nodes {
        if let Some(active) = diff.get(&(node.clone(), "Active".into())) {
            script.push_str(&format!(
                "tcapi set {} Active {} >/dev/null\n",
                quote(node),
                quote(active)
            ));
        } else {
            script.push_str(&format!(
                "tcapi set {} Active Yes >/dev/null\n",
                quote(node)
            ));
        }
    }
    for (p, c) in removed {
        script.push_str(&format!(
            "tcapi set Wan_PVC{p}_Entry{c} Active No >/dev/null\ntcapi commit Wan_PVC{p}_Entry{c} >/dev/null\ntcapi unset Wan_PVC{p}_Entry{c} >/dev/null\ntcapi unset Wan_PVC{p} >/dev/null\n"
        ));
        roots.insert("Wan".into());
    }
    for root in roots {
        if root == "Wan" {
            for node in &wan_nodes {
                script.push_str(&format!("tcapi commit {} >/dev/null\n", quote(node)));
            }
        }
        script.push_str(&format!("tcapi commit {} >/dev/null\n", quote(&root)));
    }
    script.push_str("tcapi save >/dev/null\nprintf 'FTTR_WRITE_COMPLETE\\n'\n");
    script
}

pub async fn run(
    device: Device,
    apply: bool,
    full: bool,
    revision: String,
    operation_id: u64,
    auth_epoch: u64,
) -> Result<Value, crate::provisioning::command::Failure> {
    let check_auth = || {
        if crate::omci_auth::serial_epoch(&device.optical_sn) == Some(auth_epoch) {
            Ok(())
        } else {
            Err("OMCI authentication generation changed".to_string())
        }
    };
    check_auth()?;
    let (selection, target_mac) = {
        let registry = crate::mqtt_identity::shared().lock().unwrap();
        let targets: Vec<_> = registry
            .snapshots(now())
            .into_iter()
            .filter(|s| s.optical_sn.as_deref() == Some(&device.optical_sn) && s.reason == "ready")
            .collect();
        let [target] = targets.as_slice() else {
            return Err("fresh unique optical/MQTT identity required".into());
        };
        (
            Selection {
                optical_sn: device.optical_sn.clone(),
                onu_id: target.onu_id.ok_or("missing ONU identity")?,
                binding_generation: target.binding_generation.clone(),
            },
            target.mqtt_mac.clone(),
        )
    };
    // Separate from per-publication transport locks; held for the complete transaction.
    let _transaction = super::command::lock_target(&format!("transaction-{target_mac}"))
        .map_err(|_| "device transaction already active")?;
    let path = format!("/etc/fttr/command-owned/{}.json", device.optical_sn);
    let owned: Ownership = match std::fs::read(&path) {
        Ok(bytes) => {
            serde_json::from_slice(&bytes).map_err(|_| "invalid command ownership journal")?
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ownership::default(),
        Err(_) => return Err("cannot read command ownership journal".into()),
    };
    let read_failure = |reason: String| {
        if owned.pending {
            crate::provisioning::command::Failure::uncertain(reason)
        } else {
            reason.into()
        }
    };
    // Inventory is read-only. A timeout does not cancel the remote shell, so
    // only retry this initial read, never a write or its uncertain readback.
    let inventory_text = initial_inventory_with_retry(
        || execute_once(&selection, INVENTORY),
        check_auth,
        Duration::from_secs(2),
    )
    .await
    .map_err(read_failure)?;
    let inventory = parse(&inventory_text).map_err(read_failure)?;
    check_auth().map_err(read_failure)?;
    reconcile(
        Transaction {
            device: &device,
            apply,
            full,
            revision: &revision,
            operation_id,
            binding_generation: &selection.binding_generation,
        },
        inventory,
        owned,
        |script| {
            let selection = &selection;
            async move { execute(selection, &script).await }
        },
        |owned| {
            storage::write_atomic(
                &path,
                &serde_json::to_vec(owned).map_err(|_| "serialize command journal")?,
                true,
            )
            .map_err(|_| "persist command journal".to_string())
        },
        check_auth,
    )
    .await
}

struct Transaction<'a> {
    device: &'a Device,
    apply: bool,
    full: bool,
    revision: &'a str,
    operation_id: u64,
    binding_generation: &'a str,
}

async fn reconcile<F, Fut, P, C>(
    transaction: Transaction<'_>,
    inventory: Inventory,
    owned: Ownership,
    mut execute: F,
    mut persist: P,
    check_auth: C,
) -> Result<Value, crate::provisioning::command::Failure>
where
    F: FnMut(String) -> Fut,
    Fut: std::future::Future<Output = Result<String, String>>,
    P: FnMut(&Ownership) -> Result<(), String>,
    C: Fn() -> Result<(), String>,
{
    let Transaction {
        device,
        apply,
        full,
        revision,
        operation_id,
        binding_generation,
    } = transaction;
    let unresolved = owned.pending;
    let unresolved_operation = unresolved.then(|| json!({"revision":owned.revision,"operation_id":owned.operation_id,"binding_generation":owned.binding_generation}));
    let mut desired = compile(device, &inventory, owned, full).map_err(|reason| {
        if unresolved {
            crate::provisioning::command::Failure::uncertain(reason)
        } else {
            reason.into()
        }
    })?;
    if !unresolved {
        desired.owned.revision = revision.into();
        desired.owned.operation_id = operation_id;
        desired.owned.binding_generation = binding_generation.into();
    }
    let mut diff = differences(&desired, &inventory);
    // Matching persistent fields do not prove the firmware applied a WAN.
    for owned in desired.owned.wan.values() {
        if desired.removed.contains(&(owned.parent, owned.child)) {
            continue;
        }
        let node = format!("Wan_PVC{}_Entry{}", owned.parent, owned.child);
        if desired
            .fields
            .get(&(node.clone(), "Active".into()))
            .is_some_and(|v| v == "Yes")
            && get(&inventory, &node, "__runtime_present") != "1"
        {
            diff.insert((node, "Active".into()), "Yes".into());
        }
    }
    for ((node, key), value) in &desired.fields {
        if key == "Active"
            && diff.keys().any(|(changed, _)| {
                changed == node || changed == node.split("_Entry").next().unwrap()
            })
        {
            diff.insert((node.clone(), key.clone()), value.clone());
        }
    }
    let changed = !diff.is_empty() || !desired.removed.is_empty();
    let mut actual = inventory;
    // Legacy pending has neither the original intent nor remote terminal proof.
    // Even a no-diff snapshot (or a plan) cannot settle that old transaction.
    let mut uncertain = unresolved;
    let mut write_dispatched = false;
    if apply && changed && !unresolved {
        desired.owned.pending = true;
        persist(&desired.owned)?;
        let script = write_script(&diff, &desired.removed);
        check_auth()?;
        write_dispatched = true;
        uncertain = execute(script)
            .await
            .map_or(true, |s| s.trim() != "FTTR_WRITE_COMPLETE");
        check_auth().map_err(crate::provisioning::command::Failure::uncertain)?;
        match execute(INVENTORY.into()).await.and_then(|s| parse(&s)) {
            Ok(readback) => actual = readback,
            Err(_) => uncertain = true,
        }
    }
    let remaining = differences(&desired, &actual);
    let removed_verified = desired
        .removed
        .iter()
        .all(|pair| !actual.children.contains(pair));
    let runtime_matches = |fields: &BTreeSet<(String, String)>| {
        fields.iter().all(|(node, key)| {
            key != "Active"
                || desired
                    .fields
                    .get(&(node.clone(), key.clone()))
                    .is_none_or(|v| v != "Yes")
                || get(&actual, node, "__runtime_present") == "1"
        })
    };
    let verified = remaining.is_empty()
        && removed_verified
        && !uncertain
        && desired.items.values().all(runtime_matches);
    if apply && verified {
        desired
            .owned
            .wan
            .retain(|_, owned| !desired.removed.contains(&(owned.parent, owned.child)));
        desired.owned.pending = false;
        persist(&desired.owned).map_err(|reason| {
            if write_dispatched {
                crate::provisioning::command::Failure::uncertain(reason)
            } else {
                reason.into()
            }
        })?;
    }
    let result_for = |id: &str, fields: &BTreeSet<(String, String)>| {
        let matches = fields
            .iter()
            .all(|k| desired.fields.get(k) == actual.fields.get(k))
            && runtime_matches(fields);
        let state = if uncertain {
            "unknown"
        } else if matches {
            "verified"
        } else {
            "drift"
        };
        json!({"id":id,"state":state,"updated_at":now(),"reason":if uncertain {"write outcome uncertain; fresh inventory required; no automatic replay"} else if matches {"native tcapi readback matches desired state"} else if apply {"native tcapi readback differs after apply"} else {"read-only plan: changes required"},"changes":fields.iter().filter(|k| diff.contains_key(*k)).map(|(node,key)|json!({"name":format!("{node}.{key}"),"desired":if key=="WPAPSK" {"<redacted>"} else {desired.fields.get(&(node.clone(),key.clone())).map(String::as_str).unwrap_or("")}})).collect::<Vec<_>>()})
    };
    let items: Vec<_> = device
        .items
        .iter()
        .map(|item| {
            let mut result = result_for(&item.id, desired.items.get(&item.id).unwrap());
            result["kind"] = json!(item.kind);
            result["service_type"] = json!(item.service_type);
            result
        })
        .collect();
    let cleanup = desired
        .items
        .get("wifi_cleanup")
        .map(|fields| result_for("wifi_cleanup", fields));
    Ok(
        json!({"items":items,"wifi_lifecycle":cleanup.map(|v|json!({"native_cleanup":v})).unwrap_or(json!({})),"wan_lifecycle":{"final_inventory":{"state":if uncertain {"unknown"} else if verified {"verified"} else {"drift"},"reason":"native inventory; management/shared parents protected"}},"native_ownership":desired.owned.wan,"write_performed":write_dispatched,"readback_verified":verified,"unresolved_operation":unresolved_operation}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn complete_wifi_text(ssid: &str) -> String {
        let mut text = String::new();
        for radio in ["WLan", "WLan11ac"] {
            for (node, keys) in std::iter::once((format!("{radio}_Common"), "APOn BssidNum")).chain(
                (0..4).map(|slot| {
                    (
                    format!("{radio}_Entry{slot}"),
                    "SSID EnableSSID HideSSID AuthMode EncrypType WPAPSK PMFMFPC PMFMFPR PMFSHA256",
                )
                }),
            ) {
                for key in keys.split_whitespace() {
                    let value = match key {
                        "BssidNum" => "4",
                        "SSID" => ssid,
                        _ => "1",
                    };
                    text.push_str(&format!(
                        "V|{node}|{key}|{}\n",
                        value
                            .bytes()
                            .map(|b| format!("{b:02x}"))
                            .collect::<String>()
                    ));
                }
            }
        }
        text.push_str("FTTR_INVENTORY_COMPLETE\n");
        text
    }

    #[test]
    fn inventory_requires_every_field_and_rejects_non_ascii_hex_without_panicking() {
        let text = complete_wifi_text("test");
        assert!(parse(&text).is_ok());
        let lines: Vec<_> = text.lines().collect();
        for skip in 0..lines.len() {
            let incomplete = lines
                .iter()
                .enumerate()
                .filter(|(n, _)| *n != skip)
                .map(|(_, line)| *line)
                .collect::<Vec<_>>()
                .join("\n");
            assert!(parse(&incomplete).is_err(), "accepted missing line {skip}");
        }
        for hex in ["a中", "é", "zz", "0"] {
            assert!(parse(&text.replacen("|74657374", &format!("|{hex}"), 1)).is_err());
        }
        assert!(parse(&(text.clone() + "P|0\n")).is_err());
        assert!(parse(&format!("P|0\n{text}")).is_err());
        assert!(parse(&format!("C|0|0\n{text}")).is_err());
        assert!(parse(&format!("V|Wan_PVC7|VLANID|31\n{text}")).is_err());
    }

    #[tokio::test]
    async fn legacy_pending_never_settles_or_dispatches_even_when_current_desired_matches() {
        let (device, _) = fixture();
        let desired = compile(&device, &Inventory::default(), Ownership::default(), true).unwrap();
        let mut inventory = Inventory {
            fields: desired.fields.clone(),
            ..Inventory::default()
        };
        inventory.parents.insert(0);
        inventory.children.insert((0, 0));
        inventory.fields.insert(
            ("Wan_PVC0_Entry0".into(), "__runtime_present".into()),
            "1".into(),
        );
        let mut owned = desired.owned;
        owned.pending = true;
        owned.operation_id = 9;
        owned.revision = "original-revision".into();
        owned.binding_generation = "original-binding".into();
        for apply in [false, true] {
            for drift in [false, true] {
                let mut actual = inventory.clone();
                if drift {
                    actual
                        .fields
                        .remove(&("Wan_PVC0_Entry0".into(), "LAN2".into()));
                }
                let result = reconcile(
                    Transaction {
                        device: &device,
                        apply,
                        full: true,
                        revision: "new-revision",
                        operation_id: 42,
                        binding_generation: "new-binding",
                    },
                    actual,
                    owned.clone(),
                    |_| async { panic!("pending must not dispatch") },
                    |_| panic!("pending journal must remain byte-for-byte unchanged"),
                    || Ok(()),
                )
                .await
                .unwrap();
                assert_eq!(result["items"][0]["state"], "unknown");
                assert_eq!(result["readback_verified"], false);
                assert_eq!(result["write_performed"], false);
                assert_eq!(result["unresolved_operation"]["operation_id"], 9);
                assert_eq!(
                    result["unresolved_operation"]["revision"],
                    "original-revision"
                );
            }
        }
    }

    #[tokio::test]
    async fn native_writes_require_durable_intent_and_terminal_plus_complete_readback() {
        use std::cell::RefCell;
        let (mut device, _) = fixture();
        device.items[0].wan_intent = None;
        device.items[0].wlan_intent = Some(WlanIntent {
            endpoint: EndpointRef::Ssid { radio: 2, slot: 1 },
            ssid: "wanted".into(),
            enabled: None,
            broadcast: None,
            security: None,
            key: None,
        });
        let transaction = || Transaction {
            device: &device,
            apply: true,
            full: true,
            revision: "new",
            operation_id: 42,
            binding_generation: "binding",
        };
        let result = reconcile(
            transaction(),
            parse(&complete_wifi_text("old")).unwrap(),
            Ownership::default(),
            |_| async { panic!("failed intent must not dispatch") },
            |_| Err("injected save failure".into()),
            || Ok(()),
        )
        .await;
        assert!(result.is_err());
        for terminal in [true, false] {
            let saves = RefCell::new(Vec::new());
            let calls = RefCell::new(0);
            let result = reconcile(
                transaction(),
                parse(&complete_wifi_text("old")).unwrap(),
                Ownership::default(),
                |_| {
                    *calls.borrow_mut() += 1;
                    let n = *calls.borrow();
                    async move {
                        if n == 1 {
                            if terminal {
                                Ok("FTTR_WRITE_COMPLETE".into())
                            } else {
                                Err("timeout".into())
                            }
                        } else {
                            Ok(complete_wifi_text("wanted"))
                        }
                    }
                },
                |owned| {
                    saves.borrow_mut().push(owned.pending);
                    Ok(())
                },
                || Ok(()),
            )
            .await
            .unwrap();
            assert_eq!(result["write_performed"], true);
            assert_eq!(result["readback_verified"], terminal);
            assert_eq!(
                *saves.borrow(),
                if terminal {
                    vec![true, false]
                } else {
                    vec![true]
                }
            );
            assert_eq!(
                result["items"][0]["state"],
                if terminal { "verified" } else { "unknown" }
            );
        }
        // Errors after dispatch (including a failed durable settlement) must
        // retain uncertainty instead of becoming an ordinary blocked read.
        for auth_revoked in [false, true] {
            let calls = RefCell::new(0);
            let auth_checks = RefCell::new(0);
            let saves = RefCell::new(0);
            let failure = reconcile(
                transaction(),
                parse(&complete_wifi_text("old")).unwrap(),
                Ownership::default(),
                |_| {
                    *calls.borrow_mut() += 1;
                    let n = *calls.borrow();
                    async move {
                        Ok(if n == 1 {
                            "FTTR_WRITE_COMPLETE".into()
                        } else {
                            complete_wifi_text("wanted")
                        })
                    }
                },
                |_| {
                    *saves.borrow_mut() += 1;
                    if *saves.borrow() == 2 {
                        Err("injected settlement barrier failure".into())
                    } else {
                        Ok(())
                    }
                },
                || {
                    *auth_checks.borrow_mut() += 1;
                    if auth_revoked && *auth_checks.borrow() == 2 {
                        Err("authorization revoked".into())
                    } else {
                        Ok(())
                    }
                },
            )
            .await
            .unwrap_err();
            assert!(failure.uncertain);
            assert_eq!(*calls.borrow(), if auth_revoked { 1 } else { 2 });
        }
    }

    #[tokio::test]
    async fn initial_inventory_retries_only_timeout_and_checks_auth() {
        use std::cell::Cell;
        let calls = Cell::new(0);
        let read = || {
            calls.set(calls.get() + 1);
            async {
                if calls.get() == 1 {
                    Err(("timeout".into(), true))
                } else {
                    Ok("inventory".into())
                }
            }
        };
        assert_eq!(
            initial_inventory_with_retry(read, || Ok(()), Duration::ZERO)
                .await
                .unwrap(),
            "inventory"
        );
        assert_eq!(calls.get(), 2);

        let calls = Cell::new(0);
        let read = || {
            calls.set(calls.get() + 1);
            async { Err(("identity changed".into(), false)) }
        };
        assert_eq!(
            initial_inventory_with_retry(read, || Ok(()), Duration::ZERO)
                .await
                .unwrap_err(),
            "identity changed"
        );
        assert_eq!(calls.get(), 1);

        let calls = Cell::new(0);
        let read = || {
            calls.set(calls.get() + 1);
            async { Err(("timeout".into(), true)) }
        };
        assert_eq!(
            initial_inventory_with_retry(read, || Err("auth changed".into()), Duration::ZERO)
                .await
                .unwrap_err(),
            "auth changed"
        );
        assert_eq!(calls.get(), 1);
    }
    fn fixture() -> (Device, Inventory) {
        let item = Item {
            id: "wan".into(),
            kind: ItemKind::DeviceService,
            service_type: "iptv".into(),
            parameters: vec![],
            guard: None,
            blocked: None,
            wan_ref: Some("wan".into()),
            auto_instance: None,
            wlan_intent: None,
            binding_intent: None,
            wan_intent: Some(WanIntent {
                services: vec!["IPTV".into()],
                vlan: VlanMode::Tagged(88),
                priority: 4,
                mode: ConnectionMode::Bridge,
                bindings: vec![EndpointRef::Lan(2)],
                internet: false,
                enabled: true,
            }),
        };
        (
            Device {
                id: "child".into(),
                optical_sn: "H3CT12345678".into(),
                cwmp_id: "pending:H3CT12345678".into(),
                omci_adapter: "hl202".into(),
                bootstrap_adapter: "none".into(),
                provisioning_adapter: "hl202-mqtt".into(),
                resolved_contract: None,
                connection_request: ConnectionRequest::default(),
                items: vec![item],
                wifi_cleanup: "preserve".into(),
                wifi_preserve: BTreeSet::new(),
            },
            Inventory::default(),
        )
    }
    #[test]
    fn removal_keeps_ownership_until_absence_is_verified() {
        let (mut device, mut inv) = fixture();
        let first = compile(&device, &inv, Ownership::default(), true).unwrap();
        inv.parents.insert(0);
        inv.children.insert((0, 0));
        inv.fields.insert(
            ("Wan_PVC0_Entry0".into(), "ServiceList".into()),
            "IPTV".into(),
        );
        inv.fields
            .insert(("Wan_PVC0".into(), "VLANID".into()), "88".into());
        device.items.clear();
        let deleting = compile(&device, &inv, first.owned, true).unwrap();
        assert!(deleting.removed.contains(&(0, 0)));
        assert!(deleting.owned.wan.contains_key("wan"));
        let mut pending = deleting.owned;
        pending.pending = true;
        let recovered = compile(&device, &inv, pending, true).unwrap();
        assert!(recovered.owned.pending);
        assert!(recovered.removed.contains(&(0, 0)));
        inv.children.clear();
        let absent = compile(&device, &inv, recovered.owned, true).unwrap();
        assert!(absent.owned.wan.is_empty());
    }
    #[test]
    fn logical_radio_cleanup_preserves_requested_slots() {
        let (mut device, inv) = fixture();
        device.wifi_cleanup = "disable".into();
        device.wifi_preserve.insert(6);
        device.items[0].wan_intent = None;
        device.items[0].wlan_intent = Some(WlanIntent {
            endpoint: EndpointRef::Ssid { radio: 2, slot: 1 },
            ssid: "test-network".into(),
            enabled: Some(true),
            broadcast: Some(true),
            security: None,
            key: None,
        });
        let desired = compile(&device, &inv, Ownership::default(), true).unwrap();
        assert_eq!(
            desired.fields[&("WLan_Entry0".into(), "EnableSSID".into())],
            "1"
        );
        assert_eq!(
            desired.fields[&("WLan11ac_Entry0".into(), "EnableSSID".into())],
            "0"
        );
        assert!(!desired
            .fields
            .contains_key(&("WLan11ac_Entry1".into(), "EnableSSID".into())));
        assert_eq!(desired.items["wifi_cleanup"].len(), 6);
    }
    #[test]
    fn native_plan_preserves_management_and_uses_free_parent() {
        let (device, mut inv) = fixture();
        inv.parents.insert(0);
        inv.children.insert((0, 0));
        inv.fields.insert(
            ("Wan_PVC0_Entry0".into(), "ServiceList".into()),
            "TR069_INTERNET".into(),
        );
        let desired = compile(&device, &inv, Ownership::default(), true).unwrap();
        assert_eq!(desired.owned.wan["wan"].parent, 1);
        assert!(desired
            .fields
            .keys()
            .all(|(node, _)| !node.starts_with("Wan_PVC0")));
        assert_eq!(
            desired.fields[&("Wan_PVC1_Entry0".into(), "LAN2".into())],
            "Yes"
        );
        assert_eq!(
            desired.fields[&("Wan_PVC1_Entry0".into(), "LAN1".into())],
            "No"
        );
        assert!(differences(&desired, &inv).len() > 10);
        assert!(inv.fields.len() == 1); // planning never changes observed state
    }
    #[test]
    fn shared_and_reassigned_owned_wans_fail_closed() {
        let (device, mut inv) = fixture();
        inv.parents.insert(1);
        inv.children.insert((1, 0));
        inv.children.insert((1, 1));
        inv.fields.insert(
            ("Wan_PVC1_Entry0".into(), "ServiceList".into()),
            "IPTV".into(),
        );
        inv.fields.insert(
            ("Wan_PVC1_Entry0".into(), "WanMode".into()),
            "Bridge".into(),
        );
        inv.fields
            .insert(("Wan_PVC1".into(), "VLANID".into()), "88".into());
        assert!(compile(&device, &inv, Ownership::default(), true).is_err());
        inv.children.remove(&(1, 1));
        let mut owned = Ownership::default();
        owned.wan.insert(
            "wan".into(),
            Owned {
                parent: 1,
                child: 0,
                service: "OTHER".into(),
                vlan: "88".into(),
            },
        );
        assert!(compile(&device, &inv, owned, true).is_err());
    }
    #[test]
    fn wpa3_transition_resets_required_pmf_when_returning_to_wpa2() {
        let (mut device, inv) = fixture();
        device.items[0].wan_intent = None;
        device.items[0].wlan_intent = Some(WlanIntent {
            endpoint: EndpointRef::Ssid { radio: 5, slot: 2 },
            ssid: "test".into(),
            enabled: Some(true),
            broadcast: Some(true),
            security: Some(WifiSecurity::Wpa3Sae),
            key: Some("test-only-key".into()),
        });
        let pure = compile(&device, &inv, Ownership::default(), true).unwrap();
        let field = |key: &str| ("WLan11ac_Entry1".into(), key.into());
        assert_eq!(pure.fields[&field("AuthMode")], "WPA3PSK");
        assert_eq!(pure.fields[&field("PMFMFPR")], "1");
        device.items[0].wlan_intent.as_mut().unwrap().security = Some(WifiSecurity::Wpa2PskWpa3Sae);
        let mixed = compile(&device, &inv, Ownership::default(), true).unwrap();
        assert_eq!(mixed.fields[&field("AuthMode")], "WPA2PSKWPA3PSK");
        assert_eq!(mixed.fields[&field("PMFMFPC")], "1");
        assert_eq!(mixed.fields[&field("PMFMFPR")], "0");
        device.items[0].wlan_intent.as_mut().unwrap().security = Some(WifiSecurity::Wpa2Psk);
        let observed = Inventory {
            fields: pure.fields,
            ..inv
        };
        let restored = compile(&device, &observed, Ownership::default(), true).unwrap();
        let diff = differences(&restored, &observed);
        assert_eq!(diff[&field("PMFMFPC")], "0");
        assert_eq!(diff[&field("PMFMFPR")], "0");
    }
    #[test]
    fn backend_defaults_and_explicit_override_follow_compiled_capability() {
        let (mut device, _) = fixture();
        assert_eq!(device.backend(), "command");
        device.provisioning_adapter = "hl202-cu".into();
        assert_eq!(device.backend(), "acs");
        device.provisioning_adapter = "hwtc-hw".into();
        device.omci_adapter = "hwtc".into();
        assert_eq!(device.backend(), "acs");
    }
    #[test]
    fn native_endpoints_are_not_cwmp_instances() {
        assert_eq!(
            wlan(&EndpointRef::Ssid { radio: 5, slot: 1 }).unwrap(),
            "WLan11ac_Entry0"
        );
        assert!(wlan(&EndpointRef::Ssid { radio: 5, slot: 5 }).is_err());
        assert_eq!(
            binding(&EndpointRef::Ssid { radio: 5, slot: 4 }).unwrap(),
            "SSIDAC4"
        );
    }
    #[test]
    fn framing_and_duplicates_fail_closed() {
        assert!(parse("P|0\nP|0\nFTTR_INVENTORY_COMPLETE\n").is_err());
        assert!(parse("C|0|0\n").is_err());
        assert!(protected("TR069_INTERNET"));
        assert!(!protected("INTERNET"));
    }
    #[test]
    fn secrets_and_shell_metacharacters_are_data() {
        let value = "a'$(touch /tmp/unwanted)";
        let diff = BTreeMap::from([(("WLan_Entry0".into(), "WPAPSK".into()), value.into())]);
        let script = write_script(&diff, &BTreeSet::new());
        assert!(script.contains("'a'\\''$(touch /tmp/unwanted)'"));
        assert!(script.find("tcapi commit").unwrap() < script.find("tcapi save").unwrap());
    }
}
