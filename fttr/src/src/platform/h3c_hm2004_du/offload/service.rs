use super::reconciler::{self, Admission, Reconciler};
use std::sync::{Arc, Mutex};

/// Check if current hardware platform supports HM2004-DU NPU offload.
pub fn is_supported_platform() -> bool {
    crate::platform::context::PlatformContext::detect()
        .is_ok_and(|platform| platform.id() == crate::platform::adapter::PlatformId::H3cHm2004Du)
}

/// The daemon owns this guard; stopping and publishing a mask share one lock.
/// A poll suspended in a command cannot re-arm admission after stop().
pub struct Service(pub(crate) Arc<Mutex<Admission>>);

impl Service {
    pub(crate) fn start(platform: crate::platform::context::PlatformContext) -> Self {
        debug_assert_eq!(
            platform.id(),
            crate::platform::adapter::PlatformId::H3cHm2004Du
        );
        let admission = Arc::new(Mutex::new(Admission::new(reconciler::MASK)));
        let service = Self(admission.clone());
        // Revoke stale admission before discovery, including after daemon restart.
        if let Err(error) = admission.lock().unwrap().withdraw() {
            eprintln!("[-] NPU initial withdrawal failed: {error}");
        }
        crate::runtime::spawn_service("NPU admission", async move {
            let _guard = Service(admission.clone());
            let mut reconciler = Reconciler::default();
            let mut last_error = None;
            let mut poll_interval = reconciler::POLL_FAST;
            loop {
                tokio::select! {
                    biased;
                    _ = crate::runtime::cancelled() => break,
                    _ = tokio::time::sleep(poll_interval) => (),
                }
                let result = tokio::select! {
                    biased;
                    _ = crate::runtime::cancelled() => break,
                    result = reconciler::reconcile(&admission, &mut reconciler) => result,
                };
                match result {
                    Ok(next_interval) => {
                        last_error = None;
                        poll_interval = next_interval;
                    }
                    Err(error) => {
                        poll_interval = reconciler::POLL_FAST;
                        reconciler = Reconciler::default();
                        if let Err(error) = admission.lock().unwrap().withdraw() {
                            eprintln!("[-] NPU withdrawal failed: {error}");
                        }
                        let message = error.to_string();
                        if last_error.as_ref() != Some(&message) {
                            eprintln!("[-] NPU admission paused: {message}");
                            last_error = Some(message);
                        }
                    }
                }
            }
        });
        service
    }

    pub fn stop(&self) {
        let mut admission = self.0.lock().unwrap();
        admission.stopped = true;
        if let Err(error) = admission.withdraw() {
            eprintln!("[-] NPU shutdown withdrawal failed: {error}");
        }
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn stopped_service_cannot_be_rearmed_by_an_inflight_poll() {
        let path = std::env::temp_dir().join(format!("fttr-npu-{}", std::process::id()));
        fs::write(&path, "3\n").unwrap();
        let service = Service(Arc::new(Mutex::new(Admission {
            path: path.clone(),
            stopped: false,
        })));
        service.stop();
        service.0.lock().unwrap().set(3).unwrap();
        assert_eq!(reconciler::text(&path).unwrap(), "0");
        drop(service);
        let _ = fs::remove_file(path);
    }
}
