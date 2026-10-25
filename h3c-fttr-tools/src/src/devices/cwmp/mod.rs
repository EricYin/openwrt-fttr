//! CWMP parameter contracts, independent of OMCI adapter selection.
pub mod cache;
pub mod codec;
pub mod compiler;
pub mod contract;
pub mod discovery;
pub mod identity;
pub mod matching;
pub mod observed;
pub mod path;
pub mod probe;
pub mod registry;
pub mod runtime;
pub mod vendor;

use contract::Contract;

/// Compilation hint only. Runtime discovery must validate it before operations.
/// OMCI adapter selection has no influence on this decision.
pub fn configuration_contract(
    requested: &str,
    optical_vendor: &str,
) -> Result<&'static Contract, String> {
    let id = if requested == "auto" {
        match optical_vendor {
            "H3CT" => "hl202-cu",
            "HWTC" => "hwtc-hw",
            _ => return Err("unknown device requires an explicit CWMP candidate".into()),
        }
    } else {
        requested
    };
    registry::lookup(id)
}
pub mod response;
pub mod transport;
