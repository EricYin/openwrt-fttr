//! Whitelisted read contracts observed on MGS.V1.0.0.0 (`manager_slave`).
//! Never forward raw JSON: the Wi-Fi response includes plaintext `SSIDPassword`
//! and future fields may too.
use serde_json::{json, Map, Value};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ReadKind {
    Cpu,
    Wifi,
    Radio,
    Iptv,
}

impl ReadKind {
    pub fn request(self, sequence: &str) -> Value {
        match self {
            Self::Cpu => json!({"cmd": "getsubgwcpuusage"}),
            Self::Wifi => json!({"cmd": "getsubgwWifiInfo", "SequenceId": sequence}),
            Self::Radio => {
                json!({"cmd": "getsubgwRadioInfo", "SequenceId": sequence, "RadioType": "ALL"})
            }
            Self::Iptv => json!({"cmd": "getsubgwIptvInfo", "SequenceId": sequence}),
        }
    }

    pub fn response(self, payload: &[u8], sequence: &str) -> Result<Option<Value>, String> {
        if payload.len() > 16 * 1024 {
            return Ok(None);
        }
        let Ok(v) = serde_json::from_slice::<Value>(payload.strip_suffix(&[0]).unwrap_or(payload))
        else {
            return Ok(None);
        };
        let (cmd, list, fields) = match self {
            Self::Cpu => return Ok(None),
            Self::Wifi => (
                "reportgetsubgwWifiInfo",
                "List",
                &[
                    "Enable",
                    "SSIDIndex",
                    "SSID",
                    "ENCRYPT",
                    "RadioType",
                    "SSIDAdvertisementEnabled",
                    "Standard",
                ][..],
            ),
            Self::Radio => (
                "reportgetsubgwRadioInfo",
                "radioInfo",
                &[
                    "MAC",
                    "RadioType",
                    "Enable",
                    "TransmitPower",
                    "Standard",
                    "Channel",
                    "AutoChannelEnable",
                    "ChannelsInUse",
                    "FrequencyWidth",
                    "CurrentChannelWidth",
                    "Noise",
                    "InterferencePercent",
                    "IdlePercent",
                    "RecvPackets",
                    "SendPackets",
                    "BeaconFails",
                ][..],
            ),
            Self::Iptv => ("reportgetSubgwIptvInfo", "List", &["MAC", "StbPort"][..]),
        };
        if v.get("cmd").and_then(Value::as_str) != Some(cmd)
            || v.get("SequenceId").and_then(Value::as_str) != Some(sequence)
        {
            return Ok(None);
        }
        if v.get("Status").and_then(Value::as_str) != Some("0") {
            return Err("TY read query returned non-success status".into());
        }
        let invalid = || "invalid TY read response contract".to_string();
        let items = v.get(list).and_then(Value::as_array).ok_or_else(invalid)?;
        if items.len() > 16 {
            return Err(invalid());
        }
        let mut sanitized = Vec::with_capacity(items.len());
        for item in items {
            let object = item.as_object().ok_or_else(invalid)?;
            let mut out = Map::new();
            for &field in fields {
                let text = object
                    .get(field)
                    .and_then(Value::as_str)
                    .ok_or_else(invalid)?;
                if text.len() > 256 || text.contains('\0') {
                    return Err(invalid());
                }
                out.insert(field.into(), json!(text));
            }
            sanitized.push(Value::Object(out));
        }
        let mut result = json!({
            "kind": match self {
                Self::Wifi => "wifi",
                Self::Radio => "radio",
                Self::Iptv => "iptv",
                Self::Cpu => unreachable!(),
            },
            "items": sanitized,
        });
        if self == Self::Wifi {
            for field in ["HardwareSwitch2.4G", "HardwareSwitch5G"] {
                let text = v.get(field).and_then(Value::as_str).ok_or_else(invalid)?;
                if text.len() > 16 || text.contains('\0') {
                    return Err(invalid());
                }
                result[field] = json!(text);
            }
        }
        Ok(Some(result))
    }
}
