//! Whole-device compile-time registry. Core activation/inventory never depend on adapters.
pub mod cwmp;
use crate::model::BusinessPhase;

pub fn observe_mqtt_report(source: &str, payload: Vec<u8>, received: u64) -> bool {
    #[cfg(all(feature = "device-hl202", feature = "mqtt"))]
    { hl202::telemetry::store::observe(source, payload, received) }
    #[cfg(not(all(feature = "device-hl202", feature = "mqtt")))]
    { let _ = (source, payload, received); false }
}
#[cfg(feature = "device-hl202")]
pub mod hl202;
#[cfg(feature = "device-hwtc")]
pub mod hwtc;
pub mod generic;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Adapter {
    #[cfg(feature = "device-hl202")]
    Hl202,
    #[cfg(feature = "device-hwtc")]
    Hwtc(hwtc::Profile),
    GenericBridge,
    Unavailable,
}

#[derive(Clone,Copy)]
pub enum ManagementWanStage {Configuring,Verifying}
pub fn ensure_mqtt_management_wan(device_id: &str, vlans: &[u32], priority: u32, persist_matching:bool, progress:impl FnMut(ManagementWanStage)->bool) -> Result<u32, String> {
    #[cfg(all(feature = "device-hl202", feature = "mqtt"))]
    { hl202::management_wan::ensure(device_id, vlans, priority,persist_matching,progress) }
    #[cfg(not(all(feature = "device-hl202", feature = "mqtt")))]
    { let _ = (device_id, vlans, priority,persist_matching,progress); Err("MQTT management WAN unavailable: build requires mqtt and device-hl202".into()) }
}
#[derive(Clone,Default)]
pub struct CwmpCredentials {pub username:String,pub password:String}
pub fn read_authenticated_cwmp_settings(target:&str,url:&str,credentials:&CwmpCredentials)->Result<bool,String> {
    #[cfg(all(feature="device-hl202", feature="mqtt"))] {hl202::mqtt_lifecycle::read_authenticated_settings(target,url,credentials)}
    #[cfg(not(all(feature="device-hl202", feature="mqtt")))] {let _=(target,url,credentials);Err("MQTT CWMP unavailable: build requires mqtt and device-hl202".into())}
}
pub fn configure_authenticated_cwmp_settings(target:&str,url:&str,credentials:&CwmpCredentials)->Result<(),String> {
    #[cfg(all(feature="device-hl202", feature="mqtt"))] {hl202::mqtt_lifecycle::configure_settings(target,url,credentials)}
    #[cfg(not(all(feature="device-hl202", feature="mqtt")))] {let _=(target,url,credentials);Err("MQTT CWMP unavailable: build requires mqtt and device-hl202".into())}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capabilities { pub business: bool, pub shell_commands: bool }
impl Adapter {
    pub fn capabilities(self) -> Capabilities {
        match self {
            #[cfg(feature = "device-hl202")]
            Self::Hl202 => Capabilities { business: true, shell_commands: cfg!(feature = "mqtt") },
            #[cfg(feature = "device-hwtc")]
            Self::Hwtc(_) => Capabilities { business: true, shell_commands: false },
            Self::GenericBridge => Capabilities { business: true, shell_commands: false },
            _ => Capabilities { business: false, shell_commands: false },
        }
    }
    pub fn status(self) -> BusinessPhase {
        match self {
            #[cfg(feature = "device-hl202")]
            Self::Hl202 => BusinessPhase::Configuring,
            #[cfg(feature = "device-hwtc")]
            Self::Hwtc(_) => BusinessPhase::Configuring,
            Self::GenericBridge => BusinessPhase::Configuring,
            Self::Unavailable => BusinessPhase::Unsupported,
        }
    }
    pub fn verification_status(self) -> BusinessPhase {
        match self {
            #[cfg(feature = "device-hwtc")]
            Self::Hwtc(hwtc::Profile::V152) => BusinessPhase::WanBootstrapPending,
            #[cfg(feature = "device-hwtc")]
            Self::Hwtc(hwtc::Profile::K155 | hwtc::Profile::Standard) => BusinessPhase::Verifying,
            #[cfg(feature = "device-hl202")]
            Self::Hl202 => BusinessPhase::Verifying,
            Self::GenericBridge => BusinessPhase::Verifying,
            Self::Unavailable => BusinessPhase::Verifying,
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
            Self::GenericBridge => generic::service::configure(id, Some(token)),
            _ => {
                let _ = (id, token);
                Err(std::io::Error::new(std::io::ErrorKind::Unsupported, self.status().to_string()))
            }
        }
    }
}
pub fn select(vendor: &str, identity: &crate::model::OnuIdentity) -> Adapter {
    select_with_serial(vendor, identity, None)
}

pub fn select_with_serial(vendor: &str, identity: &crate::model::OnuIdentity, serial: Option<&str>) -> Adapter {
    let equipment = identity.equipment_id.as_deref();
    if let Some(sn) = serial {
        if crate::provisioning::is_device_adapter(sn, "generic") {
            return Adapter::GenericBridge;
        }
    }
    #[cfg(feature = "device-hl202")]
    if hl202::matches(vendor, equipment) { return Adapter::Hl202; }
    #[cfg(feature = "device-hwtc")]
    if vendor == "HWTC" { return Adapter::Hwtc(hwtc::profile(equipment)); }
    let _ = (vendor, equipment);
    if serial.is_some_and(crate::provisioning::allows_generic_bridge)
        || std::env::var("FTTR_ALLOW_UNSUPPORTED_AS_BRIDGE").is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
    {
        return Adapter::GenericBridge;
    }
    Adapter::Unavailable
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[cfg(not(feature = "mqtt"))]
    fn mqtt_disabled_commands_are_local_and_inventory_remains_available() {
        let _port = crate::MQTT_TEST_PORT.lock().unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:1883").unwrap();
        listener.set_nonblocking(true).unwrap();
        let credentials = CwmpCredentials::default();
        assert!(ensure_mqtt_management_wan("02AABBCCDDEE", &[1], 0, false, |_| true).unwrap_err().contains("unavailable"));
        assert!(read_authenticated_cwmp_settings("02AABBCCDDEE", "http://acs", &credentials).unwrap_err().contains("unavailable"));
        assert!(configure_authenticated_cwmp_settings("02AABBCCDDEE", "http://acs", &credentials).unwrap_err().contains("unavailable"));
        #[cfg(feature = "device-hl202")]
        {
            assert_eq!(hl202::command::execute("02AABBCCDDEE", "fixture", std::time::Duration::from_secs(1)).unwrap_err().kind(), std::io::ErrorKind::Unsupported);
            assert_eq!(hl202::command::dispatch("all", "fixture").unwrap_err().kind(), std::io::ErrorKind::Unsupported);
            let typed = hl202::api::Query::Run { request: hl202::request::Request {
                schema_version: 1,
                target: crate::mqtt_identity::Selection { optical_sn: "H3CT12345678".into(), onu_id: 2, binding_generation: "fixture".into() },
                action: hl202::request::Action::AccountsRead,
            }};
            let reply = hl202::api::control(&serde_json::to_string(&typed).unwrap(), &[]);
            assert_eq!(reply["error"]["code"], "mqtt_unavailable");
            let identity = crate::model::OnuIdentity { equipment_id: Some("HL202-DU-C".into()), ..Default::default() };
            let adapter = select("H3CT", &identity);
            assert!(adapter.capabilities().business);
            assert!(!adapter.capabilities().shell_commands);
            assert_eq!(adapter.verification_status(), BusinessPhase::Verifying);
        }
        assert!(Adapter::GenericBridge.capabilities().business);
        assert_eq!(listener.accept().unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
    }
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
        assert_eq!(select("HWTC", &identity).status(), if cfg!(feature = "device-hwtc") { BusinessPhase::Configuring } else { BusinessPhase::Unsupported });
        assert!(!select("HWTC", &identity).capabilities().shell_commands);
        #[cfg(feature = "device-hwtc")]
        {
            assert_eq!(select("HWTC", &identity), Adapter::Hwtc(hwtc::Profile::V152));
            assert_eq!(select("HWTC", &identity).verification_status(), BusinessPhase::WanBootstrapPending);
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

    #[test]
    fn generic_bridge_adapter_capabilities_and_selection() {
        let adapter = Adapter::GenericBridge;
        assert!(adapter.capabilities().business);
        assert!(!adapter.capabilities().shell_commands);
        assert_eq!(adapter.status(), BusinessPhase::Configuring);
        assert_eq!(adapter.verification_status(), BusinessPhase::Verifying);

        let identity = crate::model::OnuIdentity::default();
        assert_eq!(select_with_serial("ZTEG", &identity, None), Adapter::Unavailable);

        std::env::set_var("FTTR_ALLOW_UNSUPPORTED_AS_BRIDGE", "1");
        assert_eq!(select_with_serial("ZTEG", &identity, None), Adapter::GenericBridge);
        std::env::remove_var("FTTR_ALLOW_UNSUPPORTED_AS_BRIDGE");
    }
}
