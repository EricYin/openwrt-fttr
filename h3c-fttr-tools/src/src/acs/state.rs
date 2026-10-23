//! CWMP task lifecycle and retention, independent of socket framing.
use super::{error_result, now, protect_management, soap, transaction, State, TaskState};
use std::io;

impl State {
    #[cfg(test)]
    pub(super) fn forget_tasks(&mut self, device: &str) {
        self.tasks.retain(|_, task| task.device != device);
        self.sessions.retain(|_, (id, _, _)| id != device);
    }

    pub(super) fn prune(&mut self) {
        let time = now();
        self.tasks.retain(|_, task| {
            task.state.is_active() || time.saturating_sub(task.sent.unwrap_or(task.created)) < 180
        });
        for task in self.tasks.values_mut() {
            if !task.state.is_active()
                && time.saturating_sub(task.sent.unwrap_or(task.created)) >= 15
            {
                task.result = None;
            }
        }
        let mut total_result_bytes: usize = self
            .tasks
            .values()
            .filter_map(|t| t.result.as_ref())
            .map(|r| r.get().len())
            .sum();
        if total_result_bytes > 256 * 1024 {
            let inactive_ids: Vec<u64> = self
                .tasks
                .iter()
                .filter(|(_, t)| !t.state.is_active() && t.result.is_some())
                .map(|(id, _)| *id)
                .collect();
            let max_drop = inactive_ids.len().saturating_sub(1);
            for id in inactive_ids.into_iter().take(max_drop) {
                if total_result_bytes <= 256 * 1024 {
                    break;
                }
                if let Some(task) = self.tasks.get_mut(&id) {
                    if let Some(res) = task.result.take() {
                        total_result_bytes = total_result_bytes.saturating_sub(res.get().len());
                    }
                }
            }
        }
    }

    pub(super) fn expire(&mut self) {
        let time = now();
        self.sessions
            .retain(|_, (_, _, at)| time.saturating_sub(*at) < 120);
        for t in self.tasks.values_mut() {
            if t.state == TaskState::Sent && time.saturating_sub(t.sent.unwrap_or(time)) >= 90 {
                t.state = TaskState::Unknown;
                t.result = Some(error_result(
                    "response timeout: device execution is unknown; no automatic write retry",
                ));
            } else if t.state == TaskState::Queued && time.saturating_sub(t.created) >= 600 {
                t.state = TaskState::Expired;
                t.result = Some(error_result("no device session within 600 seconds"));
            }
        }
        self.prune();
    }

    pub(super) fn next(
        &mut self,
        device: &str,
        namespace: &str,
    ) -> io::Result<Option<transaction::PreparedRpc>> {
        if self
            .tasks
            .values()
            .any(|t| t.device == device && t.state == TaskState::Sent)
        {
            return Ok(None);
        }
        if let Some(task) = self
            .tasks
            .values()
            .find(|t| t.device == device && t.state == TaskState::Queued)
        {
            let internal_interval = self
                .interval_authorizations
                .get(&task.id)
                .is_some_and(|grant| grant.permits(task, self));
            if let Err(reason) = if internal_interval {
                Ok(())
            } else {
                protect_management(self, device, &task.method, &task.args)
            } {
                let id = task.id;
                let task = self.tasks.get_mut(&id).unwrap();
                task.state = TaskState::Fault;
                task.result = Some(error_result(&reason));
                return Ok(None);
            }
        }
        if let Some(task) = self
            .tasks
            .values_mut()
            .find(|t| t.device == device && t.state == TaskState::Queued)
        {
            let xml = soap::request(namespace, task.id, &task.method, &task.args)?;
            let undo = transaction::DispatchUndo {
                id: task.id,
                sent: task.sent,
            };
            task.state = TaskState::Sent;
            task.sent = Some(now());
            return Ok(Some(transaction::PreparedRpc { xml, undo }));
        }
        Ok(None)
    }

    pub(super) fn response(&mut self, device: &str, message: &soap::Message) -> io::Result<()> {
        let id = message
            .id
            .strip_prefix("fttr-")
            .and_then(|id| id.parse::<u64>().ok())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "missing/mismatched CWMP ID")
            })?;
        let task = self
            .tasks
            .get_mut(&id)
            .filter(|t| t.device == device && t.state == TaskState::Sent)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "unexpected CWMP response")
            })?;
        if message.method == "Fault" {
            task.state = TaskState::Fault;
        } else if message.method == format!("{}Response", task.method) {
            if matches!(
                task.method.as_str(),
                "SetParameterValues" | "AddObject" | "DeleteObject"
            ) && !matches!(message.data["status"].as_str(), Some("0" | "1"))
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid SetParameterValues Status",
                ));
            }
            if task.method == "AddObject"
                && message.data["instance_number"]
                    .as_str()
                    .and_then(|s| s.parse::<u32>().ok())
                    .is_none_or(|n| n == 0)
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid AddObject InstanceNumber",
                ));
            }
            task.state = if matches!(
                task.method.as_str(),
                "SetParameterValues" | "AddObject" | "DeleteObject"
            ) && message.data["status"] == "1"
            {
                TaskState::AcceptedPendingApply
            } else {
                TaskState::Completed
            };
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "RPC response method mismatch",
            ));
        }
        task.result = Some(serde_json::value::to_raw_value(&message.data)?);
        self.prune();
        Ok(())
    }
}
