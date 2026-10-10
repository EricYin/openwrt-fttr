//! K155-11 standard OMCI bridge, verified by live GET and upstream data capture.
use crate::controller::business::{create_verified, verify, assign_tcont};
use crate::pon_engine::omci_transport::{lock_onu, OmciSession, StockOmccLease};
use std::{io, sync::{Arc, atomic::AtomicBool}};

struct ChannelLease { id: u8, previous: u16 }
impl Drop for ChannelLease {
    fn drop(&mut self) { let _ = crate::bosa::exchange_omci_channel(self.id, self.previous); }
}

pub fn configure(id: u8, cancellation: Option<Arc<AtomicBool>>) -> io::Result<()> {
    if !(1..=15).contains(&id) { return Err(io::Error::new(io::ErrorKind::InvalidInput, "ONU ID must be 1..15")); }
    let _lock = lock_onu(id)?;
    let mut session = OmciSession::open(id)?;
    session.require_operation();
    if let Some(token) = cancellation { session.cancel_when(token); }
    let _lease = StockOmccLease::open(id)?;
    let previous = crate::bosa::exchange_omci_channel(id, 0)?;
    let _channel = ChannelLease { id, previous };
    verify(&mut session, 256, 0, 0x8000, b"HWTC")?;
    let equipment = session.get(257, 0, 0x8000)?;
    let text = &equipment.payload[3..23];
    let end = text.iter().position(|v| *v == 0 || *v == 0xff).unwrap_or(text.len());
    if std::str::from_utf8(&text[..end]).map(str::trim) != Ok("K155-11") {
        return Err(io::Error::new(io::ErrorKind::Unsupported, "K155-11 identity required"));
    }
    // The live MIB exposes these ONU-created endpoints and priority queues.
    for (class, instance) in [(262,0x8000),(277,0x8000),(11,0x101),(11,0x102),(329,0x601)] {
        session.get(class, instance, 0x8000)?;
    }
    let mut loid = [0;24]; loid[..10].copy_from_slice(b"subGateway");
    let mut password = [0;12]; password[..10].copy_from_slice(b"0000000000");
    verify(&mut session, 256, 0, 0x0040, &loid)?;
    verify(&mut session, 256, 0, 0x0020, &password)?;
    // G.988 ONU-G credentials status, not H3C private class 65530.
    session.set(256, 0, 0x0010, &[1])?;
    verify(&mut session, 256, 0, 0x0010, &[1])?;
    println!("K155_CREDENTIALS_STATUS_READBACK_OK");
    let port = 0x100u16 + id as u16;
    let entity = 0x200u16;
    create_verified(&mut session, 272, 1, &[0,48], 0x8000, &[0,48])?;
    let bridge = [0,1,0,0x80,0,0x14,0,2,0,0x0f,0,0];
    create_verified(&mut session, 45, entity, &bridge, 0xe000, &[0,1,0])?;
    assign_tcont(&mut session, 0x8000, port)?;
    // Stable ME instances survive ONU-ID changes. Port-ID and Alloc-ID follow
    // the new registration; upstream queue 8000 belongs to T-CONT 8000.
    let mut ctp = Vec::new();
    ctp.extend(port.to_be_bytes()); ctp.extend([0x80,0,3,0x80,0,0,0,0,0,0,0,0]);
    // K155 rejects GET of direction (2000) even for an absent instance.
    // Read supported port/T-CONT/upstream/downstream pointers separately.
    let mut ctp_readback = Vec::new();
    ctp_readback.extend(port.to_be_bytes()); ctp_readback.extend([0x80,0,0x80,0,0,0]);
    match session.get(268, entity, 0x8000) {
        Ok(_) => {
            let frame = session.get(268, entity, 0x5000)?;
            if frame.payload[3..7] != [0x80,0,0x80,0] {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "existing K155 GEM topology differs"));
            }
            session.set(268, entity, 0x8000, &port.to_be_bytes())?;
            verify(&mut session, 268, entity, 0xd200, &ctp_readback)?;
        }
        Err(e) if crate::pon_engine::omci_transport::is_unknown_instance(&e) => {
            session.create(268, entity, &ctp)?;
            verify(&mut session, 268, entity, 0xd200, &ctp_readback)?;
        }
        Err(e) => return Err(e),
    }
    let iw = [2,0,1,2,0,0,0,0,1];
    create_verified(&mut session, 266, entity, &iw, 0xf200, &iw)?;
    for (instance, number, tp, pointer) in [(0x201u16,1u8,1u8,0x101u16),(0x202,2,1,0x102),(0x205,5,11,0x601),(entity,6,5,entity)] {
        let mut attrs = vec![2,0,number,tp]; attrs.extend(pointer.to_be_bytes()); attrs.extend([0,0,0,1,0,0,0]);
        create_verified(&mut session, 47, instance, &attrs, 0xf000, &attrs[..6])?;
        if tp == 5 { continue; }
        let association = if tp == 11 { 10 } else { 2 };
        let mut evto = vec![association]; evto.extend(pointer.to_be_bytes());
        create_verified(&mut session, 171, instance, &evto, 0x8200, &evto)?;
        session.set(171, instance, 0x0800, &[1])?;
        verify(&mut session, 171, instance, 0x0800, &[1])?;
        let before = session.get_table(171, instance, 0x0400)?;
        if before.is_empty() {
            let mut expected = Vec::new();
            for (outer,inner) in [(0xf8000000u32,0xf8000000u32),(0xf8000000,0xe8000000),(0xe8000000,0xe8000000)] {
                let row: Vec<u8> = [outer,inner,0x000f0000,0x000f0000].into_iter().flat_map(u32::to_be_bytes).collect();
                session.set(171, instance, 0x0400, &row)?;
                expected.extend(row);
            }
            if session.get_table(171, instance, 0x0400)? != expected {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "K155 EVTO readback differs"));
            }
        } else { println!("K155_EXISTING_EVTO_PRESERVED instance={instance:04x}"); }
    }
    create_verified(&mut session, 84, entity, &[0;26], 0xc000, &[0;25])?;
    verify(&mut session, 256, 0, 0x8000, b"HWTC")?;
    println!("K155_SERVICE_READBACK_OK gem={port:04x}; data evidence still required");
    crate::controller::management::bootstrap(&mut session,id,0x601);
    Ok(())
}
