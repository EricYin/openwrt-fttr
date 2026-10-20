//! Generic standard G.988 OMCI transparent bridge profile.
use super::inventory::BridgeInventory;
use crate::controller::business::{assign_tcont, create_gem, create_verified, verify};
use crate::pon_engine::omci::{
    platform::leases::{lock_onu, ChannelLease, StockOmccLease},
    FmcsTransport, OmciSession,
};
use crate::{bosa, pon_engine::ploam::PloamFrame};
use std::{
    io,
    sync::{atomic::AtomicBool, Arc},
};

pub fn configure(id: u8, cancellation: Option<Arc<AtomicBool>>) -> io::Result<()> {
    if !(1..=15).contains(&id) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "ONU ID must be 1..15",
        ));
    }
    let _lock = lock_onu(id)?;
    let mut session = OmciSession::new(FmcsTransport::open(id)?);
    session.guard_registration(move || crate::controller::service::operation_confirmed(id));
    if let Some(token) = cancellation {
        session.cancel_when(token);
    }
    let _lease = StockOmccLease::open(id)?;
    let _channel = ChannelLease::open(id, 0)?;

    let port = 0x100u16 + id as u16;

    // Discover before writes: entity IDs are device-owned, not template constants.
    let mib = session.upload_mib()?;
    let BridgeInventory {
        tcont: tcont_inst,
        endpoints,
    } = BridgeInventory::from_mib(&mib, port)?;
    println!("GENERIC_BRIDGE_INVENTORY id={id} tcont={tcont_inst:04x} endpoints={endpoints:?}");

    // Send Assign_Alloc-ID downstream PLOAM so the ONU recognizes Alloc-ID = port
    for _ in 0..3 {
        let _ = bosa::send_ploam_msg(&PloamFrame::assign_alloc_id(id).to_bytes());
    }
    std::thread::sleep(std::time::Duration::from_millis(100));

    // Read ONU-G vendor ID
    let vendor_frame = session.get(256, 0, 0x8000)?;
    let vendor_bytes = vendor_frame.payload.get(3..7).unwrap_or(b"UNKN");
    println!(
        "GENERIC_BRIDGE_START id={id} vendor={:?}",
        String::from_utf8_lossy(vendor_bytes)
    );

    // Set ONU-G credential status=1 if supported; ignore if rejected in SN mode
    let _ = session.set(256, 0, 0x0010, &[1]);

    assign_tcont(&mut session, tcont_inst, port)?;

    // GAL Ethernet profile (ME 272)
    create_verified(&mut session, 272, 1, &[0, 48], 0x8000, &[0, 48])?;

    // MAC Bridge Service Profile (ME 45)
    let bridge_inst = if session.get(45, 1, 0x8000).is_ok() {
        1u16
    } else if session.get(45, 0x200, 0x8000).is_ok() {
        0x200u16
    } else {
        1u16
    };
    let bridge_data = [0, 1, 0, 0x80, 0, 0x14, 0, 2, 0, 0x0f, 0, 0];
    create_verified(
        &mut session,
        45,
        bridge_inst,
        &bridge_data,
        0xe000,
        &[0, 1, 0],
    )?;

    // GEM Port Network CTP (ME 268) & GEM IW TP (ME 266)
    create_gem(&mut session, port, port, tcont_inst, bridge_inst, 1)?;

    // Uplink ANI Bridge Port (ME 47, tp=5 GEM IW TP)
    let mut network = bridge_inst.to_be_bytes().to_vec();
    network.extend([6, 5]);
    network.extend(port.to_be_bytes());
    network.extend([0, 0, 0, 1, 0, 0, 0]);
    create_verified(&mut session, 47, port, &network, 0xf000, &network[..6])?;

    // Optional VLAN Tagging Filter Data (ME 84)
    let _ = create_verified(&mut session, 84, port, &[0; 26], 0xc000, &[0; 25]);

    let mut port_num = 1u8;
    for &(endpoint, is_veip) in &endpoints {
        if port_num == 6 {
            port_num += 1;
        }
        let bp_inst = 0x200u16 + port_num as u16;
        let mut attrs = bridge_inst.to_be_bytes().to_vec();
        attrs.extend([port_num, if is_veip { 11 } else { 1 }]);
        attrs.extend(endpoint.to_be_bytes());
        attrs.extend([0, 0, 0, 1, 0, 0, 0]);
        create_verified(&mut session, 47, bp_inst, &attrs, 0xf000, &attrs[..6])?;

        let mut evto = vec![if is_veip { 10 } else { 2 }];
        evto.extend(endpoint.to_be_bytes());
        create_verified(&mut session, 171, bp_inst, &evto, 0x8200, &evto)?;
        session.set(171, bp_inst, 0x0800, &[1])?;
        verify(&mut session, 171, bp_inst, 0x0800, &[1])?;
        apply_transparent_evto_table(&mut session, bp_inst)?;
        if is_veip {
            crate::controller::management::bootstrap(&mut session, id, endpoint);
        }
        port_num += 1;
    }

    verify(&mut session, 256, 0, 0x8000, vendor_bytes)?;
    println!(
        "GENERIC_BRIDGE_SERVICE_READBACK_OK gem={port:04x} endpoints={endpoints:?}; data evidence still required"
    );
    Ok(())
}

fn apply_transparent_evto_table(session: &mut OmciSession, instance: u16) -> io::Result<()> {
    let before = session.get_table(171, instance, 0x0400)?;
    if before.is_empty() {
        let mut expected = Vec::new();
        for (outer, inner) in [
            (0xf8000000u32, 0xf8000000u32),
            (0xf8000000, 0xe8000000),
            (0xe8000000, 0xe8000000),
        ] {
            let row: Vec<u8> = [outer, inner, 0x000f0000, 0x000f0000]
                .into_iter()
                .flat_map(u32::to_be_bytes)
                .collect();
            session.set(171, instance, 0x0400, &row)?;
            expected.extend(row);
        }
        if session.get_table(171, instance, 0x0400)? != expected {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "EVTO default table readback differs",
            ));
        }
        println!("GENERIC_EVTO_TRANSPARENT_OK instance={instance:04x}");
    } else {
        println!("GENERIC_EXISTING_EVTO_PRESERVED instance={instance:04x}");
    }
    Ok(())
}
