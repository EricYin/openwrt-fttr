use super::{error, io, json, Server, State, Task, TaskState, Value};

pub(super) struct PreparedRpc {
    pub(super) xml: String,
    pub(super) undo: DispatchUndo,
}

pub(super) struct DispatchUndo {
    pub(super) id: u64,
    pub(super) sent: Option<u64>,
}

impl DispatchUndo {
    pub(super) fn restore(self, state: &mut State) {
        #[expect(
            clippy::expect_used,
            reason = "the state lock prevents removal of the just-prepared task"
        )]
        let task = state
            .tasks
            .get_mut(&self.id)
            .expect("prepared task remains locked");
        task.state = TaskState::Queued;
        task.sent = self.sent;
    }
}

impl Server {
    pub(super) fn enqueue(&self, state: &mut State, task: Task) -> Value {
        let mut evicted = Vec::new();
        while state.tasks.len() >= 256 {
            let oldest = state
                .tasks
                .iter()
                .find(|(_, task)| !task.state.is_active())
                .map(|(id, _)| *id);
            let Some(id) = oldest else {
                state.tasks.extend(evicted);
                return error("ACS task history full");
            };
            if let Some(task) = state.tasks.remove(&id) {
                evicted.push((id, task));
            }
        }
        let previous_id = state.next_id;
        let id = task.id;
        state.next_id = id;
        state.tasks.insert(id, task);
        match self.save(state) {
            Ok(()) => json!(state.tasks[&id]),
            Err(err) => {
                state.tasks.remove(&id);
                state.tasks.extend(evicted);
                state.next_id = previous_id;
                error(&format!("cannot persist queued task: {err}"))
            }
        }
    }

    pub(super) fn reset_tasks(&self, state: &mut State, device: &str) -> io::Result<()> {
        let tasks: Vec<_> = state
            .tasks
            .extract_if(.., |_, task| task.device == device)
            .collect();
        let sessions: Vec<_> = state
            .sessions
            .extract_if(.., |_, (id, _, _)| id == device)
            .collect();
        match self.save(state) {
            Ok(()) => Ok(()),
            Err(err) => {
                state.tasks.extend(tasks);
                state.sessions.extend(sessions);
                Err(err)
            }
        }
    }
}
