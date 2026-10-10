//! Whole-device compile-time registry. Core activation/inventory never depend on adapters.
#[cfg(feature = "device-hl202")]
pub mod hl202;
#[cfg(feature = "device-hwtc-k155")]
pub mod hwtc_k155;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Adapter {
    #[cfg(feature = "device-hl202")]
    Hl202,
    #[cfg(feature = "device-hwtc-k155")]
    HwtcK155,
    Unavailable,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capabilities { pub business: bool, pub shell_commands: bool }
impl Adapter {
    pub fn capabilities(self) -> Capabilities {
        match self {
            #[cfg(feature = "device-hl202")]
            Self::Hl202 => Capabilities { business: true, shell_commands: true },
            #[cfg(feature = "device-hwtc-k155")]
            Self::HwtcK155 => Capabilities { business: true, shell_commands: false },
            _ => Capabilities { business: false, shell_commands: false },
        }
    }
    pub fn status(self) -> &'static str {
        match self {
            #[cfg(feature = "device-hl202")]
            Self::Hl202 => "Configuring",
            #[cfg(feature = "device-hwtc-k155")]
            Self::HwtcK155 => "Configuring",
            Self::Unavailable => "Adapter not compiled or unsupported device",
        }
    }
    pub fn configure(self, id: u8, token: std::sync::Arc<std::sync::atomic::AtomicBool>) -> std::io::Result<()> {
        match self {
            #[cfg(feature = "device-hl202")]
            Self::Hl202 => hl202::service::configure(id, Some(0), true, true, true, Some(token)),
            #[cfg(feature = "device-hwtc-k155")]
            Self::HwtcK155 => hwtc_k155::service::configure(id, Some(token)),
            _ => {
                let _ = (id, token);
                Err(std::io::Error::new(std::io::ErrorKind::Unsupported, self.status()))
            }
        }
    }
}
pub fn select(vendor: &str, identity: &crate::model::OnuIdentity) -> Adapter {
    let equipment = identity.equipment_id.as_deref();
    #[cfg(feature = "device-hl202")]
    if hl202::matches(vendor, equipment) { return Adapter::Hl202; }
    #[cfg(feature = "device-hwtc-k155")]
    if hwtc_k155::matches(vendor, equipment) { return Adapter::HwtcK155; }
    let _ = (vendor, equipment);
    Adapter::Unavailable
}

/// Compatibility IPC delegates the entire command contract to its device adapter.
pub fn legacy_command(line: &str) -> crate::model::IpcResponse {
    #[cfg(feature = "device-hl202")]
    return hl202::legacy_commands::handle(line);
    #[cfg(not(feature = "device-hl202"))]
    {
        let _ = line;
        crate::model::IpcResponse::err("HL202 adapter not compiled")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selection_requires_model_and_respects_whole_device_features() {
        let mut identity = crate::model::OnuIdentity::default();
        assert_eq!(select("H3CT", &identity), Adapter::Unavailable);
        identity.equipment_id = Some("HL202-DU-C".into());
        assert_eq!(select("H3CT", &identity).capabilities().business, cfg!(feature = "device-hl202"));
        assert_eq!(select("HWTC", &identity), Adapter::Unavailable);
        identity.equipment_id = Some("K155-11".into());
        assert_eq!(select("HWTC", &identity).capabilities().business, cfg!(feature = "device-hwtc-k155"));
        assert!(!select("HWTC", &identity).capabilities().shell_commands);
        #[cfg(feature = "device-hwtc-k155")]
        assert_eq!(select("HWTC", &identity), Adapter::HwtcK155);
        #[cfg(not(feature = "device-hwtc-k155"))]
        assert_eq!(select("HWTC", &identity), Adapter::Unavailable);
    }
}
