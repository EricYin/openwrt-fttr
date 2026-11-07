//! Factory manager_slave JSON contract (MGS.V1.0.0.0). No shell payloads.
//! CPU reports have no transaction ID: serialize locally and quarantine an
//! uncertain request until a new online report. Other masters remain outside
//! our correlation guarantees; expose that limitation in the result.
use crate::mqtt::session::CommandSession;
use rumqttc::{Event, Packet};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    net::SocketAddr,
    sync::{Mutex, OnceLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::time::Instant;
mod queries;
pub use queries::ReadKind;

pub const REPORT_TOPIC: &str = "/cmfttr/manager_master";
const MAX_JSON: usize = 4096;
const PRESENCE_SECONDS: u64 = 35;

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Report {
    Online {
        mac: String,
        device_type: Option<serde_json::Value>,
        reason: Option<serde_json::Value>,
        reboot_reason: Option<serde_json::Value>,
        reboot_source: Option<serde_json::Value>,
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

pub fn mac(value: &str) -> Option<String> {
    (value.len() == 12 && value.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| value.to_ascii_uppercase())
}

pub fn parse(topic: &str, payload: &[u8], retained: bool) -> Option<Report> {
    if retained || topic != REPORT_TOPIC || payload.len() > MAX_JSON {
        return None;
    }
    // MQTTAsync_sendMessage includes strlen(json) + 1 in this firmware.
    // Accept precisely one optional terminator, not arbitrary trailing bytes.
    let payload = payload.strip_suffix(&[0]).unwrap_or(payload);
    let v: serde_json::Value = serde_json::from_slice(payload).ok()?;
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
            mac: mac(v.get("submac")?.as_str()?)?,
            device_type: scalar("type"),
            reason: scalar("reason"),
            reboot_reason: scalar("rebootReason"),
            reboot_source: scalar("rebootSource"),
        }),
        "reportSubGwOffLineMsg" => Some(Report::Offline {
            mac: mac(v.get("submac")?.as_str()?)?,
        }),
        "reportKeepAliveMsg" => Some(Report::KeepAlive {
            mac: mac(v.get("mac")?.as_str()?)?,
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
                mac: mac(v.get("MAC")?.as_str()?)?,
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
                let rebooted = matches!(&source.keepalive_report,
                    Some(Report::KeepAlive { uptime_seconds: previous, .. }) if uptime_seconds < previous);
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

pub fn sources() -> Vec<Source> {
    store()
        .lock()
        .unwrap()
        .sources
        .values()
        .cloned()
        .map(|mut s| {
            s.alive &= now().saturating_sub(s.presence_at) <= PRESENCE_SECONDS;
            s
        })
        .collect()
}

pub async fn run(address: SocketAddr) {
    loop {
        let result = receive(address).await;
        store().lock().unwrap().disconnect();
        eprintln!("[-] TY MQTT report connection stopped: {result:?}; reconnecting in 2s");
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}
async fn receive(address: SocketAddr) -> Result<(), String> {
    let mut session = CommandSession::connect(
        address,
        &format!("fttr-ty-reports-{}", std::process::id()),
        Instant::now() + Duration::from_secs(5),
    )
    .await?;
    session
        .subscribe(REPORT_TOPIC, Instant::now() + Duration::from_secs(5))
        .await?;
    loop {
        if let Event::Incoming(Packet::Publish(p)) = session
            .next(Instant::now() + Duration::from_secs(60))
            .await?
        {
            if let Some(report) = parse(&p.topic, &p.payload, p.retain) {
                store().lock().unwrap().observe(report, now());
            }
        }
    }
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

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Query {
    List,
    Cpu { mac: String },
    Wifi { mac: String },
    Radio { mac: String },
    Iptv { mac: String },
}

pub async fn read(
    target: &str,
    kind: ReadKind,
    validate_binding: impl Fn() -> Result<(), String>,
) -> Result<serde_json::Value, String> {
    let settings = crate::mqtt_settings::Settings::load()?;
    if !settings.ty_fs803g() {
        return Err("TY MQTT adapter disabled".into());
    }
    read_at(
        target,
        kind,
        "127.0.0.1:11883".parse().unwrap(),
        Duration::from_secs(10),
        validate_binding,
    )
    .await
}

#[cfg(test)]
async fn cpu_at(
    target: &str,
    address: SocketAddr,
    timeout: Duration,
    validate_binding: impl Fn() -> Result<(), String>,
) -> Result<serde_json::Value, String> {
    read_at(target, ReadKind::Cpu, address, timeout, validate_binding).await
}

async fn read_at(
    target: &str,
    kind: ReadKind,
    address: SocketAddr,
    timeout: Duration,
    validate_binding: impl Fn() -> Result<(), String>,
) -> Result<serde_json::Value, String> {
    let target = mac(target).ok_or("invalid TY MAC")?;
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
    // Poll the dedicated connection directly: inspect reports even while the
    // publication is being flushed, so an offline frame is never discarded.
    let mut flushed = false;
    loop {
        match session.next(deadline).await? {
            Event::Outgoing(rumqttc::Outgoing::Publish(0)) => flushed = true,
            Event::Incoming(Packet::Publish(p)) => {
                let report = match parse(&p.topic, &p.payload, p.retain) {
                    Some(Report::Offline { mac }) | Some(Report::Online { mac, .. })
                        if mac == target =>
                    {
                        return Err("TY online generation changed during query".into())
                    }
                    Some(report @ Report::Cpu { .. })
                        if flushed && kind == ReadKind::Cpu && report.mac() == target =>
                    {
                        Some(serde_json::to_value(report).unwrap())
                    }
                    _ if flushed && !p.retain && p.topic == REPORT_TOPIC => {
                        kind.response(&p.payload, &sequence)?
                    }
                    _ => None,
                };
                if let Some(report) = report {
                    validate_binding()?;
                    let state = store().lock().unwrap();
                    let s = state.sources.get(&target).ok_or("TY report disappeared")?;
                    if s.epoch != epoch
                        || !s.alive
                        || now().saturating_sub(s.presence_at) > PRESENCE_SECONDS
                    {
                        return Err("TY online generation changed".into());
                    }
                    guard.complete = true;
                    return Ok(
                        serde_json::json!({"schema_version":1,"state":"received","read_only":true,"correlation":if kind == ReadKind::Cpu { "post_publish_mac_and_command_no_wire_request_id" } else { "sequence_id_and_command" },"report":report}),
                    );
                }
            }
            _ => (),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mqtt::test_peer as peer;
    use rumqttc::{Publish, QoS, SubAck, SubscribeReasonCode};

    #[tokio::test]
    async fn structured_reads_match_sequence_and_never_return_wifi_passwords() {
        for (target, kind, cmd, fields) in [
            (
                "020000000011",
                ReadKind::Wifi,
                "reportgetsubgwWifiInfo",
                serde_json::json!({
                    "List":[{"Enable":"1","SSIDIndex":"1","SSID":"fixture","ENCRYPT":"WPA2","RadioType":"5G","SSIDAdvertisementEnabled":"true","Standard":"11ax","SSIDPassword":"fixture-secret","futureCredential":"fixture-secret"}],
                    "HardwareSwitch2.4G":"true","HardwareSwitch5G":"true"
                }),
            ),
            (
                "020000000012",
                ReadKind::Radio,
                "reportgetsubgwRadioInfo",
                serde_json::json!({
                    "radioInfo":[{"MAC":"020000000012","RadioType":"5G","Enable":"1","TransmitPower":"100","Standard":"11ax","Channel":"36","AutoChannelEnable":"0","ChannelsInUse":"36","FrequencyWidth":"80M","CurrentChannelWidth":"80M","Noise":"-90","InterferencePercent":"1","IdlePercent":"99","RecvPackets":"10","SendPackets":"20","BeaconFails":"0"}]
                }),
            ),
            (
                "020000000013",
                ReadKind::Iptv,
                "reportgetSubgwIptvInfo",
                serde_json::json!({"List":[{"MAC":"020000000013","StbPort":""}]}),
            ),
        ] {
            online(target);
            let (address, handle) = peer::spawn(move |mut stream| {
                subscribed(&mut stream);
                let Packet::Publish(p) = peer::read(&mut stream).unwrap() else {
                    panic!("expected read")
                };
                assert_eq!(p.topic, format!("/cmfttr/manager_slave/{target}"));
                assert!(!p.retain);
                assert_eq!(p.qos, QoS::AtMostOnce);
                let request: serde_json::Value = serde_json::from_slice(&p.payload).unwrap();
                let sequence = request["SequenceId"].as_str().unwrap();
                assert_eq!(request, kind.request(sequence));
                let mut response = fields;
                response["cmd"] = serde_json::json!(cmd);
                response["Status"] = serde_json::json!("0");
                response["SequenceId"] = serde_json::json!("old-sequence");
                for (seq, retained) in
                    [("old-sequence", false), (sequence, true), (sequence, false)]
                {
                    response["SequenceId"] = serde_json::json!(seq);
                    let mut payload = serde_json::to_vec(&response).unwrap();
                    payload.push(0);
                    let mut p = Publish::new(REPORT_TOPIC, QoS::AtMostOnce, payload);
                    p.retain = retained;
                    peer::write(&mut stream, Packet::Publish(p), false);
                }
                assert!(peer::read(&mut stream).is_err());
            });
            let result = read_at(target, kind, address, Duration::from_secs(2), || Ok(()))
                .await
                .unwrap();
            assert_eq!(result["correlation"], "sequence_id_and_command");
            assert_eq!(result["report"]["items"].as_array().unwrap().len(), 1);
            assert!(!result.to_string().contains("fixture-secret"));
            assert!(!result.to_string().contains("SSIDPassword"));
            handle.join().unwrap();
        }
    }

    #[tokio::test]
    async fn sequence_read_timeout_does_not_quarantine_next_request() {
        let target = "020000000014";
        online(target);
        let (address, handle) = peer::spawn(move |mut stream| {
            subscribed(&mut stream);
            assert!(matches!(
                peer::read(&mut stream).unwrap(),
                Packet::Publish(_)
            ));
            assert!(peer::read(&mut stream).is_err());
        });
        assert!(read_at(
            target,
            ReadKind::Wifi,
            address,
            Duration::from_millis(200),
            || Ok(())
        )
        .await
        .is_err());
        handle.join().unwrap();
        let state = store().lock().unwrap();
        assert!(!state.sources[target].query_busy);
        assert!(!state.sources[target].query_uncertain);
    }

    fn online(target: &str) {
        store().lock().unwrap().observe(
            Report::Online {
                mac: target.into(),
                device_type: None,
                reason: None,
                reboot_reason: None,
                reboot_source: None,
            },
            now(),
        );
    }

    fn subscribed(stream: &mut std::net::TcpStream) {
        let Packet::Subscribe(sub) = peer::read(stream).unwrap() else {
            panic!("expected subscription")
        };
        assert_eq!(sub.filters[0].path, REPORT_TOPIC);
        peer::write(
            stream,
            Packet::SubAck(SubAck::new(
                sub.pkid,
                vec![SubscribeReasonCode::Success(QoS::AtMostOnce)],
            )),
            false,
        );
    }

    fn command(stream: &mut std::net::TcpStream, target: &str) {
        let Packet::Publish(p) = peer::read(stream).unwrap() else {
            panic!("expected query")
        };
        assert_eq!(p.topic, format!("/cmfttr/manager_slave/{target}"));
        assert_eq!(p.payload.as_ref(), br#"{"cmd":"getsubgwcpuusage"}"#);
        assert!(!p.retain);
        assert_eq!(p.qos, QoS::AtMostOnce);
    }

    fn reply(stream: &mut std::net::TcpStream, target: &str, retained: bool) {
        let mut p = Publish::new(
            REPORT_TOPIC,
            QoS::AtMostOnce,
            format!(
                r#"{{"cmd":"reportgetSubgwcpuusage","MAC":"{target}","CPUUsage":"12.5","DeviceName":"fixture"}}"#
            ),
        );
        p.retain = retained;
        peer::write(stream, Packet::Publish(p), false);
    }

    #[tokio::test]
    async fn cpu_transport_contract_rejects_unrelated_and_retained_reports() {
        let target = "020000000001";
        online(target);
        let (address, handle) = peer::spawn(move |mut stream| {
            subscribed(&mut stream);
            command(&mut stream, target);
            reply(&mut stream, "020000000002", false);
            reply(&mut stream, target, true);
            reply(&mut stream, target, false);
            assert!(peer::read(&mut stream).is_err());
        });
        let result = cpu_at(target, address, Duration::from_secs(2), || Ok(()))
            .await
            .unwrap();
        assert_eq!(result["state"], "received");
        assert_eq!(result["report"]["mac"], target);
        assert_eq!(result["report"]["cpu_usage_percent"], 12.5);
        handle.join().unwrap();
        let state = store().lock().unwrap();
        assert!(!state.sources[target].query_busy);
        assert!(!state.sources[target].query_uncertain);
    }

    #[tokio::test]
    async fn cpu_timeout_quarantines_retry_until_new_online_report() {
        let target = "020000000003";
        online(target);
        let (address, handle) = peer::spawn(move |mut stream| {
            subscribed(&mut stream);
            command(&mut stream, target);
            assert!(peer::read(&mut stream).is_err());
        });
        assert!(
            cpu_at(target, address, Duration::from_millis(200), || Ok(()))
                .await
                .is_err()
        );
        handle.join().unwrap();
        assert!(store().lock().unwrap().sources[target].query_uncertain);
        let error = cpu_at(target, address, Duration::from_secs(1), || Ok(()))
            .await
            .unwrap_err();
        assert!(error.contains("uncertain"));
        online(target);
        assert!(!store().lock().unwrap().sources[target].query_uncertain);
    }

    #[tokio::test]
    async fn cpu_offline_or_changed_binding_cannot_complete_query() {
        for (target, offline) in [("020000000004", true), ("020000000005", false)] {
            online(target);
            let (address, handle) = peer::spawn(move |mut stream| {
                subscribed(&mut stream);
                command(&mut stream, target);
                if offline {
                    peer::write(
                        &mut stream,
                        Packet::Publish(Publish::new(
                            REPORT_TOPIC,
                            QoS::AtMostOnce,
                            format!(r#"{{"cmd":"reportSubGwOffLineMsg","submac":"{target}"}}"#),
                        )),
                        false,
                    );
                } else {
                    reply(&mut stream, target, false);
                }
                assert!(peer::read(&mut stream).is_err());
            });
            let calls = std::cell::Cell::new(0);
            let error = cpu_at(target, address, Duration::from_secs(2), || {
                calls.set(calls.get() + 1);
                if calls.get() > 1 {
                    Err("binding changed".into())
                } else {
                    Ok(())
                }
            })
            .await
            .unwrap_err();
            assert!(error.contains(if offline {
                "generation changed"
            } else {
                "binding changed"
            }));
            handle.join().unwrap();
        }
    }

    #[test]
    fn nul_terminated_keepalive_recovers_presence_without_clearing_uncertain_queries() {
        let heartbeat = br#"{"cmd":"reportKeepAliveMsg","mac":"02aabbccddee","DeviceType":"fixture","HWVersion":"test","SWVersion":"test","UpTime":"120","CpuPercent":"3.0","MemPercent":"42.0","UniPorts":"","Channel":"36","StbPort":""}"#;
        let mut terminated = heartbeat.to_vec();
        terminated.push(0);
        let report = parse(REPORT_TOPIC, &terminated, false).unwrap();
        terminated.push(0);
        assert!(parse(REPORT_TOPIC, &terminated, false).is_none());
        assert!(parse(REPORT_TOPIC, heartbeat, true).is_none());
        let mut s = Store::default();
        s.observe(report.clone(), 100);
        let first = &s.sources["02AABBCCDDEE"];
        assert!(first.alive);
        assert!(!first.online_reported);
        let epoch = first.epoch;
        s.sources.get_mut("02AABBCCDDEE").unwrap().query_uncertain = true;
        s.observe(report, 110);
        assert_eq!(s.sources["02AABBCCDDEE"].epoch, epoch);
        assert!(s.sources["02AABBCCDDEE"].query_uncertain);
        let rebooted = String::from_utf8(heartbeat.to_vec())
            .unwrap()
            .replace("120", "1");
        s.observe(
            parse(REPORT_TOPIC, rebooted.as_bytes(), false).unwrap(),
            120,
        );
        assert!(s.sources["02AABBCCDDEE"].epoch > epoch);
        assert!(!s.sources["02AABBCCDDEE"].query_uncertain);
        s.disconnect();
        assert!(!s.sources["02AABBCCDDEE"].alive);
    }

    #[test]
    fn strict_report_contract_and_session_invalidation() {
        let online =
            br#"{"cmd":"reportSubGwOnLineMsg","submac":"02aabbccddee","type":"subgw","reason":1}"#;
        let cpu = br#"{"cmd":"reportgetSubgwcpuusage","MAC":"02AABBCCDDEE","CPUUsage":"12.5","DeviceName":"fixture"}"#;
        assert!(parse(REPORT_TOPIC, online, true).is_none());
        assert!(parse("/fttr/maindev/data/test", cpu, false).is_none());
        assert!(parse(REPORT_TOPIC, br#"{"cmd":"getsubgwcpuusage"}"#, false).is_none());
        assert!(parse(REPORT_TOPIC, br#"{"cmd":"reportgetSubgwcpuusage","MAC":"02AABBCCDDEE","CPUUsage":"NaN","DeviceName":"fixture"}"#, false).is_none());
        let mut s = Store::default();
        s.observe(parse(REPORT_TOPIC, online, false).unwrap(), 100);
        let epoch = s.sources["02AABBCCDDEE"].epoch;
        s.observe(parse(REPORT_TOPIC, cpu, false).unwrap(), 101);
        assert!(s.sources["02AABBCCDDEE"].cpu_report.is_some());
        s.disconnect();
        assert!(!s.sources["02AABBCCDDEE"].online_reported);
        s.observe(parse(REPORT_TOPIC, online, false).unwrap(), 102);
        assert!(s.sources["02AABBCCDDEE"].epoch > epoch);
        assert!(s.sources["02AABBCCDDEE"].cpu_report.is_none());
    }
}
