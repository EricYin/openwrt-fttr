pub mod bosa;
pub(crate) mod i2c;
pub mod fpga;
pub mod ipc;
pub mod hl202_ipc;
#[cfg(feature = "mqtt")]
pub mod mqtt;
pub mod mqtt_identity;
#[cfg(any(feature = "mqtt", test))]
mod telemetry_sources;
#[cfg(any(feature = "mqtt", test))]
mod mqtt_limits;
pub mod pon_engine;
pub mod devices;
pub mod model;
pub mod controller;
pub mod acs_cli;
pub mod provisioning;
pub mod provisioning_cli;
#[cfg(feature = "acs")]
pub mod acs;
#[cfg(feature = "mqtt")]
pub mod broker;

// Unit fixtures which prove the MQTT-off boundary share the production port.
#[cfg(all(test, not(feature = "mqtt")))]
pub(crate) static MQTT_TEST_PORT: std::sync::Mutex<()> = std::sync::Mutex::new(());
