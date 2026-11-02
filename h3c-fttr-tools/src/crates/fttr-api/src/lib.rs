//! Wire contracts shared by the native ubus server and the host-side SDK generator.
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, marker::PhantomData};

pub const OBJECT: &str = "fttr";

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
    #[cfg_attr(feature = "codegen", ts(type = "\"success\" | \"error\""))]
    pub status: String,
    #[cfg_attr(
        feature = "codegen",
        ts(
            type = "\"optical_on\" | \"optical_off\" | \"optical_status\" | \"optical_reload\" | \"optical_reinitialize\" | \"optical_shutdown\""
        )
    )]
    pub action: String,
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

pub struct Method<Request, Response> {
    pub name: &'static str,
    pub sdk_name: &'static str,
    pub params: &'static [&'static str],
    pub access: Access,
    types: PhantomData<fn(Request) -> Response>,
}

impl<Q, R> Method<Q, R> {
    pub const fn new(
        name: &'static str,
        sdk_name: &'static str,
        params: &'static [&'static str],
        access: Access,
    ) -> Self {
        Self {
            name,
            sdk_name,
            params,
            access,
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
            GET_CAPABILITIES, get_capabilities, getCapabilities, EmptyRequest, Capabilities, Read, [];
            OPTICAL_STATUS, optical_status, opticalStatus, EmptyRequest, OpticalReply, Read, [];
            OPTICAL_SET, optical_set, opticalSet, OpticalSetRequest, OpticalReply, Write, [enabled];
        }
    };
}

macro_rules! declare_methods {
    ($( $id:ident, $wire:ident, $sdk:ident, $req:ident, $res:ident, $access:ident, [$($param:ident),*]; )*) => {
        $(pub const $id: Method<$req, $res> = Method::new(stringify!($wire), stringify!($sdk), &[$(stringify!($param)),*], Access::$access);)*
    };
}
fttr_methods!(declare_methods);
