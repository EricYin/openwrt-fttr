//! Cache resolved evidence, not credentials or device values. Caller supplies a
//! registration/bootstrap epoch and a schema fingerprint from readonly discovery.
use super::{contract::Contract, identity::Identity, registry};

fn record_path(serial: &str) -> Option<std::path::PathBuf> {
    if serial.len() != 12 || !serial.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return None;
    }
    #[cfg(not(test))]
    let root = std::path::PathBuf::from("/var/lib/fttr/cwmp-contracts");
    #[cfg(test)]
    let root = std::env::temp_dir().join(format!("fttr-contract-test-{}", std::process::id()));
    Some(root.join(format!("{serial}.json")))
}
pub fn load_record(serial: &str) -> Option<serde_json::Value> {
    let path = record_path(serial)?;
    if std::fs::metadata(&path).ok()?.len() > 32768 {
        return None;
    }
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}
pub fn save_record(serial: &str, value: &serde_json::Value) -> Result<(), String> {
    let path = record_path(serial).ok_or("invalid cache identity")?;
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
    let temporary = path.with_extension("next");
    std::fs::write(
        &temporary,
        serde_json::to_vec(value).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    std::fs::rename(temporary, path).map_err(|e| e.to_string())
}
pub fn invalidate_record(serial: &str) {
    if let Some(path) = record_path(serial) {
        let _ = std::fs::remove_file(path);
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scope {
    pub identity: Identity,
    pub epoch: u64,
    pub schema: String,
    pub contract_version: u32,
}
pub struct CachedResolution {
    scope: Scope,
    contract: &'static str,
    invalidated: bool,
}
impl CachedResolution {
    pub fn new(scope: Scope, contract: &'static Contract) -> Self {
        Self {
            scope,
            contract: contract.id,
            invalidated: false,
        }
    }
    pub fn resolve(&self, scope: &Scope) -> Option<&'static Contract> {
        if self.invalidated
            || &self.scope != scope
            || scope.schema.is_empty()
            || scope.contract_version != registry::VERSION
        {
            return None;
        }
        registry::lookup(self.contract).ok()
    }
    /// Parameter faults and BOOTSTRAP events invalidate prior evidence.
    pub fn invalidate(&mut self) {
        self.invalidated = true;
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cache_never_crosses_identity_firmware_schema_or_bootstrap_epochs() {
        let mut scope = Scope {
            identity: Identity {
                optical_vendor: "HWTC".into(),
                optical_serial: "s1".into(),
                cwmp_serial: "c1".into(),
                product_class: "K155".into(),
                software_version: "v1".into(),
                hardware_version: "h1".into(),
            },
            epoch: 1,
            schema: "schema-1".into(),
            contract_version: registry::VERSION,
        };
        let mut cache = CachedResolution::new(scope.clone(), &super::super::vendor::hw::BASE);
        assert!(cache.resolve(&scope).is_some());
        scope.identity.software_version = "v2".into();
        assert!(cache.resolve(&scope).is_none());
        scope.identity.software_version = "v1".into();
        scope.epoch += 1;
        assert!(cache.resolve(&scope).is_none());
        scope.epoch = 1;
        scope.schema = "schema-2".into();
        assert!(cache.resolve(&scope).is_none());
        scope.schema = "schema-1".into();
        cache.invalidate();
        assert!(cache.resolve(&scope).is_none());
    }
}
