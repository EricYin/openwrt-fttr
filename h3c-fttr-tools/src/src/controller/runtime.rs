use std::fs;
use std::io::Write;
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread::{self, sleep};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::bosa;
use crate::fpga;
use crate::ipc::{self, SubDevice};
use crate::model::{FttrSystemStatus, IpcResponse};
use crate::mqtt::MqttClient;
use crate::pon_engine::ploam::OnuState;
use crate::pon_engine::PonEngine;
use super::activation::{RangingSession, clear_offline_hardware};
use crate::pon_engine::ploam::PloamFrame;
use std::collections::{BTreeMap, BTreeSet};

mod ipc_server;

fn setup_bridge() {
    println!("[*] Initializing downstream FTTR DSA switch ports (fttr1 ~ fttr16)...");
    let _ = Command::new("ip").args(["link", "set", "molt_ploam", "up"]).status();
    let _ = Command::new("ip").args(["link", "set", "molt_omci", "up"]).status();
    let _ = Command::new("ip").args(["link", "set", "fttr0", "up"]).status();
    for id in 1..=16 {
        let ifname = format!("fttr{id}");
        let _ = Command::new("ip").args(["link", "set", &ifname, "up"]).status();
        let _ = Command::new("ip").args(["link", "set", &ifname, "master", "br-lan"]).status();
    }
    println!("[+] Downstream FTTR switch ports fttr1 ~ fttr16 bridged to br-lan.");
}

fn start_ipc_server(
    listener: UnixListener,
    devices: Arc<Mutex<Vec<SubDevice>>>,
    pon_engine: Arc<Mutex<PonEngine>>,
    services: super::service::States,
) {
    ipc_server::start(listener, ipc_handler(devices, pon_engine, services));
}

fn ipc_handler(
    devices: Arc<Mutex<Vec<SubDevice>>>,
    pon_engine: Arc<Mutex<PonEngine>>,
    services: super::service::States,
) -> impl Fn(std::os::unix::net::UnixStream, &str) + Send + Sync + 'static {
    move |mut stream, line| {
                if let Some(request) = line.strip_prefix("HL202 ") {
                    #[cfg(feature = "device-hl202")]
                    super::hl202::start(stream, request.to_string(), pon_engine.clone(), services.clone());
                    #[cfg(not(feature = "device-hl202"))]
                    { let _ = request; let _ = stream.write_all(b"{\"schema_version\":1,\"error\":{\"code\":\"adapter_disabled\",\"message\":\"HL202 adapter not compiled\"}}"); }
                } else if let Some(request) = line.strip_prefix("MIB ") {
                    let request = request.to_string();
                        let result = serde_json::from_str::<super::inventory::MibCommand>(&request)
                            .map_err(std::io::Error::other)
                            .and_then(|command| super::inventory::control(command.onu_id, command.tables, &services));
                        let response = match result {
                            Ok(report) => serde_json::to_value(report).expect("serializable MIB report"),
                            Err(error) => serde_json::json!({"schema_version": 1, "error": {"code": format!("{:?}", error.kind()), "message": error.to_string()}}),
                        };
                        let _ = stream.write_all(response.to_string().as_bytes());
                } else if let Some(request) = line.strip_prefix("PROVISIONING ") {
                    let _ = stream.write_all(crate::provisioning::control(request).as_bytes());
                } else if let Some(_request) = line.strip_prefix("ACS ") {
                    #[cfg(feature = "acs")]
                    let response = crate::acs::control(_request);
                    #[cfg(not(feature = "acs"))]
                    let response = "{\"error\":\"ACS adapter not compiled (enable Cargo feature acs)\"}".to_string();
                    let _ = stream.write_all(response.as_bytes());
                } else if line == "STATUS" {
                    let provisioning = crate::provisioning::snapshot();
                    let devs = devices.lock().unwrap();
                    let engine = pon_engine.lock().unwrap();
                    let sys_status = super::status::snapshot(&engine, &services, (&devs, provisioning));

                    #[derive(serde::Serialize)]
                    struct Status<'a> {
                        #[serde(flatten)]
                        system: &'a FttrSystemStatus,
                        ipc: ipc_server::metrics::Snapshot,
                    }
                    let status = Status { system: &sys_status, ipc: ipc_server::metrics::snapshot() };
                    if let Ok(json_str) = serde_json::to_string_pretty(&status) {
                        let _ = stream.write_all(json_str.as_bytes());
                        let _ = stream.write_all(b"\n");
                    }
                } else if line == "DISCOVER" {
                    let engine = pon_engine.lock().unwrap();
                    let frames = engine.build_discovery_frames();
                    for frame in &frames {
                        let bytes = frame.to_bytes();
                        let _ = bosa::send_ploam_msg(&bytes);
                    }
                    let _ = stream.write_all(
                        format!("{{\"status\": \"discovery_dispatched\", \"frames\": {}}}\n", frames.len()).as_bytes(),
                    );
                } else if line.starts_with("REGISTER ") {
                    let _ = stream.write_all(IpcResponse::err("manual registration disabled; serial must arrive over upstream PLOAM").to_json_line().as_bytes());
                } else if line.starts_with("PROVISION ") {
                    let _ = stream.write_all(IpcResponse::err("provisioning is managed automatically by the per-ONU controller; use STATUS for readback").to_json_line().as_bytes());
                } else if line.starts_with("AUTH ") {
                    let _ = stream.write_all(IpcResponse::err("authentication is owned by the verified vendor profile; use fttrctl mib ONU_ID --summary to inspect capabilities").to_json_line().as_bytes());
                } else if line == "SYNC_WIFI" {
                    let _ = stream.write_all(IpcResponse::err("automatic Wi-Fi synchronization has no verified payload contract").to_json_line().as_bytes());
                } else if line.starts_with("EXEC ") {
                    let _ = stream.write_all(crate::devices::legacy_command(line).to_json_line().as_bytes());
                } else if line.starts_with("SUBDEV ") {
                    let _ = stream.write_all(crate::devices::legacy_command(line).to_json_line().as_bytes());
                }
    }
}

pub fn run() {
    println!("[*] Starting H3C FTTR Master Controller Daemon (Rust fttrd)...");
    #[cfg(feature = "acs")]
    crate::acs::start_from_env();

    let _ = fs::create_dir_all(ipc::SUBDEV_DIR);

    // 1. Data Structures & IPC Socket Server (Start immediately so fttrctl can query)
    let devices: Arc<Mutex<Vec<SubDevice>>> = Arc::new(Mutex::new(Vec::new()));
    let pon_engine = Arc::new(Mutex::new(PonEngine::new()));
    crate::provisioning::start(pon_engine.clone());
    let mqtt_client_arc: Arc<Mutex<Option<MqttClient>>> = Arc::new(Mutex::new(None));
    let hardware_ready = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let service_states: super::service::States = Arc::new(Mutex::new(BTreeMap::new()));

    if let Ok(listener) = ipc::create_socket_listener() {
        start_ipc_server(
            listener,
            devices.clone(),
            pon_engine.clone(),
            service_states.clone(),
        );
        println!("[+] IPC socket server listening on {}", ipc::SOCKET_PATH);
    }

    // 2. Background Hardware Initialization (Async resilient)
    {
        let hardware_ready = hardware_ready.clone();
        thread::spawn(move || {
            let fw_path = Path::new("/lib/firmware/FTTR_TOP.sbit");
            if fw_path.exists() {
                let _ = fpga::load_bitstream(fw_path);
            }

            // Wait for FPGA bitstream programming to complete if in progress
            println!("[*] Waiting for FPGA bitstream to be ready...");
            let mut wait_count = 0;
            loop {
                match fpga::get_fpga_status() {
                    fpga::FpgaStatus::Ready => {
                        println!("[+] FPGA bitstream is ready! Proceeding with BOSA & switch init.");
                        break;
                    }
                    fpga::FpgaStatus::Failed => {
                        eprintln!("[-] FPGA bitstream programming failed in kernel! Micro-OLT not available.");
                        return;
                    }
                    fpga::FpgaStatus::Programming | fpga::FpgaStatus::NotStarted => {
                        if wait_count % 5 == 0 {
                            println!("[*] FPGA bitstream is programming in kernel ({wait_count}s elapsed)...");
                        }
                    }
                    fpga::FpgaStatus::Unknown => {
                        if wait_count > 3 {
                            break;
                        }
                    }
                }
                thread::sleep(Duration::from_secs(1));
                wait_count += 1;
                if wait_count > 90 {
                    eprintln!("[-] Timeout waiting for FPGA bitstream readiness.");
                    return;
                }
            }

            if let Err(e) = bosa::init_optical_transceiver() {
                eprintln!("[-] Optical initialization failed: {e}; discovery paused");
                return;
            }
            if let Err(e) = bosa::configure_discovery_timing() {
                eprintln!("[-] Discovery timing initialization failed: {e}; discovery paused");
                return;
            }
            setup_bridge();
            // A daemon restart establishes a new registration generation too.
            // Revoke stale carrier/mappings and request fresh activation from
            // every verified slot before the only PLOAM receiver starts.
            for id in 1..=15 {
                if let Err(error) = clear_offline_hardware(id) {
                    eprintln!("[-] Initial ONU {id} hardware reset failed: {error}");
                    return;
                }
            }
            hardware_ready.store(true, std::sync::atomic::Ordering::Release);
            println!("[+] Micro-OLT BOSA and bridge ready. Waiting for dynamic sub-gateways...");
        });
    }

    // 3. Background PLOAM discovery broadcast & reception thread
    {
        let pon_engine_ploam = pon_engine.clone();
        thread::spawn(move || {
            println!("[+] Started background PLOAM discovery & reception loop.");
            let mut last_disc = Instant::now() - Duration::from_secs(5);
            let mut last_grant = Instant::now() - Duration::from_secs(5);
            let mut last_report = Instant::now();
            let mut last_idle_sample = Instant::now() - Duration::from_secs(30);
            let (mut tx_ok, mut tx_errors, mut rx_frames, mut rx_errors) = (0u64, 0u64, 0u64, 0u64);
            let (mut grants, mut grant_errors) = (0u64, 0u64);
            let (mut rx_valid, mut rx_bad_crc_or_length) = (0u64, 0u64);
            // Only one quiet-window activation at a time. B4 event bits
            // 16..31 map directly to ONU-ID slots 0..15; no ACK is guessed.
            let mut ranging: Option<RangingSession> = None;
            let mut online: BTreeMap<u8, RangingSession> = BTreeMap::new();
            let mut pending_cleanup = BTreeSet::new();
            let mut services = super::service::Services::new(service_states);
            let mut grant_cycle = 0usize;
            loop {
                if !hardware_ready.load(std::sync::atomic::Ordering::Acquire) {
                    thread::sleep(Duration::from_millis(250));
                    continue;
                }
                for id in pending_cleanup.clone() {
                    if let Ok(_lock) = crate::pon_engine::omci::platform::leases::lock_onu(id) {
                        match clear_offline_hardware(id) {
                            Ok(()) => {
                                pending_cleanup.remove(&id);
                                last_disc = Instant::now() - Duration::from_secs(1);
                                last_grant = Instant::now() - Duration::from_secs(1);
                                println!("[+] ONU {id} offline cleanup complete; fresh discovery enabled");
                            }
                            Err(e) => eprintln!("[-] ONU {id} offline cleanup failed; discovery held: {e}"),
                        }
                    }
                }
                // Use independent 200ms overhead and 100ms serial grant
                // timers; advertise before granting.
                if ranging.is_none() && last_disc.elapsed() >= Duration::from_millis(200) {
                    let engine = pon_engine_ploam.lock().unwrap();
                    let frames = engine.build_discovery_frames();
                    drop(engine);
                    for frame in &frames {
                        let bytes = frame.to_bytes();
                        if let Err(e) = bosa::send_ploam_msg(&bytes) {
                            tx_errors += 1;
                            eprintln!("[-] Discovery PLOAM send failed: {e}");
                        } else {
                            tx_ok += 1;
                        }
                    }
                    last_disc = Instant::now();
                }
                if last_grant.elapsed() >= Duration::from_millis(50) {
                    // One directed activation window, then round-robin live ONU
                    // windows. With no activation the first window discovers new SNs.
                    let target = super::activation::grant_target(ranging.as_ref().map(|session| session.id), online.keys().copied(), grant_cycle);
                    grant_cycle = grant_cycle.wrapping_add(1);
                    let grant = (u32::from(target) << 18) | if target == 0xfe { bosa::SERIAL_NUMBER_GRANT } else { (202 * 19440 + 124) / 125 };
                    match bosa::write_fpga_reg(bosa::FPGA_SERIAL_NUMBER_GRANT, grant) {
                        Ok(()) => grants += 1,
                        Err(e) => {
                            grant_errors += 1;
                            if grant_errors == 1 || last_report.elapsed() >= Duration::from_secs(30) {
                                eprintln!("[-] Discovery grant failed: {e}");
                            }
                        }
                    }
                    last_grant = Instant::now();
                    if let Some(RangingSession { id, ranged: Some(at), allocated, confirmed, .. }) = ranging.as_mut() {
                        if !*allocated && at.elapsed() >= Duration::from_millis(40) {
                            let frame = PloamFrame::assign_alloc_id(*id);
                            let mut sent = true;
                            for _ in 0..3 {
                                if bosa::send_ploam_msg(&frame.to_bytes()).is_ok() { tx_ok += 1; }
                                else { tx_errors += 1; sent = false; }
                            }
                            *allocated = sent;
                        }
                        if *allocated && !*confirmed {
                        // Request_Password is downstream ID 09 (upstream
                        // Password is 02). Keep strict validation on replies.
                         let frame = PloamFrame::new(*id, 9, [0; 10]);
                        if bosa::send_ploam_msg(&frame.to_bytes()).is_ok() { tx_ok += 1; }
                        else { tx_errors += 1; }
                        }
                    }
                }

                if let Some(RangingSession { id, armed, measured, ranged, .. }) = ranging.as_mut() {
                    if ranged.is_none() {
                        if let Ok(events) = bosa::read_fpga_reg(0xb4) {
                            let mask = 1u32 << (16 + *id);
                            if events & mask == 0 { *armed = true; }
                            if *armed && events & mask != 0 {
                                let address = 0x01010020 + 4 * u32::from(*id);
                                if let Ok(bits) = bosa::read_fpga_reg(address) {
                                    if bits != 0 {
                                        println!("[+] Ranging measurement: ONU={id}, address=0x{address:08X}, EqD bits={bits}");
                                        *measured = Some(bits);
                                    }
                                }
                            }
                        }
                    }
                }

                // Poll for incoming PLOAM frames
                if let Ok(rx_data) = bosa::recv_ploam_msg() {
                    if !rx_data.is_empty() {
                        rx_frames += 1;
                        // Upstream 04 is No message, not a ranging/error frame.
                        if rx_data.get(1) != Some(&4) || last_idle_sample.elapsed() >= Duration::from_secs(30) {
                            println!("[*] Upstream PLOAM raw: {:02X?}", rx_data);
                            if rx_data.get(1) == Some(&4) { last_idle_sample = Instant::now(); }
                        }
                    }
                    if PloamFrame::from_bytes(&rx_data).is_some() {
                        rx_valid += 1;
                        if let Some(session) = ranging.as_mut() { session.observe(&rx_data, Instant::now()); }
                        if let Some(session) = online.get_mut(&rx_data[0]) { session.observe(&rx_data, Instant::now()); }
                        let msg_id = rx_data[1];
                        if msg_id == 0x01 && rx_data[0] == 0xff && ranging.is_none() {
                            let mut vendor = [0u8; 4];
                            let mut sn = [0u8; 4];
                            vendor.copy_from_slice(&rx_data[2..6]);
                            sn.copy_from_slice(&rx_data[6..10]);
                            println!(
                                "[+] PLOAM: Captured upstream ONU serial: Vendor={:?}, SN={:02X?}",
                                String::from_utf8_lossy(&vendor),
                                sn
                            );

                            let mut engine = pon_engine_ploam.lock().unwrap();
                            if engine.active_onus.len() >= 15 && !engine.active_onus.iter()
                                .any(|onu| onu.vendor_id == vendor && onu.serial_number == sn) {
                                eprintln!("[-] No free verified ONU slot; preserving existing identities");
                                continue;
                            }
                            let onu = engine.register_onu(vendor, sn);
                            let onu_id = onu.onu_id;
                            if online.contains_key(&onu_id) || pending_cleanup.contains(&onu_id) { drop(engine); continue; }
                            if onu_id > 15 {
                                eprintln!("[-] ONU-ID {onu_id} has no verified FPGA ranging slot");
                                drop(engine);
                                continue;
                            }
                            let act_frames = engine.activate_onu(onu_id);
                            drop(engine);
                            let baseline = bosa::read_fpga_reg(0xb4);
                            let mut assigned = baseline.is_ok();
                            if assigned {
                                for _ in 0..3 {
                                    for f in &act_frames {
                                        if let Err(e) = bosa::send_ploam_msg(&f.to_bytes()) {
                                            assigned = false;
                                            tx_errors += 1;
                                            eprintln!("[-] Assign ONU-ID {onu_id} failed: {e}");
                                        } else { tx_ok += 1; }
                                    }
                                }
                            }
                            if assigned {
                                let mut serial = [0u8; 8];
                                serial[..4].copy_from_slice(&vendor);
                                serial[4..].copy_from_slice(&sn);
                                ranging = Some(RangingSession {
                                    id: onu_id, serial, started: Instant::now(),
                                    armed: baseline.unwrap() & (1u32 << (16 + onu_id)) == 0,
                                    matched: false, measured: None, ranged: None,
                                    allocated: false, confirmed: false,
                                    last_valid_rx: Instant::now(),
                                });
                                last_grant = Instant::now() - Duration::from_secs(1);
                                println!("[*] ONU {onu_id} assigned; requesting directed ranging");
                            }
                        } else if let Some(RangingSession { id, serial, matched, ranged, allocated, confirmed, .. }) = ranging.as_mut() {
                            if msg_id == 1 && rx_data[0] == *id && rx_data[2..10] == serial[..] {
                                *matched = true;
                                println!("[+] Ranging matched unicast serial for ONU {id}");
                            } else if !*confirmed && rx_data[0] == *id && ranged.is_some()
                                && (msg_id == 2 || (*allocated && msg_id == 9 && rx_data[2] == 0x0e
                                    && rx_data[3..12] == PloamFrame::assign_alloc_id(*id).to_bytes()[..9])) {
                                *confirmed = true;
                                println!("[+] ONU {id} CRC-valid post-ranging response msg=0x{msg_id:02X}: O5 protocol confirmed");
                                if let Some(onu) = pon_engine_ploam.lock().unwrap().active_onus.iter_mut().find(|o| o.onu_id == *id) {
                                    onu.state = OnuState::O5Operation;
                                }
                                // Data-plane carrier still requires separate hardware evidence.
                            }
                        }
                    } else if !rx_data.is_empty() {
                        rx_bad_crc_or_length += 1;
                    }
                } else {
                    rx_errors += 1;
                }

                if let Some(RangingSession { id, matched, measured, ranged, .. }) = ranging.as_mut() {
                    if *matched && ranged.is_none() {
                        if let Some(bits) = *measured {
                            let frame = PloamFrame::assign_delay(*id, bits);
                            let mut sent = true;
                            for _ in 0..3 {
                                if bosa::send_ploam_msg(&frame.to_bytes()).is_ok() { tx_ok += 1; }
                                else { tx_errors += 1; sent = false; }
                            }
                            if sent {
                                *ranged = Some(Instant::now());
                                if let Some(onu) = pon_engine_ploam.lock().unwrap().active_onus.iter_mut().find(|o| o.onu_id == *id) {
                                    onu.eq_delay = Some(bits);
                                }
                                println!("[+] Ranging_Time sent: ONU={id}, measured EqD={bits} bits, copies=3; awaiting valid O5 response");
                            }
                        }
                    }
                }

                if ranging.as_ref().is_some_and(|session| session.confirmed) {
                    let session = ranging.take().unwrap();
                    let id = session.id;
                    let vendor = pon_engine_ploam.lock().unwrap().active_onus.iter().find(|onu| onu.onu_id == id).unwrap().vendor_str();
                    online.insert(id, session);
                    services.start(id, vendor);
                    println!("[+] ONU {id} moved to independent online set; discovery remains enabled (online={})", online.len());
                }
                let expired: Vec<_> = online.iter().filter(|(_, session)| session.expired(Instant::now())).map(|(&id, _)| id).collect();
                for id in expired {
                    online.remove(&id);
                    services.stop(id);
                    pon_engine_ploam.lock().unwrap().mark_offline(id);
                    let _ = bosa::set_fttr_carrier(id as u32, false);
                    pending_cleanup.insert(id);
                    println!("[-] ONU {id} online evidence expired; independent cleanup scheduled");
                }
                if let Some(session) = ranging.as_ref().filter(|s| s.expired(Instant::now())) {
                    let id = session.id;
                    println!("[-] ONU {id} activation expired (confirmed={}); revoke O5/EqD and re-register", session.confirmed);
                    pon_engine_ploam.lock().unwrap().mark_offline(id);
                    if let Err(e) = bosa::set_fttr_carrier(id as u32, false) { eprintln!("[-] ONU {id} carrier disable: {e}"); }
                    ranging = None;
                    pending_cleanup.insert(id);
                }

                if last_report.elapsed() >= Duration::from_secs(30) {
                    println!("[*] PLOAM counters: SPI TX accepted={tx_ok}, TX errors={tx_errors}, serial grants={grants}, grant errors={grant_errors}, upstream frames={rx_frames}, valid frames={rx_valid}, bad CRC/length={rx_bad_crc_or_length}, RX errors={rx_errors}");
                    last_report = Instant::now();
                }

                // Poll the receiver every 10ms.
                thread::sleep(Duration::from_millis(10));
            }
        });
    }

    // 4. Connect to MQTT Broker
    println!("[*] Connecting to local Mosquitto MQTT broker on 127.0.0.1:1883...");
    let mut last_ping = Instant::now();

    loop {
        *mqtt_client_arc.lock().unwrap() = None;
        let mut client = match MqttClient::connect("127.0.0.1:1883", "h3c-fttrd-rust") {
            Ok(c) => {
                println!("[+] Connected to local MQTT broker!");
                if let Ok(cloned) = c.try_clone() {
                    *mqtt_client_arc.lock().unwrap() = Some(cloned);
                }
                c
            }
            Err(_) => {
                sleep(Duration::from_secs(2));
                continue;
            }
        };

        if let Err(e) = client.subscribe("/fttr/maindev/data/#") {
            eprintln!("[-] Failed to subscribe to FTTR telemetry topics: {e}");
            sleep(Duration::from_secs(2));
            continue;
        }

        let prefix = "/fttr/maindev/data/";

        loop {
            match client.read_packet() {
                Ok(Some((topic, payload))) => {
                    if let Some(dev_id) = topic.strip_prefix(prefix) {
                        let Some(mac) = crate::mqtt_identity::normalize_target(dev_id) else { continue; };
                        let dev_id = mac.as_str();
                        let payload_bytes = payload.len();
                        println!(
                            "[+] Telemetry packet received from sub-gateway: {} ({} bytes)",
                            dev_id,
                            payload.len()
                        );


                        // Telemetry is observation only: it must not enable
                        // carrier or publish unverified configuration payloads.

                        // Update device list
                        let now = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs();

                        let protected = crate::mqtt_identity::shared().lock()
                            .map(|state| state.snapshots(now).into_iter()
                                .filter(|source| source.reason == "ready")
                                .map(|source| source.mqtt_mac).collect()).unwrap_or_default();
                        let mut devs = devices.lock().unwrap();
                        crate::telemetry_sources::observe(&mut devs, SubDevice {
                            id: dev_id.to_string(), last_seen: now, payload_bytes,
                        }, &protected);

                        let _ = ipc::update_status_file(&devs);
                        drop(devs);

                        crate::provisioning::observe_mqtt(dev_id, now);
                        if !crate::devices::observe_mqtt_report(dev_id, payload, now) {
                            eprintln!("[-] Telemetry report not queued for {}", dev_id);
                        }
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    eprintln!("[-] MQTT read error: {e}, reconnecting...");
                    break;
                }
            }

            if last_ping.elapsed() >= Duration::from_secs(30) {
                if let Err(_) = client.ping() {
                    break;
                }
                last_ping = Instant::now();
            }

            sleep(Duration::from_millis(50));
        }

        sleep(Duration::from_secs(2));
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use crate::pon_engine::ploam::PloamFrame;

    #[test]
    fn only_valid_directed_rx_keeps_operation_alive_and_activation_is_bounded() {
        let start = Instant::now();
        let mut session = RangingSession { id: 1, serial: [0;8], started: start, armed: true,
            matched: true, measured: Some(248248), ranged: Some(start), allocated: true,
            confirmed: true, last_valid_rx: start };
        let later = start + Duration::from_secs(14);
        session.observe(&PloamFrame::new(2, 4, [0;10]).to_bytes(), later);
        let mut bad = PloamFrame::new(1, 4, [0;10]).to_bytes();
        bad[3] ^= 1;
        session.observe(&bad, later);
        session.observe(&bad[..12], later);
        assert!(session.expired(start + Duration::from_secs(15)));
        session.observe(&PloamFrame::new(1, 4, [0;10]).to_bytes(), later);
        assert!(!session.expired(start + Duration::from_secs(15)));
        assert!(session.expired(later + Duration::from_secs(15)));
        // A measured but never confirmed activation must not remain O4 forever.
        session.confirmed = false;
        assert!(session.expired(start + Duration::from_secs(10)));
    }
}
