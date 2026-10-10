//! Strongly typed data models for H3C FTTR Micro-OLT telemetry and IPC.

use serde::{Deserialize, Serialize};

/// Physical and optical telemetry for the downstream GPON BOSA transceiver.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpticalTransceiverStatus {
    pub model: String,
    pub wavelength_tx_nm: u32,
    pub wavelength_rx_nm: u32,
    pub phy_rate_downlink_gbps: f32,
    pub phy_rate_uplink_gbps: f32,
    pub tx_power_dbm: Option<f32>,
    pub laser_bias_current_ma: Option<f32>,
    pub temperature_celsius: Option<f32>,
    pub vcc_voltage: Option<f32>,
    pub cdr_locked: Option<bool>,
}

/// Downstream FTTR DSA switch port link and carrier status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwitchPortStatus {
    pub port: u8,
    pub interface: String,
    pub carrier: bool,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub onu_id: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub onu_sn: Option<String>,
    #[serde(default = "default_link_state")]
    pub link_state: String,
}

fn default_link_state() -> String {
    "Offline".to_string()
}

/// Connected client station on sub-gateway Wi-Fi EasyMesh.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshClientStatus {
    pub mac: String,
    pub ip: String,
    pub band: String,
    pub rssi_dbm: i32,
    pub rx_rate_mbps: u32,
    pub tx_rate_mbps: u32,
}

/// Data path statistics and provisioning parameters for an ONU.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataPathStatus {
    pub interface: String,
    pub gem_ports: Vec<u16>,
    pub vlan_id: u16,
    pub tx_bytes: u64,
    pub rx_bytes: u64,
    pub current_tx_kbps: u32,
    pub current_rx_kbps: u32,
}

/// Wi-Fi Mesh radio parameters reported by sub-gateway.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WifiMeshStatus {
    pub channel_2g: u8,
    pub channel_5g: u8,
    pub bandwidth_5g: String,
    pub tx_power_pct: u8,
}

/// Comprehensive telemetry and configuration state of a sub-gateway (HL202-DU).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OnuIdentity {
    pub equipment_id: Option<String>,
    pub hardware_version: Option<String>,
    pub software_images: Vec<String>,
    pub omcc_version: Option<u8>,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubGatewayStatus {
    pub onu_id: u8,
    pub vendor: String,
    pub model: String,
    pub serial_number: String,
    pub state: String,
    #[serde(default)]
    pub business_status: String,
    #[serde(default)]
    pub management: serde_json::Value,
    #[serde(default)]
    pub provisioning: serde_json::Value,
    #[serde(default)]
    pub services: Vec<serde_json::Value>,
    #[serde(default)]
    pub ssids: Vec<serde_json::Value>,
    #[serde(default)]
    pub wifi_sync: String,
    #[serde(default)]
    pub identity: Option<OnuIdentity>,
    #[serde(default)]
    pub identity_age_seconds: Option<u64>,
    #[serde(default)]
    pub identity_current: bool,
    pub fiber_distance_m: f32,
    pub ranging_delay_rtd_ns: u32,
    #[serde(default)]
    pub equalization_delay_bits: Option<u32>,
    pub optical_rx_power_dbm: f32,
    pub optical_tx_power_dbm: f32,
    pub firmware_version: String,
    pub hardware_version: String,
    pub uptime_seconds: u64,
    pub data_path: DataPathStatus,
    pub wifi_mesh: WifiMeshStatus,
    pub connected_clients: Vec<MeshClientStatus>,
}

/// Lightweight telemetry record for sub-gateways reporting via MQTT.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryGatewayInfo {
    pub id: String,
    pub last_seen_sec_ago: u64,
    pub last_payload_bytes: usize,
}

/// Aggregated system status model returned by fttrd daemon on STATUS request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FttrSystemStatus {
    #[serde(default)]
    pub provisioning: serde_json::Value,
    #[serde(default = "default_fpga_status")]
    pub fpga_status: String,
    pub optical_transceiver: OpticalTransceiverStatus,
    pub switch_ports: Vec<SwitchPortStatus>,
    pub sub_gateways: Vec<SubGatewayStatus>,
    pub telemetry_gateways: Vec<TelemetryGatewayInfo>,
}

fn default_fpga_status() -> String {
    "ready".to_string()
}

/// Generic IPC response structure for control commands.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sn: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub onu_id: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub omci_messages: Option<usize>,
}

impl IpcResponse {
    pub fn ok(status: impl Into<String>) -> Self {
        Self {
            status: Some(status.into()),
            error: None,
            sn: None,
            action: None,
            onu_id: None,
            port: None,
            state: None,
            omci_messages: None,
        }
    }

    pub fn err(error: impl Into<String>) -> Self {
        Self {
            status: None,
            error: Some(error.into()),
            sn: None,
            action: None,
            onu_id: None,
            port: None,
            state: None,
            omci_messages: None,
        }
    }

    pub fn subdev(status: impl Into<String>, sn: impl Into<String>, action: impl Into<String>) -> Self {
        Self {
            status: Some(status.into()),
            error: None,
            sn: Some(sn.into()),
            action: Some(action.into()),
            onu_id: None,
            port: None,
            state: None,
            omci_messages: None,
        }
    }

    pub fn to_json_line(&self) -> String {
        let mut s = serde_json::to_string(self).unwrap_or_else(|_| "{\"error\":\"json serialize error\"}".to_string());
        s.push('\n');
        s
    }
}
