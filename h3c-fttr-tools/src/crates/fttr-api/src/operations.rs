use crate::CommandError;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub enum PendingStatus {
    Pending,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub enum ReadyStatus {
    Ready,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub enum JobPhase {
    Starting,
    Upload,
    Tables,
}

#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct JobProgress {
    pub phase: JobPhase,
    pub received: u32,
    pub expected: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct JobStats {
    pub accepting: bool,
    pub running: u32,
    pub stored: u32,
    pub bytes: u32,
    pub running_limit: u32,
    pub stored_limit: u32,
    pub bytes_limit: u32,
    pub result_bytes_limit: u32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct UbusMethodStats {
    #[cfg_attr(feature = "codegen", ts(type = "number"))]
    pub calls: u64,
    #[cfg_attr(feature = "codegen", ts(type = "number"))]
    pub failures: u64,
    #[cfg_attr(feature = "codegen", ts(type = "number"))]
    pub last_us: u64,
    #[cfg_attr(feature = "codegen", ts(type = "number"))]
    pub max_us: u64,
    #[cfg_attr(feature = "codegen", ts(type = "number"))]
    pub total_us: u64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub enum JobStartReply {
    Pending(JobPending),
    Error(CommandError),
}

#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct JobPending {
    pub status: PendingStatus,
    pub job: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub enum JobChunk {
    Pending {
        status: PendingStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "codegen", ts(optional))]
        progress: Option<JobProgress>,
    },
    Ready {
        status: ReadyStatus,
        chunk: u16,
        chunks: u16,
        bytes: u32,
        data: String,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub enum JobChunkReply {
    Chunk(JobChunk),
    Error(CommandError),
}
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct JobReleaseReply {
    pub released: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub enum NpuSetReply {
    Success { enabled: bool },
    Error { error: String },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub enum CalibrationSource {
    Factory,
    Firmware,
    Builtin,
}
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct FactoryCalibration {
    pub detected: bool,
    pub device: String,
    pub has_data: bool,
    pub sha256: String,
    pub data: String,
}
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct FirmwareCalibration {
    pub exists: bool,
    pub path: String,
    pub size: u32,
    pub sha256: String,
    pub data: String,
}
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct CalibrationInfo {
    pub source: CalibrationSource,
    pub active_path: String,
    pub size: u32,
    pub sha256: String,
    pub data: String,
    pub factory: FactoryCalibration,
    pub firmware: FirmwareCalibration,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub enum CalibrationInfoReply {
    Info(CalibrationInfo),
    Error(CommandError),
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub enum CalibrationSaveReply {
    Saved(CalibrationSaved),
    Error(CommandError),
}

#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct CalibrationSaved {
    pub success: bool,
    pub path: String,
    pub size: u32,
    pub sha256: String,
}

/// The final calibration job result; success=false is a business warning.
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct CalibrationApplied {
    pub success: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub output: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", ts(optional))]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct MqttSource {
    pub mqtt_mac: String,
    pub optical_sn: Option<String>,
    pub onu_id: Option<u16>,
    pub binding_generation: String,
    #[cfg_attr(feature = "codegen", ts(type = "number"))]
    pub last_seen: u64,
    pub reason: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct HlDevice {
    pub optical_sn: String,
    pub onu_id: u16,
    pub model: String,
    pub optical_online: bool,
    pub can_execute: bool,
    pub mqtt: Option<MqttSource>,
    #[cfg_attr(feature = "codegen", ts(type = "number | null"))]
    pub report_revision: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub enum HlListReply {
    List {
        #[cfg_attr(feature = "codegen", ts(type = "1"))]
        schema_version: u8,
        devices: Vec<HlDevice>,
        sources: Vec<MqttSource>,
    },
    Error {
        schema_version: u8,
        error: crate::OpticalError,
    },
}

#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct NpuSetRequest {
    pub enabled: bool,
    pub bridge: String,
}

#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct MibStartRequest {
    pub onu_id: u8,
    pub optical_sn: String,
    pub tables: bool,
}

#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct JobRequest {
    pub job: String,
    pub chunk: u16,
}

#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct JobReleaseRequest {
    pub job: String,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub enum HlTarget {
    Optical {
        optical_sn: String,
        onu_id: u16,
        binding_generation: String,
    },
    Source {
        mqtt_mac: String,
        binding_generation: String,
    },
}

#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct HlReportRequest {
    pub target: HlTarget,
}

#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct HlAccountsRequest {
    pub target: HlTarget,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub enum AccountChange {
    AccountSetPassword,
    AccountSetActive,
}

#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct HlUpdateRequest {
    pub target: HlTarget,
    pub kind: AccountChange,
    pub entry: u16,
    pub password: String,
    pub active: bool,
}

#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct HlExecRequest {
    pub target: HlTarget,
    pub script: String,
    pub timeout_seconds: u16,
}

#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct CalibrationSaveRequest {
    pub data: String,
}
