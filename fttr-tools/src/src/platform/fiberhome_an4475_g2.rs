// Copyright 2026 Lazuli Kao / openwrt-fttr contributors
// SPDX-License-Identifier: Apache-2.0

use super::adapter::{
    IdentificationStatus, PlatformAdapter, PlatformDescriptor, PlatformId, PlatformImplementation,
};

pub struct An4475G2;

impl PlatformAdapter for An4475G2 {
    const DESCRIPTOR: PlatformDescriptor = PlatformDescriptor {
        id: PlatformId::FiberhomeAn4475G2,
        vendor: "FiberHome",
        model: "AN4475-G2",
        // Do not infer a board identifier from the reported CPU similarity.
        device_tree_compatibles: &[],
        board_names: &[],
        identification: IdentificationStatus::PendingHardwareEvidence,
        implementation: PlatformImplementation::CompileableStub,
    };
}
