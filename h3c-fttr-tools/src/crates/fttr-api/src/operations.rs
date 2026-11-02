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
#[serde(untagged)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub enum JobStartReply {
    Pending { status: PendingStatus, job: String },
    Error(CommandError),
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub enum JobChunkReply {
    Pending {
        status: PendingStatus,
    },
    Ready {
        status: ReadyStatus,
        chunk: u16,
        chunks: u16,
        bytes: u32,
        data: String,
    },
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
    Saved {
        success: bool,
        path: String,
        size: u32,
        sha256: String,
    },
    Error(CommandError),
}

// Nested device models remain owned and validated by the existing adapter.
#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub enum HlListReply {
    List {
        #[cfg_attr(feature = "codegen", ts(type = "1"))]
        schema_version: u8,
        #[cfg_attr(feature = "codegen", ts(type = "Array<unknown>"))]
        devices: Vec<serde_json::Value>,
        #[cfg_attr(feature = "codegen", ts(type = "Array<unknown>"))]
        sources: Vec<serde_json::Value>,
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
