//! Endpoint replacement and retained-listener migration policy.
use super::*;

/// Change only the endpoint. Device/task history is shared with draining sessions.
pub fn reconfigure(
    enabled: bool,
    listen: &str,
    path: &str,
    username: &str,
    password: &str,
    retain_previous: bool,
) -> Result<(), String> {
    let _guard = RECONFIGURE.lock().unwrap();
    let next = transition(
        init(),
        enabled,
        listen,
        path,
        username,
        password,
        retain_previous,
    )?;
    *SERVER.get().unwrap().lock().unwrap() = next;
    Ok(())
}

pub fn configuration_matches(
    enabled: bool,
    listen: &str,
    path: &str,
    username: &str,
    password: &str,
) -> bool {
    let s = init();
    s.enabled == enabled
        && s.listen == listen
        && s.path == path
        && s.auth == (!username.is_empty()).then(|| http::basic(username, password))
        && s.failure.lock().unwrap().is_none()
        && (!enabled || (!s.stopping.load(Ordering::Acquire) && !s.stopped.load(Ordering::Acquire)))
}

pub fn retire_previous() {
    let _guard = RECONFIGURE.lock().unwrap();
    let s = init();
    for previous in std::mem::take(&mut *s.legacy.lock().unwrap()) {
        stop_listener(&previous);
    }
}

pub(super) fn stop_listener(server: &Server) {
    *server.drain_deadline.lock().unwrap() =
        Some(std::time::Instant::now() + Duration::from_secs(30));
    server.stopping.store(true, Ordering::Release);
}

pub(super) fn transition(
    old: Arc<Server>,
    enabled: bool,
    listen: &str,
    path: &str,
    username: &str,
    password: &str,
    retain_previous: bool,
) -> Result<Arc<Server>, String> {
    if crate::runtime::is_stopping() {
        return Err("ACS is stopping".into());
    }
    if let Some(reason) = &old.state.lock().unwrap().load_failure {
        return Err(reason.clone());
    }
    let auth = (!username.is_empty()).then(|| http::basic(username, password));
    if old.enabled == enabled
        && old.listen == listen
        && old.path == path
        && old.auth == auth
        && old.failure.lock().unwrap().is_none()
        && (!enabled || !old.stopped.load(Ordering::Acquire))
    {
        return Ok(old);
    }
    let listener = if enabled {
        if old.enabled && old.listen == listen {
            stop_listener(&old);
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            while !old.stopped.load(Ordering::Acquire) {
                if std::time::Instant::now() >= deadline {
                    return Err("old ACS listener did not stop".into());
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        Some(TcpListener::bind(listen).map_err(|e| format!("ACS listener bind failed: {e}"))?)
    } else {
        None
    };
    if let Some(listener) = &listener {
        listener
            .set_nonblocking(true)
            .map_err(|e| format!("ACS listener setup failed: {e}"))?;
    }
    let endpoint_id = token().map_err(|_| "cannot allocate ACS endpoint identity")?;
    let actual_listen = listener
        .as_ref()
        .map(|l| l.local_addr().map(|a| a.to_string()))
        .transpose()
        .map_err(|_| "cannot inspect ACS listener")?
        .unwrap_or_else(|| listen.into());
    let mut legacy = old.legacy.lock().unwrap().clone();
    if retain_previous && enabled && old.enabled {
        legacy.push(old.clone());
    } else {
        for previous in &legacy {
            stop_listener(previous);
        }
        legacy.clear();
    }
    let server = Arc::new(Server {
        state: old.state.clone(),
        enabled,
        listen: actual_listen,
        path: path.into(),
        state_file: old.state_file.clone(),
        auth,
        failure: Mutex::new(None),
        requests: old.requests.clone(),
        endpoint_id,
        drain_deadline: Mutex::new(None),
        legacy: Mutex::new(legacy),
        stopping: AtomicBool::new(false),
        stopped: AtomicBool::new(!enabled),
    });
    if !retain_previous || !enabled || old.listen == listen {
        stop_listener(&old);
    }
    if let Some(listener) = listener {
        listener::start(server.clone(), listener);
    }
    Ok(server)
}
