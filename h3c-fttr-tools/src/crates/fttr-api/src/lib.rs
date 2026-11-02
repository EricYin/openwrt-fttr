//! Wire contracts shared by the native ubus server and the host-side SDK generator.
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, marker::PhantomData};

pub const OBJECT: &str = "fttr";

mod queries;
pub use queries::*;

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct AdapterCapability {
    pub id: &'static str,
    pub name: &'static str,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct Capabilities {
    #[cfg_attr(feature = "codegen", ts(type = "1"))]
    pub schema_version: u32,
    pub version: &'static str,
    pub features: BTreeMap<&'static str, bool>,
    pub functions: BTreeMap<&'static str, bool>,
    pub pages: BTreeMap<&'static str, bool>,
    pub adapters: Vec<AdapterCapability>,
}

#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct OpticalReply {
    #[cfg_attr(feature = "codegen", ts(type = "1"))]
    pub schema_version: u8,
    pub status: ReplyStatus,
    pub action: OpticalAction,
    pub desired_enabled: Option<bool>,
    pub tx_enabled: Option<bool>,
    pub actor_state: OpticalState,
    pub hardware_ready: bool,
    pub discovery_active: bool,
    pub cleanup_pending: bool,
    pub error: Option<OpticalError>,
}

#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct OpticalError {
    pub code: String,
    pub message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum ReplyStatus {
    Success,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum OpticalAction {
    OpticalOn,
    OpticalOff,
    OpticalStatus,
    OpticalReload,
    OpticalReinitialize,
    OpticalShutdown,
}

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum OpticalState {
    Enabled,
    Disabled,
    Unknown,
}

#[derive(Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct EmptyRequest {}

#[derive(Deserialize)]
#[cfg_attr(feature = "codegen", derive(ts_rs::TS))]
pub struct OpticalSetRequest {
    pub enabled: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Read,
    Write,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Availability {
    Always,
    Npu,
}

pub struct Method<Request, Response> {
    pub name: &'static str,
    pub sdk_name: &'static str,
    pub params: &'static [&'static str],
    pub access: Access,
    pub availability: Availability,
    types: PhantomData<fn(Request) -> Response>,
}

impl<Q, R> Method<Q, R> {
    pub const fn new(
        name: &'static str,
        sdk_name: &'static str,
        params: &'static [&'static str],
        access: Access,
        availability: Availability,
    ) -> Self {
        Self {
            name,
            sdk_name,
            params,
            access,
            availability,
            types: PhantomData,
        }
    }
}

// This list is consumed by both registration and code generation. Adding an entry
// also requires a typed handler at the server's registration site.
#[macro_export]
macro_rules! fttr_methods {
    ($consumer:ident) => {
        $consumer! {
            GET_CAPABILITIES, get_capabilities, getCapabilities, EmptyRequest, Capabilities, Read, Always, [];
            OPTICAL_STATUS, optical_status, opticalStatus, EmptyRequest, OpticalReply, Read, Always, [];
            OPTICAL_SET, optical_set, opticalSet, OpticalSetRequest, OpticalReply, Write, Always, [enabled];
            GET_STATUS, get_status, getStatus, EmptyRequest, StatusReply, Read, Always, [];
            GET_SYNC, get_sync, getSync, EmptyRequest, SyncStatusReply, Read, Always, [];
            CONFIG_CHECK, config_check, configCheck, EmptyRequest, ConfigCheckReply, Read, Always, [];
            NPU_STATUS, npu_status, npuStatus, EmptyRequest, NpuStatus, Read, Npu, [];
        }
    };
}

macro_rules! declare_methods {
    ($( $id:ident, $wire:ident, $sdk:ident, $req:ident, $res:ident, $access:ident, $availability:ident, [$($param:ident),*]; )*) => {
        $(pub const $id: Method<$req, $res> = Method::new(stringify!($wire), stringify!($sdk), &[$(stringify!($param)),*], Access::$access, Availability::$availability);)*
    };
}
fttr_methods!(declare_methods);
