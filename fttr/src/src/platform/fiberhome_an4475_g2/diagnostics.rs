use super::{
    protocol::{self, Fsl},
    transport::Device,
};
use crate::platform::{
    backend::PlatformDiagnostics,
    diagnostics::{RegisterSession, RegisterSpace},
    resources::PonMacReadback,
};
use std::{io, path::Path};

pub(crate) struct FslDiagnostics;
impl PlatformDiagnostics for FslDiagnostics {
    fn validate_register_access(
        &self,
        space: RegisterSpace,
        address: u32,
        row: u32,
        words: usize,
    ) -> io::Result<()> {
        protocol::validate(space, address, row, words)
    }
    fn open_registers(&self, endpoint: &Path) -> io::Result<Box<dyn RegisterSession>> {
        Ok(Box::new(Fsl::new(Device::open(endpoint)?)))
    }
}

impl RegisterSession for Fsl<Device> {
    fn read(
        &mut self,
        space: RegisterSpace,
        address: u32,
        row: u32,
        words: usize,
    ) -> io::Result<Vec<u32>> {
        Fsl::read(self, space, address, row, words)
    }
    fn write(
        &mut self,
        space: RegisterSpace,
        address: u32,
        row: u32,
        words: &[u32],
    ) -> io::Result<()> {
        Fsl::write(self, space, address, row, words)
    }
    fn pon_mac_readback(&mut self) -> io::Result<PonMacReadback> {
        let tx = self.read_register(protocol::OPT_CONFIG)?;
        let fec = self.read_register(protocol::DS_FEC_CONFIG)?;
        Ok(PonMacReadback {
            raw: Some(tx),
            tx_enabled: Some(tx & protocol::TX_DISABLE == 0),
            fec_enabled: Some(fec & protocol::DS_FEC_ENABLE != 0),
        })
    }
    fn set_optical_tx_enable(&mut self, enabled: bool) -> io::Result<()> {
        self.set_tx_enabled(enabled)
    }
    fn set_downstream_fec_enable(&mut self, enabled: bool) -> io::Result<()> {
        self.set_fec_enabled(enabled)
    }
}
