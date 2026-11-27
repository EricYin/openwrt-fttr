//! Single-owner, bounded process-restart checkpoint in tmpfs.
use super::storage;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    io::{self, Read},
    sync::{Mutex, OnceLock},
};

pub(crate) const PREFIX: &str = "/tmp/fttr/state/";
pub(crate) const FILE: &str = "/tmp/fttr/state.json";
/// All runtime checkpoint namespaces are declared here, never at call sites.
#[derive(Clone, Copy)]
pub(crate) enum Record {
    Recovery,
    Bootstrap,
    Management,
    Native,
    External,
    Wan,
    Wifi,
    Reset,
    Admission,
    Acs,
}
impl Record {
    pub(crate) fn path(self, id: &str) -> String {
        let name = match self {
            Self::Recovery => "recovery",
            Self::Bootstrap => "bootstrap",
            Self::Management => "management",
            Self::Native => "command-owned",
            Self::External => "command-external",
            Self::Wan => "wan-owned",
            Self::Wifi => "wifi-owned",
            Self::Reset => "reset-pending",
            Self::Admission => "admission",
            Self::Acs => "acs",
        };
        let directory = format!("{PREFIX}{name}");
        if id.is_empty() {
            #[cfg(test)]
            let directory = storage::test_io::registry_directory(&directory);
            format!("{directory}.json")
        } else if matches!(self, Self::Native) {
            #[cfg(test)]
            let directory = storage::test_io::registry_directory(&directory);
            format!("{directory}/{id}.json")
        } else {
            storage::registry_path(&directory, id)
        }
    }
}
pub(crate) const LEGACY_HISTORY: &str = "/tmp/fttr/provisioning-state.json";
pub(crate) fn approved(serial: &str) -> bool {
    storage::read(Record::Admission.path(serial))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .is_some_and(|value| value["serial"] == serial && value["approved"] == true)
}
pub(crate) fn approve(serial: &str) -> io::Result<()> {
    storage::write_atomic(
        &Record::Admission.path(serial),
        &serde_json::to_vec(&serde_json::json!({"serial":serial,"approved":true}))
            .map_err(io::Error::other)?,
        true,
    )
}
const LIMIT: usize = 8 * 1024 * 1024;
const RECORDS: usize = 2048;

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Checkpoint {
    schema: u32,
    #[serde(deserialize_with = "unique_records")]
    records: BTreeMap<String, Value>,
}
fn unique_records<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<BTreeMap<String, Value>, D::Error> {
    struct Records;
    impl<'de> serde::de::Visitor<'de> for Records {
        type Value = BTreeMap<String, Value>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("unique bounded runtime records")
        }
        fn visit_map<M: serde::de::MapAccess<'de>>(
            self,
            mut map: M,
        ) -> Result<Self::Value, M::Error> {
            let mut records = BTreeMap::new();
            while let Some((key, value)) = map.next_entry::<String, Value>()? {
                if !valid(&key) || records.len() >= RECORDS || records.insert(key, value).is_some()
                {
                    return Err(serde::de::Error::custom(
                        "invalid, duplicate or excessive runtime records",
                    ));
                }
            }
            Ok(records)
        }
    }
    d.deserialize_map(Records)
}
struct Store {
    file: String,
    cached: Option<(Stamp, Checkpoint)>,
    acknowledged: bool,
}
#[derive(PartialEq, Eq)]
struct Stamp(u64, u64, u64, i64, i64);
impl Store {
    fn new(file: String) -> Self {
        Self {
            file,
            cached: None,
            acknowledged: false,
        }
    }
    fn stamp(&self) -> io::Result<Option<Stamp>> {
        use std::os::unix::fs::MetadataExt;
        match std::fs::metadata(&self.file) {
            Ok(m) => Ok(Some(Stamp(
                m.dev(),
                m.ino(),
                m.len(),
                m.mtime(),
                m.mtime_nsec(),
            ))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }
    fn current(&mut self) -> io::Result<&Checkpoint> {
        let stamp = self.stamp()?;
        if !self
            .cached
            .as_ref()
            .is_some_and(|(old, _)| Some(old) == stamp.as_ref())
        {
            self.cached = None;
            self.acknowledged = false;
            let state = self.load()?;
            // Missing tmp state is never cached as an approval or old intent.
            if let Some(stamp) = stamp {
                self.cached = Some((stamp, state));
            } else {
                return Err(io::Error::from(io::ErrorKind::NotFound));
            }
        }
        Ok(&self.cached.as_ref().unwrap().1)
    }
}
impl Store {
    fn load(&self) -> io::Result<Checkpoint> {
        let file = match std::fs::File::open(&self.file) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Ok(Checkpoint {
                    schema: 1,
                    ..Default::default()
                })
            }
            Err(e) => return Err(e),
        };
        let mut bytes = Vec::new();
        file.take((LIMIT + 1) as u64).read_to_end(&mut bytes)?;
        if bytes.len() > LIMIT {
            return Err(io::Error::other("runtime checkpoint exceeds limit"));
        }
        let state: Checkpoint = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
        if state.schema != 1
            || state.records.len() > RECORDS
            || state.records.keys().any(|key| !valid(key))
        {
            return Err(io::Error::other("invalid runtime checkpoint"));
        }
        Ok(state)
    }
    fn change(&mut self, key: &str, value: Option<Value>) -> io::Result<()> {
        if !valid(key) {
            return Err(io::Error::other("invalid runtime record key"));
        }
        let stamp = self.stamp()?;
        let acknowledged = self.acknowledged;
        self.acknowledged = false;
        let (mut state, acknowledged) = match self.cached.take() {
            Some((old, state)) if Some(&old) == stamp.as_ref() => (state, acknowledged),
            _ => (self.load()?, false),
        };
        if state.records.get(key) == value.as_ref() && (acknowledged || stamp.is_none()) {
            if let Some(stamp) = stamp {
                self.cached = Some((stamp, state));
                self.acknowledged = acknowledged;
            }
            return Ok(());
        }
        match value {
            Some(value) => {
                state.records.insert(key.into(), value);
            }
            None => {
                state.records.remove(key);
            }
        }
        if state.records.len() > RECORDS {
            return Err(io::Error::other("too many runtime records"));
        }
        let bytes = serde_json::to_vec(&state).map_err(io::Error::other)?;
        if bytes.len() > LIMIT {
            return Err(io::Error::other("runtime checkpoint exceeds limit"));
        }
        // On any failure, including rename followed by directory-sync failure,
        // leave the cache invalid and re-read the actual checkpoint next time.
        storage::write_atomic(&self.file, &bytes, true)?;
        if let Some(stamp) = self.stamp()? {
            self.cached = Some((stamp, state));
            self.acknowledged = true;
        }
        Ok(())
    }
}
fn valid(key: &str) -> bool {
    !key.is_empty() && key.len() <= 256 && !key.contains("..") && !key.starts_with('/')
}
fn store() -> &'static Mutex<Store> {
    static STORE: OnceLock<Mutex<Store>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(Store::new(FILE.into())))
}
pub(crate) fn read(key: &str) -> io::Result<Vec<u8>> {
    let mut store = store()
        .lock()
        .map_err(|_| io::Error::other("runtime store unavailable"))?;
    let state = store.current()?;
    let value = state
        .records
        .get(key)
        .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
    serde_json::to_vec(value).map_err(io::Error::other)
}
pub(crate) fn write(key: &str, bytes: &[u8]) -> io::Result<()> {
    if bytes.len() > LIMIT {
        return Err(io::Error::other("runtime record exceeds limit"));
    }
    let value = serde_json::from_slice(bytes).map_err(io::Error::other)?;
    store()
        .lock()
        .map_err(|_| io::Error::other("runtime store unavailable"))?
        .change(key, Some(value))
}
pub(crate) fn remove(key: &str) -> io::Result<()> {
    store()
        .lock()
        .map_err(|_| io::Error::other("runtime store unavailable"))?
        .change(key, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_tmp_record_requires_new_explicit_write_admission() {
        let serial = "TEST00000083";
        assert!(!approved(serial));
        approve(serial).unwrap();
        assert!(approved(serial));
        storage::remove_registry(&Record::Admission.path(serial)).unwrap();
        assert!(!approved(serial));
    }
    #[test]
    fn restart_corruption_and_atomic_removal_preserve_unresolved_records() {
        let root = storage::test_io::registry_directory("/tmp/fttr/state-tests");
        let file = format!("{root}/state.json");
        let mut first = Store::new(file.clone());
        first
            .change(
                "command/TEST00000001",
                Some(serde_json::json!({"pending":true})),
            )
            .unwrap();
        first
            .change(
                "budgets/TEST00000002",
                Some(serde_json::json!({"attempts":5})),
            )
            .unwrap();
        let mut restarted = Store::new(file);
        assert_eq!(restarted.load().unwrap().records.len(), 2);
        storage::test_io::fail_next(storage::test_io::Point::Rename);
        assert!(restarted.change("command/TEST00000001", None).is_err());
        assert_eq!(
            restarted.load().unwrap().records["command/TEST00000001"]["pending"],
            true
        );
        restarted.change("budgets/TEST00000002", None).unwrap();
        assert_eq!(restarted.load().unwrap().records.len(), 1);
        std::fs::write(&restarted.file, b"broken").unwrap();
        assert!(restarted.change("new", Some(Value::Null)).is_err());
        assert_eq!(std::fs::read(&restarted.file).unwrap(), b"broken");
    }
    #[test]
    fn cached_reads_notice_file_loss_replacement_and_duplicate_records() {
        let root = storage::test_io::registry_directory("/tmp/fttr/cache-test");
        let mut store = Store::new(format!("{root}/state.json"));
        store
            .change("admission/test", Some(serde_json::json!({"approved":true})))
            .unwrap();
        let pointer = store.current().unwrap() as *const Checkpoint;
        assert_eq!(store.current().unwrap() as *const Checkpoint, pointer);
        std::fs::remove_file(&store.file).unwrap();
        assert!(store.current().is_err());
        assert!(store.cached.is_none());
        std::fs::write(
            &store.file,
            br#"{"schema":1,"records":{"same":{},"same":{}}}"#,
        )
        .unwrap();
        assert!(store.current().is_err());
        assert!(store.change("next", Some(Value::Null)).is_err());
        std::fs::write(&store.file, br#"{"schema":1,"records":{}}"#).unwrap();
        assert!(store.current().unwrap().records.is_empty());
    }
    #[test]
    fn unchanged_records_skip_io_only_after_ack_and_failed_barriers_are_retried() {
        let root = storage::test_io::registry_directory("/tmp/fttr/ack-test");
        let mut store = Store::new(format!("{root}/state.json"));
        let value = serde_json::json!({"pending":true});
        store.change("intent", Some(value.clone())).unwrap();
        storage::test_io::events();
        store.change("intent", Some(value.clone())).unwrap();
        assert!(storage::test_io::events().is_empty());
        let mut restarted = Store::new(store.file.clone());
        restarted.current().unwrap();
        storage::test_io::fail_next(storage::test_io::Point::DirectorySync);
        assert!(restarted.change("intent", Some(value.clone())).is_err());
        storage::test_io::fail_next(storage::test_io::Point::DirectorySync);
        assert!(restarted.change("intent", Some(value.clone())).is_err());
        restarted.change("intent", Some(value)).unwrap();
        assert!(restarted.acknowledged);
    }
}
