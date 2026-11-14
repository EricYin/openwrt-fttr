//! Factory `manager_slave` JSON contract (MGS.V1.0.0.0) over TCP 11883.
//!
//! Provides presence/keepalive telemetry and whitelisted readback queries
//! (`/cmfttr/manager_slave/<MAC>` -> `/cmfttr/manager_master`) to complement
//! the IF5 write channel.
use super::if5::WlanConfigEntry;
use crate::mqtt::session::CommandSession;
use rumqttc::{Event, Packet};
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    net::SocketAddr,
    sync::{Mutex, OnceLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::time::Instant;

pub mod queries;
pub use queries::ReadKind;

pub const REPORT_TOPIC: &str = "/cmfttr/manager_master";
const MAX_JSON: usize = 4096;
const PRESENCE_SECONDS: u64 = 35;

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Report {
    Online {
        mac: String,
        device_type: Option<Value>,
        reason: Option<Value>,
        reboot_reason: Option<Value>,
        reboot_source: Option<Value>,
    },
    Offline {
        mac: String,
    },
    Cpu {
        mac: String,
        cpu_usage_percent: f64,
        device_name: String,
    },
    KeepAlive {
        mac: String,
        device_type: String,
        hardware_version: String,
        software_version: String,
        uptime_seconds: u64,
        cpu_usage_percent: f64,
        memory_usage_percent: f64,
        uni_ports: String,
        channel: String,
        stb_port: String,
    },
}

impl Report {
    fn mac(&self) -> &str {
        match self {
            Self::Online { mac, .. }
            | Self::Offline { mac }
            | Self::Cpu { mac, .. }
            | Self::KeepAlive { mac, .. } => mac,
        }
    }
}

pub fn normalize_mac(value: &str) -> Option<String> {
    let compact: String = value
        .chars()
        .filter(|c| !matches!(c, ':' | '-' | '.'))
        .collect();
    (compact.len() == 12 && compact.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| compact.to_ascii_uppercase())
}

pub fn parse(topic: &str, payload: &[u8], retained: bool) -> Option<Report> {
    if retained || topic != REPORT_TOPIC || payload.len() > MAX_JSON {
        return None;
    }
    // MQTTAsync_sendMessage includes strlen(json) + 1 in this firmware.
    let payload = payload.strip_suffix(&[0]).unwrap_or(payload);
    let v: Value = serde_json::from_slice(payload).ok()?;
    let text = |name: &str, max: usize| -> Option<String> {
        let value = v.get(name)?.as_str()?;
        (value.len() <= max && !value.contains('\0')).then(|| value.to_owned())
    };
    let percent = |name: &str| -> Option<f64> {
        let n: f64 = v.get(name)?.as_str()?.parse().ok()?;
        (n.is_finite() && (0.0..=100.0).contains(&n)).then_some(n)
    };
    let scalar = |name: &str| {
        v.get(name)
            .filter(|v| {
                v.is_number() || v.is_boolean() || v.as_str().is_some_and(|s| s.len() <= 128)
            })
            .cloned()
    };
    match v.get("cmd")?.as_str()? {
        "reportSubGwOnLineMsg" => Some(Report::Online {
            mac: normalize_mac(v.get("submac")?.as_str()?)?,
            device_type: scalar("type"),
            reason: scalar("reason"),
            reboot_reason: scalar("rebootReason"),
            reboot_source: scalar("rebootSource"),
        }),
        "reportSubGwOffLineMsg" => Some(Report::Offline {
            mac: normalize_mac(v.get("submac")?.as_str()?)?,
        }),
        "reportKeepAliveMsg" => Some(Report::KeepAlive {
            mac: normalize_mac(v.get("mac")?.as_str()?)?,
            device_type: text("DeviceType", 128)?,
            hardware_version: text("HWVersion", 128)?,
            software_version: text("SWVersion", 128)?,
            uptime_seconds: v.get("UpTime")?.as_str()?.parse().ok()?,
            cpu_usage_percent: percent("CpuPercent")?,
            memory_usage_percent: percent("MemPercent")?,
            uni_ports: text("UniPorts", 1024)?,
            channel: text("Channel", 32)?,
            stb_port: text("StbPort", 128)?,
        }),
        "reportgetSubgwcpuusage" => {
            let usage: f64 = v.get("CPUUsage")?.as_str()?.parse().ok()?;
            let name = v.get("DeviceName")?.as_str()?;
            if !usage.is_finite() || !(0.0..=100.0).contains(&usage) || name.len() > 128 {
                return None;
            }
            Some(Report::Cpu {
                mac: normalize_mac(v.get("MAC")?.as_str()?)?,
                cpu_usage_percent: usage,
                device_name: name.into(),
            })
        }
        _ => None,
    }
}

#[derive(Clone, Default, Serialize)]
pub struct Source {
    pub mac: String,
    pub online_reported: bool,
    pub alive: bool,
    pub epoch: u64,
    pub received_at: u64,
    pub presence_at: u64,
    pub report: Option<Report>,
    pub cpu_report: Option<Report>,
    pub keepalive_report: Option<Report>,
    pub query_busy: bool,
    pub query_uncertain: bool,
}

#[derive(Default)]
struct Store {
    sources: BTreeMap<String, Source>,
    epoch: u64,
}

fn store() -> &'static Mutex<Store> {
    static STORE: OnceLock<Mutex<Store>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(Store::default()))
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

impl Store {
    fn observe(&mut self, report: Report, received_at: u64) {
        let mac = report.mac().to_owned();
        if !self.sources.contains_key(&mac) && self.sources.len() >= 64 {
            return;
        }
        let source = self.sources.entry(mac.clone()).or_insert_with(|| Source {
            mac,
            ..Source::default()
        });
        source.received_at = received_at;
        match &report {
            Report::Online { .. } => {
                self.epoch += 1;
                source.epoch = self.epoch;
                source.online_reported = true;
                source.alive = true;
                source.presence_at = received_at;
                source.cpu_report = None;
                source.keepalive_report = None;
                source.query_uncertain = false;
                source.report = Some(report);
            }
            Report::Offline { .. } => {
                source.online_reported = false;
                source.alive = false;
                source.cpu_report = None;
                source.keepalive_report = None;
                source.report = Some(report);
            }
            Report::Cpu { .. } => source.cpu_report = Some(report),
            Report::KeepAlive { uptime_seconds, .. } => {
                let rebooted = matches!(
                    &source.keepalive_report,
                    Some(Report::KeepAlive { uptime_seconds: previous, .. }) if uptime_seconds < previous
                );
                if !source.alive || rebooted {
                    self.epoch += 1;
                    source.epoch = self.epoch;
                    source.query_uncertain = false;
                    source.cpu_report = None;
                    source.online_reported = false;
                }
                source.alive = true;
                source.presence_at = received_at;
                source.keepalive_report = Some(report);
            }
        }
    }

    fn disconnect(&mut self) {
        for source in self.sources.values_mut() {
            source.online_reported = false;
            source.alive = false;
            source.cpu_report = None;
        }
    }
}

pub fn observe_packet(topic: &str, payload: &[u8], retained: bool) -> Option<Report> {
    let report = parse(topic, payload, retained)?;
    store().lock().unwrap().observe(report.clone(), now());
    Some(report)
}

pub fn disconnect_all() {
    store().lock().unwrap().disconnect();
}

pub fn sources() -> Vec<Source> {
    let current = now();
    store()
        .lock()
        .unwrap()
        .sources
        .values()
        .cloned()
        .map(|mut s| {
            s.alive &= current.saturating_sub(s.presence_at) <= PRESENCE_SECONDS;
            s
        })
        .collect()
}

pub fn source_for_mac(mac_addr: &str) -> Option<Source> {
    let key = normalize_mac(mac_addr)?;
    let current = now();
    let mut source = store().lock().unwrap().sources.get(&key).cloned()?;
    source.alive &= current.saturating_sub(source.presence_at) <= PRESENCE_SECONDS;
    Some(source)
}

pub fn is_online(mac_addr: &str) -> bool {
    source_for_mac(mac_addr).is_some_and(|s| s.alive)
}

struct QueryGuard {
    mac: String,
    epoch: u64,
    complete: bool,
    quarantine: bool,
}

impl Drop for QueryGuard {
    fn drop(&mut self) {
        if let Some(s) = store().lock().unwrap().sources.get_mut(&self.mac) {
            s.query_busy = false;
            if s.epoch == self.epoch && !self.complete && self.quarantine {
                s.query_uncertain = true;
            }
        }
    }
}

pub async fn read_at(
    target: &str,
    kind: ReadKind,
    address: SocketAddr,
    timeout: Duration,
    validate_binding: impl Fn() -> Result<(), String>,
) -> Result<Value, String> {
    let target = normalize_mac(target).ok_or("invalid TY MAC")?;
    let epoch = {
        let mut state = store().lock().unwrap();
        let s = state
            .sources
            .get_mut(&target)
            .ok_or("TY online report unavailable")?;
        if !s.alive
            || now().saturating_sub(s.presence_at) > PRESENCE_SECONDS
            || s.query_busy
            || (s.query_uncertain && kind == ReadKind::Cpu)
        {
            return Err(
                "TY offline, busy, or previous query uncertain; new online report required".into(),
            );
        }
        s.query_busy = true;
        s.epoch
    };
    let mut guard = QueryGuard {
        mac: target.clone(),
        epoch,
        complete: false,
        quarantine: kind == ReadKind::Cpu,
    };
    let deadline = Instant::now() + timeout;
    let mut session = CommandSession::connect(
        address,
        &format!("fttr-ty-query-{}-{target}", std::process::id()),
        deadline,
    )
    .await?;
    session.subscribe(REPORT_TOPIC, deadline).await?;
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let sequence = format!(
        "fttr-{:x}-{:x}-{:x}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    let request =
        serde_json::to_vec(&kind.request(&sequence)).map_err(|_| "TY query encoding failed")?;
    validate_binding()?;
    {
        let state = store().lock().unwrap();
        let s = state.sources.get(&target).ok_or("TY report disappeared")?;
        if s.epoch != epoch || !s.alive || now().saturating_sub(s.presence_at) > PRESENCE_SECONDS {
            return Err("TY online generation changed".into());
        }
        session.enqueue_publish(&format!("/cmfttr/manager_slave/{target}"), &request)?;
    }

    let mut flushed = false;
    loop {
        match session.next(deadline).await? {
            Event::Outgoing(rumqttc::Outgoing::Publish(0)) => flushed = true,
            Event::Incoming(Packet::Publish(p)) => {
                if let Some(report) = parse(&p.topic, &p.payload, p.retain) {
                    if matches!(&report, Report::Offline { mac } | Report::Online { mac, .. } if mac == &target)
                    {
                        return Err("TY online generation changed during query".into());
                    }
                    if let Report::Cpu {
                        mac,
                        cpu_usage_percent,
                        device_name,
                    } = report
                    {
                        if kind == ReadKind::Cpu && flushed && mac == target {
                            validate_binding()?;
                            let mut state = store().lock().unwrap();
                            let s = state.sources.get_mut(&target).ok_or("TY report missing")?;
                            if s.epoch != epoch {
                                return Err("TY online generation changed".into());
                            }
                            s.cpu_report = Some(Report::Cpu {
                                mac,
                                cpu_usage_percent,
                                device_name: device_name.clone(),
                            });
                            guard.complete = true;
                            return Ok(json!({
                                "mac": target,
                                "cpu_usage_percent": cpu_usage_percent,
                                "device_name": device_name,
                            }));
                        }
                    }
                    continue;
                }
                if flushed && p.topic == REPORT_TOPIC && !p.retain {
                    if let Some(result) = kind.response(&p.payload, &sequence)? {
                        validate_binding()?;
                        let state = store().lock().unwrap();
                        let s = state.sources.get(&target).ok_or("TY report missing")?;
                        if s.epoch != epoch {
                            return Err("TY online generation changed".into());
                        }
                        guard.complete = true;
                        return Ok(result);
                    }
                }
            }
            _ => {}
        }
    }
}

fn map_cmfttr_encrypt_to_if5(encrypt: &str) -> (String, u8) {
    let norm = encrypt.trim().to_ascii_uppercase();
    match norm.as_str() {
        "NONE" | "OPEN" | "" => ("None".into(), 0),
        "WPA2-PERSONAL" | "WPA2-PSK" | "WPA2PSK" | "WPA2" => ("WPA2-Personal".into(), 1),
        "MIXED-WPAPSK2" | "WPA-PSK/WPA2-PSK" | "WPAPSKWPA2PSK" | "WPA/WPA2-PSK" => {
            ("MIXED-WPAPSK2".into(), 3)
        }
        "WPA3-SAE" | "WPA3PSK" | "WPA3" | "SAE" => ("WPA3-SAE".into(), 1),
        "WPA2-PSK/WPA3-SAE" | "WPA2PSKWPA3PSK" | "WPA2/WPA3-PSK" => ("WPA2-PSK/WPA3-SAE".into(), 1),
        _ => (encrypt.trim().to_owned(), 1),
    }
}

pub fn parse_wifi_entries(result: &Value) -> Result<BTreeMap<u32, WlanConfigEntry>, String> {
    let items = result
        .get("items")
        .and_then(Value::as_array)
        .ok_or("missing CMFTTR wifi items")?;
    let mut entries = BTreeMap::new();
    for item in items {
        let index: u32 = item
            .get("SSIDIndex")
            .and_then(Value::as_str)
            .and_then(|s| s.parse().ok())
            .ok_or("invalid CMFTTR SSIDIndex")?;
        if !(1..=8).contains(&index) {
            continue;
        }
        let radio = match item.get("RadioType").and_then(Value::as_str).unwrap_or("") {
            r if r.starts_with('5') => "5G",
            r if r.starts_with('2') => "2.4G",
            _ if index <= 4 => "2.4G",
            _ => "5G",
        };
        let enable: u8 = item
            .get("Enable")
            .and_then(Value::as_str)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let ssid = item
            .get("SSID")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let (security_mode, encrypt) =
            map_cmfttr_encrypt_to_if5(item.get("ENCRYPT").and_then(Value::as_str).unwrap_or(""));
        let advertise: u8 = item
            .get("SSIDAdvertisementEnabled")
            .and_then(Value::as_str)
            .and_then(|s| s.parse().ok())
            .unwrap_or(1);
        entries.insert(
            index,
            WlanConfigEntry {
                radio: radio.into(),
                index,
                enable,
                ssid,
                security_mode,
                pwd: String::new(),
                encrypt,
                max_associate_num: 32,
                ssid_advertisement_enabled: advertise,
            },
        );
    }
    Ok(entries)
}

pub async fn query_wifi_snapshot(
    address: SocketAddr,
    target_mac: &str,
) -> Result<BTreeMap<u32, WlanConfigEntry>, String> {
    let value = read_at(
        target_mac,
        ReadKind::Wifi,
        address,
        Duration::from_secs(10),
        || Ok(()),
    )
    .await?;
    parse_wifi_entries(&value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_online_offline_keepalive_and_nul_terminator() {
        let online = br#"{"cmd":"reportSubGwOnLineMsg","submac":"f86691d0a8f8","type":"TY-FS803G","reason":"poweron"}"#;
        let mut online_nul = online.to_vec();
        online_nul.push(0);
        match parse(REPORT_TOPIC, &online_nul, false) {
            Some(Report::Online { mac, .. }) => assert_eq!(mac, "F86691D0A8F8"),
            other => panic!("unexpected online report: {other:?}"),
        }

        let keepalive = br#"{"cmd":"reportKeepAliveMsg","mac":"f86691d0a8f8","DeviceType":"TY-FS803G","HWVersion":"HV1.0.0.0","SWVersion":"MGS.V1.0.0.0","UpTime":"120","CpuPercent":"12.5","MemPercent":"45.0","UniPorts":"1:UP","Channel":"36","StbPort":"0"}"#;
        match parse(REPORT_TOPIC, keepalive, false) {
            Some(Report::KeepAlive {
                mac,
                uptime_seconds,
                cpu_usage_percent,
                ..
            }) => {
                assert_eq!(mac, "F86691D0A8F8");
                assert_eq!(uptime_seconds, 120);
                assert!((cpu_usage_percent - 12.5).abs() < f64::EPSILON);
            }
            other => panic!("unexpected keepalive: {other:?}"),
        }

        assert!(parse(REPORT_TOPIC, online, true).is_none());
    }

    #[test]
    fn redacts_wifi_password_and_maps_even_and_odd_indices() {
        let payload = br#"{"cmd":"reportgetsubgwWifiInfo","SequenceId":"seq-1","Status":"0","HardwareSwitch2.4G":"1","HardwareSwitch5G":"1","List":[{"Enable":"1","SSIDIndex":"2","SSID":"EVEN_2G","SSIDPassword":"SecretPassword","ENCRYPT":"WPA2-PSK","RadioType":"2.4G","SSIDAdvertisementEnabled":"1","Standard":"ax"},{"Enable":"1","SSIDIndex":"5","SSID":"ODD_5G","SSIDPassword":"SecretPassword","ENCRYPT":"WPA3-SAE","RadioType":"5G","SSIDAdvertisementEnabled":"1","Standard":"ax"}]}"#;
        let parsed = ReadKind::Wifi.response(payload, "seq-1").unwrap().unwrap();
        let serialized = parsed.to_string();
        assert!(!serialized.contains("SecretPassword"));

        let mapped = parse_wifi_entries(&parsed).unwrap();
        assert_eq!(mapped[&2].ssid, "EVEN_2G");
        assert_eq!(mapped[&2].security_mode, "WPA2-Personal");
        assert_eq!(mapped[&2].radio, "2.4G");
        assert_eq!(mapped[&5].ssid, "ODD_5G");
        assert_eq!(mapped[&5].security_mode, "WPA3-SAE");
        assert_eq!(mapped[&5].radio, "5G");
    }
}
