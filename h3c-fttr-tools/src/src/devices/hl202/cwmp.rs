use crate::devices::cwmp::{contract::*, vendor::cu};

pub const CONTRACT: Contract = Contract {
    id: "hl202-cu",
    writes_verified: true,
    capability_groups: Capabilities {
        wan: Support::Supported,
        wlan: Support::Supported,
        binding: Support::Supported,
    },
    references: Some(ReferenceEncoding::FullPathsComma),
    service_encoding: Some(crate::devices::cwmp::codec::ServiceEncoding::Comma),
    wan: WanContract {
        binding: Some(ignore_wlan(
            reference(connection("X_CU_LanInterface")),
            &[10, 14],
        )),
        cloud_cpe: Some(boolean(connection("X_CU_CloudCPEEnable"))),
        ..cu::BASE.wan
    },
    wlan: WlanContract {
        radio_slots: Some([[1, 2, 3, 4], [5, 6, 7, 8]]),
        wpa2_beacon: "WPA2",
        wpa_wpa2_beacon: "WPA/WPA2",
        nat_writable_exception: true,
        passphrase_fallback: true,
    },
    ..cu::BASE
};
