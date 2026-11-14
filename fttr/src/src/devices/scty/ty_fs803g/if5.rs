//! CMIOT/Andlink IF5 MQTT wire protocol & coordinator for TY-FS803G.
use super::boot_mock;
use crate::mqtt::session::CommandSession;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex, OnceLock,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::oneshot;
use tokio::time::Instant;

static SEQ_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct WlanConfigEntry {
    pub radio: String,
    pub index: u32,
    pub enable: u8,
    #[serde(rename = "SSID")]
    pub ssid: String,
    pub security_mode: String,
    pub pwd: String,
    pub encrypt: u8,
    pub max_associate_num: u32,
    #[serde(rename = "SSIDAdvertisementEnabled")]
    pub ssid_advertisement_enabled: u8,
}

#[derive(Clone, Debug, Default)]
pub struct ApConfigSnapshot {
    pub band_steering_status: Option<i64>,
    pub configurations: Vec<WlanConfigEntry>,
}

struct PendingWaiter {
    seq_id: String,
    suffix: String,
    tx: oneshot::Sender<Result<Value, String>>,
}

#[derive(Default)]
pub struct Coordinator {
    online: HashMap<String, bool>,
    last_seen: HashMap<String, u64>,
    waiters: HashMap<String, PendingWaiter>,
}

static COORDINATOR: OnceLock<Mutex<Coordinator>> = OnceLock::new();

pub fn shared_coordinator() -> &'static Mutex<Coordinator> {
    COORDINATOR.get_or_init(|| Mutex::new(Coordinator::default()))
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn next_seq_id() -> String {
    let n = SEQ_COUNTER.fetch_add(1, Ordering::Relaxed);
    let secs = now_secs() % 1_000_000;
    // managePlugin truncates seqId at 31 bytes (strncpy v30, ..., 0x1F).
    format!("fttr-{secs}-{n}")
}

/// Observes a message received on `/device/<deviceId>/upward`.
///
/// Returns normalized 12-hex MAC when the device identity is known and the
/// event represents active presence (`Offline` LWT returns `None`).
pub fn observe_upward(device_id: &str, payload: &[u8], now: u64) -> Option<String> {
    let value: Value = serde_json::from_slice(payload).ok()?;
    let event_type = value["eventType"].as_str().unwrap_or("");
    let json_mac = value["deviceMac"]
        .as_str()
        .and_then(crate::mqtt_identity::normalize_target);

    if let Some(mac) = &json_mac {
        let ip = value["ipAddress"].as_str().unwrap_or("");
        boot_mock::shared_registry()
            .lock()
            .unwrap()
            .record_boot(mac, device_id, ip, "", "", "", "", now);
    }

    let mac = json_mac.or_else(|| {
        boot_mock::shared_registry()
            .lock()
            .unwrap()
            .lookup_mac(device_id)
    });

    if event_type == "Offline" {
        shared_coordinator()
            .lock()
            .unwrap()
            .online
            .insert(device_id.to_owned(), false);
        return None;
    }

    if let Some(suffix) = event_type.strip_prefix("XData_") {
        let seq = value["seqId"].as_str().unwrap_or("");
        let mut coord = shared_coordinator().lock().unwrap();
        coord.online.insert(device_id.to_owned(), true);
        coord.last_seen.insert(device_id.to_owned(), now);
        let matched = coord
            .waiters
            .get(device_id)
            .is_some_and(|w| w.seq_id == seq && w.suffix == suffix);
        if matched {
            if let Some(waiter) = coord.waiters.remove(device_id) {
                let _ = waiter.tx.send(Ok(value));
            }
        }
    } else if let Some(suffix) = event_type.strip_prefix("Response_") {
        // Firmware Response_* on control/query error omits seqId; correlate by (deviceId, suffix).
        let seq = value["seqId"].as_str().unwrap_or("");
        let code = value["respCode"].as_i64().unwrap_or(-1);
        let cont = value["respCont"].as_str().unwrap_or("IF5 command rejected");
        let mut coord = shared_coordinator().lock().unwrap();
        coord.online.insert(device_id.to_owned(), true);
        coord.last_seen.insert(device_id.to_owned(), now);
        let matched = coord
            .waiters
            .get(device_id)
            .is_some_and(|w| w.suffix == suffix && (seq.is_empty() || w.seq_id == seq));
        if matched {
            if let Some(waiter) = coord.waiters.remove(device_id) {
                let _ = waiter
                    .tx
                    .send(Err(format!("IF5 {event_type} ({code}): {cont}")));
            }
        }
    } else {
        let mut coord = shared_coordinator().lock().unwrap();
        coord.online.insert(device_id.to_owned(), true);
        coord.last_seen.insert(device_id.to_owned(), now);
    }

    mac
}

pub fn is_online(device_id: &str) -> bool {
    shared_coordinator()
        .lock()
        .unwrap()
        .online
        .get(device_id)
        .copied()
        .unwrap_or(false)
}

/// Sends a single serialized IF5 JSON request on `/device/<device_id>/downward`
/// and awaits the matching `XData_<Suffix>` or `Response_<Suffix>` reply from
/// the telemetry upward channel.
pub async fn request(
    broker_addr: SocketAddr,
    device_id: &str,
    function: &str,
    data: Value,
    timeout: Duration,
) -> Result<Value, String> {
    let (_, suffix) = function
        .split_once('_')
        .ok_or_else(|| format!("invalid IF5 function name: {function}"))?;
    let seq_id = next_seq_id();
    let topic = format!("/device/{device_id}/downward");
    let body = serde_json::to_vec(&json!({
        "function": function,
        "seqId": seq_id,
        "data": data,
    }))
    .map_err(|e| format!("encode IF5 request: {e}"))?;

    let (tx, rx) = oneshot::channel();
    {
        let mut coord = shared_coordinator().lock().unwrap();
        if coord.waiters.contains_key(device_id) {
            return Err(format!("IF5 request already in flight for {device_id}"));
        }
        coord.waiters.insert(
            device_id.to_owned(),
            PendingWaiter {
                seq_id: seq_id.clone(),
                suffix: suffix.to_owned(),
                tx,
            },
        );
    }

    struct WaiterCleanup<'a>(&'a str);
    impl Drop for WaiterCleanup<'_> {
        fn drop(&mut self) {
            let _ = shared_coordinator()
                .lock()
                .map(|mut c| c.waiters.remove(self.0));
        }
    }
    let _cleanup = WaiterCleanup(device_id);

    let publish_deadline = Instant::now() + Duration::from_secs(5);
    let mut session =
        CommandSession::connect(broker_addr, &format!("fttr-if5-{seq_id}"), publish_deadline)
            .await?;
    session.enqueue_publish(&topic, &body)?;
    session.flushed(publish_deadline).await?;
    drop(session);

    match tokio::time::timeout(timeout, rx).await {
        Ok(Ok(reply)) => reply,
        Ok(Err(_)) => Err(format!("IF5 reply channel dropped for {device_id}")),
        Err(_) => Err(format!(
            "IF5 timeout waiting for {function} reply (seqId={seq_id}) from {device_id}"
        )),
    }
}

pub fn parse_ap_config_info(reply: &Value) -> Result<ApConfigSnapshot, String> {
    let data = &reply["data"];
    if !data.is_object() {
        return Err("XData_ApConfigInfo missing data object".into());
    }
    let band_steering_status = data["BandSteeringStatus"].as_i64();
    let mut configurations = Vec::new();
    if let Some(items) = data["Configurations"].as_array() {
        for item in items {
            let index = item["Index"]
                .as_u64()
                .and_then(|v| u32::try_from(v).ok())
                .ok_or_else(|| "ApConfigInfo configuration missing numeric Index".to_owned())?;
            configurations.push(WlanConfigEntry {
                radio: item["Radio"]
                    .as_str()
                    .unwrap_or(if index <= 4 { "2.4G" } else { "5G" })
                    .to_owned(),
                index,
                enable: item["Enable"].as_u64().unwrap_or(0) as u8,
                ssid: item["SSID"].as_str().unwrap_or("").to_owned(),
                security_mode: item["SecurityMode"].as_str().unwrap_or("None").to_owned(),
                pwd: item["Pwd"].as_str().unwrap_or("").to_owned(),
                encrypt: item["Encrypt"].as_u64().unwrap_or(0) as u8,
                max_associate_num: item["MaxAssociateNum"].as_u64().unwrap_or(32) as u32,
                ssid_advertisement_enabled: item["SSIDAdvertisementEnabled"].as_u64().unwrap_or(1)
                    as u8,
            });
        }
    }
    Ok(ApConfigSnapshot {
        band_steering_status,
        configurations,
    })
}

pub async fn query_ap_config_info(
    broker_addr: SocketAddr,
    device_id: &str,
) -> Result<ApConfigSnapshot, String> {
    let reply = request(
        broker_addr,
        device_id,
        "XQuery_ApConfigInfo",
        json!({}),
        Duration::from_secs(15),
    )
    .await?;
    parse_ap_config_info(&reply)
}

pub async fn sync_single_wlan(
    broker_addr: SocketAddr,
    device_id: &str,
    entry: &WlanConfigEntry,
) -> Result<(), String> {
    let _reply = request(
        broker_addr,
        device_id,
        "XControl_WiFiParameterSync",
        json!({
            "Configurations": [entry],
        }),
        Duration::from_secs(15),
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upward_observation_learns_identity_correlates_xdata_and_handles_seqless_errors() {
        let device_id = "IF5021122334455";
        let mboot = br#"{"eventType":"MBoot","deviceId":"IF5021122334455","deviceMac":"02:11:22:33:44:55","ipAddress":"192.0.2.10"}"#;
        assert_eq!(
            observe_upward(device_id, mboot, 1000).as_deref(),
            Some("021122334455")
        );
        assert!(is_online(device_id));

        let (tx, mut rx) = oneshot::channel();
        {
            let mut c = shared_coordinator().lock().unwrap();
            c.waiters.insert(
                device_id.into(),
                PendingWaiter {
                    seq_id: "seq-1".into(),
                    suffix: "ApConfigInfo".into(),
                    tx,
                },
            );
        }
        let reply = br#"{"eventType":"XData_ApConfigInfo","deviceId":"IF5021122334455","seqId":"seq-1","data":{"BandSteeringStatus":0,"Configurations":[{"Radio":"2.4G","Index":1,"Enable":1,"SSID":"Test2G","SecurityMode":"WPA2-Personal","Pwd":"secret123","Encrypt":1,"MaxAssociateNum":32,"SSIDAdvertisementEnabled":1}]}}"#;
        assert_eq!(
            observe_upward(device_id, reply, 1001).as_deref(),
            Some("021122334455")
        );
        let parsed = parse_ap_config_info(&rx.try_recv().unwrap().unwrap()).unwrap();
        assert_eq!(parsed.band_steering_status, Some(0));
        assert_eq!(parsed.configurations.len(), 1);
        assert_eq!(parsed.configurations[0].index, 1);

        let (tx_err, mut rx_err) = oneshot::channel();
        {
            let mut c = shared_coordinator().lock().unwrap();
            c.waiters.insert(
                device_id.into(),
                PendingWaiter {
                    seq_id: "seq-2".into(),
                    suffix: "WiFiParameterSync".into(),
                    tx: tx_err,
                },
            );
        }
        let err_reply = br#"{"eventType":"Response_WiFiParameterSync","deviceId":"IF5021122334455","respCode":1000,"respCont":"Invalid command"}"#;
        observe_upward(device_id, err_reply, 1002);
        let err = rx_err.try_recv().unwrap().unwrap_err();
        assert!(err.contains("1000"));
        assert!(err.contains("Invalid command"));

        let offline = br#"{"eventType":"Offline","deviceId":"IF5021122334455"}"#;
        assert_eq!(observe_upward(device_id, offline, 1003), None);
        assert!(!is_online(device_id));
    }
}
