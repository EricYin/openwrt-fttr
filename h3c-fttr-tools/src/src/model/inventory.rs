use serde::{Deserialize, Serialize};

/// Fresh ONU-reported configuration, not proof of active OLT forwarding.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OnuOmciInventory {
    pub age_seconds: u64,
    pub gem_ports: Vec<OnuGemPort>,
    pub vlan_filters: Vec<OnuVlanFilter>,
    pub vlan_operations: Vec<OnuVlanOperation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OnuGemPort {
    pub entity_id: u16,
    pub port_id: Option<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OnuVlanFilter {
    pub entity_id: u16,
    pub filter_list_hex: Option<String>,
    pub forward_operation: Option<u32>,
    pub number_of_entries: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OnuVlanOperation {
    pub entity_id: u16,
    /// Complete 16-byte G.988 class 171 attribute 6 rules, without inventing a scalar VID.
    pub received_frame_rules_hex: Option<Vec<String>>,
    pub error: Option<String>,
}
