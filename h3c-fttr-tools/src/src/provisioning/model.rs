//! Internal business models. Serialized names are part of the existing status API.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

macro_rules! wire_enum {
    ($name:ident { $($variant:ident => $wire:literal),+ $(,)? }) => {
        #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
        pub enum $name {
            $(#[serde(rename = $wire)] $variant),+
        }

        impl $name {
            pub(super) const fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $wire),+ }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

wire_enum!(OperationPhase {
    Waiting => "waiting",
    WanReconciling => "wan_reconciling",
    WanVerifying => "wan_verifying",
    WifiReconciling => "wifi_reconciling",
    Discovering => "discovering",
    Reading => "reading",
    Disabling => "disabling",
    DisablingRead => "disabling_read",
    Writing => "writing",
    Verifying => "verifying",
    Completed => "completed",
});

wire_enum!(WanPhase {
    Waiting => "waiting",
    Inventory => "inventory",
    Classifying => "classifying",
    AddingParent => "adding_parent",
    ChildNames => "child_names",
    AddingChild => "adding_child",
    DeleteGuard => "delete_guard",
    Deleting => "deleting",
    VerifyDelete => "verify_delete",
    EmptyParent => "empty_parent",
    DeletingParent => "deleting_parent",
    VerifyParentDelete => "verify_parent_delete",
    Next => "next",
    Done => "done",
});

wire_enum!(WifiPhase {
    Waiting => "waiting",
    Inventory => "inventory",
    Adding => "adding",
    VerifyAdd => "verify_add",
    Deleting => "deleting",
    VerifyDelete => "verify_delete",
    ReadingDisable => "reading_disable",
    Disabling => "disabling",
    VerifyingDisable => "verifying_disable",
    Next => "next",
    Done => "done",
});

wire_enum!(ItemKind {
    DeviceService => "device_service",
    Ssid => "ssid",
    PortBinding => "port_binding",
    WanActivation => "wan_activation",
});

wire_enum!(ParameterKind {
    String => "string",
    Boolean => "boolean",
    UnsignedInt => "unsignedInt",
});

/// Values retain their CWMP lexical representation (including 0/1 booleans).
#[derive(Clone, Serialize, Deserialize)]
pub struct Parameter {
    pub name: String,
    pub value: String,
    pub kind: ParameterKind,
    pub secret: bool,
}

#[derive(Clone)]
pub struct Item {
    pub id: String,
    pub kind: ItemKind,
    pub service_type: String,
    pub parameters: Vec<Parameter>,
    pub guard: Option<String>,
    pub blocked: Option<String>,
    pub wan_ref: Option<String>,
    pub auto_instance: Option<u32>,
}

#[derive(Clone)]
pub struct Device {
    pub id: String,
    pub optical_sn: String,
    pub cwmp_id: String,
    pub adapter: String,
    pub items: Vec<Item>,
    pub wifi_cleanup: String,
    pub wifi_preserve: BTreeSet<u32>,
}

#[derive(Clone, Default)]
pub struct Config {
    pub revision: String,
    pub enabled: bool,
    pub automatic: bool,
    pub triggers: BTreeSet<String>,
    pub interval: u64,
    pub freshness: u64,
    pub devices: Vec<Device>,
    pub assignments: BTreeMap<String, (String, String)>,
    pub groups: BTreeMap<String, String>,
    pub management: Option<Management>,
    pub labels: BTreeMap<String, String>,
}

#[derive(Clone, Serialize)]
pub struct Management {
    pub vlan: u32,
    pub priority: u32,
    pub acs_url: String,
}
