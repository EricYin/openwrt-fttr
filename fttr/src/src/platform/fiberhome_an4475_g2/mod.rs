// Copyright 2026 Lazuli Kao / openwrt-fttr contributors
// SPDX-License-Identifier: Apache-2.0

use super::adapter::{
    IdentificationStatus, PlatformAdapter, PlatformDescriptor, PlatformId, PlatformImplementation,
};

#[cfg(feature = "platform-fiberhome-an4475-g2")]
pub mod calibration;
#[cfg(feature = "platform-fiberhome-an4475-g2")]
pub(crate) mod diagnostics;
#[cfg(feature = "platform-fiberhome-an4475-g2")]
pub mod downstream;
#[cfg(feature = "platform-fiberhome-an4475-g2")]
pub mod initialization;
#[cfg(feature = "platform-fiberhome-an4475-g2")]
pub mod management;
#[cfg(feature = "platform-fiberhome-an4475-g2")]
pub mod optical;
#[cfg(feature = "platform-fiberhome-an4475-g2")]
pub mod polling;
pub mod profile;
#[cfg(feature = "platform-fiberhome-an4475-g2")]
pub mod protocol;
#[cfg(feature = "platform-fiberhome-an4475-g2")]
pub mod serdes_apb;
#[cfg(all(test, feature = "platform-fiberhome-an4475-g2"))]
mod test_support;
#[cfg(feature = "platform-fiberhome-an4475-g2")]
mod transport;
#[cfg(feature = "platform-fiberhome-an4475-g2")]
pub mod upstream;

pub struct An4475G2;

impl PlatformAdapter for An4475G2 {
    const DESCRIPTOR: PlatformDescriptor = PlatformDescriptor {
        id: PlatformId::FiberhomeAn4475G2,
        vendor: "FiberHome",
        model: "AN4475-G2",
        // Exact PonWrt board DTS identity, not the original generic AN7581 EVB ID.
        // This confirms source identity only, not live wiring or hardware readiness.
        device_tree_compatibles: &["fiberhome,an4475-g2"],
        board_names: &["fiberhome,an4475-g2"],
        identification: IdentificationStatus::Verified,
        implementation: PlatformImplementation::DiagnosticsOnly,
    };
}
