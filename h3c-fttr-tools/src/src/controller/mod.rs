//! Production controller: one daemon owns activation and per-ONU business workers.
mod activation;
pub mod business;
pub mod health;
pub mod inventory;
pub mod management;
pub mod profile;
mod runtime;
#[cfg(feature = "device-hl202")]
mod hl202;
pub(crate) mod service;
pub use runtime::run;
