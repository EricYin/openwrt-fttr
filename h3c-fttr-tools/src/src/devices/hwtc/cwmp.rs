use crate::devices::cwmp::{contract::Contract, vendor::hw};

// Preserve existing HWTC WAN/WLAN behavior. Binding remains unavailable.
pub const CONTRACT: Contract = Contract {
    id: "hwtc-hw",
    writes_verified: true,
    wlan: crate::devices::cwmp::contract::WlanContract {
        radio_slots: Some([[1, 2, 3, 4], [5, 6, 7, 8]]),
        ..hw::BASE.wlan
    },
    capability_groups: crate::devices::cwmp::contract::Capabilities {
        wan: crate::devices::cwmp::contract::Support::Supported,
        wlan: crate::devices::cwmp::contract::Support::Supported,
        binding: crate::devices::cwmp::contract::Support::Unverified,
    },
    ..hw::BASE
};
