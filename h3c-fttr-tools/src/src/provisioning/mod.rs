//! Desired-state provisioning. Keep wire formats at the controller boundary.
//!
//! `controller` coordinates operations; its `parameters` child executes diffs.
//! `wan` and `wifi` own their respective lifecycle state machines. `model`
//! defines internal types, `policy` centralizes WAN protection, and `storage`
//! provides persistence mechanics without choosing formats or retry behavior.
mod bootstrap;
mod config;
mod controller;
mod model;
mod policy;
mod storage;
mod templates;
mod wan;
mod wifi;

use controller::acs;
#[cfg(test)]
use controller::RPC;
pub(crate) use bootstrap::observe_mqtt;
pub use controller::{control, snapshot, start};
pub(crate) use controller::{management_desired, management_result, management_version};
pub(crate) use policy::contains_service_token;
#[cfg(feature = "acs")]
pub(crate) use policy::{owns_business_wan, protected_wan_parent};
