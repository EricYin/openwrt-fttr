use super::connection_request_http::{self, Outcome, Target};
use super::{init, now, Server, TaskState};
use crate::provisioning::model::ConnectionRequest;
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::Once;
use std::time::{Duration, Instant};

const COOLDOWN: Duration = Duration::from_secs(30);
static START: Once = Once::new();

#[derive(Clone, Serialize)]
pub(super) struct Status {
    attempted_at: u64,
    finished_at: Option<u64>,
    #[serde(flatten)]
    outcome: Outcome,
}

#[derive(Default)]
pub(super) struct Worker {
    attempts: BTreeMap<String, Instant>,
}

impl Worker {
    #[cfg(test)]
    pub(super) fn poll(
        &mut self,
        server: &Server,
        configs: &BTreeMap<String, ConnectionRequest>,
        at: Instant,
    ) {
        crate::runtime::block_on(self.poll_async(server, configs, at));
    }

    async fn poll_async(
        &mut self,
        server: &Server,
        configs: &BTreeMap<String, ConnectionRequest>,
        at: Instant,
    ) {
        if !server.enabled || server.stopped.load(std::sync::atomic::Ordering::Acquire) {
            return;
        }
        let job = {
            let mut state = server.state.lock().unwrap();
            if state.load_failure.is_some() {
                return;
            }
            state.expire();
            self.attempts.retain(|id, _| state.devices.contains_key(id));
            state
                .connection_requests
                .retain(|id, _| configs.get(id).is_some_and(|c| c.enabled));
            let candidate = configs
                .iter()
                .filter(|(id, config)| {
                    config.enabled
                        && state.devices.contains_key(*id)
                        && self
                            .attempts
                            .get(*id)
                            .is_none_or(|last| at.duration_since(*last) >= COOLDOWN)
                        && state
                            .tasks
                            .values()
                            .any(|t| t.device == **id && t.state == TaskState::Queued)
                        && !state
                            .tasks
                            .values()
                            .any(|t| t.device == **id && t.state == TaskState::Sent)
                })
                .min_by_key(|(id, _)| self.attempts.get(*id).copied());
            let Some((id, config)) = candidate else {
                return;
            };
            let device = &state.devices[id];
            let target = device.inform["parameters"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|p| {
                    matches!(
                        p["name"].as_str(),
                        Some(
                            "InternetGatewayDevice.ManagementServer.ConnectionRequestURL"
                                | "Device.ManagementServer.ConnectionRequestURL"
                        )
                    )
                })
                .and_then(|p| p["value"].as_str())
                .ok_or(Outcome::InvalidUrl)
                .and_then(|url| Target::parse(url, &device.peer));
            self.attempts.insert(id.clone(), at);
            if let Some(proof) =
                super::connection_request_proof::Proof::new(&state.devices[id], config)
            {
                state.connection_request_proofs.insert(id.clone(), proof);
            } else {
                state.connection_request_proofs.remove(id);
            }
            state.connection_requests.insert(
                id.clone(),
                Status {
                    attempted_at: now(),
                    finished_at: None,
                    outcome: Outcome::Requesting,
                },
            );
            (id.clone(), config.clone(), target)
        };
        let (id, credentials, target) = job;
        let outcome = match target {
            Ok(target) => connection_request_http::request_async(&target, &credentials).await,
            Err(outcome) => outcome,
        };
        let mut state = server.state.lock().unwrap();
        if let Some(proof) = state.connection_request_proofs.get_mut(&id) {
            proof.authenticated(matches!(outcome, Outcome::Accepted { .. }));
        }
        if let Some(status) = state.connection_requests.get_mut(&id) {
            status.finished_at = Some(now());
            status.outcome = outcome;
        }
    }
}

pub(super) fn start() {
    START.call_once(|| {
        crate::runtime::spawn_service("ACS connection requests", async {
            let mut worker = Worker::default();
            while !crate::runtime::is_stopping() {
                let Ok((server, configs)) = crate::runtime::spawn_blocking(|| {
                    let configs = crate::provisioning::connection_request_devices();
                    let server = init();
                    super::periodic_policy::poll(&server, &configs);
                    (server, configs)
                })
                .await
                else {
                    break;
                };
                worker.poll_async(&server, &configs, Instant::now()).await;
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        });
    });
}
