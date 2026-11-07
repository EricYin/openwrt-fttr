//! HL202-DU-C: verified OMCI topology, private authentication and MQTT shell contract.
pub mod api;
pub mod command;
#[cfg(feature = "mqtt-hl202")]
pub mod command_binding;
#[cfg(feature = "mqtt-hl202")]
mod command_transport;
pub mod cwmp;
#[cfg(feature = "mqtt-hl202")]
pub mod management_wan;
#[cfg(feature = "mqtt-hl202")]
pub mod mqtt_lifecycle;
#[cfg(feature = "mqtt-hl202")]
pub mod provisioning;
pub mod request;
pub mod script;
pub mod service;
pub mod telemetry;
pub(super) fn matches(vendor: &str, equipment: Option<&str>) -> bool {
    vendor == "H3CT" && equipment == Some("HL202-DU-C")
}
#[cfg(feature = "mqtt-hl202")]
pub mod accounts;
