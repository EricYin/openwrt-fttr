//! Production controller: one daemon owns activation and per-ONU business workers.
mod activation;
mod attribute_probe;
mod lifetime;
pub mod business;
pub mod health;
pub mod inventory;
pub mod management;
#[cfg(feature = "mqtt")]
pub mod mqtt;
pub mod profile;
mod runtime;
mod status;
mod telemetry;
#[cfg(feature = "device-hl202")]
mod hl202;
pub(crate) mod service;
pub use runtime::run;
