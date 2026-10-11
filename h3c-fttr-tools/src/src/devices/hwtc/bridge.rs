//! Shared bridge topology, initially verified on K155-11. Not a WAN creation contract.
use crate::controller::business::{create_verified, verify, assign_tcont};
use crate::pon_engine::omci_transport::OmciSession;
use std::io;

pub(super) fn configure(session: &mut OmciSession, port: u16) -> io::Result<()> {
    let entity = 0x200u16;
    create_verified(session, 272, 1, &[0,48], 0x8000, &[0,48])?;
    let bridge = [0,1,0,0x80,0,0x14,0,2,0,0x0f,0,0];
    create_verified(session, 45, entity, &bridge, 0xe000, &[0,1,0])?;
    assign_tcont(session, 0x8000, port)?;
    // Stable instances survive ONU-ID changes; queue 8000 belongs to T-CONT 8000.
    let mut ctp = Vec::new();
    ctp.extend(port.to_be_bytes()); ctp.extend([0x80,0,3,0x80,0,0,0,0,0,0,0,0]);
    // K155 rejects GET of direction (2000). Read supported pointers separately.
    let mut ctp_readback = Vec::new();
    ctp_readback.extend(port.to_be_bytes()); ctp_readback.extend([0x80,0,0x80,0,0,0]);
    match session.get(268, entity, 0x8000) {
        Ok(_) => {
            let frame = session.get(268, entity, 0x5000)?;
            if frame.payload[3..7] != [0x80,0,0x80,0] {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "existing HWTC GEM topology differs"));
            }
            session.set(268, entity, 0x8000, &port.to_be_bytes())?;
            verify(session, 268, entity, 0xd200, &ctp_readback)?;
        }
        Err(e) if crate::pon_engine::omci_transport::is_unknown_instance(&e) => {
            session.create(268, entity, &ctp)?;
            verify(session, 268, entity, 0xd200, &ctp_readback)?;
        }
        Err(e) => return Err(e),
    }
    let iw = [2,0,1,2,0,0,0,0,1];
    create_verified(session, 266, entity, &iw, 0xf200, &iw)?;
    for (instance, number, tp, pointer) in [(0x201u16,1u8,1u8,0x101u16),(0x202,2,1,0x102),(0x205,5,11,0x601),(entity,6,5,entity)] {
        let mut attrs = vec![2,0,number,tp]; attrs.extend(pointer.to_be_bytes()); attrs.extend([0,0,0,1,0,0,0]);
        create_verified(session, 47, instance, &attrs, 0xf000, &attrs[..6])?;
        if tp == 5 { continue; }
        let association = if tp == 11 { 10 } else { 2 };
        let mut evto = vec![association]; evto.extend(pointer.to_be_bytes());
        create_verified(session, 171, instance, &evto, 0x8200, &evto)?;
        session.set(171, instance, 0x0800, &[1])?;
        verify(session, 171, instance, 0x0800, &[1])?;
        let before = session.get_table(171, instance, 0x0400)?;
        if before.is_empty() {
            let mut expected = Vec::new();
            for (outer,inner) in [(0xf8000000u32,0xf8000000u32),(0xf8000000,0xe8000000),(0xe8000000,0xe8000000)] {
                let row: Vec<u8> = [outer,inner,0x000f0000,0x000f0000].into_iter().flat_map(u32::to_be_bytes).collect();
                session.set(171, instance, 0x0400, &row)?;
                expected.extend(row);
            }
            if session.get_table(171, instance, 0x0400)? != expected {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "HWTC EVTO readback differs"));
            }
        } else { println!("HWTC_EXISTING_EVTO_PRESERVED instance={instance:04x}"); }
    }
    create_verified(session, 84, entity, &[0;26], 0xc000, &[0;25])?;
    Ok(())
}
