//! COMHEART TY-FS803G (SCTY, Broadcom BCM68252) specific OMCI transparent bridge profile.
//!
//! Direct OMCI provisioning bypasses the 309-entity MIB upload which takes minutes
//! on Broadcom CMS protocol stacks and times out.
//!
//! Broadcom BCM68252 RDPA Root TM Transition Rule:
//! In `rcl_tContObject` (0x0002f664), Broadcom checks whether the previous Alloc-ID was
//! 255 (0x00FF, unassigned initial value) via `rutGpon_isAllocIdInitValue`.
//! If old Alloc-ID != 255, Broadcom skips `rutGpon_createPriorityQueueByTcont`, leaving `wan0`
//! without a root TM on RDPA hardware.
//! Any CPU frame sent out via `wan0` (such as DHCP Discover from the sub-gateway) then fails in
//! `_rdpa_cpu_send_sysb: send pkt failed, rc -5` (BDMF_ERR_NOENT).
//!
//! To trigger Root TM and queue allocation in RDPA hardware:
//! 1. Set ME 262 Alloc-ID to 255 (0x00FF).
//! 2. Short delay (50ms).
//! 3. Set ME 262 Alloc-ID to target (0x100 + id).
//! Broadcom will observe `old_alloc == 255` and execute `rutGpon_createPriorityQueueByTcont`,
//! which initializes `tmctl_portTmInit` on RDPA.

use crate::controller::business::{create_verified_lenient, verify};
use crate::pon_engine::omci::{
    platform::leases::{lock_onu, ChannelLease, StockOmccLease},
    FmcsTransport, OmciSession,
};
use crate::{bosa, pon_engine::ploam::PloamFrame};
use std::{
    io,
    sync::{atomic::AtomicBool, Arc},
    time::Duration,
};

pub const TY_FS803G_TCONT: u16 = 0x8000;
pub const TY_FS803G_PRIO_QUEUE: u16 = 0x8000;
pub const TY_FS803G_TRAFFIC_SCHEDULER: u16 = 0x8000;
pub const TY_FS803G_VEIP_ENDPOINT: u16 = 0x0401;
pub const TY_FS803G_VEIP_BP: u16 = 0x0201;
pub const TY_FS803G_VEIP_EVTO: u16 = 0x0201;
pub const TY_FS803G_BRIDGE_PROFILE: u16 = 1;
pub const TY_FS803G_GAL_PROFILE: u16 = 1;

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
    let mut lease = StockOmccLease::open(id)?;
    let _channel = ChannelLease::open(id, 0)?;

    let port = 0x100u16 + id as u16;

    println!("SCTY_TY_FS803G_PROVISION_START id={id} port={port:04x}");

    // Send Assign_Alloc-ID downstream PLOAM (0x0A) so the ONU hardware recognizes business Alloc-ID = port
    let mut p = [0u8; 10];
    p[0] = (port >> 4) as u8;
    p[1] = (port << 4) as u8;
    p[2] = 1;
    for _ in 0..3 {
        let _ = bosa::send_ploam_msg(&PloamFrame::new(id, 0x0a, p).to_bytes());
    }
    std::thread::sleep(Duration::from_millis(50));

    // Verify vendor
    let vendor_frame = session.get(256, 0, 0x8000)?;
    let vendor_bytes = vendor_frame.payload.get(3..7).unwrap_or(b"UNKN").to_vec();
    println!(
        "SCTY_TY_FS803G_VENDOR id={id} vendor={:?}",
        String::from_utf8_lossy(&vendor_bytes)
    );

    // Set ONU-G credential status=1 if supported
    let _ = session.set(256, 0, 0x0010, &[1]);

    // Retire stale ANI/GEM entries from other slots
    retire_stale_ani(&mut session, port);

    // Clean up any stale current_port entities before T-CONT transition
    let _ = session.delete(47, port);
    let _ = session.delete(266, port);
    let _ = session.delete(268, port);
    let _ = session.delete(84, port);

    // Broadcom BCM68252 Root TM transition:
    // rcl_tContObject requires old Alloc-ID to be 255 (0x00FF) to call
    // rutGpon_createPriorityQueueByTcont and initialize wan0 hardware Root TM.
    let current_alloc = session
        .get(262, TY_FS803G_TCONT, 0x8000)
        .ok()
        .and_then(|frame| {
            frame
                .payload
                .get(3..5)
                .map(|b| u16::from_be_bytes([b[0], b[1]]))
        });

    println!("SCTY_TY_FS803G_TCONT_CURRENT instance={TY_FS803G_TCONT:04x} alloc={current_alloc:?}");

    if current_alloc != Some(255) {
        println!("SCTY_TY_FS803G_TCONT_RESET_255 instance={TY_FS803G_TCONT:04x}");
        let _ = session.set(262, TY_FS803G_TCONT, 0x8000, &255u16.to_be_bytes());
        std::thread::sleep(Duration::from_millis(50));
    }

    println!("SCTY_TY_FS803G_TCONT_SET_TARGET instance={TY_FS803G_TCONT:04x} alloc={port:04x}");
    session.set(262, TY_FS803G_TCONT, 0x8000, &port.to_be_bytes())?;
    std::thread::sleep(Duration::from_millis(50));
    verify(
        &mut session,
        262,
        TY_FS803G_TCONT,
        0x8000,
        &port.to_be_bytes(),
    )?;

    // Priority Queue ME 277 0x8000: bind TrafficSchedulerGPointer (mask 0x0400) to 0x8000
    if let Err(error) = session.set(
        277,
        TY_FS803G_PRIO_QUEUE,
        0x0400,
        &TY_FS803G_TRAFFIC_SCHEDULER.to_be_bytes(),
    ) {
        println!("SCTY_TY_FS803G_PRIO_QUEUE_SCHED_SKIPPED: {error}");
    }

    // GAL Ethernet profile (ME 272)
    if let Err(error) = create_verified_lenient(
        &mut session,
        272,
        TY_FS803G_GAL_PROFILE,
        &[0, 48],
        0x8000,
        &[0, 48],
    ) {
        println!("SCTY_TY_FS803G_GAL_REUSED instance={TY_FS803G_GAL_PROFILE:04x}: {error}");
    }

    // MAC Bridge Service Profile (ME 45)
    let bridge_data = [0, 1, 0, 0x80, 0, 0x14, 0, 2, 0, 0x0f, 0, 0];
    if let Err(error) = create_verified_lenient(
        &mut session,
        45,
        TY_FS803G_BRIDGE_PROFILE,
        &bridge_data,
        0xe000,
        &[0, 1, 0],
    ) {
        println!(
            "SCTY_TY_FS803G_BRIDGE_PROFILE_REUSED instance={TY_FS803G_BRIDGE_PROFILE:04x}: {error}"
        );
    }

    // GEM Port Network CTP (ME 268) & GEM IW TP (ME 266)
    create_gem_port(
        &mut session,
        port,
        port,
        TY_FS803G_TCONT,
        TY_FS803G_PRIO_QUEUE,
        TY_FS803G_BRIDGE_PROFILE,
        TY_FS803G_GAL_PROFILE,
    )?;

    // Uplink ANI Bridge Port (ME 47, tp=5 GEM IW TP, port_num=6)
    let ani_port_num = 6u8;
    let mut network = TY_FS803G_BRIDGE_PROFILE.to_be_bytes().to_vec();
    network.push(ani_port_num);
    network.push(5);
    network.extend(port.to_be_bytes());
    network.extend([0, 0, 0, 1, 0, 0, 0]);
    create_verified_lenient(&mut session, 47, port, &network, 0xf000, &network[..6])?;

    // Optional VLAN Tagging Filter Data (ME 84)
    let _ = create_verified_lenient(&mut session, 84, port, &[0; 26], 0xc000, &[0; 25]);

    // Check or create VEIP Bridge Port (ME 47 instance 0x0201)
    let bp_inst = TY_FS803G_VEIP_BP;
    if session.get(47, bp_inst, 0x8000).is_err() {
        let mut attrs = TY_FS803G_BRIDGE_PROFILE.to_be_bytes().to_vec();
        attrs.push(1); // port_num 1
        attrs.push(11); // tp_type 11 (VEIP)
        attrs.extend(TY_FS803G_VEIP_ENDPOINT.to_be_bytes());
        attrs.extend([0, 0, 0, 1, 0, 0, 0]);
        create_verified_lenient(&mut session, 47, bp_inst, &attrs, 0xf000, &attrs[..6])?;
        println!("SCTY_TY_FS803G_VEIP_BP_CREATED instance={bp_inst:04x}");
    } else {
        println!("SCTY_TY_FS803G_VEIP_BP_REUSED instance={bp_inst:04x}");
    }

    // Check or create EVTO (ME 171 instance 0x0201)
    let evto_inst = TY_FS803G_VEIP_EVTO;
    if session.get(171, evto_inst, 0x8000).is_err() {
        let mut evto = vec![10]; // association type 10 (VEIP)
        evto.extend(TY_FS803G_VEIP_ENDPOINT.to_be_bytes());
        create_verified_lenient(&mut session, 171, evto_inst, &evto, 0x8200, &evto)?;
        println!("SCTY_TY_FS803G_EVTO_CREATED instance={evto_inst:04x}");
    } else {
        println!("SCTY_TY_FS803G_EVTO_REUSED instance={evto_inst:04x}");
    }

    // Downstream mode transparent
    match session.set(171, evto_inst, 0x0800, &[1]) {
        Ok(()) => {
            let _ = verify(&mut session, 171, evto_inst, 0x0800, &[1]);
        }
        Err(error) => {
            println!(
                "SCTY_TY_FS803G_EVTO_DOWNSTREAM_MODE_SKIPPED instance={evto_inst:04x}: {error}"
            );
        }
    }

    if let Err(error) = session.with_timeout(Duration::from_secs(5), |session| {
        apply_transparent_evto_table(session, evto_inst)
    }) {
        if matches!(
            error.kind(),
            io::ErrorKind::NotConnected | io::ErrorKind::Interrupted
        ) {
            return Err(error);
        }
        println!("SCTY_TY_FS803G_EVTO_TABLE_SKIPPED instance={evto_inst:04x}: {error}");
    }

    // Ensure VEIP ME 329 administrative state is unlocked (0) to trigger Broadcom rutRgFull_configVeip
    if let Err(error) = session.set(329, TY_FS803G_VEIP_ENDPOINT, 0x8000, &[0]) {
        println!("SCTY_TY_FS803G_VEIP_UNLOCK_SKIPPED: {error}");
    }

    // Unlock LAN Ethernet UNIs (ME 11 instances 0xff03, 0xff04)
    for uni in [0xff03u16, 0xff04] {
        let _ = session.set(11, uni, 0x2000, &[0]);
    }

    // Bootstrap TR-069 ME 340 management server
    println!("SCTY_TY_FS803G_TR069_BOOTSTRAP id={id} instance={TY_FS803G_VEIP_ENDPOINT:04x}");
    crate::controller::management::bootstrap(&mut session, id, TY_FS803G_VEIP_ENDPOINT);

    verify(&mut session, 256, 0, 0x8000, &vendor_bytes)?;
    println!(
        "SCTY_TY_FS803G_BRIDGE_SERVICE_READBACK_OK gem={port:04x} veip={TY_FS803G_VEIP_ENDPOINT:04x}"
    );

    // Keep Alloc-ID and FPGA channel permanently armed for business
    lease.disarm();
    println!("SCTY_TY_FS803G_LEASE_PERSISTED id={id} port={port:04x}");
    Ok(())
}

fn retire_stale_ani(session: &mut OmciSession, current_port: u16) {
    for old in 0x100u16..=0x10f {
        if old == current_port {
            continue;
        }
        // Only attempt deletion if GEM CTP (ME 268) or ANI BP (ME 47) actually exists
        if session.get(268, old, 0x8000).is_ok() || session.get(47, old, 0x8000).is_ok() {
            println!("SCTY_TY_FS803G_RETIRE_STALE_ANI instance={old:04x}");
            let _ = session.delete(47, old);
            let _ = session.delete(266, old);
            let _ = session.delete(268, old);
            let _ = session.delete(84, old);
        }
    }
}

fn create_gem_port(
    session: &mut OmciSession,
    instance: u16,
    port: u16,
    tcont: u16,
    prio_queue: u16,
    bridge: u16,
    gal: u16,
) -> io::Result<()> {
    let mut ctp = Vec::with_capacity(16);
    ctp.extend(port.to_be_bytes());
    ctp.extend(tcont.to_be_bytes());
    ctp.push(3);
    ctp.extend(prio_queue.to_be_bytes());
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
            println!(
                "GEM_UPSTREAM_TM_REPAIRED instance={instance:04x} prio_queue={prio_queue:04x}"
            );
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
    println!(
        "SCTY_TY_FS803G_EVTO_TABLE instance={instance:04x} len={}",
        before.len()
    );
    // Ensure all 3 transparent rules (untagged, priority tagged, single tagged) are present
    for (outer, inner) in [
        (0xf8000000u32, 0xf8000000u32),
        (0xf8000000, 0xe8000000),
        (0xe8000000, 0xe8000000),
    ] {
        let row: Vec<u8> = [outer, inner, 0x000f0000, 0x000f0000]
            .into_iter()
            .flat_map(u32::to_be_bytes)
            .collect();
        let _ = session.set(171, instance, 0x0400, &row);
    }
    let after = session.get_table(171, instance, 0x0400)?;
    println!(
        "SCTY_TY_FS803G_EVTO_TRANSPARENT_READY instance={instance:04x} len={}",
        after.len()
    );
    Ok(())
}
