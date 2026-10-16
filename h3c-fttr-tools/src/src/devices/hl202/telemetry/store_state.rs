use super::Report;
use serde::Serialize;

#[derive(Default, Serialize)]
pub struct State {
    pub revision: u64,
    pub last_received: u64,
    pub last_processed_receipt: Option<u64>,
    pub last_good: Option<u64>,
    pub error: Option<String>,
    pub report: Option<Report>,
}

impl State {
    pub(super) fn apply(&mut self, received: u64, result: Result<Report, String>) {
        self.revision = self.revision.wrapping_add(1);
        self.last_received = self.last_received.max(received);
        self.last_processed_receipt = Some(received);
        match result {
            Ok(report) => {
                self.last_good = Some(received);
                self.report = Some(report);
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
    }
}
