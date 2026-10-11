use std::{io, sync::{Arc, atomic::AtomicBool}};

pub fn configure(id: u8, cancellation: Option<Arc<AtomicBool>>) -> io::Result<()> {
    super::super::service::configure_profile(id, cancellation, super::super::Profile::K155)
}
