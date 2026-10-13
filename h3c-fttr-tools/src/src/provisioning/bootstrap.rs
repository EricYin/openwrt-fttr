//! Per-device ACS bootstrap; MQTT, settings verification and Inform are independent evidence.
use super::model::{Device, Management};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

const STATE_FILE: &str = "/var/lib/fttr/acs-bootstrap.json";
const TRANSPORT_FRESHNESS: u64 = 60;

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Phase {
    #[default]
    WaitingTransport,
    WaitingIdentity,
    Reading,
    Configuring,
    Verifying,
    WaitingInform,
    Ready,
    RetryWait,
    ManualRequired,
    NotConfigured,
    WaitingAcs,
    AwaitingApply,
    DiscoveringManagementWan,
    ConfiguringManagementWan,
    VerifyingManagementWan,
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default)]
struct Record {
    optical_sn: String,
    cwmp_id: String,
    mqtt_target: Option<String>,
    binding_source: String,
    method: String,
    desired_version: String,
    generation: u64,
    state: Phase,
    reason: String,
    attempts: u32,
    next_retry_at: u64,
    updated_at: u64,
    settings_verified_at: Option<u64>,
    last_inform: Option<u64>,
    expected_acs_url:String,
    last_error: Option<String>,
    transport_epoch: u64,
    completed_authorization: u64,
    #[serde(skip)]
    worker: Option<u64>,
    #[serde(skip)]
    initial_inform: Option<Option<u64>>,
    #[serde(skip)]
    optical_online: bool,
    #[serde(skip)]
    require_inform_after: u64,
    #[serde(skip)]
    last_observed_time: u64,
    #[serde(skip)]
    removed: bool,
}

#[derive(Default)]
struct Transport {
    last_seen: u64,
    epoch: u64,
}

#[derive(Default)]
struct Coordinator {
    records: BTreeMap<String, Record>,
    transport: BTreeMap<String, Transport>,
    links: BTreeMap<String, String>,
    link_error: Option<String>,
    saved: Vec<u8>,
    persistence_error: Option<String>,
}

static COORDINATOR: OnceLock<Mutex<Coordinator>> = OnceLock::new();
fn coordinator() -> &'static Mutex<Coordinator> {
    COORDINATOR.get_or_init(|| {
        let mut c = Coordinator::default();
        match std::fs::read(STATE_FILE) {
            Ok(bytes) => match serde_json::from_slice::<BTreeMap<String, Record>>(&bytes) {
                Ok(records) => c.records = records,
                Err(e) => c.persistence_error = Some(format!("bootstrap state decode: {e}")),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => c.persistence_error = Some(format!("bootstrap state read: {e}")),
        }
        for r in c.records.values_mut() {
            r.state = Phase::WaitingTransport;
            r.reason = "daemon restarted; fresh transport and Inform required".into();
            r.next_retry_at = 0;
        }
        Mutex::new(c)
    })
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn normalize_target(topic: &str) -> Option<String> {
    let value = topic
        .strip_suffix("/fttr_data.tar.gz")
        .unwrap_or(topic)
        .replace(':', "");
    (value.len() == 12 && value.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| value.to_ascii_uppercase())
}

pub(crate) fn observe_mqtt(topic: &str, time: u64) {
    if let Some(target) = normalize_target(topic) {
        let mut c = coordinator().lock().unwrap();
        let t = c.transport.entry(target).or_default();
        if t.last_seen == 0 || time.saturating_sub(t.last_seen) > TRANSPORT_FRESHNESS {
            t.epoch = t.epoch.saturating_add(1);
        }
        t.last_seen = time;
    }
}

pub(super) fn observe_onu_links(result: Result<BTreeMap<String, String>, String>) {
    let mut c = coordinator().lock().unwrap();
    match result {
        Ok(links) => {
            c.links = links;
            c.link_error = None;
        }
        Err(e) => {
            c.links.clear();
            c.link_error = Some(e);
        }
    }
}

fn retry_delay(attempts: u32) -> u64 {
    (5u64.saturating_mul(1u64 << attempts.saturating_sub(1).min(6))).min(300)
}

fn version(device: &Device, management: Option<&Management>, target: Option<&str>) -> String {
    let bytes = serde_json::to_vec(&(
        device.optical_sn.as_str(),
        device.cwmp_id.as_str(),
        device.adapter.as_str(),
        management,
        target,
    ))
    .unwrap_or_default();
    let hash = bytes.iter().fold(0xcbf29ce484222325u64, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x100000001b3)
    });
    format!("{hash:016x}")
}

impl Record {
    fn invalidate(&mut self, time: u64, reason: &str) {
        self.generation = self.generation.saturating_add(1);
        self.settings_verified_at = None;
        self.require_inform_after = time;
        self.attempts = 0;
        self.next_retry_at = 0;
        self.last_error = None;
        self.set_phase(Phase::WaitingTransport, time, reason);
    }
    fn set_phase(&mut self, state: Phase, time: u64, reason: &str) {
        if self.state != state || self.reason != reason {
            self.state = state;
            self.reason = reason.into();
            self.updated_at = time;
        }
    }
    fn fresh_inform(&self, at: Option<u64>, time: u64, freshness: u64, online: bool) -> bool {
        online
            && at.is_some_and(|at| {
                at <= time
                    && time.saturating_sub(at) <= freshness
                    && at >= self.require_inform_after
                    && self
                        .initial_inform
                        .is_some_and(|initial| initial != Some(at))
            })
    }
    fn finish(&mut self, generation: u64, result: Result<bool, String>, time: u64) {
        if self.worker == Some(generation) {
            self.worker = None;
        }
        if self.generation != generation || self.state == Phase::Ready {
            return;
        }
        match result {
            Ok(true) => {
                self.settings_verified_at = Some(time);
                self.last_error = None;
                self.next_retry_at = time.saturating_add(60);
                self.set_phase(Phase::WaitingInform, time, "CWMP settings verified; waiting for a fresh Inform to confirm ACS reachability");
            }
            Ok(false) => (),
            Err(e) => {
                self.last_error = Some(e);
                self.next_retry_at = time.saturating_add(retry_delay(self.attempts));
                self.set_phase(Phase::RetryWait, time, "bootstrap attempt failed or has unknown outcome; retry will read current settings first");
            }
        }
    }
}

pub(super) struct Observation<'a> {
    pub device: &'a Device,
    pub management: Option<&'a Management>,
    pub last_inform: Option<u64>,
    pub online: bool,
    pub new_boot: bool,
    pub freshness: u64,
    pub time: u64,
    pub acs_available: bool,
    pub authorization: Option<(u64, bool)>,
}

struct Job {
    id: String,
    generation: u64,
    target: String,
    url: String,
    persist_matching: bool,
    vlan: u32,
    priority: u32,
    authorization_epoch:u64,
}

pub(super) fn retain_devices(ids: &std::collections::BTreeSet<String>) {
    let mut c = coordinator().lock().unwrap();
    for (id, r) in &mut c.records {
        if !ids.contains(id) && !r.removed {
            r.invalidate(
                now(),
                "device removed from configuration; obsolete worker must exit",
            );
            r.removed = true;
        }
        if ids.contains(id) {
            r.removed = false;
        }
    }
    c.records
        .retain(|id, r| ids.contains(id) || r.worker.is_some());
    c.persist();
}

/// Scheduler entry point. Remote commands execute outside controller and bootstrap locks.
pub(super) fn tick(observation: Observation<'_>) {
    let mut c = coordinator().lock().unwrap();
    let job = c.schedule(observation);
    c.persist();
    drop(c);
    if let Some(job) = job {
        std::thread::spawn(move || {
            run(
                job.id,
                job.generation,
                job.target,
                job.url,
                job.persist_matching,
                job.vlan,
                job.priority,
                job.authorization_epoch,
            )
        });
    }
}

impl Coordinator {
    fn automatic_target(&self, sn: &str, time: u64) -> Result<Option<String>, &'static str> {
        let candidates: Vec<_> = self
            .transport
            .iter()
            .filter(|(mac, t)| {
                t.last_seen <= time
                    && time.saturating_sub(t.last_seen) <= TRANSPORT_FRESHNESS
                    && self.links.get(*mac).is_some_and(|linked| linked == sn)
            })
            .map(|(mac, _)| mac.clone())
            .collect();
        match candidates.as_slice() {
            [] => Ok(None),
            [mac] => Ok(Some(mac.clone())),
            _ => Err("multiple fresh MQTT targets originate from this ONU; automatic identity is ambiguous"),
        }
    }

    fn schedule(&mut self, observation: Observation<'_>) -> Option<Job> {
        let Observation {
            device,
            management,
            last_inform: at,
            online,
            new_boot,
            freshness,
            time,
            acs_available,
            authorization,
        } = observation;
        let automatic = self.automatic_target(&device.optical_sn, time);
        let automatic_target = automatic.as_ref().ok().and_then(|t| t.as_deref());
        let binding_source = if automatic_target.is_some() {
            "onu_link"
        } else {
            "unresolved"
        };
        let target = automatic_target;
        let transport = target
            .and_then(|t| self.transport.get(t))
            .map(|t| (t.last_seen, t.epoch));
        let r = self.records.entry(device.id.clone()).or_default();
        if r.last_observed_time > time {
            r.invalidate(time, "system clock moved backwards; fresh Inform required");
            r.initial_inform = Some(at);
        }
        r.last_observed_time = time;
        let desired = format!("{}-{}",version(device, management, target),authorization.map(|(epoch,_)|epoch).unwrap_or_default());
        let initialized = !r.desired_version.is_empty();
        if r.initial_inform.is_none() {
            r.initial_inform = Some(at);
        }
        if r.desired_version != desired {
            r.invalidate(
                time,
                "ACS bootstrap configuration or device identity changed",
            );
            r.desired_version = desired;
            r.initial_inform = Some(at);
        }
        r.optical_sn = device.optical_sn.clone();
        r.cwmp_id = device.cwmp_id.clone();
        r.mqtt_target = target.map(str::to_owned);
        r.binding_source = binding_source.into();
        r.method = if device.adapter == "hl202" && cfg!(feature = "device-hl202") {
            "mqtt"
        } else {
            "manual"
        }
        .into();
        if r.optical_online && !online {
            r.invalidate(
                time,
                "optical device offline; previous settings evidence invalidated",
            );
        }
        r.optical_online = online;
        if let Some((_, epoch)) = transport {
            if r.transport_epoch != epoch {
                if r.transport_epoch != 0 {
                    r.invalidate(time, "MQTT transport reappeared; recheck device settings");
                }
                r.transport_epoch = epoch;
            }
        }
        if new_boot && initialized && at != r.last_inform {
            r.invalidate(
                at.unwrap_or(time),
                "device BOOT Inform invalidated previous settings evidence",
            );
        }
        r.last_inform = at;
        r.expected_acs_url=management.map(|m|m.acs_url.clone()).unwrap_or_default();
        // A recent Inform proves reachability, but does not prove that the
        // currently authorized ACS credentials have been written to the CPE.
        // Require a post-readback Inform before declaring the bootstrap ready.
        let settings_verified = device.adapter != "hl202"
            || !cfg!(feature = "device-hl202")
            || r.settings_verified_at.is_some_and(|verified| at.is_some_and(|at| at >= verified));
        let fresh = acs_available
            && settings_verified
            && r.fresh_inform(at, time, freshness, online);
        if r.state == Phase::Ready && !fresh {
            r.invalidate(
                time,
                "previous Inform expired; fresh management evidence required",
            );
        }
        if fresh {
            if let Some((epoch, _))=authorization {r.completed_authorization=epoch;}
            r.set_phase(
                Phase::Ready,
                time,
                "fresh CWMP Inform received; management WAN bootstrap is unnecessary",
            );
            r.next_retry_at = 0;
            r.last_error = None;
        } else if r.worker.is_some() {
            // Keep the slot until an obsolete worker exits; never overlap per-device commands.
        } else if management.is_none() {
            r.set_phase(
                Phase::NotConfigured,
                time,
                "management ACS URL is not configured",
            );
        } else if !acs_available {
            r.set_phase(
                Phase::WaitingAcs,
                time,
                "local ACS is disabled, unavailable or has a listener error",
            );
        } else if authorization.is_none_or(|(epoch,automatic)|!automatic && (epoch==0 || epoch==r.completed_authorization)) {
            r.set_phase(Phase::AwaitingApply,time,"management drift or reset detected; apply management configuration to authorize recovery");
        } else if device.adapter != "hl202" || !cfg!(feature = "device-hl202") {
            r.set_phase(Phase::ManualRequired, time, "automatic ACS bootstrap is unavailable for this adapter; follow the current management settings");
        } else if target.is_none() {
            r.set_phase(Phase::WaitingIdentity, time, automatic.err().unwrap_or("waiting for a unique fresh MQTT source MAC learned on this device's online ONU interface"));
        } else if !online
            || transport.is_none_or(|(seen, _)| {
                seen > time || time.saturating_sub(seen) > TRANSPORT_FRESHNESS
            })
        {
            r.set_phase(
                Phase::WaitingTransport,
                time,
                "waiting for optical online and fresh MQTT telemetry from the associated device",
            );
        } else if let (Some(target), Some(management)) = (target, management) {
            if time >= r.next_retry_at {
                r.attempts = r.attempts.saturating_add(1);
                r.worker = Some(r.generation);
                r.set_phase(
                    Phase::Reading,
                    time,
                    "reading current CWMP settings before any conditional write",
                );
                return Some(Job {
                    id: device.id.clone(),
                    generation: r.generation,
                    target: target.to_owned(),
                    url: management.acs_url.clone(),
                    persist_matching: r.settings_verified_at.is_none() || r.last_error.is_some(),
                    vlan: management.vlan,
                    priority: management.priority,
                    authorization_epoch:authorization.map(|(epoch,_)|epoch).unwrap_or_default(),
                });
            }
            if r.state != Phase::WaitingInform {
                r.set_phase(
                    Phase::RetryWait,
                    time,
                    "retry scheduled; the next attempt starts with settings readback",
                );
            }
        }
        None
    }
}

impl Coordinator {
    fn persist(&mut self) {
        let bytes = serde_json::to_vec(&self.records).unwrap_or_default();
        if bytes != self.saved {
            match super::storage::write_atomic(STATE_FILE, &bytes, true) {
                Ok(()) => {
                    self.saved = bytes;
                    self.persistence_error = None;
                }
                Err(e) => {
                    self.persistence_error = Some(format!("bootstrap state persistence: {e}"))
                }
            }
        }
    }
}

fn progress(id: &str, generation: u64, state: Phase, reason: &str) -> bool {
    let mut c = coordinator().lock().unwrap();
    let Some(r) = c.records.get_mut(id) else {
        return false;
    };
    if r.generation != generation || r.worker != Some(generation) || r.state == Phase::Ready {
        return false;
    }
    if matches!(state,Phase::Configuring|Phase::ConfiguringManagementWan) {
        r.settings_verified_at = None;
    }
    r.set_phase(state, now(), reason);
    c.persist();
    true
}

fn run(id: String, generation: u64, target: String, url: String, persist_matching: bool, vlan: u32, priority: u32, authorization_epoch:u64) {
    let result = (|| {
        let credentials=super::management_network::credentials(authorization_epoch).ok_or("management authorization changed before execution")?;
        if !progress(&id, generation, Phase::DiscoveringManagementWan, "discovering the dedicated TR069 management WAN") { return Ok(false); }
        crate::devices::ensure_mqtt_management_wan(&target, vlan, priority,persist_matching,|stage|super::management_network::credentials(authorization_epoch).is_some()&&match stage {
            crate::devices::ManagementWanStage::Configuring=>progress(&id,generation,Phase::ConfiguringManagementWan,"conditionally configuring and persisting the dedicated TR069 WAN"),
            crate::devices::ManagementWanStage::Verifying=>progress(&id,generation,Phase::VerifyingManagementWan,"reading back management VLAN, routing, DHCP and unbound interfaces"),
        })?;
        if !progress(&id,generation,Phase::Reading,"reading CWMP configuration after management WAN verification") {return Ok(false);}
        let matched = crate::devices::read_authenticated_cwmp_settings(&target, &url,&credentials)?;
        if !matched || persist_matching {
            if !progress(
                &id,
                generation,
                Phase::Configuring,
                "conditionally correcting and persisting CWMP settings",
            ) {
                return Ok(false);
            }
            if super::management_network::credentials(authorization_epoch).is_none() {return Ok(false);}
            crate::devices::configure_authenticated_cwmp_settings(&target, &url,&credentials)?;
        }
        if !progress(
            &id,
            generation,
            Phase::Verifying,
            "independently reading back CWMP settings",
        ) {
            return Ok(false);
        }
        if !crate::devices::read_authenticated_cwmp_settings(&target, &url,&credentials)? {
            return Err("CWMP settings do not match after configuration".into());
        }
        Ok(true)
    })();
    let mut c = coordinator().lock().unwrap();
    if let Some(r) = c.records.get_mut(&id) {
        r.finish(generation, result, now());
    }
    c.persist();
}

fn public_url(url: &str) -> String {
    match url.split_once("://") {
        Some((scheme, rest)) => {
            let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
            let host = authority
                .rsplit_once('@')
                .map_or(authority, |(_, host)| host);
            format!("{scheme}://{host}/{path}")
        }
        None => url.into(),
    }
}

pub(super) fn is_ready(id: &str) -> bool {
    coordinator()
        .lock()
        .unwrap()
        .records
        .get(id)
        .is_some_and(|r| r.state == Phase::Ready)
}
pub(super) fn is_ready_for_management(id:&str,sn:&str,url:&str,epoch:u64)->bool {coordinator().lock().unwrap().records.get(id).is_some_and(|r|r.state==Phase::Ready&&r.optical_sn==sn&&r.expected_acs_url==url&&r.completed_authorization==epoch)}
pub(super) fn omci_write_allowed(id:&str,epoch:u64,automatic:bool)->bool {let c=coordinator().lock().unwrap();c.records.get(id).is_none_or(|r|r.state!=Phase::Ready&&(automatic||(epoch!=0&&r.completed_authorization!=epoch)))}

pub(super) fn view(id: &str, management: Option<&Management>, acs: &Value) -> Value {
    let c = coordinator().lock().unwrap();
    let Some(r) = c.records.get(id) else {
        return json!({"state":"pending","reason":"bootstrap observation not started"});
    };
    let mut view = serde_json::to_value(r).unwrap_or_default();
    view["inflight"] = json!(r.worker.is_some());
    view["persistence_error"] = json!(c.persistence_error);
    view["link_identity_error"] = json!(c.link_error);
    view["listener"] = json!({"enabled":acs["enabled"],"listen":acs["listen"],"path":acs["path"],"error":acs["error"]});
    view["mqtt_last_seen"] = json!(r
        .mqtt_target
        .as_ref()
        .and_then(|t| c.transport.get(t))
        .map(|t| t.last_seen));
    if let Some(m) = management {
        let url = public_url(&m.acs_url);
        view["desired"] = json!({"vlan":m.vlan,"priority":m.priority,"acs_url":url});
        if let (Some(listen), Some(path)) = (acs["listen"].as_str(), acs["path"].as_str()) {
            let expected = format!("http://{listen}{path}");
            if url != expected {
                view["endpoint_notice"] = json!(format!("Configured ACS URL {url} differs from the local listener {expected}; confirm routing or forwarding and wait for a fresh Inform."));
            }
        }
        view["manual_steps"] = if r.state == Phase::Ready {
            json!([])
        } else {
            json!([
            format!("If no existing management path reaches the ACS, configure TR069 VLAN {} with priority {}; retain a working management WAN.", m.vlan, m.priority),
            format!("Enable CWMP and periodic Inform; set ACS URL to {url}."),
            "Confirm a new Inform reaches this ACS; MQTT online alone is not CWMP verification."
        ])
        };
    }
    view
}

#[cfg(test)]
mod tests {
    use super::*;
    fn device(id: &str, adapter: &str) -> Device {
        Device {
            id: id.into(),
            optical_sn: "H3CT12345678".into(),
            cwmp_id: id.into(),
            adapter: adapter.into(),
            omci_management: false,
            items: Vec::new(),
            wifi_cleanup: "preserve".into(),
            wifi_preserve: Default::default(),
        }
    }
    fn schedule(
        c: &mut Coordinator,
        d: &Device,
        m: &Management,
        learned_target: Option<&str>,
        time: u64,
        at: Option<u64>,
    ) -> Option<Job> {
        if let Some(mac) = learned_target {
            c.links.insert(mac.into(), d.optical_sn.clone());
        }
        c.schedule(Observation {
            device: d,
            management: Some(m),
            last_inform: at,
            online: true,
            new_boot: false,
            freshness: 180,
            time,
            acs_available: true,
            authorization: Some((1,true)),
        })
    }
    #[cfg(feature = "device-hl202")]
    #[test]
    fn manual_authorization_is_consumed_by_fresh_inform_and_reset_needs_a_new_grant() {
        let mut c=Coordinator::default();let d=device("manual","hl202");let m=Management{vlan:46,priority:0,acs_url:"http://192.0.2.1:7547/cwmp".into()};let mac="02AABBCCDDEE";
        c.links.insert(mac.into(),d.optical_sn.clone());c.transport.insert(mac.into(),Transport{last_seen:100,epoch:1});
        let observe=|c:&mut Coordinator,time,at,new_boot,epoch|c.schedule(Observation{device:&d,management:Some(&m),last_inform:at,online:true,new_boot,freshness:180,time,acs_available:true,authorization:Some((epoch,false))});
        let first=observe(&mut c,100,None,false,1).unwrap();c.records.get_mut(&d.id).unwrap().finish(first.generation,Ok(true),101);assert!(observe(&mut c,103,Some(102),false,1).is_none());assert_eq!(c.records[&d.id].state,Phase::Ready);
        assert!(observe(&mut c,104,None,true,1).is_none());assert_eq!(c.records[&d.id].state,Phase::AwaitingApply);
        let next=observe(&mut c,105,None,false,2).unwrap();assert_eq!(next.authorization_epoch,2);assert_ne!(next.generation,first.generation);
    }
    #[cfg(feature = "device-hl202")]
    #[test]
    fn existing_inform_does_not_skip_mqtt_settings_readback_and_post_write_inform_is_required() {
        let mut c=Coordinator::default();let d=device("manual","hl202");let m=Management{vlan:46,priority:0,acs_url:"http://192.0.2.1:7547/cwmp".into()};let mac="02AABBCCDDEE";
        c.links.insert(mac.into(),d.optical_sn.clone());c.transport.insert(mac.into(),Transport{last_seen:100,epoch:1});
        c.records.insert(d.id.clone(),Record{desired_version:format!("{}-1",version(&d,Some(&m),Some(mac))),optical_sn:d.optical_sn.clone(),initial_inform:Some(Some(90)),..Default::default()});
        let first=schedule(&mut c,&d,&m,Some(mac),100,Some(95)).expect("fresh existing Inform must not skip credential readback");
        c.records.get_mut(&d.id).unwrap().finish(first.generation,Ok(true),101);
        assert!(schedule(&mut c,&d,&m,Some(mac),102,Some(95)).is_none());
        assert_ne!(c.records[&d.id].state,Phase::Ready,"an Inform predating settings verification cannot confirm the new credentials");
        assert!(schedule(&mut c,&d,&m,Some(mac),103,Some(102)).is_none());
        assert_eq!(c.records[&d.id].state,Phase::Ready,"a subsequent Inform confirms the verified settings");
    }
    #[cfg(feature = "device-hl202")]
    #[test]
    fn onu_link_binds_only_unique_fresh_mqtt_source_and_invalidates_reassigned_link() {
        let mut c = Coordinator::default();
        let d = device("automatic", "hl202");
        let m = Management {
            vlan: 46,
            priority: 0,
            acs_url: "http://192.0.2.1/cwmp".into(),
        };
        let mac = "02AABBCCDDEE";
        c.transport.insert(
            mac.into(),
            Transport {
                last_seen: 100,
                epoch: 1,
            },
        );
        assert!(schedule(&mut c, &d, &m, None, 100, None).is_none());
        assert_eq!(c.records[&d.id].state, Phase::WaitingIdentity);
        c.links.insert(mac.into(), d.optical_sn.clone());
        let job = schedule(&mut c, &d, &m, None, 101, None).unwrap();
        assert_eq!(job.target, mac);
        assert_eq!(c.records[&d.id].binding_source, "onu_link");
        c.links.insert(mac.into(), "OTHER12345678".into());
        assert!(schedule(&mut c, &d, &m, None, 102, None).is_none());
        assert_ne!(c.records[&d.id].generation, job.generation);
        c.records
            .get_mut(&d.id)
            .unwrap()
            .finish(job.generation, Ok(true), 103);
        assert!(c.records[&d.id].settings_verified_at.is_none());
        schedule(&mut c, &d, &m, None, 104, None);
        assert_eq!(c.records[&d.id].state, Phase::WaitingIdentity);
        c.links.insert(mac.into(), d.optical_sn.clone());
        c.links.insert("02AABBCCDDFF".into(), d.optical_sn.clone());
        c.transport.insert(
            "02AABBCCDDFF".into(),
            Transport {
                last_seen: 104,
                epoch: 1,
            },
        );
        assert!(schedule(&mut c, &d, &m, None, 105, None).is_none());
        assert!(c.records[&d.id].reason.contains("ambiguous"));
        c.transport.get_mut(mac).unwrap().last_seen = 170;
        assert_eq!(
            c.automatic_target(&d.optical_sn, 170).unwrap(),
            Some(mac.into())
        );
    }

    #[cfg(feature = "device-hl202")]
    #[test]
    fn continuously_online_device_retries_without_overlapping_or_blocking_other_devices() {
        let mut c = Coordinator::default();
        let d = device("first", "hl202");
        let mut other = device("second", "hl202");
        other.optical_sn = "H3CT87654321".into();
        let m = Management {
            vlan: 46,
            priority: 7,
            acs_url: "http://192.0.2.1:7547/cwmp".into(),
        };
        let mac = "02AABBCCDDEE";
        c.transport.insert(
            mac.into(),
            Transport {
                last_seen: 100,
                epoch: 1,
            },
        );
        let first = schedule(&mut c, &d, &m, Some(mac), 100, None).unwrap();
        assert!(schedule(&mut c, &d, &m, Some(mac), 101, None).is_none());
        c.transport.insert(
            "02AABBCCDDFF".into(),
            Transport {
                last_seen: 101,
                epoch: 1,
            },
        );
        assert!(schedule(&mut c, &other, &m, Some("02AABBCCDDFF"), 101, None).is_some());
        c.records.get_mut("first").unwrap().finish(
            first.generation,
            Err("unknown timeout".into()),
            102,
        );
        assert!(schedule(&mut c, &d, &m, Some(mac), 106, None).is_none());
        c.transport.get_mut(mac).unwrap().last_seen = 107;
        let retry = schedule(&mut c, &d, &m, Some(mac), 107, None).unwrap();
        assert_eq!(c.records["first"].attempts, 2);
        assert!(retry.persist_matching);
        c.records
            .get_mut("first")
            .unwrap()
            .finish(retry.generation, Ok(true), 108);
        assert_eq!(c.records["first"].state, Phase::WaitingInform);
        assert!(schedule(&mut c, &d, &m, Some(mac), 110, Some(109)).is_none());
        assert_eq!(c.records["first"].state, Phase::Ready);
    }
    #[cfg(feature = "device-hl202")]
    #[test]
    fn new_configuration_waits_for_old_worker_then_uses_new_url_and_discards_old_result() {
        let mut c = Coordinator::default();
        let d = device("first", "hl202");
        let mut m = Management {
            vlan: 46,
            priority: 0,
            acs_url: "http://192.0.2.1/cwmp".into(),
        };
        let mac = "02AABBCCDDEE";
        c.transport.insert(
            mac.into(),
            Transport {
                last_seen: 100,
                epoch: 1,
            },
        );
        let old = schedule(&mut c, &d, &m, Some(mac), 100, None).unwrap();
        m.acs_url = "http://192.0.2.2/cwmp".into();
        assert!(schedule(&mut c, &d, &m, Some(mac), 101, None).is_none());
        c.records
            .get_mut("first")
            .unwrap()
            .finish(old.generation, Ok(true), 102);
        assert!(c.records["first"].settings_verified_at.is_none());
        let new = schedule(&mut c, &d, &m, Some(mac), 103, None).unwrap();
        assert_ne!(old.generation, new.generation);
        assert_eq!(new.url, m.acs_url);
    }
    #[test]
    fn manual_device_becomes_ready_without_mqtt_and_clock_rollback_cannot_reuse_old_inform() {
        let mut c = Coordinator::default();
        let d = device("manual", "hwtc");
        let m = Management {
            vlan: 46,
            priority: 4,
            acs_url: "http://192.0.2.1/cwmp".into(),
        };
        assert!(schedule(&mut c, &d, &m, None, 110, Some(100)).is_none());
        assert_eq!(c.records["manual"].state, Phase::ManualRequired);
        assert!(schedule(&mut c, &d, &m, None, 112, Some(111)).is_none());
        assert_eq!(c.records["manual"].state, Phase::Ready);
        schedule(&mut c, &d, &m, None, 90, Some(111));
        assert_eq!(c.records["manual"].state, Phase::ManualRequired);
        schedule(&mut c, &d, &m, None, 92, Some(91));
        assert_eq!(c.records["manual"].state, Phase::Ready);
    }
    #[test]
    fn restored_inform_is_not_fresh_and_devices_have_independent_evidence() {
        let first = Record {
            initial_inform: Some(Some(100)),
            require_inform_after: 110,
            ..Record::default()
        };
        let second = Record {
            initial_inform: Some(None),
            ..Record::default()
        };
        assert!(!first.fresh_inform(Some(100), 120, 180, true));
        assert!(!first.fresh_inform(Some(109), 120, 180, true));
        assert!(first.fresh_inform(Some(111), 120, 180, true));
        assert!(!first.fresh_inform(Some(111), 120, 180, false));
        assert!(second.fresh_inform(Some(109), 120, 180, true));
    }
    #[test]
    fn invalidation_retains_worker_slot_until_completion() {
        let mut r = Record {
            generation: 3,
            worker: Some(3),
            settings_verified_at: Some(10),
            ..Record::default()
        };
        r.invalidate(20, "reset");
        assert_eq!(r.generation, 4);
        assert_eq!(r.worker, Some(3));
        assert_eq!(r.settings_verified_at, None);
        let recovered: Record = serde_json::from_value(serde_json::to_value(&r).unwrap()).unwrap();
        assert_eq!(recovered.worker, None);
        assert_eq!(recovered.initial_inform, None);
    }
    #[test]
    fn retry_is_bounded_and_mac_mapping_is_generic() {
        assert_eq!(retry_delay(1), 5);
        assert_eq!(retry_delay(3), 20);
        assert_eq!(retry_delay(u32::MAX), 300);
        assert_eq!(
            normalize_target("02aabbccddee/fttr_data.tar.gz"),
            Some("02AABBCCDDEE".into())
        );
        assert!(normalize_target("H3CT12345678").is_none());
        assert!(normalize_target("../02AABBCCDDEE").is_none());
        assert_eq!(
            public_url("http://user:secret@192.0.2.1:7547/cwmp"),
            "http://192.0.2.1:7547/cwmp"
        );
    }
}
