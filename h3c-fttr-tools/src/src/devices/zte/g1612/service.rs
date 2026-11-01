//! ZXHN G1612 specific OMCI transparent bridge profile.
//!
//! Direct OMCI provisioning bypasses the costly 351-entity MIB upload
//! on ZTE protocol stacks which produce 27ms duplicate frames and socket timeouts.
//! Known hardware profile:
//! - T-CONT instance: 0x8002 (32770)
//! - VEIP instance: 0x0301 (769)
//! - Bridge Port instance: 0x0201 (port_num 1)
//! - EVTO instance: 0x0201
//! - ME 340 (TR-069) instance: 0x0301
use crate::controller::business::{assign_tcont, create_verified_lenient, verify};
use crate::pon_engine::omci::{
    platform::leases::{lock_onu, ChannelLease, StockOmccLease},
    FmcsTransport, OmciSession,
};
use crate::{bosa, pon_engine::ploam::PloamFrame};
use std::{
    io,
    sync::{atomic::AtomicBool, Arc},
};

pub const G1612_TCONT: u16 = 0x8002;
pub const G1612_VEIP_ENDPOINT: u16 = 0x0301;
pub const G1612_VEIP_BP: u16 = 0x0201;
pub const G1612_VEIP_EVTO: u16 = 0x0201;
pub const G1612_BRIDGE_PROFILE: u16 = 1;
pub const G1612_GAL_PROFILE: u16 = 1;

pub fn configure(id: u8, cancellation: Option<Arc<AtomicBool>>) -> io::Result<()> {
    if id >= crate::pon_engine::ONU_SLOTS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "ONU ID must be 0..15",
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

    println!("ZTE_G1612_PROVISION_START id={id} port={port:04x}");

    // Send Assign_Alloc-ID downstream PLOAM
    for _ in 0..3 {
        let _ = bosa::send_ploam_msg(&PloamFrame::assign_alloc_id(id).to_bytes());
    }
    std::thread::sleep(std::time::Duration::from_millis(50));

    // Verify vendor
    let vendor_frame = session.get(256, 0, 0x8000)?;
    let vendor_bytes = vendor_frame.payload.get(3..7).unwrap_or(b"UNKN").to_vec();
    println!(
        "ZTE_G1612_VENDOR id={id} vendor={:?}",
        String::from_utf8_lossy(&vendor_bytes)
    );

    // Set ONU-G credential status=1 if supported
    let _ = session.set(256, 0, 0x0010, &[1]);

    // Retire stale ANI/GEM entries from other slots
    retire_stale_ani(&mut session, port);

    // T-CONT allocation to 0x8002
    if let Err(error) = assign_tcont(&mut session, G1612_TCONT, port) {
        println!(
            "ZTE_G1612_TCONT_ALLOC_SKIPPED instance={G1612_TCONT:04x} alloc={port:04x}: {error}"
        );
    }

    // GAL Ethernet profile (ME 272)
    if let Err(error) = create_verified_lenient(
        &mut session,
        272,
        G1612_GAL_PROFILE,
        &[0, 48],
        0x8000,
        &[0, 48],
    ) {
        println!("ZTE_G1612_GAL_REUSED instance={G1612_GAL_PROFILE:04x}: {error}");
    }

    // MAC Bridge Service Profile (ME 45)
    let bridge_data = [0, 1, 0, 0x80, 0, 0x14, 0, 2, 0, 0x0f, 0, 0];
    if let Err(error) = create_verified_lenient(
        &mut session,
        45,
        G1612_BRIDGE_PROFILE,
        &bridge_data,
        0xe000,
        &[0, 1, 0],
    ) {
        println!("ZTE_G1612_BRIDGE_PROFILE_REUSED instance={G1612_BRIDGE_PROFILE:04x}: {error}");
    }

    // GEM Port Network CTP (ME 268) & GEM IW TP (ME 266)
    create_gem_port(
        &mut session,
        port,
        port,
        G1612_TCONT,
        G1612_BRIDGE_PROFILE,
        G1612_GAL_PROFILE,
    )?;

    // Uplink ANI Bridge Port (ME 47, tp=5 GEM IW TP, port_num=6)
    let ani_port_num = 6u8;
    let mut network = G1612_BRIDGE_PROFILE.to_be_bytes().to_vec();
    network.push(ani_port_num);
    network.push(5);
    network.extend(port.to_be_bytes());
    network.extend([0, 0, 0, 1, 0, 0, 0]);
    create_verified_lenient(&mut session, 47, port, &network, 0xf000, &network[..6])?;

    // Optional VLAN Tagging Filter Data (ME 84)
    let _ = create_verified_lenient(&mut session, 84, port, &[0; 26], 0xc000, &[0; 25]);

    // Check or create VEIP Bridge Port (ME 47 instance 0x0201)
    let bp_inst = G1612_VEIP_BP;
    if session.get(47, bp_inst, 0x8000).is_err() {
        let mut attrs = G1612_BRIDGE_PROFILE.to_be_bytes().to_vec();
        attrs.push(1); // port_num 1
        attrs.push(11); // tp_type 11 (VEIP)
        attrs.extend(G1612_VEIP_ENDPOINT.to_be_bytes());
        attrs.extend([0, 0, 0, 1, 0, 0, 0]);
        create_verified_lenient(&mut session, 47, bp_inst, &attrs, 0xf000, &attrs[..6])?;
        println!("ZTE_G1612_VEIP_BP_CREATED instance={bp_inst:04x}");
    } else {
        println!("ZTE_G1612_VEIP_BP_REUSED instance={bp_inst:04x}");
    }

    // Check or create EVTO (ME 171 instance 0x0201)
    let evto_inst = G1612_VEIP_EVTO;
    if session.get(171, evto_inst, 0x8000).is_err() {
        let mut evto = vec![10]; // association type 10 (VEIP)
        evto.extend(G1612_VEIP_ENDPOINT.to_be_bytes());
        create_verified_lenient(&mut session, 171, evto_inst, &evto, 0x8200, &evto)?;
        println!("ZTE_G1612_EVTO_CREATED instance={evto_inst:04x}");
    } else {
        println!("ZTE_G1612_EVTO_REUSED instance={evto_inst:04x}");
    }

    // Downstream mode transparent
    match session.set(171, evto_inst, 0x0800, &[1]) {
        Ok(()) => {
            let _ = verify(&mut session, 171, evto_inst, 0x0800, &[1]);
        }
        Err(error) => {
            println!("ZTE_G1612_EVTO_DOWNSTREAM_MODE_SKIPPED instance={evto_inst:04x}: {error}");
        }
    }

    if let Err(error) = apply_transparent_evto_table(&mut session, evto_inst) {
        println!("ZTE_G1612_EVTO_TABLE_SKIPPED instance={evto_inst:04x}: {error}");
    }

    // Bootstrap TR-069 ME 340 management server
    println!("ZTE_G1612_TR069_BOOTSTRAP id={id} instance={G1612_VEIP_ENDPOINT:04x}");
    crate::controller::management::bootstrap(&mut session, id, G1612_VEIP_ENDPOINT);

    verify(&mut session, 256, 0, 0x8000, &vendor_bytes)?;
    println!("ZTE_G1612_BRIDGE_SERVICE_READBACK_OK gem={port:04x} veip={G1612_VEIP_ENDPOINT:04x}");
    Ok(())
}

fn retire_stale_ani(session: &mut OmciSession, current_port: u16) {
    for old in 0x100u16..=0x10f {
        if old == current_port {
            continue;
        }
        let _ = session.delete(47, old);
        let _ = session.delete(266, old);
        let _ = session.delete(268, old);
        let _ = session.delete(84, old);
    }
}

fn create_gem_port(
    session: &mut OmciSession,
    instance: u16,
    port: u16,
    tcont: u16,
    bridge: u16,
    gal: u16,
) -> io::Result<()> {
    let mut ctp = Vec::with_capacity(16);
    ctp.extend(port.to_be_bytes());
    ctp.extend(tcont.to_be_bytes());
    ctp.push(3);
    ctp.extend(tcont.to_be_bytes());
    ctp.extend([0, 0]);
    ctp.push(0);
    ctp.extend([0, 0]);
    ctp.push(0);
    ctp.extend([0, 0]);
    ctp.push(0);
    create_verified_lenient(session, 268, instance, &ctp, 0xe000, &ctp[..5])?;
    match session.get(268, instance, 0x1000) {
        Ok(frame) if frame.payload.get(3..5) != Some(&ctp[5..7]) => {
            session.set(268, instance, 0x1000, &ctp[5..7])?;
            println!("GEM_UPSTREAM_TM_REPAIRED instance={instance:04x} tcont={tcont:04x}");
        }
        Ok(_) => {}
        Err(_) => {}
    }
    let mut iw = Vec::new();
    iw.extend(instance.to_be_bytes());
    iw.push(1);
    iw.extend(bridge.to_be_bytes());
    iw.extend([0, 0]);
    iw.extend(gal.to_be_bytes());
    create_verified_lenient(session, 266, instance, &iw, 0xf200, &iw)
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
        println!("ZTE_G1612_EVTO_TRANSPARENT_OK instance={instance:04x}");
    } else {
        println!("ZTE_G1612_EXISTING_EVTO_PRESERVED instance={instance:04x}");
    }
    Ok(())
}
