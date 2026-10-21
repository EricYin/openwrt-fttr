//! FMCS envelope and Linux packet socket. The envelope is not part of G.988.
use super::super::{session::OmciTransport, wire::BaselineFrame};
use std::{ffi::CString, io};

#[repr(C)]
struct Address {
    family: u16,
    protocol: u16,
    index: i32,
    hatype: u16,
    kind: u8,
    length: u8,
    bytes: [u8; 8],
}
#[repr(C)]
struct IoVec {
    base: *mut u8,
    length: usize,
}
#[repr(C)]
struct MsgHdr {
    name: *mut Address,
    name_length: u32,
    iov: *mut IoVec,
    iov_length: usize,
    control: *mut usize,
    control_length: usize,
    flags: i32,
}
extern "C" {
    fn socket(domain: i32, kind: i32, protocol: i32) -> i32;
    fn bind(fd: i32, address: *const Address, length: u32) -> i32;
    fn send(fd: i32, buffer: *const u8, length: usize, flags: i32) -> isize;
    fn sendto(
        fd: i32,
        buffer: *const u8,
        length: usize,
        flags: i32,
        address: *const Address,
        address_length: u32,
    ) -> isize;
    fn recvmsg(fd: i32, message: *mut MsgHdr, flags: i32) -> isize;
    fn setsockopt(fd: i32, level: i32, option: i32, value: *const i32, length: u32) -> i32;
    fn if_nametoindex(name: *const std::ffi::c_char) -> u32;
    fn close(fd: i32) -> i32;
}
/// One RAII packet socket for both sending and capturing, including GEM VLAN evidence.
pub struct PacketSocket {
    fd: i32,
    index: i32,
}
impl Drop for PacketSocket {
    fn drop(&mut self) {
        unsafe {
            close(self.fd);
        }
    }
}
impl PacketSocket {
    pub fn open(name: &str) -> io::Result<Self> {
        let name = CString::new(name).map_err(io::Error::other)?;
        let index = unsafe { if_nametoindex(name.as_ptr()) } as i32;
        if index == 0 {
            return Err(io::Error::last_os_error());
        }
        let fd = unsafe { socket(17, 3, 3u16.to_be() as i32) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let socket = Self { fd, index };
        let enabled = 1;
        if unsafe { setsockopt(fd, 263, 8, &enabled, 4) } < 0 {
            return Err(io::Error::last_os_error());
        }
        let address = Address {
            family: 17,
            protocol: 3u16.to_be(),
            index,
            hatype: 0,
            kind: 0,
            length: 0,
            bytes: [0; 8],
        };
        if unsafe { bind(fd, &address, std::mem::size_of::<Address>() as u32) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(socket)
    }
    pub fn transmit(&self, bytes: &[u8]) -> io::Result<()> {
        check_send(
            unsafe { send(self.fd, bytes.as_ptr(), bytes.len(), 0) },
            bytes.len(),
        )
    }
    pub fn send_fmcs(&self, id: u8, frame: &BaselineFrame) -> io::Result<()> {
        let bytes = envelope(id, frame);
        let address = Address {
            family: 17,
            protocol: 0,
            index: self.index,
            hatype: 0,
            kind: 0,
            length: 6,
            bytes: [0xff; 8],
        };
        check_send(
            unsafe {
                sendto(
                    self.fd,
                    bytes.as_ptr(),
                    bytes.len(),
                    0,
                    &address,
                    std::mem::size_of::<Address>() as u32,
                )
            },
            bytes.len(),
        )
    }
    pub fn receive_into<'a>(
        &self,
        buffer: &'a mut [u8],
    ) -> io::Result<Option<(&'a [u8], u8, Option<u16>)>> {
        let mut address: Address = unsafe { std::mem::zeroed() };
        let mut control = [0usize; 16];
        let mut iov = IoVec {
            base: buffer.as_mut_ptr(),
            length: buffer.len(),
        };
        let mut message = MsgHdr {
            name: &mut address,
            name_length: std::mem::size_of::<Address>() as u32,
            iov: &mut iov,
            iov_length: 1,
            control: control.as_mut_ptr(),
            control_length: std::mem::size_of_val(&control),
            flags: 0,
        };
        let count = unsafe { recvmsg(self.fd, &mut message, 0x40) };
        if count < 0 {
            let error = io::Error::last_os_error();
            if matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) {
                return Ok(None);
            }
            return Err(error);
        }
        let valid_len = (count as usize).min(buffer.len());
        let raw = unsafe {
            std::slice::from_raw_parts(
                control.as_ptr() as *const u8,
                message.control_length.min(std::mem::size_of_val(&control)),
            )
        };
        let header = std::mem::size_of::<usize>() + 8;
        let vlan = if raw.len() >= header + 20
            && i32::from_ne_bytes(raw[8..12].try_into().unwrap()) == 263
            && i32::from_ne_bytes(raw[12..16].try_into().unwrap()) == 8
        {
            let status = u32::from_ne_bytes(raw[header..header + 4].try_into().unwrap());
            if status & (1 << 4) != 0 {
                Some(u16::from_ne_bytes(
                    raw[header + 16..header + 18].try_into().unwrap(),
                ))
            } else {
                None
            }
        } else {
            None
        };
        Ok(Some((&buffer[..valid_len], address.kind, vlan)))
    }

    pub fn receive(&self) -> io::Result<Option<(Vec<u8>, u8, Option<u16>)>> {
        let mut buffer = [0; 2048];
        match self.receive_into(&mut buffer)? {
            Some((bytes, kind, vlan)) => Ok(Some((bytes.to_vec(), kind, vlan))),
            None => Ok(None),
        }
    }
}
fn check_send(count: isize, expected: usize) -> io::Result<()> {
    if count < 0 {
        return Err(io::Error::last_os_error());
    }
    if count as usize != expected {
        return Err(io::Error::new(
            io::ErrorKind::WriteZero,
            "short packet send",
        ));
    }
    Ok(())
}
pub fn envelope(id: u8, frame: &BaselineFrame) -> [u8; 56] {
    let mut bytes = [0; 56];
    bytes[..4].copy_from_slice(&3u32.to_be_bytes());
    bytes[4..8].copy_from_slice(&(id as u32).to_be_bytes());
    bytes[8..].copy_from_slice(&frame.to_bytes());
    bytes
}
pub fn decode_envelope(bytes: &[u8], kind: u8, id: u8) -> io::Result<Option<BaselineFrame>> {
    if kind == 4
        || bytes.len() != 56
        || bytes[..4] != 3u32.to_be_bytes()
        || bytes[4..8] != (id as u32).to_be_bytes()
    {
        return Ok(None);
    }
    BaselineFrame::from_bytes(&bytes[8..]).map(Some)
}
pub struct FmcsTransport {
    id: u8,
    socket: PacketSocket,
}
impl FmcsTransport {
    pub fn open(id: u8) -> io::Result<Self> {
        Ok(Self {
            id,
            socket: PacketSocket::open("molt_omci")?,
        })
    }
}
impl OmciTransport for FmcsTransport {
    fn send(&mut self, frame: &BaselineFrame) -> io::Result<()> {
        self.socket.send_fmcs(self.id, frame)
    }
    fn receive(&mut self) -> io::Result<Option<BaselineFrame>> {
        match self.socket.receive()? {
            Some((bytes, kind, _)) => match decode_envelope(&bytes, kind, self.id) {
                Ok(frame) => Ok(frame),
                Err(_) => Ok(None), // corrupt/unrelated RX is not a transaction response
            },
            None => Ok(None),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn captured_response_and_onu_echo_isolation() {
        let mut bytes = [0; 56];
        bytes[..8].copy_from_slice(&[0, 0, 0, 3, 0, 0, 0, 1]);
        bytes[8..23].copy_from_slice(&[
            0x65, 0xf0, 0x29, 0x0a, 1, 0, 0, 0, 0, 0x80, 0, 0x48, 0x33, 0x43, 0x54,
        ]);
        bytes[51] = 0x28;
        bytes[52..].copy_from_slice(&[0xd8, 0xcc, 0x94, 0xb4]);
        let request = BaselineFrame::new(0x65f0, 0x49, 256, 0, [0; 32]);
        assert!(decode_envelope(&bytes, 0, 1)
            .unwrap()
            .unwrap()
            .answers(&request));
        assert!(decode_envelope(&bytes, 4, 1).unwrap().is_none());
        assert!(decode_envelope(&bytes, 0, 2).unwrap().is_none());
        bytes[24] ^= 1;
        assert!(decode_envelope(&bytes, 0, 1).is_err());
    }
}
