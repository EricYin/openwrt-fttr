//! Modular GPON Micro-OLT Protocol Engine
//!
//! Provides decoupled protocol processing:
//! - `ploam`: ITU-T G.984.3 O1-O5 discovery & ranging state machine
//! - `omci`: ITU-T G.988 MIB baseline provisioning

pub mod omci;
pub mod ploam;

use self::ploam::{GponOnu, OnuState, PloamFrame};

pub struct PonEngine {
    pub active_onus: Vec<GponOnu>,
    next_onu_id: u8,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serial_and_assignment_do_not_claim_operation() {
        let mut engine = PonEngine::new();
        let onu = engine.register_onu(*b"TEST", [0x12, 0x34, 0x56, 0x78]);
        assert_eq!(onu.state, OnuState::O3SerialNumber);
        let frames = engine.activate_onu(onu.onu_id);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].message_id, ploam::PLOAM_ASSIGN_ONU_ID);
        assert_eq!(engine.active_onus[0].state, OnuState::O4Ranging);
        assert_eq!(engine.active_onus[0].eq_delay, None);
        assert_eq!(engine.active_onus[0].firmware_version, "Unknown");
    }

    #[test]
    fn receiver_rejects_bad_crc_and_truncated_messages() {
        let frame = PloamFrame::new(0xff, 1, [0x54, 0x45, 0x53, 0x54, 1, 2, 3, 4, 0, 0]);
        let mut bytes = frame.to_bytes();
        assert!(PloamFrame::from_bytes(&bytes).is_some());
        assert!(PloamFrame::from_bytes(&bytes[..12]).is_none());
        bytes[6] ^= 1;
        assert!(PloamFrame::from_bytes(&bytes).is_none());
    }

    #[test]
    fn captured_ranging_vector_and_corrupt_post_ranging_frames() {
        // Independent device trace: slot 1 measurement 0x3c9b8, absolute
        // main path, bit units. The received bad ACK must remain rejected.
        assert_eq!(PloamFrame::assign_delay(1, 248248).to_bytes(),
            [1, 4, 0, 0, 3, 0xc9, 0xb8, 0, 0, 0, 0, 0, 0xe8]);
        assert!(PloamFrame::from_bytes(&[1, 2, 0x30, 0x31, 0x32, 0x33,
            0x34, 0x35, 0x36, 0x36, 0x38, 0x38, 0x26]).is_none());
        assert!(PloamFrame::from_bytes(&[1, 9, 0x0e, 1, 0x0e, 1,
            0, 0x10, 0, 0, 0, 1, 0xe2]).is_none());
        // Native, unmodified frame after TN=10 / shift=0: CRC and copied
        // Assign_Alloc-ID payload both match, unlike the capture above.
        let ack = PloamFrame::from_bytes(&[1, 9, 0x0e, 1, 0x0e, 1,
            0, 0x10, 0, 0, 0, 0, 0xe2]).unwrap();
        assert_eq!(ack.payload[0], 0x0e);
        assert_eq!(ack.payload[1..], PloamFrame::assign_alloc_id(1).to_bytes()[..9]);
    }

    #[test]
    fn retry_discards_previous_eqd_and_operation_state() {
        let mut engine = PonEngine::new();
        let onu = engine.register_onu(*b"TEST", [1, 2, 3, 4]);
        engine.active_onus[0].eq_delay = Some(248248);
        engine.active_onus[0].state = OnuState::O5Operation;
        engine.activate_onu(onu.onu_id);
        assert_eq!(engine.active_onus[0].eq_delay, None);
        assert_eq!(engine.active_onus[0].state, OnuState::O4Ranging);
    }

    #[test]
    fn offline_retains_identity_but_requires_new_activation_evidence() {
        let mut engine = PonEngine::new();
        let old = engine.register_onu(*b"TEST", [1,2,3,4]);
        engine.active_onus[0].state = OnuState::O5Operation;
        engine.active_onus[0].eq_delay = Some(248248);
        engine.mark_offline(old.onu_id);
        assert_eq!(engine.active_onus[0].state, OnuState::O1Initial);
        assert_eq!(engine.active_onus[0].eq_delay, None);
        let again = engine.register_onu(old.vendor_id, old.serial_number);
        assert_eq!(again.onu_id, old.onu_id);
        assert_eq!(engine.active_onus.len(), 1);
        engine.activate_onu(again.onu_id);
        assert_eq!(engine.active_onus[0].state, OnuState::O4Ranging);
        assert_eq!(engine.active_onus[0].eq_delay, None);
    }
}

impl PonEngine {
    pub fn new() -> Self {
        Self {
            active_onus: Vec::new(),
            next_onu_id: 1,
        }
    }

    /// Generates initial PLOAM downstream frames to discover sub-gateways on the ODN
    pub fn build_discovery_frames(&self) -> Vec<PloamFrame> {
        vec![
            PloamFrame::upstream_overhead(),
            PloamFrame::extended_burst_length(),
        ]
    }

    /// Processes an ONU discovered on the PON link (from upstream PLOAM or auto-ranging)
    pub fn register_onu(&mut self, vendor: [u8; 4], sn: [u8; 4]) -> GponOnu {
        // Check if already registered
        for onu in &self.active_onus {
            if onu.vendor_id == vendor && onu.serial_number == sn {
                return onu.clone();
            }
        }

        let id = self.next_onu_id;
        self.next_onu_id = (self.next_onu_id % 16) + 1;

        let mut onu = GponOnu::new(id, vendor, sn);
        println!(
            "[+] PON Engine: New ONU discovered! ID={}, Vendor={}, SN={}",
            onu.onu_id,
            onu.vendor_str(),
            onu.sn_hex()
        );

        onu.state = OnuState::O3SerialNumber;
        self.active_onus.push(onu.clone());
        onu
    }

    /// Forget activation evidence while retaining the serial-to-slot assignment.
    pub fn mark_offline(&mut self, onu_id: u8) {
        if let Some(onu) = self.active_onus.iter_mut().find(|o| o.onu_id == onu_id) {
            // Retain identity/slot only, never evidence from the previous activation.
            onu.state = OnuState::O1Initial;
            onu.eq_delay = None;
        }
    }

    /// Assign the ONU ID; fresh ranging and a validated reply must establish O5.
    pub fn activate_onu(&mut self, onu_id: u8) -> Vec<PloamFrame> {
        if let Some(onu) = self.active_onus.iter_mut().find(|o| o.onu_id == onu_id) {
            println!("[*] PLOAM: Assigning ONU {}; awaiting measured ranging response", onu_id);
            onu.state = OnuState::O4Ranging;
            onu.eq_delay = None; // A new attempt must not inherit an old measurement.
            vec![
                PloamFrame::assign_onu_id(onu_id, onu.vendor_id, onu.serial_number),
            ]
        } else {
            Vec::new()
        }
    }

}
