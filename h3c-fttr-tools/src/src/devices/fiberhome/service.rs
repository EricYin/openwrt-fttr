//! Standard discovered bridge plus bounded FiberHome grant policy.
//! HG6371F-C and HG6371F expose different UNI inventories; never use SCTY's
//! fixed-entity cleanup. Generic discovery handles bootstrap and slot migration.
use std::{
    io,
    sync::{atomic::AtomicBool, Arc},
};

pub(super) fn configure(id: u8, cancellation: Option<Arc<AtomicBool>>) -> io::Result<()> {
    if id >= crate::pon_engine::ONU_SLOTS {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "ONU slot"));
    }
    crate::devices::generic::service::configure_with_grant_floors(
        id,
        cancellation,
        Some((60, 1300)),
    )?;
    println!("FIBERHOME_HG6371_BRIDGE_READY id={id} business=1300 management=60 GEM_RX=unverified");
    Ok(())
}
