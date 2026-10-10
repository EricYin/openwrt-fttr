//! K155-11: standard OMCI bridge service; proprietary shell management unavailable.
pub(super) fn matches(vendor: &str, equipment: Option<&str>) -> bool {
    vendor == "HWTC" && equipment == Some("K155-11")
}
pub mod service;
#[cfg(feature = "acs")]
mod acs;
