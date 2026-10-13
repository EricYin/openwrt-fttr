use super::super::contract::*;

// Candidate only: presence is not evidence that this firmware supports writes.
pub const BASE: Contract = Contract {
    supported_services: &["INTERNET", "OTHER", "IPTV", "VOIP"],
    id: "vendor-ct-candidate",
    vendor: "ct",
    writes_verified: false,
    capability_groups: UNVERIFIED,
    references: None,
    requirements: OperationRequirements {
        disable_wan_before_write: true,
        settle_seconds: 0,
    },
    service_encoding: None,
    wan: WanContract {
        service: services(connection("X_CT-COM_ServiceList")),
        vlan: unsigned(connection("X_CT-COM_VLANID")),
        enabled: None,
        priority: unsigned(connection("X_CT-COM_802-1p")),
        binding: None,
        cloud_cpe: None,
    },
    wlan: WlanContract {
        radio_slots: None,
        wpa2_beacon: "11i",
        nat_writable_exception: false,
        passphrase_fallback: false,
    },
};
