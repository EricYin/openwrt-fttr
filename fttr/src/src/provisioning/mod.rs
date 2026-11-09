//! Desired-state provisioning. Keep wire formats at the controller boundary.
//!
//! `controller` coordinates operations; its `parameters` child executes diffs.
//! `wan` and `wifi` own their respective lifecycle state machines. `model`
//! defines internal types, `policy` centralizes WAN protection, and `storage`
//! provides persistence mechanics without choosing formats or retry behavior.
mod bootstrap;
mod config;
pub(crate) use config::words as uci_words;
pub(crate) mod controller;
pub(crate) mod health_watch;
pub(crate) mod link_identity;
mod management_network;
pub mod model;
mod policy;
pub(crate) mod storage;
mod templates;
mod wan;
mod wifi;

#[cfg(feature = "mqtt")]
pub(crate) use bootstrap::observe_mqtt;
use controller::acs;
#[cfg(feature = "acs")]
pub(crate) use controller::connection_request_devices;
#[cfg(test)]
use controller::RPC;
pub(crate) use controller::{
    allows_generic_bridge, is_device_adapter, management_desired, management_result,
};
pub use controller::{control, snapshot, start};
pub(crate) use policy::contains_service_token;
#[cfg(feature = "acs")]
pub(crate) use policy::{owns_business_wan, protected_wan_parent};
