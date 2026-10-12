//! Bounded OMCI GET diagnostic. The optional OMCC trial restores register settings on exit.
use crate::pon_engine::omci::{send_omci_frame, OmciMessage};
use crate::{bosa, pon_engine::omci_transport::StockOmccLease};
use std::{ffi::CString, io, time::{Duration, Instant}};
use std::sync::atomic::{AtomicBool, Ordering};
static STOP: AtomicBool = AtomicBool::new(false);
extern "C" fn stop(_: i32) { STOP.store(true, Ordering::Relaxed); }

#[repr(C)]
struct Address { family: u16, protocol: u16, index: i32, hatype: u16, kind: u8, length: u8, bytes: [u8; 8] }
#[repr(C)]
struct IoVec { base: *mut u8, length: usize }
#[repr(C)]
struct MsgHdr { name: *mut Address, name_length: u32, iov: *mut IoVec, iov_length: usize, control: *mut usize, control_length: usize, flags: i32 }
extern "C" {
    fn socket(domain: i32, kind: i32, protocol: i32) -> i32;
    fn bind(fd: i32, address: *const Address, length: u32) -> i32;
    fn send(fd: i32, buffer: *const u8, length: usize, flags: i32) -> isize;
    fn recvmsg(fd: i32, message: *mut MsgHdr, flags: i32) -> isize;
    fn setsockopt(fd: i32, level: i32, option: i32, value: *const i32, length: u32) -> i32;
    fn if_nametoindex(name: *const std::ffi::c_char) -> u32;
    fn close(fd: i32) -> i32;
    fn signal(number: i32, handler: usize) -> usize;
}
struct Capture(i32);
struct ChannelLease { id: u8, previous: u16 }
impl Drop for ChannelLease {
    fn drop(&mut self) {
        if let Err(e) = bosa::exchange_omci_channel(self.id, self.previous) {
            eprintln!("OMCI channel restore failed: {e}");
        }
    }
}
struct CarrierLease { id: u8, previous: bool }
impl Drop for CarrierLease {
    fn drop(&mut self) {
        if let Err(e) = bosa::set_fttr_carrier(self.id as u32, self.previous) {
            eprintln!("carrier restore failed: {e}");
        }
    }
}
impl Drop for Capture { fn drop(&mut self) { unsafe { close(self.0); } } }
impl Capture {
    fn transmit(&self, bytes: &[u8]) -> io::Result<()> {
        let count = unsafe { send(self.0, bytes.as_ptr(), bytes.len(), 0) };
        if count < 0 { return Err(io::Error::last_os_error()); }
        if count as usize != bytes.len() { return Err(io::Error::new(io::ErrorKind::WriteZero, "short raw frame send")); }
        Ok(())
    }
    fn open(name: &str) -> io::Result<Self> {
        let name = CString::new(name).unwrap();
        let index = unsafe { if_nametoindex(name.as_ptr()) };
        if index == 0 { return Err(io::Error::last_os_error()); }
        let fd = unsafe { socket(17, 3, (3u16).to_be() as i32) };
        if fd < 0 { return Err(io::Error::last_os_error()); }
        let capture = Self(fd);
        let enabled = 1;
        if unsafe { setsockopt(fd, 263, 8, &enabled, 4) } < 0 { return Err(io::Error::last_os_error()); }
        let address = Address { family: 17, protocol: (3u16).to_be(), index: index as i32, hatype: 0, kind: 0, length: 0, bytes: [0;8] };
        if unsafe { bind(fd, &address, std::mem::size_of::<Address>() as u32) } < 0 { return Err(io::Error::last_os_error()); }
        Ok(capture)
    }
    fn receive(&self) -> io::Result<Option<(Vec<u8>,u8,Option<u16>)>> {
        let mut buffer = [0;2048];
        let mut address: Address = unsafe { std::mem::zeroed() };
        let mut control = [0usize;16];
        let mut iov = IoVec { base: buffer.as_mut_ptr(), length: buffer.len() };
        let mut message = MsgHdr { name: &mut address, name_length: std::mem::size_of::<Address>() as u32,
            iov: &mut iov, iov_length: 1, control: control.as_mut_ptr(), control_length: std::mem::size_of_val(&control), flags: 0 };
        let count = unsafe { recvmsg(self.0, &mut message, 0x40) };
        if count < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::WouldBlock { return Ok(None); }
            return Err(error);
        }
        // Linux PACKET_AUXDATA: cmsghdr followed by status,len,snaplen,mac,net,vlan_tci,tpid.
        let raw = unsafe { std::slice::from_raw_parts(control.as_ptr() as *const u8, message.control_length.min(std::mem::size_of_val(&control))) };
        let header = std::mem::size_of::<usize>() + 8;
        let vlan = if raw.len() >= header + 20 && i32::from_ne_bytes(raw[8..12].try_into().unwrap()) == 263
            && i32::from_ne_bytes(raw[12..16].try_into().unwrap()) == 8 {
            let status = u32::from_ne_bytes(raw[header..header+4].try_into().unwrap());
            if status & (1 << 4) != 0 { Some(u16::from_ne_bytes(raw[header+16..header+18].try_into().unwrap())) } else { None }
        } else { None };
        Ok(Some((buffer[..count as usize].to_vec(), address.kind, vlan)))
    }
}
fn data_gem_rx(id: u8, bytes: &[u8], kind: u8, vlan: Option<u16>) -> bool {
    kind != 4 && vlan == Some(0x100 + id as u16) && bytes.len() >= 14
        && bytes[6] & 1 == 0 && bytes[6..12] != [0;6]
        && bytes[6..12] != [0x55;6] && bytes[6..12] != [0xaa;6]
}
pub fn diagnostic() -> io::Result<()> {
    let args: Vec<_> = std::env::args().collect();
    unsafe { signal(2, stop as *const () as usize); signal(15, stop as *const () as usize); }
    let vendor = crate::ipc::send_ipc_command("STATUS").ok()
        .and_then(|reply| serde_json::from_str::<crate::model::FttrSystemStatus>(&reply).ok())
        .and_then(|status| status.sub_gateways.into_iter().find(|onu| args.get(1).is_some_and(|id| id == &onu.onu_id.to_string())))
        .and_then(|onu| onu.vendor.as_bytes().try_into().ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "live ONU vendor identity unavailable"))?;
    run(args, None, vendor, None)
}
pub(super) fn watch(id: u8, cancellation: std::sync::Arc<AtomicBool>, vendor: [u8;4], requests: &std::sync::mpsc::Receiver<super::inventory::MibRequest>) -> io::Result<()> {
    run(vec!["integrated-health".into(), id.to_string(), "60".into(), "--stock-omcc".into(), "--1905".into(), "--watch".into()], Some(cancellation), vendor, Some(requests))
}
fn run(args: Vec<String>, cancellation: Option<std::sync::Arc<AtomicBool>>, vendor: [u8;4], requests: Option<&std::sync::mpsc::Receiver<super::inventory::MibRequest>>) -> io::Result<()> {
    if args.len() < 3 || args[3..].iter().any(|arg| arg != "--stock-omcc" && arg != "--stock-padding" && arg != "--mapped-omcc" && arg != "--arp" && arg != "--1905" && arg != "--activate-verified-link" && arg != "--watch") { return Err(io::Error::new(io::ErrorKind::InvalidInput, "usage: fttr-omci-probe ONU_ID SECONDS [--stock-omcc] [--stock-padding] [--mapped-omcc] [--arp] [--1905] [--activate-verified-link] [--watch]")); }
    let id: u8 = args[1].parse().map_err(|_| io::Error::new(io::ErrorKind::InvalidInput,"ONU_ID"))?;
    let seconds: u64 = args[2].parse().map_err(|_| io::Error::new(io::ErrorKind::InvalidInput,"SECONDS"))?;
    if id > 15 || seconds == 0 || seconds > 60 { return Err(io::Error::new(io::ErrorKind::InvalidInput,"ONU_ID 0..15, SECONDS 1..60")); }
    let watch = args.iter().any(|arg| arg == "--watch");
    if watch && (id == 0 || args.iter().any(|arg| arg == "--mapped-omcc" || arg == "--stock-padding")) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "watch requires ONU 1..15 and the driver OMCI transport"));
    }
    let _operation_lock = crate::pon_engine::omci_transport::lock_onu(id)?;
    if cancellation.as_ref().is_some_and(|token| token.load(Ordering::Acquire)) { return Ok(()); }
    if watch && !crate::pon_engine::omci_transport::operation_confirmed(id) {
        return Err(io::Error::new(io::ErrorKind::NotConnected, "ONU registration not active"));
    }
    // The watch owns readiness: begin closed and reopen only on fresh evidence.
    let _watch_carrier = if watch {
        bosa::set_fttr_carrier(id as u32, false)?;
        Some(CarrierLease { id, previous: false })
    } else { None };
    let omci = Capture::open("molt_omci")?;
    let physical = Capture::open("fttr0")?;
    let data = if args.iter().any(|arg| arg == "--arp" || arg == "--1905") { Some(Capture::open(&format!("fttr{id}"))?) } else { None };
    let _trial = if args[3..].iter().any(|arg| arg == "--stock-omcc") { Some(StockOmccLease::open(id)?) } else { None };
    // Restore the initial Alloc-ID before launching watch.
    // A previous diagnostic may have restored a stale host channel mapping.
    let _channel = if watch { Some(ChannelLease { id, previous: bosa::exchange_omci_channel(id, 0)? }) } else { None };
    // Avoid reusing the same transaction ID across separate diagnostic runs.
    let mut tci = ((std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .map_err(io::Error::other)?.as_millis() as u16) & 0x7fff).max(1);
    let mut payload = [0;32];
    payload[0] = 0x80; // ONU-G vendor ID, class 256, instance 0.
    let request = OmciMessage::new(tci, 0x49, 256, 0, payload);
    println!("GET ONU={id} TCI={tci:04x} class=256 instance=0 mask=8000 bytes={:02X?}", request.to_bytes());
    if args[3..].iter().any(|arg| arg == "--stock-padding" || arg == "--mapped-omcc") {
        // Pad the proprietary envelope to 68 bytes.
        // Keep the 48-byte OMCI message and its CRC intact.
        let mut wire = [0u8;68];
        wire[..6].fill(0xaa);
        wire[6..12].fill(0x55);
        wire[12..14].copy_from_slice(&0x8100u16.to_be_bytes());
        let vid = if args.iter().any(|arg| arg == "--mapped-omcc") { 0x100 + id as u16 } else { id as u16 };
        wire[14..16].copy_from_slice(&(0x2000u16 | vid).to_be_bytes());
        wire[16..64].copy_from_slice(&request.to_bytes());
        physical.transmit(&wire)?;
        println!("STOCK_PADDING physical envelope length=68");
    } else {
        send_omci_frame(id, &request)?;
    }
    println!("TX accepted by socket; awaiting response (not proof of delivery)");
    if data.is_some() {
        let mac = std::fs::read_to_string(format!("/sys/class/net/fttr{id}/address"))?;
        let mac: Vec<u8> = mac.trim().split(':').map(|v| u8::from_str_radix(v,16).map_err(io::Error::other)).collect::<io::Result<_>>()?;
        if args.iter().any(|arg| arg == "--arp") {
        let mut arp = [0u8;60];
        arp[..6].fill(0xff); arp[6..12].copy_from_slice(&mac);
        arp[12..22].copy_from_slice(&[8,6,0,1,8,0,6,4,0,1]);
        arp[22..28].copy_from_slice(&mac);
        arp[28..32].copy_from_slice(&[192,168,2,254]);
        arp[38..42].copy_from_slice(&[192,168,2,1]);
        // Diagnose the hardware path before marking the virtual port's carrier.
        let mut envelope = [0u8;68];
        envelope[..12].copy_from_slice(&arp[..12]);
        envelope[12..14].copy_from_slice(&0x8100u16.to_be_bytes());
        envelope[14..16].copy_from_slice(&(0x100u16 + id as u16).to_be_bytes());
        envelope[16..64].copy_from_slice(&arp[12..]);
        physical.transmit(&envelope)?;
        println!("ARP diagnostic target=192.168.2.1 (stock default), sender=192.168.2.254; no IP/interface configuration changed");
        // The backup's WAN PVC uses VLAN 41. Compare tagged and untagged
        // reachability without treating that backup as the active IP profile.
        let mut tagged = Vec::with_capacity(72);
        tagged.extend_from_slice(&envelope[..16]);
        tagged.extend_from_slice(&[0x81,0,0,41]);
        tagged.extend_from_slice(&envelope[16..]);
        physical.transmit(&tagged)?;
        println!("ARP diagnostic also sent with inner VLAN 41 (backup WAN profile)");
        }
        if args.iter().any(|arg| arg == "--1905") {
            // IEEE 1905.1 topology discovery: AL MAC, interface MAC, end TLV.
            // This is discovery only; it does not provision Wi-Fi or a controller.
            let mut wire = [0u8;68];
            wire[..6].copy_from_slice(&[1,0x80,0xc2,0,0,0x13]);
            wire[6..12].copy_from_slice(&mac);
            wire[12..14].copy_from_slice(&0x8100u16.to_be_bytes());
            wire[14..16].copy_from_slice(&(0x100u16 + id as u16).to_be_bytes());
            wire[16..18].copy_from_slice(&0x893au16.to_be_bytes());
            wire[18..26].copy_from_slice(&[0,0,0,0,(tci >> 8) as u8,tci as u8,0,0x80]);
            wire[26..29].copy_from_slice(&[1,0,6]);
            wire[29..35].copy_from_slice(&mac);
            wire[35..38].copy_from_slice(&[2,0,6]);
            wire[38..44].copy_from_slice(&mac);
            physical.transmit(&wire)?;
            println!("IEEE1905 topology discovery transmitted; awaiting actual child traffic");
        }
    }
    let start = Instant::now();
    let mut received = 0;
    let mut matched = 0;
    let mut gem_received = 0;
    let mut carrier_lease = None;
    let activate = args.iter().any(|arg| arg == "--activate-verified-link");
    let mut last_get = None;
    let mut last_data = None;
    let mut next_get = start + Duration::from_secs(5);
    let mut next_state = start;
    let management_version=crate::provisioning::management_version();
    let mut watchdog_start = start;
    while !STOP.load(Ordering::Relaxed) && (watch || start.elapsed() < Duration::from_secs(seconds)) {
        if watch && crate::provisioning::management_version()!=management_version {return Err(io::Error::other("OMCI management configuration changed"));}
        if cancellation.as_ref().is_some_and(|token| token.load(Ordering::Acquire)) { return Ok(()); }
        if let Some(request) = requests.and_then(|requests| requests.try_recv().ok()) {
            let result = if Instant::now() >= request.deadline {
                Err(io::Error::new(io::ErrorKind::TimedOut, "MIB request expired before monitor became ready"))
            } else {
                super::inventory::read_owned(id, request.summary, cancellation.clone(), request.deadline)
            };
            let complete = result.is_ok();
            let _ = request.reply.send(result);
            // The upload is serialized with health GETs under the same lock.
            // Resume with fresh evidence, without rerunning business provisioning.
            // A completed, validated upload is fresh OMCI liveness evidence.
            last_get = complete.then(Instant::now);
            watchdog_start = Instant::now();
            next_get = watchdog_start;
        }
        if watch && Instant::now() >= next_state {
            if !crate::pon_engine::omci_transport::operation_confirmed(id) {
                return Err(io::Error::new(io::ErrorKind::NotConnected, "ONU registration lost; release OMCC and revalidate"));
            }
            next_state = Instant::now() + Duration::from_secs(1);
        }
        if watch && Instant::now() >= next_get {
            tci = (tci.wrapping_add(1) & 0x7fff).max(1);
            send_omci_frame(id, &OmciMessage::new(tci, 0x49, 256, 0, payload))?;
            next_get = Instant::now() + Duration::from_secs(5);
        }
        let mut captures = vec![("molt_omci", &omci), ("fttr0", &physical)];
        if let Some(data) = &data { captures.push(("data", data)); }
        for (name, capture) in captures {
            // Bound each drain pass even on a busy physical interface.
            for _ in 0..32 {
                let Some((bytes, kind, vlan)) = capture.receive()? else { break };
                if !watch { println!("{}ms {name} packet_type={kind} vlan_tci={vlan:04X?} bytes={:02X?}", start.elapsed().as_millis(), bytes); }
                if name == "molt_omci" && kind != 4 {
                    received += 1;
                    if bytes.len() == 56 && bytes[..4] == 3u32.to_be_bytes() && bytes[4..8] == (id as u32).to_be_bytes() {
                        match OmciMessage::from_bytes(&bytes[8..]) {
                            Ok(frame) if frame.transaction_id == tci && frame.message_type == 0x29
                                && frame.class_id == 256 && frame.instance_id == 0 => {
                                if frame.payload[0] == 0 && frame.payload[1..3] == [0x80,0] && frame.payload[3..7] == vendor { matched += 1; last_get = Some(Instant::now()); }
                                if !watch { println!("MATCHED_GET_VALID result={} mask={:02X?} data={:02X?}",
                                    frame.payload[0], &frame.payload[1..3], &frame.payload[3..]);
                                }
                            }
                            Ok(_) => {}
                            Err(e) => println!("REJECTED_OMCI {e}"),
                        }
                    }
                }
                // Physical RX, not a socket echo: verified data GEM channel,
                // with a real Ethernet payload distinct from OMCI envelopes.
                if name == "fttr0" && data_gem_rx(id, &bytes, kind, vlan) {
                    gem_received += 1;
                    last_data = Some(Instant::now());
                    if !watch || gem_received == 1 { println!("DATA_GEM_RX_CONFIRMED source={:02X?} ethertype={:02X?}", &bytes[6..12], &bytes[12..14]); }
                }
                if (activate || watch) && last_get.is_some_and(|t: Instant| t.elapsed() < Duration::from_secs(15))
                    && last_data.is_some_and(|t: Instant| t.elapsed() < Duration::from_secs(180)) && carrier_lease.is_none() {
                    let previous = !watch && std::fs::read_to_string(format!("/sys/class/net/fttr{id}/carrier"))?.trim() == "1";
                    bosa::set_fttr_carrier(id as u32, true)?;
                    carrier_lease = Some(CarrierLease { id, previous });
                    println!("VERIFIED_LINK_ENABLED OMCI_GET=true GEM_RX=true watch={watch}; management/Wi-Fi requires separate verification");
                }
            }
        }
        if watch && carrier_lease.is_some() && (last_get.is_none_or(|t| t.elapsed() >= Duration::from_secs(15))
            || last_data.is_none_or(|t| t.elapsed() >= Duration::from_secs(180))) {
            drop(carrier_lease.take());
            println!("VERIFIED_LINK_DISABLED stale OMCI or data evidence");
        }
        if watch && last_get.unwrap_or(watchdog_start).elapsed() >= Duration::from_secs(30) {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "OMCI watch lost ONU; supervisor must revalidate service configuration"));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    println!("RESULT OMCI incoming={received} CRC-valid matched GET responses={matched} data_GEM_received={gem_received}; no OMCI ME configuration was written");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::data_gem_rx;
    #[test]
    fn captured_child_topology_query_is_data_but_echo_and_omci_are_not() {
        let mut frame = [0u8;60];
        frame[..14].copy_from_slice(&[2,0xfe,0x88,0x75,0x81,1,0x1c,0x94,0x68,0x5d,0xf9,0x98,0x89,0x3a]);
        assert!(data_gem_rx(1, &frame, 3, Some(0x0101)));
        assert!(!data_gem_rx(1, &frame, 4, Some(0x0101)));
        for vlan in [None, Some(0x0102), Some(0x2101)] {
            assert!(!data_gem_rx(1, &frame, 3, vlan));
        }
        assert!(!data_gem_rx(1, &frame[..13], 3, Some(0x0101)));
        frame[6..12].fill(0x55);
        assert!(!data_gem_rx(1, &frame, 3, Some(0x0101)));
        frame[6..12].fill(0);
        assert!(!data_gem_rx(1, &frame, 3, Some(0x0101)));
    }
}
