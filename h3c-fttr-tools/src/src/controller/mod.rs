//! Production controller: one daemon owns activation and per-ONU business workers.
mod activation;
pub mod business;
pub mod health;
pub mod inventory;
pub mod profile;
mod runtime;
mod service;
pub use runtime::run;
