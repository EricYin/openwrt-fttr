//! Bounded in-memory diagnostics, independent of ubus connections.
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    io::{self, Write},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const CHUNK: usize = 32768;
const MAX_BYTES: usize = 4 * 1024 * 1024;
const TOTAL_BYTES: usize = 16 * 1024 * 1024;
const TTL: Duration = Duration::from_secs(600);

#[derive(Clone)]
pub struct Jobs(Arc<Mutex<State>>);
struct State {
    jobs: BTreeMap<String, Job>,
    running: usize,
    sequence: u64,
    epoch: u128,
}
struct Job {
    kind: &'static str,
    created: Instant,
    result: Option<Vec<u8>>,
    released: bool,
}

pub fn error(message: &str) -> Value {
    json!({"error":message})
}

impl Default for Jobs {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(State {
            jobs: BTreeMap::new(),
            running: 0,
            sequence: 0,
            epoch: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
        })))
    }
}

impl Jobs {
    pub fn start(
        &self,
        kind: &'static str,
        work: impl FnOnce() -> Value + Send + 'static,
    ) -> Value {
        let Ok(mut state) = self.0.try_lock() else {
            return error("Job queue busy");
        };
        state.jobs.retain(|_, job| {
            job.result.is_none() || (!job.released && job.created.elapsed() < TTL)
        });
        if state.running >= 4 || state.jobs.len() >= 32 {
            return error("Job capacity reached");
        }
        state.sequence += 1;
        let id = format!("{:x}-{:x}", state.epoch, state.sequence);
        state.jobs.insert(
            id.clone(),
            Job {
                kind,
                created: Instant::now(),
                result: None,
                released: false,
            },
        );
        state.running += 1;
        drop(state);
        let jobs = self.clone();
        let worker_id = id.clone();
        let spawn = std::thread::Builder::new()
            .name(format!("fttr-{kind}"))
            .spawn(move || {
                let value = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work))
                    .unwrap_or_else(|_| error("Worker stopped; execution outcome is unknown"));
                let mut output = Limited(Vec::new());
                if serde_json::to_writer(&mut output, &value).is_err() {
                    output.0 = error("Job result exceeds transport limit")
                        .to_string()
                        .into_bytes();
                }
                if let Ok(mut state) = jobs.0.lock() {
                    state.running -= 1;
                    let total: usize = state
                        .jobs
                        .values()
                        .filter_map(|job| job.result.as_ref())
                        .map(Vec::len)
                        .sum();
                    // Reserve space for bounded error envelopes for every slot.
                    if total + output.0.len() > TOTAL_BYTES - 4096 {
                        output.0 = error("Job result storage full").to_string().into_bytes();
                    }
                    if let Some(job) = state.jobs.get_mut(&worker_id) {
                        job.result = Some(output.0);
                    }
                    state.jobs.retain(|_, job| {
                        job.result.is_none() || (!job.released && job.created.elapsed() < TTL)
                    });
                }
            });
        if spawn.is_err() {
            if let Ok(mut state) = self.0.lock() {
                state.jobs.remove(&id);
                state.running -= 1;
            }
            return error("Cannot start job worker");
        }
        json!({"status":"pending","job":id})
    }

    pub fn result(&self, id: &str, chunk: u16, kind: Option<&str>) -> Value {
        let Ok(mut state) = self.0.try_lock() else {
            return error("Job queue busy");
        };
        let Some(job) = state.jobs.get(id) else {
            return error("Job expired");
        };
        if job.released || job.created.elapsed() >= TTL {
            if job.result.is_some() {
                state.jobs.remove(id);
            }
            return error("Job expired");
        }
        if kind.is_some_and(|kind| kind != job.kind) {
            return error("Invalid job type");
        }
        let Some(bytes) = &job.result else {
            return json!({"status":"pending"});
        };
        let chunks = bytes.len().div_ceil(CHUNK);
        let offset = usize::from(chunk) * CHUNK;
        if offset >= bytes.len() {
            return error("Invalid result chunk");
        }
        json!({"status":"ready","chunk":chunk,"chunks":chunks,"bytes":bytes.len(),"data":STANDARD.encode(&bytes[offset..bytes.len().min(offset + CHUNK)])})
    }

    // Release discards results; it does not retry or cancel side effects.
    pub fn release(&self, id: &str) -> Value {
        let Ok(mut state) = self.0.try_lock() else {
            return error("Job queue busy");
        };
        if let Some(job) = state.jobs.get_mut(id) {
            job.released = true;
            if job.result.is_some() {
                state.jobs.remove(id);
            }
        }
        json!({"released":true})
    }
}

struct Limited(Vec<u8>);
impl Write for Limited {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.0.len() + bytes.len() > MAX_BYTES {
            return Err(io::Error::other("result limit"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn chunks_are_stable_and_worker_panic_is_not_success() {
        let jobs = Jobs::default();
        let value = json!({"text":"x".repeat(CHUNK+50)});
        let expected = value.clone();
        let id = jobs.start("test", move || value)["job"]
            .as_str()
            .unwrap()
            .to_owned();
        for _ in 0..1000 {
            if jobs.result(&id, 0, None)["status"] == "ready" {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        let first = jobs.result(&id, 0, None);
        let second = jobs.result(&id, 1, None);
        assert_eq!(first["chunks"], 2);
        assert_eq!(first["bytes"], second["bytes"]);
        let mut bytes = STANDARD.decode(first["data"].as_str().unwrap()).unwrap();
        bytes.extend(STANDARD.decode(second["data"].as_str().unwrap()).unwrap());
        assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap(), expected);
        let id = jobs.start("test", || panic!("injected worker failure"))["job"]
            .as_str()
            .unwrap()
            .to_owned();
        for _ in 0..1000 {
            if jobs.result(&id, 0, None)["status"] == "ready" {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        let reply = jobs.result(&id, 0, None);
        let result: Value =
            serde_json::from_slice(&STANDARD.decode(reply["data"].as_str().unwrap()).unwrap())
                .unwrap();
        assert!(result["error"].as_str().unwrap().contains("unknown"));
    }
    #[test]
    fn admission_is_bounded_and_release_does_not_cancel_workers() {
        let jobs = Jobs::default();
        let barrier = Arc::new(std::sync::Barrier::new(5));
        let mut ids = Vec::new();
        for _ in 0..4 {
            let barrier = barrier.clone();
            ids.push(
                jobs.start("test", move || {
                    barrier.wait();
                    json!({"done":true})
                })["job"]
                    .as_str()
                    .unwrap()
                    .to_owned(),
            );
        }
        assert!(jobs.start("test", || Value::Null).get("error").is_some());
        assert_eq!(jobs.result(&ids[0], 0, None)["status"], "pending");
        jobs.release(&ids[0]);
        barrier.wait();
        for _ in 0..100 {
            if jobs.0.lock().unwrap().running == 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(jobs.0.lock().unwrap().running, 0);
        assert!(jobs.result(&ids[0], 0, None).get("error").is_some());
        assert!(jobs
            .result(&ids[1], 0, Some("other"))
            .get("error")
            .is_some());
        let result = jobs.result(&ids[1], 0, None);
        let decoded = STANDARD.decode(result["data"].as_str().unwrap()).unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&decoded).unwrap(),
            json!({"done":true})
        );
        assert!(jobs.result(&ids[1], 1, None).get("error").is_some());
    }
    #[test]
    fn result_size_and_expiry_are_enforced() {
        let mut limited = Limited(Vec::new());
        assert!(limited.write_all(&vec![0; MAX_BYTES + 1]).is_err());
        let jobs = Jobs::default();
        jobs.0.lock().unwrap().jobs.insert(
            "expired".into(),
            Job {
                kind: "test",
                created: Instant::now() - TTL,
                result: Some(vec![0]),
                released: false,
            },
        );
        assert!(jobs.result("expired", 0, None).get("error").is_some());
        assert!(jobs.0.lock().unwrap().jobs.is_empty());
    }
}
