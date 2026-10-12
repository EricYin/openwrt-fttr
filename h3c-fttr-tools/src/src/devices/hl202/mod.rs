//! HL202-DU-C: verified OMCI topology, private authentication and MQTT shell contract.
pub mod command;
pub mod legacy_commands;
pub mod mqtt_lifecycle;
pub mod management_wan;
pub mod service;
pub(super) fn matches(vendor: &str, equipment: Option<&str>) -> bool {
    vendor == "H3CT" && equipment == Some("HL202-DU-C")
}
