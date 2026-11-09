// Copyright 2026 Lazuli Kao / openwrt-fttr contributors
// SPDX-License-Identifier: Apache-2.0

use super::adapter::{
    IdentificationStatus, PlatformAdapter, PlatformDescriptor, PlatformId, PlatformImplementation,
};

pub struct Hm2004Du;

impl PlatformAdapter for Hm2004Du {
    const DESCRIPTOR: PlatformDescriptor = PlatformDescriptor {
        id: PlatformId::H3cHm2004Du,
        vendor: "H3C",
        model: "HM2004-DU",
        device_tree_compatibles: &["h3c,hm2004-du"],
        board_names: &["h3c,hm2004-du"],
        identification: IdentificationStatus::Verified,
        implementation: PlatformImplementation::ExistingH3cRuntime,
    };
}
