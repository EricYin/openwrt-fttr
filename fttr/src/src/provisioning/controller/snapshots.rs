//! Durable per-device recovery boundary. The old history remains a UI cache.
use super::*;
use crate::provisioning::storage;
use serde::{Deserialize, Serialize};
use std::io::{self, Read};

const DIRECTORY: &str = "/etc/fttr/provisioning-snapshots";
const MAX_DEVICES: usize = 256; // Includes retained orphan transactions.
const MAX_BYTES: usize = 128 * 1024;
const LEGACY_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReadRecovery {
    pub attempts: u32,
    pub retry_at: u64,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    schema: u32,
    device: String,
    serial: String,
    cwmp_id: String,
    operation: Option<Value>,
    owned: BTreeMap<String, wan::Owned>,
    attempts: u32,
    retry_at: u64,
    reasons: scheduler::Reasons,
    read: ReadRecovery,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Index {
    schema: u32,
    devices: BTreeSet<String>,
}

pub(super) struct Store {
    directory: String,
    frames: BTreeMap<String, Snapshot>,
    acknowledged: BTreeMap<String, Vec<u8>>,
    error: Option<String>,
    load_error: bool,
}

fn read_bounded(path: &str, max: usize) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take((max + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > max {
        return Err(io::Error::other("recovery state exceeds size limit"));
    }
    Ok(bytes)
}
fn invalid() -> io::Error {
    io::Error::other("invalid recovery schema, identity, or budget")
}
impl Snapshot {
    fn validate(&self) -> io::Result<()> {
        if self.schema != 1
            || self.device.is_empty()
            || self.serial.is_empty()
            || self.attempts > 5
            || self.read.attempts > 3
            || self.reasons.0 & !127 != 0
        {
            return Err(invalid());
        }
        if let Some(op) = &self.operation {
            if op["device"] != self.device
                || op["optical_sn"] != self.serial
                || op["operation_id"].as_u64().is_none()
                || !matches!(op["mode"].as_str(), Some("plan" | "apply"))
                || serde_json::from_value::<OperationPhase>(op["state"].clone()).is_err()
                || !op["items"].is_array()
            {
                return Err(invalid());
            }
            for item in op["items"].as_array().unwrap() {
                // Active operations also contain phase/pending placeholders. An
                // unrecognized state must not become an apparently resolved
                // owner just because the history decoder ignores that state.
                if item["state"] != "pending"
                    && ItemOutcome::from_result(item).is_none()
                    && serde_json::from_value::<OperationPhase>(item["state"].clone()).is_err()
                {
                    return Err(invalid());
                }
            }
        }
        Ok(())
    }
}
impl Store {
    fn new(directory: String) -> Self {
        Self {
            directory,
            frames: BTreeMap::new(),
            acknowledged: BTreeMap::new(),
            error: None,
            load_error: false,
        }
    }
    pub(super) fn blocked(&self) -> bool {
        self.error.is_some()
    }
    pub(super) fn view(&self) -> Value {
        json!({"schema":1,"state":if self.blocked() {"blocked"} else {"durable"},"error":self.error,"devices":self.frames.len(),"per_device_limit":MAX_BYTES})
    }
    fn index_path(&self) -> String {
        format!("{}/index.json", self.directory)
    }
    fn path(&self, id: &str) -> String {
        storage::registry_path(&self.directory, id)
    }
    fn load(&mut self) -> io::Result<bool> {
        let bytes = match read_bounded(&self.index_path(), MAX_BYTES) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                match std::fs::read_dir(&self.directory) {
                    Ok(entries) => {
                        for entry in entries {
                            if entry?.path().extension().is_some_and(|s| s == "json") {
                                return Err(invalid());
                            }
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::NotFound => (),
                    Err(e) => return Err(e),
                }
                return Ok(false);
            }
            Err(e) => return Err(e),
        };
        let index: Index = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
        if index.schema != 1 || index.devices.len() > MAX_DEVICES {
            return Err(invalid());
        }
        // Include newly renamed files from an interrupted index update. Missing
        // indexed files are corruption, never evidence that an owner disappeared.
        let mut files = BTreeSet::new();
        for entry in std::fs::read_dir(&self.directory)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name == "index.json" || name.ends_with(".tmp") {
                continue;
            }
            if !entry.file_type()?.is_file() || !name.ends_with(".json") {
                return Err(invalid());
            }
            files.insert(entry.path().to_string_lossy().into_owned());
            if files.len() > MAX_DEVICES {
                return Err(invalid());
            }
        }
        for id in &index.devices {
            if !files.contains(&self.path(id)) {
                return Err(invalid());
            }
        }
        let mut frames = BTreeMap::new();
        for file in files {
            let bytes = read_bounded(&file, MAX_BYTES)?;
            let frame: Snapshot = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
            frame.validate()?;
            if file != self.path(&frame.device)
                || frames.insert(frame.device.clone(), frame).is_some()
            {
                return Err(invalid());
            }
            self.acknowledged.insert(file, bytes);
        }
        self.frames = frames;
        // Re-establish file/directory barriers even when the previous process
        // saw the rename but never acknowledged its durability.
        self.acknowledged.clear();
        Ok(true)
    }
    fn save(&mut self, mut write: impl FnMut(&str, &[u8]) -> io::Result<()>) -> io::Result<()> {
        let result = self.save_inner(&mut write);
        if result.is_err() {
            self.acknowledged.clear();
        }
        result
    }
    fn save_inner(
        &mut self,
        mut write: impl FnMut(&str, &[u8]) -> io::Result<()>,
    ) -> io::Result<()> {
        if self.load_error {
            return Err(io::Error::other(
                "recovery load failed; operator repair required",
            ));
        }
        if self.frames.len() > MAX_DEVICES {
            return Err(invalid());
        }
        if self
            .frames
            .keys()
            .map(|id| self.path(id))
            .collect::<BTreeSet<_>>()
            .len()
            != self.frames.len()
        {
            return Err(invalid());
        }
        for frame in self.frames.values() {
            frame.validate()?;
            let bytes = serde_json::to_vec(frame)?;
            if bytes.len() > MAX_BYTES {
                return Err(invalid());
            }
            let path = self.path(&frame.device);
            if self.acknowledged.get(&path) != Some(&bytes) {
                write(&path, &bytes)?;
                self.acknowledged.insert(path, bytes);
            }
        }
        let bytes = serde_json::to_vec(&Index {
            schema: 1,
            devices: self.frames.keys().cloned().collect(),
        })?;
        let path = self.index_path();
        if self.acknowledged.get(&path) != Some(&bytes) {
            write(&path, &bytes)?;
            self.acknowledged.insert(path, bytes);
        }
        Ok(())
    }
}

impl Controller {
    fn restore_frames(&mut self, store: &Store) {
        let operations: Vec<_> = store
            .frames
            .values()
            .filter_map(|f| f.operation.clone())
            .collect();
        let owned: BTreeMap<_, _> = store
            .frames
            .values()
            .filter(|f| !f.owned.is_empty())
            .map(|f| (f.cwmp_id.clone(), f.owned.clone()))
            .collect();
        self.recover(
            json!({"operations": operations, "owned": owned})
                .to_string()
                .as_bytes(),
        );
        for frame in store.frames.values() {
            self.attempts.insert(frame.device.clone(), frame.attempts);
            self.next_retry_at
                .insert(frame.device.clone(), frame.retry_at);
            self.automatic_ready
                .insert(frame.device.clone(), frame.reasons);
            self.read_recovery
                .insert(frame.device.clone(), frame.read.clone());
        }
    }
    pub(super) fn load_snapshots(&mut self) {
        let mut store = Store::new(DIRECTORY.into());
        match store.load() {
            Ok(true) => {
                self.restore_frames(&store);
            }
            Ok(false) => {
                let result =
                    match read_bounded("/var/lib/fttr/provisioning-state.json", LEGACY_BYTES) {
                        Ok(bytes) => {
                            // Validate before using the legacy decoder, which historically
                            // ignored malformed input. Do not silently migrate empty state.
                            serde_json::from_slice::<Value>(&bytes)
                                .map_err(|_| invalid())
                                .and_then(|v| {
                                    let ops = v["operations"].as_array().ok_or_else(invalid)?;
                                    if ops.len() > MAX_DEVICES || !v["owned"].is_object() {
                                        return Err(invalid());
                                    }
                                    for op in ops {
                                        if !op["device"].is_string()
                                            || !op["optical_sn"].is_string()
                                            || !op["items"].is_array()
                                        {
                                            return Err(invalid());
                                        }
                                    }
                                    self.recover(&bytes);
                                    Ok(())
                                })
                        }
                        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
                        Err(e) => Err(e),
                    };
                if let Err(e) = result {
                    store.error = Some(e.to_string());
                    store.load_error = true;
                }
            }
            Err(e) => {
                store.error = Some(e.to_string());
                store.load_error = true;
            }
        }
        self.snapshots = Some(store);
        self.persist_snapshots(); // Migration must be durable before program_start.
    }

    pub(super) fn persist_snapshots(&mut self) -> bool {
        let Some(mut store) = self.snapshots.take() else {
            return true;
        };
        let mut identities: BTreeMap<String, (String, String)> = self
            .config
            .devices
            .iter()
            .map(|d| (d.id.clone(), (d.optical_sn.clone(), d.cwmp_id.clone())))
            .collect();
        for (id, view) in &self.recovered {
            if let (Some(sn), Some(cwmp)) = (view["optical_sn"].as_str(), view["cwmp_id"].as_str())
            {
                identities.insert(id.clone(), (sn.into(), cwmp.into()));
            }
        }
        for (id, op) in &self.operations {
            identities.insert(
                id.clone(),
                (op.device.optical_sn.clone(), op.device.cwmp_id.clone()),
            );
        }
        for (id, (serial, cwmp_id)) in identities {
            let operation = self
                .operations
                .get(&id)
                .map(Operation::view)
                .or_else(|| self.recovered.get(&id).cloned());
            store.frames.insert(
                id.clone(),
                Snapshot {
                    schema: 1,
                    serial,
                    cwmp_id: cwmp_id.clone(),
                    operation,
                    owned: self.owned.get(&cwmp_id).cloned().unwrap_or_default(),
                    attempts: self.attempts.get(&id).copied().unwrap_or(0),
                    retry_at: self.next_retry_at.get(&id).copied().unwrap_or(0),
                    reasons: self.automatic_ready.get(&id).copied().unwrap_or_default(),
                    read: self.read_recovery.get(&id).cloned().unwrap_or_default(),
                    device: id,
                },
            );
        }
        let success = match store.save(|path, bytes| storage::write_atomic(path, bytes, true)) {
            Ok(()) => {
                store.error = None;
                true
            }
            Err(e) => {
                store.acknowledged.clear();
                store.error = Some(format!("durable recovery snapshot: {e}"));
                false
            }
        };
        if !success {
            self.failure = store.error.clone();
        }
        self.snapshots = Some(store);
        success
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use storage::test_io::{self, Point};

    fn store(name: &str) -> Store {
        Store::new(test_io::registry_directory(&format!("{DIRECTORY}/{name}")))
    }
    fn frame() -> Snapshot {
        let mut op = super::super::tests::operation(OperationPhase::Writing, true);
        op.id = 77;
        Snapshot {
            schema: 1,
            device: op.device.id.clone(),
            serial: op.device.optical_sn.clone(),
            cwmp_id: op.device.cwmp_id.clone(),
            operation: Some(op.view()),
            owned: BTreeMap::new(),
            attempts: 5,
            retry_at: 900,
            reasons: scheduler::Reasons(3),
            read: ReadRecovery {
                attempts: 2,
                retry_at: 800,
            },
        }
    }
    fn write(path: &str, bytes: &[u8]) -> io::Result<()> {
        storage::write_atomic(path, bytes, true)
    }

    #[test]
    fn restart_retains_exhausted_budget_original_owner_and_interrupted_intent() {
        let mut s = store("restart");
        let f = frame();
        s.frames.insert(f.device.clone(), f.clone());
        s.save(write).unwrap();
        let mut loaded = Store::new(s.directory.clone());
        assert!(loaded.load().unwrap());
        assert!(
            loaded.acknowledged.is_empty(),
            "restart re-establishes durability barriers"
        );
        let mut c = Controller::default();
        c.restore_frames(&loaded); // No configured device: orphan still survives.
        assert_eq!(c.attempts[&f.device], 5);
        assert_eq!(c.next_retry_at[&f.device], 900);
        assert_eq!(c.read_recovery[&f.device].attempts, 2);
        assert_eq!(c.read_recovery[&f.device].retry_at, 800);
        assert!(c.previous_facts(&f.device).unwrap().unresolved);
        assert_eq!(c.recovered[&f.device]["operation_id"], 77);
        assert!(c.next_id >= 77);
    }

    #[test]
    fn failed_barrier_cannot_be_acknowledged_by_identical_content_or_reversion() {
        for point in [Point::FileSync, Point::Rename, Point::DirectorySync] {
            let mut s = store(&format!("barrier-{point:?}"));
            let f = frame();
            s.frames.insert(f.device.clone(), f.clone());
            test_io::fail_next(point);
            assert!(s.save(write).is_err());
            assert!(s.acknowledged.is_empty());
            test_io::fail_next(point);
            assert!(s.save(write).is_err());
            s.save(write).unwrap();
            s.frames.get_mut(&f.device).unwrap().attempts = 4;
            test_io::fail_next(Point::DirectorySync);
            assert!(s.save(write).is_err());
            s.frames.get_mut(&f.device).unwrap().attempts = 5;
            let mut writes = 0;
            s.save(|path, bytes| {
                writes += 1;
                write(path, bytes)
            })
            .unwrap();
            assert!(
                writes > 0,
                "old ack was invalidated after failed changed snapshot"
            );
        }
    }

    #[test]
    fn missing_schema_identity_budget_and_size_evidence_fail_closed() {
        for mode in 0..9 {
            let mut s = store(&format!("invalid-{mode}"));
            let f = frame();
            s.frames.insert(f.device.clone(), f.clone());
            s.save(write).unwrap();
            let path = s.path(&f.device);
            match mode {
                0 => storage::remove_registry(&path).unwrap(),
                1 => {
                    let mut f = f.clone();
                    f.schema = 2;
                    write(&path, &serde_json::to_vec(&f).unwrap()).unwrap();
                }
                2 => {
                    let mut f = f.clone();
                    f.attempts = 6;
                    write(&path, &serde_json::to_vec(&f).unwrap()).unwrap();
                }
                3 => {
                    let mut f = f.clone();
                    f.operation.as_mut().unwrap()["optical_sn"] = json!("TEST_OTHER");
                    write(&path, &serde_json::to_vec(&f).unwrap()).unwrap();
                }
                4 => write(&path, &vec![b' '; MAX_BYTES + 1]).unwrap(),
                5 => write(&path, b"{corrupt").unwrap(),
                6 => storage::remove_registry(&s.index_path()).unwrap(),
                7 | 8 => {
                    let mut f = f.clone();
                    if mode == 7 {
                        f.operation.as_mut().unwrap()["state"] = json!("unrecognized_phase");
                    } else {
                        f.operation.as_mut().unwrap()["items"][0]["state"] =
                            json!("unrecognized_outcome");
                    }
                    write(&path, &serde_json::to_vec(&f).unwrap()).unwrap();
                }
                _ => unreachable!(),
            }
            assert!(
                Store::new(s.directory.clone()).load().is_err(),
                "mode {mode}"
            );
        }
    }

    #[test]
    fn unindexed_rename_and_removed_configuration_cannot_drop_an_orphan() {
        let mut s = store("orphan");
        s.save(write).unwrap();
        let f = frame();
        write(&s.path(&f.device), &serde_json::to_vec(&f).unwrap()).unwrap();
        assert!(s.load().unwrap());
        assert_eq!(s.frames.len(), 1);
        let mut c = Controller::default();
        c.restore_frames(&s);
        c.snapshots = Some(s);
        assert!(c.persist_snapshots());
        assert_eq!(c.snapshots.as_ref().unwrap().frames.len(), 1);
    }

    #[test]
    fn corrupt_load_and_failed_save_never_admit_a_new_operation() {
        let mut c = Controller::default();
        let op = super::super::tests::operation(OperationPhase::Completed, false);
        let id = op.device.id.clone();
        c.config.devices.push(op.device);
        let mut s = store("blocked");
        s.load_error = true;
        s.error = Some("invalid snapshot".into());
        c.snapshots = Some(s);
        assert!(!c.persist_snapshots());
        assert!(c
            .start(&id, false, false)
            .unwrap_err()
            .contains("durable recovery state"));
        assert!(!c.persist_snapshots());
        assert!(!std::path::Path::new(&c.snapshots.as_ref().unwrap().index_path()).exists());
    }
}
