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
        if path.starts_with(crate::provisioning::runtime_store::PREFIX) {
            let bytes = match crate::provisioning::storage::read(path) {
                Ok(bytes) => bytes,
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
                Err(e) => return Err(e),
            };
            let mut state: Self = serde_json::from_slice(&bytes)?;
            for task in state.tasks.values_mut() {
                if !is_write(&task.method)
                    || !matches!(
                        task.state,
                        TaskState::Sent | TaskState::Unknown | TaskState::AcceptedPendingApply
                    )
                {
                    return Err(io::Error::other("invalid unresolved RPC checkpoint"));
                }
                task.state = TaskState::Unknown;
                task.args = serde_json::json!({"_restart_unresolved":true});
                task.result = Some(error_result(
                    "unresolved write before process restart; fresh observation does not settle it",
                ));
                state
                    .results
                    .insert(task.id, super::retention::ResultInfo::protected());
            }
            return Ok(state);
        }
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
            if self
                .state_file
                .starts_with(crate::provisioning::runtime_store::PREFIX)
            {
                let restart = restart_state(state);
                if restart.tasks.is_empty() {
                    return crate::provisioning::storage::remove_registry(&self.state_file)
                        .map_err(io::Error::other);
                }
                return crate::provisioning::storage::write_atomic(
                    &self.state_file,
                    &serde_json::to_vec(&restart)?,
                    true,
                );
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

fn is_write(method: &str) -> bool {
    matches!(
        method,
        "SetParameterValues"
            | "AddObject"
            | "DeleteObject"
            | "Reboot"
            | "FactoryReset"
            | "Download"
    )
}
fn restart_state(state: &State) -> State {
    let mut restart = State::default();
    for task in state.tasks.values().filter(|t| {
        is_write(&t.method)
            && matches!(
                t.state,
                TaskState::Sent | TaskState::Unknown | TaskState::AcceptedPendingApply
            )
    }) {
        restart.next_id = restart.next_id.max(task.id);
        restart.tasks.insert(
            task.id,
            super::Task {
                id: task.id,
                device: task.device.clone(),
                method: task.method.clone(),
                args: serde_json::json!({"_restart_unresolved":true}),
                state: task.state,
                created: task.created,
                sent: task.sent,
                result: None,
            },
        );
    }
    restart
}

#[cfg(test)]
mod checkpoint_tests {
    use super::*;
    #[test]
    fn restart_records_exclude_readbacks_credentials_queued_work_and_completed_history() {
        let mut state = State::default();
        for (id, method, phase) in [
            (1, "SetParameterValues", TaskState::Sent),
            (2, "GetParameterValues", TaskState::Sent),
            (3, "AddObject", TaskState::Queued),
            (4, "SetParameterValues", TaskState::Completed),
        ] {
            state.tasks.insert(
                id,
                super::super::Task {
                    id,
                    device: "TEST00000084".into(),
                    method: method.into(),
                    args: serde_json::json!({"password":"do-not-checkpoint"}),
                    state: phase,
                    created: 1,
                    sent: Some(2),
                    result: Some(error_result("do-not-checkpoint")),
                },
            );
        }
        let saved = restart_state(&state);
        assert_eq!(saved.tasks.len(), 1);
        assert_eq!(saved.tasks[&1].state, TaskState::Sent);
        assert!(!String::from_utf8(serde_json::to_vec(&saved).unwrap())
            .unwrap()
            .contains("do-not-checkpoint"));
    }
}
