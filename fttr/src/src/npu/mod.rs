//! FTTR bridge and routed offload control-plane and hardware diagnostics.
//!
//! Submodules:
//! - `command`: Bounded sync/async process execution without shell or leaking child processes.
//! - `ppe`: Debugfs PPE counter, flow entry parsing, and status aggregation.
//! - `reconciler`: Topology discovery, fw4 hook inspection, and Fast-Path sysfs link checks.
//! - `service`: Background supervision service with adaptive polling and safe admission lifecycle.

pub mod command;
pub mod ppe;
pub mod reconciler;
pub mod service;
pub mod settings;

use crate::model::NpuStatus;
use std::{io, path::Path, time::Duration};

pub use ppe::{parse_ppe_entry, status_from};
pub use reconciler::{hooks_ready, text_offload_flag, topology, Reconciler, Topology};
pub use service::{is_supported_platform, Service};

/// Synchronous NPU status query used by fttrctl and snapshot telemetry.
pub fn status() -> NpuStatus {
    let mut status = status_from(
        Path::new("/etc/config/fttr"),
        Path::new("/sys/kernel/debug/ppe"),
        Path::new("/sys/module/h3c_fmcs/parameters"),
    );

    // Inspect active modes' flowtables.
    let routed = status.wan_upstream || status.wan_downstream;
    let bridge = status.onu_bridge;
    let hooks = (|| -> io::Result<bool> {
        let mut all_ready = true;
        if routed {
            if !status.routed_mask_valid {
                return Err(io::Error::other("routed port mask unavailable"));
            }
            let json = command::run_sync(
                "nft",
                &["-j", "list", "flowtable", "inet", "fw4", "ft"],
                Duration::from_secs(2),
            )?;
            let table: serde_json::Value = serde_json::from_str(&json).map_err(io::Error::other)?;
            let has_flags = table["nftables"].as_array().is_some_and(|entries| {
                entries
                    .iter()
                    .any(|entry| entry["flowtable"].get("flags").is_some())
            });
            let text = if has_flags {
                String::new()
            } else {
                command::run_sync(
                    "nft",
                    &["list", "flowtable", "inet", "fw4", "ft"],
                    Duration::from_secs(2),
                )?
            };
            if !reconciler::routed_hooks_ready(&table, &text) {
                all_ready = false;
            }
        }
        if bridge {
            if !status.mask_valid {
                return Err(io::Error::other("bridge port mask unavailable"));
            }
            let json = command::run_sync(
                "nft",
                &["-j", "list", "flowtable", "bridge", "fw4", "fb"],
                Duration::from_secs(2),
            )?;
            let table: serde_json::Value = serde_json::from_str(&json).map_err(io::Error::other)?;
            let ports = status
                .active_ports
                .iter()
                .map(|p| (p.clone(), Vec::new()))
                .collect::<Vec<_>>();
            let has_flags = table["nftables"].as_array().is_some_and(|entries| {
                entries
                    .iter()
                    .any(|entry| entry["flowtable"].get("flags").is_some())
            });
            let text = if has_flags {
                String::new()
            } else {
                command::run_sync(
                    "nft",
                    &["list", "flowtable", "bridge", "fw4", "fb"],
                    Duration::from_secs(2),
                )?
            };
            if !hooks_ready(&table, &ports, &text) {
                all_ready = false;
            }
        }
        Ok(all_ready)
    })();

    match hooks {
        Ok(ready) => {
            status.hooks_valid = true;
            status.fw4_hooks_ready = ready;
        }
        Err(error) => status.errors.push(format!("fw4 hooks: {error}")),
    }

    status
}
