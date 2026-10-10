//! ITU-T G.988 Baseline OMCI (ONU Management and Control Interface) Engine
//!
//! Synthesizes standard 48-byte baseline OMCI messages to provision:
//! - MIB Reset
//! - T-CONT Allocation
//! - GEM Port Network CTP & GEM Interworking TP
//! - MAC Bridge Service Profile & Port mapping
//! - Extended VLAN Tagging Operation (VLAN 1 bridge for LAN)
//! - Ethernet UNI administrative unlock

pub const OMCI_BASELINE_DEVICE_ID: u8 = 0x0A;

// OMCI Action Types (with AR = 0x40 flag set)
pub const OMCI_ACTION_CREATE: u8 = 0x40 | 0x04;
pub const OMCI_ACTION_DELETE: u8 = 0x40 | 0x05;
pub const OMCI_ACTION_SET: u8 = 0x40 | 0x08;
pub const OMCI_ACTION_GET: u8 = 0x40 | 0x09;
pub const OMCI_ACTION_MIB_RESET: u8 = 0x40 | 0x0F;

// Standard ITU-T G.988 Managed Entity (ME) Class IDs
pub const ME_ONT_DATA: u16 = 2;
pub const ME_PPTP_ETHERNET_UNI: u16 = 11;
pub const ME_MAC_BRIDGE_SERVICE_PROFILE: u16 = 45;
pub const ME_MAC_BRIDGE_PORT_CONFIG_DATA: u16 = 47;
pub const ME_EXT_VLAN_TAGGING_OP: u16 = 171;
pub const ME_TCONT: u16 = 262;
pub const ME_GEM_INTERWORKING_TP: u16 = 266;
pub const ME_GEM_PORT_NETWORK_CTP: u16 = 268;

/// Standard 48-byte Baseline OMCI Message
#[derive(Debug, Clone, Copy)]
pub struct OmciMessage {
    pub transaction_id: u16,
    pub message_type: u8,
    pub device_id: u8,
    pub class_id: u16,
    pub instance_id: u16,
    pub payload: [u8; 32],
}

impl OmciMessage {
    /// Decode legacy GPON baseline framing only after validating the CPCS trailer and CRC.
    pub fn from_bytes(bytes: &[u8]) -> std::io::Result<Self> {
        use std::io::{Error, ErrorKind};
        if bytes.len() != 48 || bytes[3] != OMCI_BASELINE_DEVICE_ID {
            return Err(Error::new(ErrorKind::InvalidData, "invalid baseline OMCI length/device"));
        }
        if bytes[40..44] != [0, 0, 0, 0x28]
            || bytes[44..48] != Self::compute_crc32(&bytes[..44]).to_be_bytes()
        {
            return Err(Error::new(ErrorKind::InvalidData, "invalid GPON OMCI trailer/CRC"));
        }
        let mut payload = [0; 32];
        payload.copy_from_slice(&bytes[8..40]);
        Ok(Self::new(
            u16::from_be_bytes([bytes[0], bytes[1]]), bytes[2],
            u16::from_be_bytes([bytes[4], bytes[5]]),
            u16::from_be_bytes([bytes[6], bytes[7]]), payload,
        ))
    }

    pub fn new(
        transaction_id: u16,
        message_type: u8,
        class_id: u16,
        instance_id: u16,
        payload: [u8; 32],
    ) -> Self {
        Self {
            transaction_id,
            message_type,
            device_id: OMCI_BASELINE_DEVICE_ID,
            class_id,
            instance_id,
            payload,
        }
    }

    /// Serializes to the standard 48-byte ITU-T G.988 frame
    pub fn to_bytes(&self) -> [u8; 48] {
        let mut out = [0u8; 48];
        out[0] = ((self.transaction_id >> 8) & 0xFF) as u8;
        out[1] = (self.transaction_id & 0xFF) as u8;
        out[2] = self.message_type;
        out[3] = self.device_id;
        out[4] = ((self.class_id >> 8) & 0xFF) as u8;
        out[5] = (self.class_id & 0xFF) as u8;
        out[6] = ((self.instance_id >> 8) & 0xFF) as u8;
        out[7] = (self.instance_id & 0xFF) as u8;
        out[8..40].copy_from_slice(&self.payload);
        out[40] = 0x00; // CPCS-UU
        out[41] = 0x00; // CPI
        out[42] = 0x00; // Length MSB
        out[43] = 0x28; // Length LSB (40 payload bytes)

        // Compute CRC-32 (ITU-T G.988)
        let crc = Self::compute_crc32(&out[..44]);
        out[44] = ((crc >> 24) & 0xFF) as u8;
        out[45] = ((crc >> 16) & 0xFF) as u8;
        out[46] = ((crc >> 8) & 0xFF) as u8;
        out[47] = (crc & 0xFF) as u8;
        out
    }

    fn compute_crc32(bytes: &[u8]) -> u32 {
        let mut crc = 0xFFFFFFFFu32;
        for &b in bytes {
            crc ^= (b as u32) << 24;
            for _ in 0..8 {
                if (crc & 0x80000000) != 0 {
                    crc = (crc << 1) ^ 0x04C11DB7;
                } else {
                    crc <<= 1;
                }
            }
        }
        !crc
    }

    /// 1. MIB Reset (Resets ONU internal MIB to clean state)
    pub fn mib_reset(tx_id: u16) -> Self {
        Self::new(tx_id, OMCI_ACTION_MIB_RESET, ME_ONT_DATA, 0, [0u8; 32])
    }

    /// 2. Create T-CONT (Alloc-ID = 256 for default data channel)
    pub fn create_tcont(tx_id: u16, tcont_inst: u16, alloc_id: u16) -> Self {
        let mut payload = [0u8; 32];
        payload[0] = ((alloc_id >> 8) & 0xFF) as u8;
        payload[1] = (alloc_id & 0xFF) as u8;
        payload[2] = 0x01; // Policy: strict priority
        Self::new(tx_id, OMCI_ACTION_SET, ME_TCONT, tcont_inst, payload)
    }

    /// 3. Create GEM Port Network CTP (Port ID 256 mapped to T-CONT)
    pub fn create_gem_port(tx_id: u16, gem_port_id: u16, tcont_inst: u16) -> Self {
        let mut payload = [0u8; 32];
        payload[0] = ((gem_port_id >> 8) & 0xFF) as u8;
        payload[1] = (gem_port_id & 0xFF) as u8;
        payload[2] = ((tcont_inst >> 8) & 0xFF) as u8;
        payload[3] = (tcont_inst & 0xFF) as u8;
        payload[4] = 0x03; // Direction: bidirectional
        Self::new(tx_id, OMCI_ACTION_CREATE, ME_GEM_PORT_NETWORK_CTP, gem_port_id, payload)
    }

    /// 4. Create GEM Interworking TP
    pub fn create_gem_interworking_tp(tx_id: u16, gem_port_id: u16) -> Self {
        let mut payload = [0u8; 32];
        payload[0] = ((gem_port_id >> 8) & 0xFF) as u8;
        payload[1] = (gem_port_id & 0xFF) as u8;
        payload[2] = 0x05; // Service type: Ethernet bridge
        payload[3] = 0x00; // MAC Bridge Pointer
        payload[4] = 0x01;
        Self::new(tx_id, OMCI_ACTION_CREATE, ME_GEM_INTERWORKING_TP, gem_port_id, payload)
    }

    /// 5. Create MAC Bridge Service Profile (Bridge instance 1)
    pub fn create_mac_bridge_service_profile(tx_id: u16, bridge_inst: u16) -> Self {
        let mut payload = [0u8; 32];
        payload[0] = 0x00; // Spanning tree disabled
        payload[1] = 0x08; // Learning: enabled
        payload[2] = 0x01; // Port bridging: enabled
        Self::new(tx_id, OMCI_ACTION_CREATE, ME_MAC_BRIDGE_SERVICE_PROFILE, bridge_inst, payload)
    }

    /// 6. Unlock Ethernet Port UNI (Class 11, Admin State = 0)
    pub fn unlock_ethernet_uni(tx_id: u16, uni_inst: u16) -> Self {
        let mut payload = [0u8; 32];
        payload[0] = 0x80; // Attribute mask: Admin state
        payload[1] = 0x00; // Admin state = 0 (Unlocked)
        Self::new(tx_id, OMCI_ACTION_SET, ME_PPTP_ETHERNET_UNI, uni_inst, payload)
    }


    /// 10. Get Equipment ID (ME 257 ONU2-G, Attr 1 Equipment ID mask: 0x8000)
    pub fn get_equipment_id(tx_id: u16) -> Self {
        let mut payload = [0u8; 32];
        payload[0] = 0x80;
        payload[1] = 0x00;
        Self::new(tx_id, OMCI_ACTION_GET, 257, 0, payload)
    }

    /// 11. Get Software Version (ME 7 Software Image, Instance 0, Attr 1 Version mask: 0x8000)
    pub fn get_software_version(tx_id: u16) -> Self {
        let mut payload = [0u8; 32];
        payload[0] = 0x80;
        payload[1] = 0x00;
        Self::new(tx_id, OMCI_ACTION_GET, 7, 0, payload)
    }

    /// 12. Get Hardware Version (ME 256 ONU-G, Attr 3 Version mask: 0x2000)
    pub fn get_hardware_version(tx_id: u16) -> Self {
        let mut payload = [0u8; 32];
        payload[0] = 0x20;
        payload[1] = 0x00;
        Self::new(tx_id, OMCI_ACTION_GET, 256, 0, payload)
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpon_get_wire_vector_and_invalid_receive_frames() {
        let mut wire = [0u8; 48];
        wire[..10].copy_from_slice(&[0x12, 0x34, 0x49, 0x0a, 1, 0, 0, 0, 0x80, 0]);
        wire[43] = 0x28;
        wire[44..].copy_from_slice(&[0x7e, 0xdf, 0x0a, 0x44]);
        let decoded = OmciMessage::from_bytes(&wire).unwrap();
        assert_eq!(decoded.transaction_id, 0x1234);
        assert_eq!(decoded.class_id, 256);
        assert_eq!(decoded.to_bytes(), wire);
        assert!(OmciMessage::from_bytes(&wire[..47]).is_err());
        wire[10] ^= 1;
        assert!(OmciMessage::from_bytes(&wire).is_err());
        wire[10] ^= 1;
        wire[43] = 0;
        assert!(OmciMessage::from_bytes(&wire).is_err());
        assert_eq!(ME_TCONT, 262);
    }
}

#[repr(C)]
struct SockAddrLl {
    sll_family: u16,
    sll_protocol: u16,
    sll_ifindex: i32,
    sll_hatype: u16,
    sll_pkttype: u8,
    sll_halen: u8,
    sll_addr: [u8; 8],
}

#[cfg(unix)]
extern "C" {
    fn socket(domain: i32, ty: i32, protocol: i32) -> i32;
    fn sendto(s: i32, buf: *const u8, len: usize, flags: i32, to: *const SockAddrLl, tolen: u32) -> isize;
    fn if_nametoindex(ifname: *const std::ffi::c_char) -> u32;
    fn close(fd: i32) -> i32;
}

pub fn send_omci_frame(onu_id: u8, msg: &OmciMessage) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::ffi::CString;

        // 1. Send via molt_omci (virtual netdev)
        let ifname_molt = CString::new("molt_omci").unwrap();
        let ifindex_molt = unsafe { if_nametoindex(ifname_molt.as_ptr()) };
        if ifindex_molt == 0 {
            return Err(std::io::Error::last_os_error());
        }
        if ifindex_molt != 0 {
            let sock = unsafe { socket(17 /* AF_PACKET */, 3 /* SOCK_RAW */, (0x0003u16).to_be() as i32) };
            if sock < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if sock >= 0 {
                let mut sll: SockAddrLl = unsafe { std::mem::zeroed() };
                sll.sll_family = 17;
                sll.sll_ifindex = ifindex_molt as i32;
                sll.sll_halen = 6;
                sll.sll_addr[..6].copy_from_slice(&[0xff; 6]);

                // The FMCS kernel transport requires an 8-byte header:
                // [0..3]: htonl(3) (OMCI frame type)
                // [4..7]: htonl(onu_id as u32) (target port / ONU ID)
                // [8..55]: 48-byte ITU-T G.988 OMCI message
                let mut packet = Vec::with_capacity(56);
                packet.extend_from_slice(&(3u32.to_be_bytes()));
                packet.extend_from_slice(&((onu_id as u32).to_be_bytes()));
                packet.extend_from_slice(&msg.to_bytes());

                let sent = unsafe {
                    sendto(
                        sock,
                        packet.as_ptr(),
                        packet.len(),
                        0,
                        &sll as *const _,
                        std::mem::size_of::<SockAddrLl>() as u32,
                    )
                };
                let error = if sent < 0 { Some(std::io::Error::last_os_error()) } else { None };
                unsafe { close(sock); }
                if let Some(error) = error { return Err(error); }
                if sent as usize != packet.len() {
                    return Err(std::io::Error::new(std::io::ErrorKind::WriteZero, "short OMCI send"));
                }
            }
        }

        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (onu_id, msg);
        Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "OMCI requires Linux AF_PACKET"))
    }
}
