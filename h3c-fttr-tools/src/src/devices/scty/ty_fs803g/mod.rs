//! COMHEART TY-FS803G, observed with MGS.V1.0.0.0 / HV1.0.0.0.
//!
//! The complete MIB advertises standard T-CONT/GEM, Ethernet UNIs and a VEIP.
//! Discover and reuse those resources; instance numbers are not a fixed template.
//! This is an OMCI bridge profile, not a CWMP identity or Wi-Fi contract.
pub(super) fn matches(equipment: Option<&str>) -> bool {
    equipment == Some("COMHEART TY-FS803G")
}

pub(super) fn configure(
    id: u8,
    cancellation: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> std::io::Result<()> {
    crate::devices::generic::service::configure(id, cancellation)
}
