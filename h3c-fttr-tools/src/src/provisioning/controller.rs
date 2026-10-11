//! Desired-state controller. CWMP traffic runs independently of optical workers.
use super::model::{ItemKind, OperationPhase, ParameterKind};
use super::{config, wan, wifi};
#[path = "parameters.rs"]
mod parameters;
use config::{Config, Device, Item, Parameter};
use parameters::advance;
#[cfg(test)]
use parameters::equivalent;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn error(s: &str) -> Value {
    json!({"error":s})
}
#[cfg(all(feature = "acs", not(test)))]
pub(super) fn acs(v: Value) -> Value {
    serde_json::from_str(&crate::acs::control(&v.to_string()))
        .unwrap_or_else(|_| error("invalid ACS reply"))
}
#[cfg(all(not(feature = "acs"), not(test)))]
pub(super) fn acs(_: Value) -> Value {
    error("ACS adapter not compiled")
}
#[cfg(test)]
thread_local! { pub(super) static RPC: std::cell::RefCell<(std::collections::VecDeque<Value>,Vec<Value>)> = std::cell::RefCell::new((std::collections::VecDeque::new(),Vec::new())); }
#[cfg(test)]
pub(super) fn acs(v: Value) -> Value {
    RPC.with(|r| {
        let mut r = r.borrow_mut();
        r.1.push(v);
        r.0.pop_front().expect("unexpected RPC")
    })
}

#[derive(Clone)]
struct Operation {
    id: u64,
    device: Device,
    revision: String,
    apply: bool,
    management: bool,
    index: usize,
    stage: OperationPhase,
    task: Option<u64>,
    roots: Vec<String>,
    writable: BTreeSet<String>,
    changes: Vec<Parameter>,
    uncertain: bool,
    pending: bool,
    written: bool,
    passphrase_fallback: bool,
    results: BTreeMap<String, Value>,
    started: u64,
    wan: wan::Reconciler,
    wifi: wifi::Reconciler,
}
impl Operation {
    fn new(id: u64, mut device: Device, revision: String, apply: bool, management: bool) -> Self {
        let mut activations = Vec::new();
        for service in &mut device.items {
            if service.kind != ItemKind::DeviceService {
                continue;
            }
            if let Some(enable) = service
                .parameters
                .iter_mut()
                .find(|p| p.name.ends_with(".Enable"))
            {
                if matches!(enable.value.as_str(), "1" | "true") {
                    let desired = enable.clone();
                    enable.value = "0".into();
                    let mut activation = service.clone();
                    activation.id = format!("{}__activation", service.id);
                    activation.kind = ItemKind::WanActivation;
                    activation.parameters = vec![desired];
                    activations.push(activation);
                }
            }
        }
        for binding in device
            .items
            .iter_mut()
            .filter(|i| i.kind == ItemKind::PortBinding)
        {
            if let Some(activation) = activations.iter().find(|i| i.wan_ref == binding.wan_ref) {
                let mut enable = activation.parameters[0].clone();
                enable.value = "0".into();
                binding.parameters.push(enable);
            }
        }
        device.items.extend(activations);
        let wan = wan::Reconciler::new(&device, BTreeMap::new(), false);
        let wifi = wifi::Reconciler::new(&device, wifi::Registry::default(), false);
        Self {
            id,
            device,
            revision,
            apply,
            management,
            index: 0,
            stage: OperationPhase::Waiting,
            task: None,
            wan,
            wifi,
            roots: Vec::new(),
            writable: BTreeSet::new(),
            changes: Vec::new(),
            uncertain: false,
            pending: false,
            written: false,
            passphrase_fallback: false,
            results: BTreeMap::new(),
            started: now(),
        }
    }
    fn item(&self) -> &Item {
        &self.device.items[self.index]
    }
    fn finish_item(&mut self, state: &str, reason: &str) {
        let i = self.item().clone();
        if i.kind == ItemKind::Ssid && state == "verified" && self.apply {
            for p in &i.parameters {
                if let Some(n) = wifi::instance(&p.name) {
                    self.wifi.registry.owned.insert(n);
                }
            }
        }
        self.results.insert(i.id.clone(), json!({"id":i.id,"kind":i.kind,"service_type":i.service_type,"state":state,"reason":reason,"revision":self.revision,"operation_id":self.id,"task_id":self.task,"updated_at":now(),"changes":self.changes.iter().map(|p| json!({"name":p.name,"desired":if p.secret {"<redacted>"} else {&p.value}})).collect::<Vec<_>>()}));
        if i.kind == ItemKind::WanActivation && state != "verified" {
            if let Some(service) = i.wan_ref.clone() {
                if let Some(result) = self.results.get_mut(&service) {
                    result["state"] = json!(state);
                    result["reason"] = json!(format!("WAN activation: {reason}"));
                }
            }
        }
        self.index += 1;
        self.task = None;
        self.stage = OperationPhase::Waiting;
        self.roots.clear();
        self.writable.clear();
        self.changes.clear();
        self.uncertain = false;
        self.pending = false;
        self.written = false;
        self.passphrase_fallback = false;
        if self.index >= self.device.items.len() {
            self.stage = OperationPhase::WifiReconciling;
        }
    }
    fn view(&self) -> Value {
        let mut items: Vec<Value> = self.results.values().cloned().collect();
        for i in self.device.items.iter().skip(self.index) {
            items.push(json!({"id":i.id,"kind":i.kind,"service_type":i.service_type,"state":if i.id == self.item().id { self.stage.as_str() } else {"pending"},"task_id":if i.id == self.item().id {self.task} else {None},"revision":self.revision}));
        }
        json!({"operation_id":self.id,"device":self.device.id,"optical_sn":self.device.optical_sn,"cwmp_id":self.device.cwmp_id,"revision":self.revision,"mode":if self.apply {"apply"} else {"plan"},"state":self.stage,"started_at":self.started,"items":items,"wan_objects":self.wan.owned,"wan_lifecycle":self.wan.results,"wan_stage":self.wan.stage,"wifi_objects":self.wifi.registry,"wifi_lifecycle":self.wifi.results,"wifi_stage":self.wifi.stage,"wifi_cleanup":self.device.wifi_cleanup})
    }
}
#[derive(Default)]
struct Controller {
    config: Config,
    failure: Option<String>,
    next_id: u64,
    operations: BTreeMap<String, Operation>,
    management: BTreeMap<String, Value>,
    last_online: BTreeSet<String>,
    last_boot: BTreeMap<String, u64>,
    last_periodic: u64,
    recovered: BTreeMap<String, Value>,
    saved: String,
    owned: BTreeMap<String, BTreeMap<String, wan::Owned>>,
    wifi_owned: BTreeMap<String, wifi::Registry>,
    omci_management: BTreeMap<String, Value>,
}
static CONTROLLER: OnceLock<Mutex<Controller>> = OnceLock::new();
fn controller() -> &'static Mutex<Controller> {
    CONTROLLER.get_or_init(|| Mutex::new(Controller::default()))
}
pub(crate) fn management_desired() -> Option<(config::Management, String)> {
    let c = controller().lock().unwrap();
    c.config
        .management
        .clone()
        .map(|m| (m, c.config.revision.clone()))
}
pub(crate) fn management_result(serial: &str, result: Value) {
    controller()
        .lock()
        .unwrap()
        .omci_management
        .insert(serial.into(), result);
}
pub(crate) fn management_version() -> String {
    let c = controller().lock().unwrap();
    serde_json::to_string(&c.config.management).unwrap_or_default()
}
impl Controller {
    fn start(&mut self, target: &str, apply: bool, management: bool) -> Result<Vec<u64>, String> {
        self.start_selected(target, apply, management, None)
    }
    fn start_selected(
        &mut self,
        target: &str,
        apply: bool,
        management: bool,
        item: Option<&str>,
    ) -> Result<Vec<u64>, String> {
        if apply && !self.config.enabled {
            return Err("provisioning disabled by UCI".into());
        }
        let mut devices: Vec<_> = self
            .config
            .devices
            .iter()
            .filter(|d| {
                target == "all"
                    || target == d.id
                    || target == d.optical_sn
                    || self
                        .config
                        .assignments
                        .get(&d.id)
                        .is_some_and(|(group, _)| target == format!("group:{group}"))
            })
            .cloned()
            .collect();
        if let Some(item) = item {
            if devices
                .iter()
                .any(|d| d.items.iter().any(|i| i.id == item && i.wan_ref.is_some()))
            {
                return Err(
                    "ordered WAN reconciliation requires full-device synchronization".into(),
                );
            }
            for d in &mut devices {
                d.items.retain(|i| i.id == item);
            }
            devices.retain(|d| !d.items.is_empty());
        }
        if devices.is_empty() {
            return Err("no configured device matches target".into());
        }
        if devices.iter().any(|d| {
            self.operations
                .get(&d.id)
                .is_some_and(|o| o.stage != OperationPhase::Completed)
        }) {
            return Err("device already has an active operation".into());
        }
        let mut ids = Vec::new();
        for d in devices {
            self.next_id += 1;
            ids.push(self.next_id);
            let mut op = Operation::new(
                self.next_id,
                d.clone(),
                self.config.revision.clone(),
                apply,
                management,
            );
            let owned = wan::load(&d.cwmp_id)?
                .unwrap_or_else(|| self.owned.get(&d.cwmp_id).cloned().unwrap_or_default());
            op.wan = wan::Reconciler::new(&d, owned, item.is_none());
            op.wifi = wifi::Reconciler::new(&d, wifi::load(&d.cwmp_id)?, item.is_none());
            if item.is_some() {
                let previous = self
                    .operations
                    .get(&d.id)
                    .map(Operation::view)
                    .or_else(|| self.recovered.get(&d.id).cloned());
                if let Some(items) = previous.as_ref().and_then(|p| p["items"].as_array()) {
                    for i in items {
                        if let Some(id) = i["id"].as_str() {
                            if !d.items.iter().any(|selected| selected.id == id) {
                                op.results.insert(id.into(), i.clone());
                            }
                        }
                    }
                }
            }
            if d.items.is_empty() {
                op.stage = OperationPhase::WanReconciling;
            }
            self.operations.insert(d.id.clone(), op);
        }
        Ok(ids)
    }
    fn snapshot(&self) -> Value {
        let devices: Vec<_> = self.config.devices.iter().map(|d| {
            let mut operation = self.operations.get(&d.id).map(Operation::view).or_else(|| self.recovered.get(&d.id).cloned()).unwrap_or_else(|| json!({"state":"not_started","items":d.items.iter().map(|i| json!({"id":i.id,"kind":i.kind,"service_type":i.service_type,"state":"not_started"})).collect::<Vec<_>>() }));
            operation["current_revision"] = json!(operation["revision"] == self.config.revision);
            operation["management_fresh"] = json!(self.management.get(&d.id).is_some_and(|m| m["state"] == "online"));
            if let Some(owned)=self.owned.get(&d.cwmp_id) { operation["wan_objects"]=json!(owned); }
            if let Some(owned)=self.wifi_owned.get(&d.cwmp_id) { operation["wifi_objects"]=json!(owned); }
            let previous=operation["items"].as_array().cloned().unwrap_or_default();
            let items: Vec<_>=d.items.iter().map(|desired| {
                let mut item=previous.iter().find(|i| i["id"] == desired.id).cloned().unwrap_or_else(|| json!({"id":desired.id,"kind":desired.kind,"service_type":desired.service_type,"state":"not_started"}));
                item["name"]=json!(self.config.labels.get(desired.id.rsplit("__").next().unwrap_or(&desired.id)));
                item["current_revision"]=json!(item["revision"] == self.config.revision);
                item["management_fresh"]=operation["management_fresh"].clone(); item
            }).collect();
            operation["items"]=json!(items);
            let ssids: Vec<_> = items.iter().filter(|i| i["kind"] == "ssid").cloned().collect();
            let services: Vec<_> = items.iter().filter(|i| i["kind"] != "ssid").cloned().collect();
            let wifi_sync=if operation["wifi_lifecycle"].as_object().is_some_and(|m|m.values().any(|i|matches!(i["state"].as_str(),Some("blocked"|"failed"|"unknown")))) { "attention_required" } else if operation["wifi_stage"].is_string() && operation["wifi_stage"] != "done" { "pending" } else { aggregate(&ssids) };
            let assignment=self.config.assignments.get(&d.id);
            json!({"device":d.id,"name":self.config.labels.get(&d.id),"group":assignment.map(|a|&a.0),"group_name":assignment.and_then(|a|self.config.labels.get(&a.0)),"template":assignment.map(|a|&a.1),"template_name":assignment.and_then(|a|self.config.labels.get(&a.1)),"optical_sn":d.optical_sn,"adapter":d.adapter,"omci_management":self.omci_management.get(&d.optical_sn).cloned().unwrap_or_else(||json!({"state":"pending"})),"management":self.management.get(&d.id),"provisioning":operation,"services":services,"ssids":ssids,"wifi_sync":wifi_sync})
        }).collect();
        json!({"enabled":self.config.enabled,"mode":if self.config.automatic {"auto"} else {"manual"},"method":"diff","revision":self.config.revision,"triggers":self.config.triggers,"interval":self.config.interval,"omci_management":{"desired":self.config.management,"state":"not_verified"},"groups":self.config.groups.iter().map(|(id,t)|json!({"id":id,"name":self.config.labels.get(id),"template":t})).collect::<Vec<_>>(),"error":self.failure,"devices":devices,"pending_devices":devices.iter().filter(|d| d["provisioning"]["state"] != "completed").map(|d| d["device"].clone()).collect::<Vec<_>>()})
    }
}
fn discovered_identity(devices: &Value, optical_sn: &str, adapter: &str) -> Option<String> {
    let hex = format!(
        "{}{}",
        optical_sn[..4]
            .bytes()
            .map(|b| format!("{b:02X}"))
            .collect::<String>(),
        &optical_sn[4..]
    );
    let matches: Vec<_> = devices["devices"]
        .as_object()?
        .iter()
        .filter(|(_, d)| {
            let serial = d["serial"].as_str().unwrap_or("");
            match adapter {
                "hl202" => d["product_class"] == "HL202-DU-C" && serial.contains(optical_sn),
                "hwtc" => optical_sn.starts_with("HWTC") && serial.eq_ignore_ascii_case(&hex),
                _ => false,
            }
        })
        .map(|(id, _)| id.clone())
        .collect();
    if matches.len() == 1 {
        matches.into_iter().next()
    } else {
        None
    }
}
fn aggregate(items: &[Value]) -> &str {
    if items.is_empty() {
        "not_configured"
    } else if items.iter().all(|i| i["state"] == "verified") {
        if items.iter().any(|i| i["current_revision"] != true) {
            "stale_revision"
        } else if items.iter().any(|i| i["management_fresh"] != true) {
            "stale_management"
        } else {
            "verified"
        }
    } else if items
        .iter()
        .any(|i| matches!(i["state"].as_str(), Some("failed" | "blocked" | "unknown")))
    {
        "attention_required"
    } else if items.iter().any(|i| i["state"] == "accepted_unverified") {
        "accepted_unverified"
    } else {
        "pending"
    }
}
pub fn snapshot() -> Value {
    controller().lock().unwrap().snapshot()
}
pub fn control(request: &str) -> String {
    let reply = (|| -> Result<Value, String> {
        let v: Value = serde_json::from_str(request).map_err(|_| "invalid provisioning request")?;
        match v["op"].as_str().unwrap_or("") {
            "check" => {
                let cfg = config::load()?;
                Ok(json!({"valid":true,"revision":cfg.revision,"devices":cfg.devices.len()}))
            }
            "reload" => {
                let cfg = config::load()?;
                let mut c = controller().lock().unwrap();
                // Do not allow old queued writes to race a new configuration revision.
                if c.operations
                    .values()
                    .any(|o| o.stage != OperationPhase::Completed)
                {
                    return Err("active synchronization: finish operation before reload".into());
                }
                let changed = cfg.revision != c.config.revision;
                c.config = cfg;
                c.failure = None;
                if changed
                    && c.config.enabled
                    && c.config.automatic
                    && c.config.triggers.contains("config_change")
                    && !c.config.devices.is_empty()
                {
                    c.start("all", true, false)?;
                }
                Ok(c.snapshot())
            }
            "status" => Ok(snapshot()),
            "operation" => {
                let c = controller().lock().unwrap();
                Ok(c.operations
                    .values()
                    .find(|o| Some(o.id) == v["id"].as_u64())
                    .map(Operation::view)
                    .unwrap_or_else(|| error("unknown operation")))
            }
            "plan" | "apply" => {
                let mut c = controller().lock().unwrap();
                let ids = c.start_selected(
                    v["target"].as_str().unwrap_or("all"),
                    v["op"] == "apply",
                    v["management"].as_bool().unwrap_or(false),
                    v["item"].as_str(),
                )?;
                Ok(json!({"operation_ids":ids,"revision":c.config.revision}))
            }
            _ => Err("unknown provisioning operation".into()),
        }
    })()
    .unwrap_or_else(|e| error(&e));
    reply.to_string()
}

pub fn start(engine: Arc<Mutex<crate::pon_engine::PonEngine>>) {
    {
        let mut c = controller().lock().unwrap();
        match config::load() {
            Ok(cfg) => c.config = cfg,
            Err(e) => c.failure = Some(e),
        }
        if let Ok(data) = std::fs::read("/var/lib/fttr/provisioning-state.json") {
            if let Ok(v) = serde_json::from_slice::<Value>(&data) {
                if let Some(owned) = v.get("owned") {
                    if let Ok(owned) = serde_json::from_value(owned.clone()) {
                        c.owned = owned;
                    }
                }
                if let Some(a) = v.as_array().or_else(|| v["operations"].as_array()) {
                    for operation in a {
                        let mut operation = operation.clone();
                        c.next_id = c
                            .next_id
                            .max(operation["operation_id"].as_u64().unwrap_or(0));
                        if operation["state"] != "completed" {
                            operation["state"] = json!("interrupted");
                        }
                        if let Some(id) = operation["device"].as_str().map(str::to_string) {
                            c.recovered.insert(id, operation);
                        }
                    }
                }
            }
        }
        let identities: Vec<_> = c.config.devices.iter().map(|d| d.cwmp_id.clone()).collect();
        for id in identities {
            match wan::load(&id) {
                Ok(Some(owned)) => {
                    c.owned.insert(id.clone(), owned);
                }
                Ok(None) => (),
                Err(e) => c.failure = Some(e),
            }
            match wifi::load(&id) {
                Ok(owned) => {
                    c.wifi_owned.insert(id, owned);
                }
                Err(e) => c.failure = Some(e),
            }
        }
    }
    std::thread::spawn(move || loop {
        // Copy optical preconditions, release the PON lock before any ACS access.
        let online: BTreeSet<_> = engine
            .lock()
            .unwrap()
            .active_onus
            .iter()
            .filter(|o| o.state == crate::pon_engine::ploam::OnuState::O5Operation)
            .map(|o| o.full_sn_str())
            .collect();
        let devices = acs(json!({"op":"devices"}));
        let mut c = controller().lock().unwrap();
        for d in &mut c.config.devices {
            if d.cwmp_id.starts_with("pending:") {
                if let Some(id) = discovered_identity(&devices, &d.optical_sn, &d.adapter) {
                    d.cwmp_id = id;
                }
            }
        }
        let config = c.config.clone();
        let time = now();
        let periodic = config.interval > 0
            && time.saturating_sub(c.last_periodic) >= config.interval
            && config.triggers.contains("periodic");
        if periodic {
            c.last_periodic = time;
        }
        let mut fresh = BTreeSet::new();
        for d in &config.devices {
            let observed = &devices["devices"][&d.cwmp_id];
            let at = observed["last_inform"].as_u64();
            let reachable = online.contains(&d.optical_sn)
                && at.is_some_and(|at| time.saturating_sub(at) <= config.freshness);
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
            let previous = c
                .operations
                .get(&d.id)
                .map(Operation::view)
                .or_else(|| c.recovered.get(&d.id).cloned());
            let needs_manual = previous.as_ref().is_some_and(|p| {
                p["state"] == "interrupted"
                    || ["wan_lifecycle", "wifi_lifecycle"].iter().any(|key| {
                        p[*key]
                            .as_object()
                            .is_some_and(|m| m.values().any(|i| i["state"] == "unknown"))
                    })
                    || p["items"].as_array().is_some_and(|a| {
                        a.iter().any(|i| {
                            matches!(
                                i["state"].as_str(),
                                Some("unknown" | "accepted_pending_apply" | "accepted_unverified")
                            )
                        })
                    })
            });
            if !needs_manual
                && reachable
                && config.enabled
                && config.automatic
                && (periodic
                    || (config.triggers.contains("device_online")
                        && !c.last_online.contains(&d.id))
                    || (new_boot && config.triggers.contains("cwmp_boot")))
            {
                let _ = c.start(&d.id, true, false);
            }
            if let Some(op) = c.operations.get_mut(&d.id) {
                if op.device.cwmp_id.starts_with("pending:") && !d.cwmp_id.starts_with("pending:") {
                    op.device.cwmp_id = d.cwmp_id.clone();
                    match (wan::load(&d.cwmp_id), wifi::load(&d.cwmp_id)) {
                        (Ok(owned), Ok(registry)) => {
                            let cleanup = op.wan.full_sync();
                            op.wan = wan::Reconciler::new(
                                &op.device,
                                owned.unwrap_or_default(),
                                cleanup,
                            );
                            op.wifi = wifi::Reconciler::new(&op.device, registry, cleanup);
                        }
                        _ => {
                            op.stage = OperationPhase::Completed;
                            op.wan.results.insert(
                                "identity".into(),
                                json!({"state":"blocked","reason":"cannot load device ownership"}),
                            );
                        }
                    }
                }
                if op.stage != OperationPhase::Completed {
                    let ready = op
                        .wan
                        .tick(&mut op.device, op.apply, op.management, reachable);
                    if let Err(error) = op.wan.save(&op.device.cwmp_id) {
                        op.wan.results.insert(
                            "persistence".into(),
                            json!({"state":"blocked","reason":error}),
                        );
                        op.stage = OperationPhase::Completed;
                    } else if ready {
                        op.wan.block_unresolved(&mut op.device);
                        let finish = op.index >= op.device.items.len();
                        if finish
                            && op
                                .wan
                                .results
                                .get("final_inventory")
                                .is_some_and(|r| r["state"] == "blocked")
                        {
                            for item in op.device.items.iter().filter(|i| i.wan_ref.is_some()) {
                                if let Some(result) = op.results.get_mut(&item.id) {
                                    result["state"] = json!("blocked");
                                    result["reason"]=json!("final full WAN inventory does not match ordered configuration");
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
                                    .is_some_and(|r| r["state"] == "verified")
                            });
                        if finish && op.wan.begin_final_verification(op.apply) {
                            op.stage = OperationPhase::WanVerifying;
                        } else if op
                            .wifi
                            .tick(&op.device, op.apply, reachable, finish, verified)
                        {
                            if finish {
                                op.stage = OperationPhase::Completed;
                            } else {
                                op.wifi.block_unresolved(&mut op.device);
                                if matches!(
                                    op.stage,
                                    OperationPhase::WanReconciling
                                        | OperationPhase::WifiReconciling
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
                                r["state"] == "verified" && r["revision"] == op.revision
                            }) {
                                if let Some(owned) = op.wan.owned.get_mut(&item.id) {
                                    if let Some(p) = item.parameters.iter().find(|p| {
                                        p.name.ends_with("ServiceList")
                                            || p.name.ends_with("SERVICELIST")
                                    }) {
                                        owned.service_list = p.value.clone();
                                    }
                                }
                            }
                        }
                    } else {
                        op.stage = OperationPhase::WanReconciling;
                    }
                    if now().saturating_sub(op.started) >= 900
                        && op.stage != OperationPhase::Completed
                        && acs(json!({"op":"busy","device":op.device.cwmp_id}))["busy"] != true
                    {
                        op.wan.results.insert("timeout".into(),json!({"state":"unknown","reason":"operation deadline exceeded; review outstanding WAN/configuration state before retry"}));
                        op.stage = OperationPhase::Completed;
                    }
                }
                if let Err(error) = op.wan.save(&op.device.cwmp_id) {
                    op.wan.results.insert(
                        "persistence".into(),
                        json!({"state":"blocked","reason":error}),
                    );
                }
                if let Err(error) = op.wifi.save(&op.device.cwmp_id) {
                    op.wifi.results.insert(
                        "persistence".into(),
                        json!({"state":"blocked","reason":error}),
                    );
                    op.stage = OperationPhase::Completed;
                }
                let wifi_owned = op.wifi.registry.clone();
                c.wifi_owned.insert(d.cwmp_id.clone(), wifi_owned);
                let op = c.operations.get(&d.id).unwrap();
                let owned = op.wan.owned.clone();
                c.owned.insert(d.cwmp_id.clone(), owned);
            }
        }
        c.last_online = fresh;
        let mut history = c.recovered.clone();
        for (id, o) in &c.operations {
            history.insert(id.clone(), o.view());
        }
        let saved =
            json!({"operations":history.values().collect::<Vec<_>>(),"owned":c.owned}).to_string();
        if saved != c.saved {
            let result = super::storage::write_atomic(
                "/var/lib/fttr/provisioning-state.json",
                saved.as_bytes(),
                false,
            );
            match result {
                Ok(()) => c.saved = saved,
                Err(e) => c.failure = Some(format!("provisioning state persistence: {e}")),
            }
        }
        drop(c);
        std::thread::sleep(Duration::from_secs(1));
    });
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hwtc_identity_matches_serial_across_models() {
        for model in ["K155-11", "V152", "OTHER-HWTC"] {
            let devices =
                json!({"devices":{"child":{"product_class":model,"serial":"485754433c6de5b3"}}});
            assert_eq!(
                discovered_identity(&devices, "HWTC3C6DE5B3", "hwtc"),
                Some("child".into())
            );
            assert_eq!(discovered_identity(&devices, "HWTC3E5E5EB6", "hwtc"), None);
        }
    }
    #[test]
    fn hwtc_identity_rejects_other_vendors_and_ambiguous_serials() {
        let devices =
            json!({"devices":{"child":{"product_class":"V152","serial":"483343543C6DE5B3"}}});
        assert_eq!(discovered_identity(&devices, "H3CT3C6DE5B3", "hwtc"), None);
        let devices = json!({"devices":{
            "one":{"product_class":"V152","serial":"485754433C6DE5B3"},
            "two":{"product_class":"K155-11","serial":"485754433C6DE5B3"}
        }});
        assert_eq!(discovered_identity(&devices, "HWTC3C6DE5B3", "hwtc"), None);
    }
    #[test]
    fn bool_and_binding_equivalence() {
        let mut p = Parameter {
            name: "Enable".into(),
            value: "1".into(),
            kind: ParameterKind::Boolean,
            secret: false,
        };
        assert!(equivalent(&p, "true"));
        assert!(!equivalent(&p, "false"));
        p.name = "X_CU_LanInterface".into();
        p.kind = ParameterKind::String;
        p.value = "a,b".into();
        assert!(equivalent(&p, "b,a"));
        let root = "InternetGatewayDevice.LANDevice.1.WLANConfiguration.";
        p.value = format!("{root}2,{root}6");
        assert!(equivalent(
            &p,
            &format!("{root}2,{root}6,{root}10,{root}14")
        ));
        assert!(!equivalent(&p, &format!("{root}2,{root}6,{root}11")));
    }
    fn operation(stage: OperationPhase, secret: bool) -> Operation {
        let item = Item {
            id: "wifi".into(),
            kind: ItemKind::Ssid,
            service_type: String::new(),
            guard: None,
            blocked: None,
            wan_ref: None,
            auto_instance: None,
            parameters: vec![Parameter {
                name: "InternetGatewayDevice.LANDevice.1.WLANConfiguration.1.SSID".into(),
                value: "wanted".into(),
                kind: ParameterKind::String,
                secret,
            }],
        };
        let mut o = Operation::new(
            1,
            Device {
                id: "child".into(),
                optical_sn: "H3CT12345678".into(),
                cwmp_id: "id".into(),
                adapter: "hl202".into(),
                items: vec![item],
                wifi_cleanup: "preserve".into(),
                wifi_preserve: BTreeSet::new(),
            },
            "rev".into(),
            true,
            false,
        );
        o.stage = stage;
        o.task = Some(1);
        o
    }
    fn responses(v: Vec<Value>) {
        RPC.with(|r| *r.borrow_mut() = (v.into(), Vec::new()));
    }
    fn readback(value: &str) -> Value {
        json!({"state":"completed","result":{"parameters":[{"name":"InternetGatewayDevice.LANDevice.1.WLANConfiguration.1.SSID","value":value}]}})
    }
    #[test]
    fn unknown_write_is_read_before_any_retry() {
        let mut o = operation(OperationPhase::Writing, false);
        responses(vec![
            json!({"state":"unknown"}),
            json!({"id":2}),
            readback("old"),
        ]);
        advance(&mut o, true);
        assert_eq!(o.stage, OperationPhase::Verifying);
        advance(&mut o, true);
        assert_eq!(o.results["wifi"]["state"], "unknown");
        RPC.with(|r| assert_eq!(r.borrow().1[1]["method"], "GetParameterValues"));
    }
    #[test]
    fn status_one_requires_matching_readback() {
        let mut o = operation(OperationPhase::Writing, false);
        responses(vec![
            json!({"state":"accepted_pending_apply"}),
            json!({"id":2}),
            readback("old"),
        ]);
        advance(&mut o, true);
        advance(&mut o, true);
        assert_eq!(o.results["wifi"]["state"], "accepted_pending_apply");
    }
    #[test]
    fn credential_write_does_not_require_readback() {
        let mut o = operation(OperationPhase::Writing, true);
        let key = Parameter {
            name:
                "InternetGatewayDevice.LANDevice.1.WLANConfiguration.1.PreSharedKey.1.KeyPassphrase"
                    .into(),
            value: "secret12".into(),
            kind: ParameterKind::String,
            secret: true,
        };
        o.device.items[0].parameters = vec![key.clone()];
        o.changes = vec![key];
        responses(vec![
            json!({"state":"completed"}),
            json!({"id":2}),
            json!({"state":"completed","result":{"parameters":[]}}),
        ]);
        advance(&mut o, true);
        advance(&mut o, true);
        assert_eq!(o.results["wifi"]["state"], "verified");
        assert!(!o.view().to_string().contains("wanted"));
    }
    #[test]
    fn hl202_passphrase_fault_retries_vendor_keypassphrase() {
        let mut o = operation(OperationPhase::Writing, true);
        let secret = Parameter {
            name:
                "InternetGatewayDevice.LANDevice.1.WLANConfiguration.2.PreSharedKey.1.KeyPassphrase"
                    .into(),
            value: "secret12".into(),
            kind: ParameterKind::String,
            secret: true,
        };
        o.device.items[0].parameters = vec![secret.clone()];
        o.changes = vec![secret];
        let fault = json!({"state":"fault","result":{"fault_code":"9003","parameters":[{"name":"InternetGatewayDevice.LANDevice.1.WLANConfiguration.2.PreSharedKey.1.KeyPassphrase","fault_code":"9007","fault_string":"Invalid parameter value"}]}});
        let retried = json!({"id":9});
        let ok = json!({"state":"completed"});
        let read_ok = json!({"id":10});
        let readback = json!({"state":"completed","result":{"parameters":[{"name":"InternetGatewayDevice.LANDevice.1.WLANConfiguration.2.PreSharedKey.1.KeyPassphrase","value":""},{"name":"InternetGatewayDevice.LANDevice.1.WLANConfiguration.2.KeyPassphrase","value":"secret12"}]}});
        responses(vec![fault, retried, ok, read_ok, readback]);
        advance(&mut o, true);
        assert_eq!(o.stage, OperationPhase::Writing);
        assert_eq!(o.task, Some(9));
        RPC.with(|r| {
            let b = r.borrow();
            let q = b.1.last().unwrap();
            assert_eq!(q["method"], "SetParameterValues");
            assert_eq!(
                q["args"]["parameters"][0]["name"],
                "InternetGatewayDevice.LANDevice.1.WLANConfiguration.2.KeyPassphrase"
            );
        });
        advance(&mut o, true);
        advance(&mut o, true);
        assert_eq!(o.results["wifi"]["state"], "verified");
    }
    #[test]
    fn hwtc_passphrase_fault_does_not_retry() {
        let mut o = operation(OperationPhase::Writing, true);
        o.device.adapter = "hwtc".into();
        o.changes = vec![Parameter {
            name:
                "InternetGatewayDevice.LANDevice.1.WLANConfiguration.1.PreSharedKey.1.KeyPassphrase"
                    .into(),
            value: "secret12".into(),
            kind: ParameterKind::String,
            secret: true,
        }];
        let fault = json!({"state":"fault","result":{"fault_code":"9003","parameters":[{"name":"InternetGatewayDevice.LANDevice.1.WLANConfiguration.1.PreSharedKey.1.KeyPassphrase","fault_code":"9007"}]}});
        responses(vec![fault]);
        advance(&mut o, true);
        assert_eq!(o.results["wifi"]["state"], "failed");
    }
    #[test]
    fn management_wan_is_never_converted_to_business_bridge() {
        let mut o = operation(OperationPhase::Reading, false);
        o.device.items[0].guard = Some("guard".into());
        o.device.items[0].service_type = "internet".into();
        let mut reply = readback("old");
        reply["result"]["parameters"]
            .as_array_mut()
            .unwrap()
            .push(json!({"name":"guard","value":"TR069_INTERNET"}));
        responses(vec![reply]);
        advance(&mut o, true);
        assert_eq!(o.results["wifi"]["state"], "blocked");
    }
    #[test]
    fn wan_changes_disable_first_and_activation_requires_bindings() {
        let mut base = operation(OperationPhase::Reading, false);
        let root = "InternetGatewayDevice.WANDevice.1.WANConnectionDevice.4.WANIPConnection.1.";
        let service = Item {
            id: "wan".into(),
            kind: ItemKind::DeviceService,
            service_type: "internet".into(),
            guard: None,
            blocked: None,
            wan_ref: Some("wan".into()),
            auto_instance: None,
            parameters: vec![
                Parameter {
                    name: format!("{root}Enable"),
                    value: "1".into(),
                    kind: ParameterKind::Boolean,
                    secret: false,
                },
                Parameter {
                    name: format!("{root}ConnectionType"),
                    value: "IP_Bridged".into(),
                    kind: ParameterKind::String,
                    secret: false,
                },
            ],
        };
        let mut binding = service.clone();
        binding.id = "binding".into();
        binding.kind = ItemKind::PortBinding;
        binding.parameters = vec![Parameter {
            name: format!("{root}X_CU_LanInterface"),
            value: "wanted".into(),
            kind: ParameterKind::String,
            secret: false,
        }];
        base.device.items = vec![service, binding];
        let mut o = Operation::new(1, base.device, "rev".into(), true, false);
        o.stage = OperationPhase::Reading;
        o.task = Some(1);
        assert_eq!(o.device.items.last().unwrap().kind, ItemKind::WanActivation);
        responses(vec![
            json!({"state":"completed","result":{"parameters":[{"name":format!("{root}Enable"),"value":"1"},{"name":format!("{root}ConnectionType"),"value":"IP_Routed"}]}}),
            json!({"id":2}),
        ]);
        advance(&mut o, true);
        assert_eq!(o.stage, OperationPhase::Disabling);
        RPC.with(|r| {
            let b = r.borrow();
            let parameters = b.1.last().unwrap()["args"]["parameters"]
                .as_array()
                .unwrap();
            assert_eq!(parameters.len(), 1);
            assert_eq!(parameters[0]["name"], format!("{root}Enable"));
            assert_eq!(parameters[0]["value"], "0");
        });
        o.index = 2;
        o.stage = OperationPhase::Waiting;
        o.task = None;
        o.results
            .insert("wan".into(), json!({"state":"verified","revision":"rev"}));
        o.results
            .insert("binding".into(), json!({"state":"failed","revision":"rev"}));
        responses(vec![json!({"busy":false})]);
        advance(&mut o, true);
        assert_eq!(o.results["wan__activation"]["state"], "blocked");
        assert_eq!(o.results["wan"]["state"], "blocked");
    }
    #[test]
    fn matching_active_wan_is_not_disabled_on_every_sync() {
        let mut o = operation(OperationPhase::Reading, false);
        let root = "InternetGatewayDevice.WANDevice.1.WANConnectionDevice.4.WANIPConnection.1.";
        let item = &mut o.device.items[0];
        item.id = "wan".into();
        item.kind = ItemKind::DeviceService;
        item.wan_ref = Some("wan".into());
        item.parameters = vec![
            Parameter {
                name: format!("{root}Enable"),
                value: "1".into(),
                kind: ParameterKind::Boolean,
                secret: false,
            },
            Parameter {
                name: format!("{root}ConnectionType"),
                value: "IP_Bridged".into(),
                kind: ParameterKind::String,
                secret: false,
            },
        ];
        let mut o = Operation::new(1, o.device, "rev".into(), true, false);
        o.stage = OperationPhase::Reading;
        o.task = Some(1);
        responses(vec![
            json!({"state":"completed","result":{"parameters":[{"name":format!("{root}Enable"),"value":"1"},{"name":format!("{root}ConnectionType"),"value":"IP_Bridged"}]}}),
        ]);
        advance(&mut o, true);
        assert_eq!(o.results["wan"]["state"], "verified");
        RPC.with(|r| {
            assert!(r
                .borrow()
                .1
                .iter()
                .all(|v| v["method"] != "SetParameterValues"))
        });
    }
}
