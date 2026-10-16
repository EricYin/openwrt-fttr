use super::{error_result, Server, State, TaskState};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::os::unix::fs::OpenOptionsExt;

const MAX_STATE_BYTES: u64 = 8 * 1024 * 1024;

impl State {
    pub(super) fn load(path: &str) -> io::Result<Self> {
        let file = match File::open(path) {
            Ok(file) => file,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(err) => return Err(err),
        };
        if file.metadata()?.len() > MAX_STATE_BYTES {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "ACS state exceeds 8 MiB"));
        }
        let mut state: Self = serde_json::from_reader(BufReader::new(file.take(MAX_STATE_BYTES)))?;
        for task in state.tasks.values_mut().filter(|t| t.state == TaskState::Sent) {
            task.state = TaskState::Unknown;
            task.result = Some(error_result("daemon restarted after dispatch; execution is unknown, not automatically retried"));
        }
        state.next_id = state.next_id.max(state.tasks.keys().next_back().copied().unwrap_or(0));
        Ok(state)
    }

    pub(super) fn load_or_block(path: &str) -> Self {
        match Self::load(path) {
            Ok(state) => state,
            Err(err) => Self { load_failure: Some(format!("state load: {err}")), ..Self::default() },
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
            let file = OpenOptions::new().create(true).truncate(true).write(true).mode(0o600).open(&temp)?;
            let mut writer = BufWriter::new(file);
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
