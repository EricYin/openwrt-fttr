//! Whole-device compile-time registry. Core activation/inventory never depend on adapters.
#[cfg(feature = "device-hl202")]
pub mod hl202;
#[cfg(feature = "device-hwtc")]
pub mod hwtc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Adapter {
    #[cfg(feature = "device-hl202")]
    Hl202,
    #[cfg(feature = "device-hwtc")]
    Hwtc(hwtc::Profile),
    Unavailable,
}

#[derive(Clone,Copy)]
pub enum ManagementWanStage {Configuring,Verifying}
pub fn ensure_mqtt_management_wan(device_id: &str, vlan: u32, priority: u32, persist_matching:bool, progress:impl FnMut(ManagementWanStage)->bool) -> Result<(), String> {
    #[cfg(feature = "device-hl202")]
    { hl202::management_wan::ensure(device_id, vlan, priority,persist_matching,progress) }
    #[cfg(not(feature = "device-hl202"))]
    { let _ = (device_id, vlan, priority,persist_matching,progress); Err("MQTT management WAN adapter is not compiled".into()) }
}
#[derive(Clone,Default)]
pub struct CwmpCredentials {pub username:String,pub password:String}
pub fn read_authenticated_cwmp_settings(target:&str,url:&str,credentials:&CwmpCredentials)->Result<bool,String> {
    #[cfg(feature="device-hl202")] {hl202::mqtt_lifecycle::read_authenticated_settings(target,url,credentials)}
    #[cfg(not(feature="device-hl202"))] {let _=(target,url,credentials);Err("MQTT CWMP adapter is not compiled".into())}
}
pub fn configure_authenticated_cwmp_settings(target:&str,url:&str,credentials:&CwmpCredentials)->Result<(),String> {
    #[cfg(feature="device-hl202")] {hl202::mqtt_lifecycle::configure_settings(target,url,credentials)}
    #[cfg(not(feature="device-hl202"))] {let _=(target,url,credentials);Err("MQTT CWMP adapter is not compiled".into())}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capabilities { pub business: bool, pub shell_commands: bool }
impl Adapter {
    pub fn capabilities(self) -> Capabilities {
        match self {
            #[cfg(feature = "device-hl202")]
            Self::Hl202 => Capabilities { business: true, shell_commands: true },
            #[cfg(feature = "device-hwtc")]
            Self::Hwtc(_) => Capabilities { business: true, shell_commands: false },
            _ => Capabilities { business: false, shell_commands: false },
        }
    }
    pub fn status(self) -> &'static str {
        match self {
            #[cfg(feature = "device-hl202")]
            Self::Hl202 => "Configuring",
            #[cfg(feature = "device-hwtc")]
            Self::Hwtc(_) => "Configuring",
            Self::Unavailable => "Adapter not compiled or unsupported device",
        }
    }
    pub fn verification_status(self) -> &'static str {
        match self {
            #[cfg(feature = "device-hwtc")]
            Self::Hwtc(hwtc::Profile::V152) => "WAN bootstrap pending",
            _ => "Verifying",
        }
    }
    pub fn configure(self, id: u8, token: std::sync::Arc<std::sync::atomic::AtomicBool>) -> std::io::Result<()> {
        match self {
            #[cfg(feature = "device-hl202")]
            Self::Hl202 => hl202::service::configure(id, Some(0), true, true, true, Some(token)),
            #[cfg(feature = "device-hwtc")]
            Self::Hwtc(profile) => match profile {
                hwtc::Profile::K155 => hwtc::k155::service::configure(id, Some(token)),
                hwtc::Profile::V152 => hwtc::v152::service::configure(id, Some(token)),
                hwtc::Profile::Standard => hwtc::service::configure(id, Some(token)),
            },
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
    #[cfg(feature = "device-hwtc")]
    if vendor == "HWTC" { return Adapter::Hwtc(hwtc::profile(equipment)); }
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
    fn selection_respects_vendor_model_rules_and_whole_device_features() {
        let mut identity = crate::model::OnuIdentity::default();
        assert_eq!(select("H3CT", &identity), Adapter::Unavailable);
        assert_eq!(select("HWTC", &identity).capabilities().business, cfg!(feature = "device-hwtc"));
        #[cfg(feature = "device-hwtc")]
        assert_eq!(select("HWTC", &identity), Adapter::Hwtc(hwtc::Profile::Standard));
        identity.equipment_id = Some("HL202-DU-C".into());
        assert_eq!(select("H3CT", &identity).capabilities().business, cfg!(feature = "device-hl202"));
        assert_eq!(select("HWTC", &identity).capabilities().business, cfg!(feature = "device-hwtc"));
        assert_eq!(select("OTHER", &identity), Adapter::Unavailable);
        identity.equipment_id = Some("K155-11".into());
        assert_eq!(select("HWTC", &identity).capabilities().business, cfg!(feature = "device-hwtc"));
        #[cfg(feature = "device-hwtc")]
        assert_eq!(select("HWTC", &identity), Adapter::Hwtc(hwtc::Profile::K155));
        identity.equipment_id = Some("V152".into());
        assert_eq!(select("HWTC", &identity).capabilities().business, cfg!(feature = "device-hwtc") );
        assert_eq!(select("HWTC", &identity).status(), if cfg!(feature = "device-hwtc") { "Configuring" } else { "Adapter not compiled or unsupported device" });
        assert!(!select("HWTC", &identity).capabilities().shell_commands);
        #[cfg(feature = "device-hwtc")]
        {
            assert_eq!(select("HWTC", &identity), Adapter::Hwtc(hwtc::Profile::V152));
            assert_eq!(select("HWTC", &identity).verification_status(), "WAN bootstrap pending");
            identity.equipment_id = Some("OTHER-HWTC".into());
            assert_eq!(select("HWTC", &identity), Adapter::Hwtc(hwtc::Profile::Standard));
        }
        #[cfg(not(feature = "device-hwtc"))]
        assert_eq!(select("HWTC", &identity), Adapter::Unavailable);
    }

    #[test]
    fn mqtt_connected_dispatch_reports_unsupported_identities() {
        assert!(configure_authenticated_cwmp_settings("HWTC3C6DE5B3/fttr_data.tar.gz", "http://acs",&CwmpCredentials::default())
            .is_err());
        assert!(configure_authenticated_cwmp_settings("H3CT12345678", "http://acs",&CwmpCredentials::default()).is_err());
    }
}
