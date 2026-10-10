//! Serialized legacy-GPON OMCI transactions over the FMCS control netdev.
use super::{omci::{send_omci_frame, OmciMessage}, ploam::PloamFrame};
use crate::bosa;
use std::{io, time::{Duration, Instant, SystemTime, UNIX_EPOCH}};

/// Shared bitmap changes are serialized across daemon workers and diagnostic processes.
/// A lease restores only its own bit, never a stale snapshot of another ONU.
pub fn update_onu_enable(id: u8, enabled: bool) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    extern "C" { fn flock(fd: i32, operation: i32) -> i32; }
    if id > 15 { return Err(io::Error::new(io::ErrorKind::InvalidInput, "ONU slot")); }
    let lock = std::fs::OpenOptions::new().create(true).truncate(false).read(true).write(true)
        .open("/tmp/fttr-hardware.lock")?;
    if unsafe { flock(lock.as_raw_fd(), 2) } != 0 { return Err(io::Error::last_os_error()); }
    let value = bosa::read_fpga_reg(0x01000008)?;
    bosa::write_fpga_reg(0x01000008, enable_bit(value, id, enabled))
}
fn enable_bit(value: u32, id: u8, enabled: bool) -> u32 {
    if enabled { value | (1u32 << id) } else { value & !(1u32 << id) }
}

/// Serialize service teardown and daemon reactivation. Never unlink lock files.
pub fn lock_onu(id: u8) -> io::Result<std::fs::File> {
    use std::os::fd::AsRawFd;
    extern "C" { fn flock(fd: i32, operation: i32) -> i32; }
    let file = std::fs::OpenOptions::new().create(true).truncate(false).read(true).write(true)
        .open(format!("/tmp/fttr-onu-{id}.lock"))?;
    if unsafe { flock(file.as_raw_fd(), 2 | 4) } != 0 { return Err(io::Error::last_os_error()); }
    Ok(file)
}

pub fn operation_confirmed(id: u8) -> bool {
    crate::ipc::send_ipc_command("STATUS").ok()
        .and_then(|reply| serde_json::from_str::<crate::model::FttrSystemStatus>(&reply).ok())
        .is_some_and(|status| status.sub_gateways.iter()
            .any(|onu| onu.onu_id == id && onu.state == "O5Operation"))
}

#[derive(Debug)]
pub struct OmciResultError { pub result: u8 }
impl std::fmt::Display for OmciResultError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "OMCI result={}", self.result) }
}
impl std::error::Error for OmciResultError {}
pub fn is_unknown_instance(error: &io::Error) -> bool {
    error.get_ref().and_then(|e| e.downcast_ref::<OmciResultError>()).is_some_and(|e| e.result == 5)
}

/// A bounded, reversible OMCC setup with register restoration on teardown.
pub struct StockOmccLease { id: u8, saved: Vec<(u32, u32)> }
impl StockOmccLease {
    pub fn open(id: u8) -> io::Result<Self> {
        if id > 15 { return Err(io::Error::new(io::ErrorKind::InvalidInput, "ONU has no verified hardware slot")); }
        let mut saved = Vec::new();
        for addr in [0x01000008, 0x0500009c + 16 * id as u32, 0x050000a4 + 16 * id as u32] {
            saved.push((addr, bosa::read_fpga_reg(addr)?));
        }
        let lease = Self { id, saved };
        for (index, &(addr, old)) in lease.saved.iter().enumerate() {
            let value = if index == 0 { old | (1 << id) } else { 0x11000000 };
            println!("STOCK_OMCC {addr:08x}: {old:08x}->{value:08x}");
            if index == 0 { update_onu_enable(id, true)?; }
            else { bosa::write_fpga_reg(addr, value)?; }
        }
        lease.configure_port(true)?;
        std::thread::sleep(Duration::from_millis(500));
        Ok(lease)
    }
    fn configure_port(&self, enable: bool) -> io::Result<()> {
        let port = 0x100u16 + self.id as u16;
        let mut payload = [0; 10];
        payload[0] = (port >> 4) as u8;
        payload[1] = (port << 4) as u8;
        payload[2] = if enable { 1 } else { 0xff };
        let bytes = PloamFrame::new(self.id, 0x0a, payload).to_bytes();
        for _ in 0..3 { bosa::send_ploam_msg(&bytes)?; }
        Ok(())
    }
}
impl Drop for StockOmccLease {
    fn drop(&mut self) {
        if let Err(e) = self.configure_port(false) { eprintln!("OMCC disable failed: {e}"); }
        for &(addr, value) in self.saved.iter().rev() {
            let result = if addr == 0x01000008 { update_onu_enable(self.id, value & (1u32 << self.id) != 0) }
                else { bosa::write_fpga_reg(addr, value) };
            if let Err(e) = result { eprintln!("OMCC restore {addr:08x} failed: {e}"); }
        }
    }
}

/// Match only validated PDUs, excluding socket echoes and other ONUs/transactions.
fn matched_response(bytes: &[u8], kind: u8, id: u8, request: &OmciMessage) -> Option<OmciMessage> {
    if kind == 4 || bytes.len() != 56 || bytes[..4] != 3u32.to_be_bytes()
        || bytes[4..8] != (id as u32).to_be_bytes() { return None; }
    let frame = OmciMessage::from_bytes(&bytes[8..]).ok()?;
    if frame.transaction_id != request.transaction_id
        || frame.message_type != ((request.message_type & 0x1f) | 0x20)
        || frame.class_id != request.class_id || frame.instance_id != request.instance_id { return None; }
    Some(frame)
}

#[repr(C)]
struct Address { family: u16, protocol: u16, index: i32, hatype: u16, kind: u8, length: u8, bytes: [u8; 8] }
#[cfg(target_os = "linux")]
extern "C" {
    fn socket(domain: i32, kind: i32, protocol: i32) -> i32;
    fn bind(fd: i32, address: *const Address, length: u32) -> i32;
    fn recvfrom(fd: i32, buffer: *mut u8, length: usize, flags: i32, address: *mut Address, address_length: *mut u32) -> isize;
    fn if_nametoindex(name: *const std::ffi::c_char) -> u32;
    fn close(fd: i32) -> i32;
}
pub struct OmciSession { fd: i32, id: u8, next_tci: u16, require_operation: bool, cancellation: Option<std::sync::Arc<std::sync::atomic::AtomicBool>> }
impl OmciSession {
    pub fn open(id: u8) -> io::Result<Self> {
        #[cfg(target_os = "linux")]
        {
            let name = std::ffi::CString::new("molt_omci").unwrap();
            let index = unsafe { if_nametoindex(name.as_ptr()) };
            if index == 0 { return Err(io::Error::last_os_error()); }
            let fd = unsafe { socket(17, 3, 3u16.to_be() as i32) };
            if fd < 0 { return Err(io::Error::last_os_error()); }
            let next_tci = ((SystemTime::now().duration_since(UNIX_EPOCH).map_err(io::Error::other)?.as_millis() as u16) & 0x7fff).max(1);
            let session = Self { fd, id, next_tci, require_operation: false, cancellation: None };
            let address = Address { family: 17, protocol: 3u16.to_be(), index: index as i32, hatype: 0, kind: 0, length: 0, bytes: [0;8] };
            if unsafe { bind(fd, &address, std::mem::size_of::<Address>() as u32) } < 0 { return Err(io::Error::last_os_error()); }
            Ok(session)
        }
        #[cfg(not(target_os = "linux"))]
        { let _ = id; Err(io::Error::new(io::ErrorKind::Unsupported, "OMCI requires Linux AF_PACKET")) }
    }
    pub fn require_operation(&mut self) { self.require_operation = true; }
    pub fn cancel_when(&mut self, token: std::sync::Arc<std::sync::atomic::AtomicBool>) { self.cancellation = Some(token); }
    fn check_active(&self) -> io::Result<()> {
        if self.cancellation.as_ref().is_some_and(|token| token.load(std::sync::atomic::Ordering::Acquire))
            || (self.require_operation && !operation_confirmed(self.id)) {
            return Err(io::Error::new(io::ErrorKind::NotConnected, "ONU registration generation no longer active"));
        }
        Ok(())
    }
    pub fn transact(&mut self, action: u8, class: u16, instance: u16, payload: [u8;32]) -> io::Result<OmciMessage> {
        let request = OmciMessage::new(self.next_tci, action, class, instance, payload);
        self.next_tci = (self.next_tci.wrapping_add(1) & 0x7fff).max(1);
        #[cfg(target_os = "linux")]
        for attempt in 1..=3 {
            self.check_active()?;
            send_omci_frame(self.id, &request)?;
            let deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < deadline {
                self.check_active()?;
                let mut bytes = [0; 2048];
                let mut address: Address = unsafe { std::mem::zeroed() };
                let mut length = std::mem::size_of::<Address>() as u32;
                let count = unsafe { recvfrom(self.fd, bytes.as_mut_ptr(), bytes.len(), 0x40, &mut address, &mut length) };
                if count < 0 {
                    let error = io::Error::last_os_error();
                    if error.kind() != io::ErrorKind::WouldBlock { return Err(error); }
                    std::thread::sleep(Duration::from_millis(2));
                } else if let Some(frame) = matched_response(&bytes[..count as usize], address.kind, self.id, &request) {
                    if class == 65530 {
                        println!("OMCI_REPLY TCI={:04x} type={:02x} class={} instance={:04x} attempt={} attributes=redacted", frame.transaction_id, frame.message_type, class, instance, attempt);
                    } else {
                        println!("OMCI_REPLY TCI={:04x} type={:02x} class={} instance={:04x} attempt={} wire={:02X?}", frame.transaction_id, frame.message_type, class, instance, attempt, frame.to_bytes());
                    }
                    return Ok(frame);
                }
            }
        }
        Err(io::Error::new(io::ErrorKind::TimedOut, format!("OMCI TCI={:04x} class={} instance={:04x}", request.transaction_id, class, instance)))
    }
    pub fn get(&mut self, class: u16, instance: u16, mask: u16) -> io::Result<OmciMessage> {
        let mut payload = [0;32];
        payload[..2].copy_from_slice(&mask.to_be_bytes());
        let response = self.transact(0x49, class, instance, payload)?;
        if response.payload[0] != 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, OmciResultError { result: response.payload[0] }));
        }
        if response.payload[1..3] != mask.to_be_bytes() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, format!("OMCI GET result={} mask={:02x?}", response.payload[0], &response.payload[1..3])));
        }
        Ok(response)
    }
    pub fn set(&mut self, class: u16, instance: u16, mask: u16, data: &[u8]) -> io::Result<()> {
        if data.len() > 30 { return Err(io::Error::new(io::ErrorKind::InvalidInput, "baseline SET attributes exceed payload")); }
        let mut payload = [0;32];
        payload[..2].copy_from_slice(&mask.to_be_bytes());
        payload[2..2+data.len()].copy_from_slice(data);
        let response = self.transact(0x48, class, instance, payload)?;
        if response.payload[0] != 0 { return Err(io::Error::new(io::ErrorKind::InvalidData, format!("OMCI SET class={class} instance={instance:04x} result={} details={:02x?}", response.payload[0], &response.payload[1..5]))); }
        Ok(())
    }
    /// GET establishes the table snapshot; GET NEXT reads successive 29-byte parts.
    pub fn get_table(&mut self, class: u16, instance: u16, mask: u16) -> io::Result<Vec<u8>> {
        let response = self.get(class, instance, mask)?;
        let length = u32::from_be_bytes(response.payload[3..7].try_into().unwrap()) as usize;
        if length > 4096 { return Err(io::Error::new(io::ErrorKind::InvalidData, "OMCI table exceeds diagnostic bound")); }
        let mut data = Vec::with_capacity(length);
        for sequence in 0..length.div_ceil(29) {
            let mut payload = [0;32];
            payload[..2].copy_from_slice(&mask.to_be_bytes());
            payload[2..4].copy_from_slice(&(sequence as u16).to_be_bytes());
            let part = self.transact(0x5a, class, instance, payload)?;
            if part.payload[0] != 0 || part.payload[1..3] != mask.to_be_bytes() {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "OMCI GET NEXT rejected table request"));
            }
            let count = (length - data.len()).min(29);
            data.extend_from_slice(&part.payload[3..3+count]);
        }
        Ok(data)
    }
    pub fn create(&mut self, class: u16, instance: u16, attributes: &[u8]) -> io::Result<()> {
        if attributes.len() > 32 { return Err(io::Error::new(io::ErrorKind::InvalidInput, "baseline CREATE attributes exceed payload")); }
        let mut payload = [0;32];
        payload[..attributes.len()].copy_from_slice(attributes);
        let response = self.transact(0x44, class, instance, payload)?;
        if response.payload[0] != 0 { return Err(io::Error::new(io::ErrorKind::InvalidData, format!("OMCI CREATE class={class} instance={instance:04x} result={} details={:02x?}", response.payload[0], &response.payload[1..5]))); }
        Ok(())
    }
    pub fn delete(&mut self, class: u16, instance: u16) -> io::Result<()> {
        let response = self.transact(0x46, class, instance, [0;32])?;
        if response.payload[0] != 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, OmciResultError { result: response.payload[0] }));
        }
        Ok(())
    }
    pub fn upload_mib(&mut self) -> io::Result<Vec<MibRecord>> {
        let start = self.transact(0x4d, 2, 0, [0;32])?;
        let count = u16::from_be_bytes([start.payload[0], start.payload[1]]);
        if count == 0 || count > 2048 { return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid/beyond supported MIB upload count")); }
        println!("MIB_UPLOAD commands={count}");
        let mut records = Vec::new();
        for sequence in 0..count {
            let mut payload = [0;32];
            payload[..2].copy_from_slice(&sequence.to_be_bytes());
            let response = self.transact(0x4e, 2, 0, payload)?;
            let p = response.payload;
            let record = MibRecord { sequence, class: u16::from_be_bytes([p[0],p[1]]), instance: u16::from_be_bytes([p[2],p[3]]), mask: u16::from_be_bytes([p[4],p[5]]), data: p[6..].to_vec() };
            if record.class == 0 { return Err(io::Error::new(io::ErrorKind::InvalidData, "MIB upload returned empty entity")); }
            records.push(record);
        }
        Ok(records)
    }
}
impl Drop for OmciSession {
    fn drop(&mut self) { #[cfg(target_os = "linux")] unsafe { close(self.fd); } }
}
#[derive(Debug, serde::Serialize)]
pub struct MibRecord { pub sequence: u16, pub class: u16, pub instance: u16, pub mask: u16, pub data: Vec<u8> }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn disabling_one_slot_preserves_other_live_slots() {
        let both = enable_bit(enable_bit(0x80000000, 1, true), 2, true);
        assert_eq!(enable_bit(both, 1, false), 0x80000004);
        assert_eq!(enable_bit(both, 2, false), 0x80000002);
    }
    #[test]
    fn create_is_allowed_only_for_unknown_instance() {
        for result in 1..=9 {
            let error = io::Error::new(io::ErrorKind::InvalidData, OmciResultError { result });
            assert_eq!(is_unknown_instance(&error), result == 5);
        }
        assert!(!is_unknown_instance(&io::Error::new(io::ErrorKind::TimedOut, "no response")));
        assert!(!is_unknown_instance(&io::Error::new(io::ErrorKind::InvalidData, "mask mismatch")));
    }
    #[test]
    fn captured_response_and_transaction_isolation() {
        // Actual successful H3CT ONU-G GET after padded TX and VLAN RX fix.
        let mut wire = [0;56];
        wire[..8].copy_from_slice(&[0,0,0,3,0,0,0,1]);
        wire[8..23].copy_from_slice(&[0x65,0xf0,0x29,0x0a,1,0,0,0,0,0x80,0,0x48,0x33,0x43,0x54]);
        wire[51] = 0x28;
        wire[52..].copy_from_slice(&[0xd8,0xcc,0x94,0xb4]);
        let request = OmciMessage::new(0x65f0,0x49,256,0,[0;32]);
        assert!(matched_response(&wire,0,1,&request).is_some());
        assert!(matched_response(&wire,4,1,&request).is_none());
        assert!(matched_response(&wire,0,2,&request).is_none());
        let other = OmciMessage::new(0x65f1,0x49,256,0,[0;32]);
        assert!(matched_response(&wire,0,1,&other).is_none());
        wire[24] ^= 1;
        assert!(matched_response(&wire,0,1,&request).is_none());
    }
}
