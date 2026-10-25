use super::{error_result, Server, State, TaskState};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::os::unix::fs::OpenOptionsExt;

const MAX_STATE_BYTES: u64 = 8 * 1024 * 1024;

struct LimitedWriter<W> {
    inner: W,
    remaining: u64,
}
impl<W: Write> Write for LimitedWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() as u64 > self.remaining {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "ACS state exceeds 8 MiB",
            ));
        }
        let written = self.inner.write(bytes)?;
        self.remaining -= written as u64;
        Ok(written)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl State {
    pub(super) fn load(path: &str) -> io::Result<Self> {
        let file = match File::open(path) {
            Ok(file) => file,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(err) => return Err(err),
        };
        if file.metadata()?.len() > MAX_STATE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "ACS state exceeds 8 MiB",
            ));
        }
        let budget = std::sync::Arc::new(super::resources::Budget::default());
        let mut state: Self = super::resources::restoring(budget.clone(), || {
            serde_json::from_reader(BufReader::new(file.take(MAX_STATE_BYTES)))
        })?;
        state.budget = budget;
        for task in state
            .tasks
            .values_mut()
            .filter(|t| t.state == TaskState::Sent)
        {
            task.state = TaskState::Unknown;
            task.result = Some(error_result(
                "daemon restarted after dispatch; execution is unknown, not automatically retried",
            ));
        }
        for (id, task) in &state.tasks {
            if !task.state.is_active() {
                let info = state.results.entry(*id).or_default();
                info.completed_at.get_or_insert_with(super::now);
                if info.status.is_empty() {
                    info.status = "available".into();
                }
            }
        }
        state.next_id = state
            .next_id
            .max(state.tasks.keys().next_back().copied().unwrap_or(0));
        state.prune();
        for task in state.tasks.values_mut() {
            if let Some(response) = task.result.take() {
                if response.charged_already() {
                    task.result = Some(response);
                    continue;
                }
                let permit = state
                    .budget
                    .reserve(response.bytes(), super::resources::RESULT_BYTES)
                    .ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            "restored responses exceed memory budget",
                        )
                    })?;
                task.result = Some(response.with_charge(permit));
            }
        }
        Ok(state)
    }

    pub(super) fn load_or_block(path: &str) -> Self {
        match Self::load(path) {
            Ok(state) => state,
            Err(err) => Self {
                load_failure: Some(format!("state load: {err}")),
                ..Self::default()
            },
        }
    }
}

impl Server {
    pub(super) fn save(&self, state: &State) -> io::Result<()> {
        let result = (|| -> io::Result<()> {
            if let Some(reason) = &state.load_failure {
                return Err(io::Error::new(io::ErrorKind::InvalidData, reason.clone()));
            }
            let path = std::path::Path::new(&self.state_file);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            let temp = format!("{}.tmp", self.state_file);
            let file = OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .mode(0o600)
                .open(&temp)?;
            let mut writer = LimitedWriter {
                inner: BufWriter::new(file),
                remaining: MAX_STATE_BYTES,
            };
            serde_json::to_writer(&mut writer, state)?;
            writer.flush()?;
            fs::rename(temp, path)
        })();
        if let Err(err) = &result {
            *self.failure.lock().unwrap() = Some(format!("state persistence: {err}"));
        }
        result
    }
}
