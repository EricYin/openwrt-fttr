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

// V152 exposes individual unsignedInt 0/1 switches, not a reference string.
pub const V152_CONTRACT: Contract = Contract {
    id: "hwtc-v152-hw",
    references: Some(crate::devices::cwmp::contract::ReferenceEncoding::HwLanBindFlags),
    wan: crate::devices::cwmp::contract::WanContract {
        binding: Some(crate::devices::cwmp::contract::unsigned(
            crate::devices::cwmp::contract::connection("X_HW_LANBIND.Lan1Enable"),
        )),
        ..CONTRACT.wan
    },
    capability_groups: crate::devices::cwmp::contract::Capabilities {
        binding: crate::devices::cwmp::contract::Support::Supported,
        ..CONTRACT.capability_groups
    },
    ..CONTRACT
};

// K155-11 is a pure bridge FTTR sub-gateway with hardware-bridged SSIDs; no LAN/SSID binding switches.
pub const K155_CONTRACT: Contract = Contract {
    id: "hwtc-k155-hw",
    writes_verified: true,
    wlan: crate::devices::cwmp::contract::WlanContract {
        radio_slots: Some([[1, 2, 3, 4], [5, 6, 7, 8]]),
        wpa2_beacon: "11i",
        wpa_wpa2_beacon: "WPAand11i",
        nat_writable_exception: false,
        passphrase_fallback: false,
    },
    capability_groups: crate::devices::cwmp::contract::Capabilities {
        wan: crate::devices::cwmp::contract::Support::Supported,
        wlan: crate::devices::cwmp::contract::Support::Supported,
        binding: crate::devices::cwmp::contract::Support::Unsupported,
    },
    ..hw::BASE
};
