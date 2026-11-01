//! HL202-DU-C: verified OMCI topology, private authentication and MQTT shell contract.
pub mod command;
pub mod request;
pub mod api;
pub mod telemetry;
#[cfg(feature = "mqtt")]
mod command_transport;
#[cfg(feature = "mqtt")]
pub mod command_binding;
#[cfg(feature = "mqtt")]
pub mod provisioning;
pub mod script;
#[cfg(feature = "mqtt")]
pub mod mqtt_lifecycle;
#[cfg(feature = "mqtt")]
pub mod management_wan;
pub mod service;
pub mod cwmp;
pub(super) fn matches(vendor: &str, equipment: Option<&str>) -> bool {
    vendor == "H3CT" && equipment == Some("HL202-DU-C")
}
#[cfg(feature = "mqtt")]
pub mod accounts;
