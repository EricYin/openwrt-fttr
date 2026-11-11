//! Platform operation contract and the single implementation registration point.
use super::{
    adapter::{PlatformDescriptor, PlatformId},
    context::{ChannelGuard, ManagementLease, PlatformContext},
    openwrt::wake::Wake,
    resources::Resources,
    runtime::SelectionError,
};
use crate::{
    controller::inventory::WorkerRequest,
    pon_engine::{omci::session::OmciTransport, ploam::PloamFrame},
};
use std::{
    fmt::Debug,
    io,
    path::Path,
    sync::{atomic::AtomicBool, mpsc::Receiver, Arc},
};

/// Shared workers hold this interface, never a concrete platform implementation.
pub(crate) trait PlatformBackend: Debug + Sync {
    fn descriptor(&self) -> PlatformDescriptor;
    fn resources(&self) -> &'static Resources;
    fn transport(&self, id: u8) -> io::Result<Box<dyn OmciTransport>>;
    fn management_lease(&self, id: u8) -> io::Result<Box<dyn ManagementLease>>;
    fn channel(&self, id: u8, channel: u16) -> io::Result<Box<dyn ChannelGuard>>;
    fn exchange_channel(&self, id: u8, channel: u16) -> io::Result<u16>;
    fn send_ploam(&self, frame: &PloamFrame) -> io::Result<()>;
    fn run(&self, platform: PlatformContext) -> Result<(), SelectionError>;

    fn diagnostics(&self) -> Option<&dyn PlatformDiagnostics> {
        None
    }
    fn health_diagnostic(&self, _platform: PlatformContext) -> io::Result<()> {
        Err(unsupported("evidence diagnostic"))
    }
    fn watch(
        &self,
        _platform: PlatformContext,
        _id: u8,
        _cancellation: Arc<AtomicBool>,
        _vendor: [u8; 4],
        _requests: &Receiver<WorkerRequest>,
        _wake: Arc<Wake>,
    ) -> io::Result<()> {
        Err(unsupported("evidence monitor"))
    }
}

/// Optional hardware diagnostics. A platform need not have an FPGA or UX3326.
pub(crate) trait PlatformDiagnostics: Sync {
    fn read_fpga_reg(&self, _address: u32) -> io::Result<u32> {
        Err(unsupported("FPGA read"))
    }
    fn write_fpga_reg(&self, _address: u32, _value: u32) -> io::Result<()> {
        Err(unsupported("FPGA write"))
    }
    fn read_bosa_reg(&self, _address: u32) -> io::Result<u32> {
        Err(unsupported("BOSA read"))
    }
    fn write_bosa_reg(&self, _address: u32, _value: u32) -> io::Result<()> {
        Err(unsupported("BOSA write"))
    }
    fn load_bitstream(&self, _path: &Path) -> io::Result<()> {
        Err(unsupported("FPGA loading"))
    }
    fn firmware_status(&self) -> String {
        "unsupported".into()
    }
    fn fpga_diag(&self, _args: &[String]) -> io::Result<()> {
        Err(unsupported("FPGA diagnostic"))
    }
    fn set_optical_tx_enable(&self, _enabled: bool) -> io::Result<()> {
        Err(unsupported("optical TX control"))
    }
    fn get_optical_tx_enable(&self) -> io::Result<bool> {
        Err(unsupported("optical TX readback"))
    }
}

pub(crate) fn resolve(id: PlatformId) -> Result<&'static dyn PlatformBackend, SelectionError> {
    if !super::runtime::is_compiled(id) {
        return Err(SelectionError::PlatformNotCompiled(id));
    }
    match id {
        #[cfg(feature = "platform-h3c-hm2004-du")]
        PlatformId::H3cHm2004Du => Ok(&super::h3c_hm2004_du::Hm2004Du),
        _ => Err(SelectionError::BackendNotImplemented(id)),
    }
}

pub(crate) fn unsupported(operation: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        format!("platform {operation} is unavailable"),
    )
}
