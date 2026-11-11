//! Optional diagnostics delegated to the explicitly selected platform backend.
use super::{
    backend::{unsupported, PlatformDiagnostics},
    context::PlatformContext,
};
use std::{io, path::Path};

#[derive(Clone, Copy)]
pub struct HardwareDiagnostics(PlatformContext);
impl HardwareDiagnostics {
    pub fn new(platform: PlatformContext) -> Self {
        Self(platform)
    }
    fn operations(self) -> io::Result<&'static dyn PlatformDiagnostics> {
        self.0
            .backend()
            .diagnostics()
            .ok_or_else(|| unsupported("raw diagnostics"))
    }
    pub fn read_fpga_reg(self, address: u32) -> io::Result<u32> {
        self.operations()?.read_fpga_reg(address)
    }
    pub fn write_fpga_reg(self, address: u32, value: u32) -> io::Result<()> {
        self.operations()?.write_fpga_reg(address, value)
    }
    pub fn read_bosa_reg(self, address: u32) -> io::Result<u32> {
        self.operations()?.read_bosa_reg(address)
    }
    pub fn write_bosa_reg(self, address: u32, value: u32) -> io::Result<()> {
        self.operations()?.write_bosa_reg(address, value)
    }
    pub fn load_bitstream(self, path: &Path) -> io::Result<()> {
        self.operations()?.load_bitstream(path)
    }
    pub fn firmware_status(self) -> String {
        self.operations()
            .map(|ops| ops.firmware_status())
            .unwrap_or_else(|_| "unsupported".into())
    }
    pub fn fpga_diag(self, args: &[String]) -> io::Result<()> {
        self.operations()?.fpga_diag(args)
    }
    pub fn set_optical_tx_enable(self, enabled: bool) -> io::Result<()> {
        self.operations()?.set_optical_tx_enable(enabled)
    }
    pub fn get_optical_tx_enable(self) -> io::Result<bool> {
        self.operations()?.get_optical_tx_enable()
    }
}
