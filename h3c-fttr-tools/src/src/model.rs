//! Strongly typed data models for H3C FTTR Micro-OLT telemetry and IPC.

use serde::{Deserialize, Serialize};

mod business;
pub use business::{BusinessPhase, BusinessStatus};

mod measurement;
pub use measurement::{MeasurementEvidence, Measurements};

mod inventory;
pub use inventory::{OnuGemPort, OnuOmciInventory, OnuVlanFilter, OnuVlanOperation};

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tx_enabled: Option<bool>,
    pub measurements: Measurements,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reserved_sn: Option<String>,
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
    pub rssi_dbm: Option<i32>,
    pub rx_rate_mbps: Option<u32>,
    pub tx_rate_mbps: Option<u32>,
}

/// Offload policy and system engine state, distinct from per-ONU hardware hits.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DataPathOffloadStatus {
    /// Either driver opt-in bit; unknown unless both modes are known or one is enabled.
    pub enabled: Option<bool>,
    #[serde(default)]
    pub routed_enabled: Option<bool>,
    #[serde(default)]
    pub bridge_enabled: Option<bool>,
    /// Global Ethernet PPE attachment; does not prove this ONU has a bound flow.
    pub engine_attached: Option<bool>,
    /// Unknown until per-flow hardware evidence is available.
    pub forwarding_verified: Option<bool>,
}

/// An active or recorded PPE hardware flow entry from debugfs.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PpeFlowEntry {
    pub index: String,
    pub state: String,
    pub pkt_type: String,
    pub orig_tuple: String,
    pub new_tuple: String,
    pub eth: String,
    pub vlan: String,
    pub packets: u64,
    pub bytes: u64,
    pub counters_valid: bool,
    pub packets_exact: String,
    pub bytes_exact: String,
}

/// Aggregated real-time NPU / PPE hardware offload status and statistics.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct NpuStatus {
    pub sampled_at_ms: u64,
    pub errors: Vec<String>,
    pub config_valid: bool,
    pub engine_valid: bool,
    pub mask_valid: bool,
    pub routed_mask_valid: bool,
    pub debug_valid: bool,
    pub flows_valid: bool,
    pub entries_valid: bool,
    pub counters_valid: bool,
    pub hooks_valid: bool,
    pub total_packets_exact: String,
    pub total_bytes_exact: String,
    pub supported: bool,
    pub engine_attached: bool,
    pub bridge_offload_enabled: bool,
    pub bridge_offload_mask: u16,
    pub routed_offload_mask: u16,
    pub bridge_offload_debug: bool,
    pub bridge: String,
    pub active_ports: Vec<String>,
    pub fw4_hooks_ready: bool,
    pub total_entries: u32,
    pub bound_entries: u32,
    pub total_packets: u64,
    pub total_bytes: u64,
    pub bound_flows: Vec<PpeFlowEntry>,
}

/// Data path statistics and provisioning parameters for an ONU.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataPathStatus {
    pub interface: String,
    pub gem_ports: Option<Vec<u16>>,
    pub vlan_id: Option<u16>,
    pub onu_inventory: Option<OnuOmciInventory>,
    pub tx_bytes: Option<u64>,
    pub rx_bytes: Option<u64>,
    pub current_tx_kbps: Option<f64>,
    pub current_rx_kbps: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub olt_channel_gem: Option<u16>,
    /// Matching ONU GEM configuration and observed software RX; not PPE hit proof.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forwarding_verified: Option<bool>,
    #[serde(default)]
    pub offload: DataPathOffloadStatus,
    pub measurements: Measurements,
}

/// Wi-Fi Mesh radio parameters reported by sub-gateway.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WifiMeshStatus {
    pub channel_2g: Option<u8>,
    pub channel_5g: Option<u8>,
    pub bandwidth_5g: Option<String>,
    pub tx_power_pct: Option<u8>,
    pub measurements: Measurements,
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
    pub state: crate::pon_engine::ploam::OnuState,
    #[serde(default)]
    pub business_status: BusinessStatus,
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
    pub fiber_distance_m: Option<f32>,
    pub ranging_delay_rtd_ns: Option<u32>,
    #[serde(default)]
    pub equalization_delay_bits: Option<u32>,
    pub optical_rx_power_dbm: Option<f64>,
    pub optical_tx_power_dbm: Option<f64>,
    pub firmware_version: String,
    pub hardware_version: String,
    pub uptime_seconds: Option<f64>,
    pub data_path: DataPathStatus,
    pub wifi_mesh: WifiMeshStatus,
    pub connected_clients: Option<Vec<MeshClientStatus>>,
    pub measurements: Measurements,
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
    pub onu_pinning: serde_json::Value,
    #[serde(default)]
    pub provisioning: serde_json::Value,
    #[serde(default = "default_fpga_status")]
    pub fpga_status: String,
    pub optical_transceiver: OpticalTransceiverStatus,
    pub switch_ports: Vec<SwitchPortStatus>,
    pub sub_gateways: Vec<SubGatewayStatus>,
    pub telemetry_gateways: Vec<TelemetryGatewayInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub npu: Option<NpuStatus>,
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

    pub fn subdev(
        status: impl Into<String>,
        sn: impl Into<String>,
        action: impl Into<String>,
    ) -> Self {
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
        let mut s = serde_json::to_string(self)
            .unwrap_or_else(|_| "{\"error\":\"json serialize error\"}".to_string());
        s.push('\n');
        s
    }
}
