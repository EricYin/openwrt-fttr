//! Optional, bounded CWMP endpoint. It never takes a PON/OMCI lock.
mod http;
mod soap;
mod task;
mod persistence;
mod readback;
mod transaction;
mod connection_request;
mod connection_request_http;
mod periodic_policy;
mod periodic_task;
mod connection_request_proof;
#[cfg(test)]
mod periodic_policy_tests;
#[cfg(test)]
mod connection_request_tests;
#[cfg(test)]
mod compact_tests;
#[cfg(test)]
mod durability_tests;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, BufReader, Read};
#[cfg(test)]
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    mpsc, Arc, Mutex, OnceLock,
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use task::{error_result, Task, TaskState};

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn error(message: &str) -> Value {
    json!({"error": message})
}
#[derive(Clone, Serialize, Deserialize)]
struct Device {
    serial: String,
    oui: String,
    product_class: String,
    peer: String,
    namespace: String,
    last_inform: u64,
    #[serde(default)]
    endpoint_id: String,
    inform: Value,
}
#[derive(Clone, Default, Serialize, Deserialize)]
struct State {
    devices: BTreeMap<String, Device>,
    tasks: BTreeMap<u64, Task>,
    next_id: u64,
    #[serde(skip)]
    sessions: BTreeMap<String, (String, String, u64)>,
    #[serde(skip)]
    load_failure: Option<String>,
    #[serde(skip)]
    connection_requests: BTreeMap<String, connection_request::Status>,
    #[serde(skip)]
    periodic_inform: BTreeMap<String, periodic_policy::Policy>,
    #[serde(skip)]
    interval_authorizations: BTreeMap<u64, periodic_task::Authorization>,
    #[serde(skip)]
    connection_request_proofs: BTreeMap<String, connection_request_proof::Proof>,
}
struct Server {
    state: Arc<Mutex<State>>,
    enabled: bool,
    listen: String,
    path: String,
    state_file: String,
    auth: Option<String>,
    failure: Mutex<Option<String>>,
    requests: Arc<AtomicUsize>,
    endpoint_id: String,
    drain_deadline: Mutex<Option<std::time::Instant>>,
    legacy:Mutex<Vec<Arc<Server>>>,
    stopping: AtomicBool,
    stopped: AtomicBool,
}
static SERVER: OnceLock<Mutex<Arc<Server>>> = OnceLock::new();
static RECONFIGURE: Mutex<()> = Mutex::new(());
fn env(name: &str, fallback: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| fallback.into())
}
fn init() -> Arc<Server> {
    SERVER.get_or_init(|| {
        // A configured address is not authorization to activate a listener.
        let enabled = false;
        let state_file = env("FTTR_ACS_STATE", "/var/lib/fttr/acs-state.json");
        let state = State::load_or_block(&state_file);
        let failure = state.load_failure.clone();
        let user = env("FTTR_ACS_USERNAME", ""); let password = env("FTTR_ACS_PASSWORD", "");
        Mutex::new(Arc::new(Server { state: Arc::new(Mutex::new(state)), enabled, listen: String::new(), path: "/cwmp".into(), state_file,
            auth: (!user.is_empty()).then(|| http::basic(&user, &password)), failure: Mutex::new(failure), requests:Arc::new(AtomicUsize::new(0)), endpoint_id:String::new(), drain_deadline:Mutex::new(None),legacy:Mutex::new(Vec::new()), stopping:AtomicBool::new(false), stopped:AtomicBool::new(true) }))
    }).lock().unwrap().clone()
}
impl Server {
    fn execute_value(&self, request: Value) -> Value {
        if request["op"] == "task" {
            let mut state = self.state.lock().unwrap();
            if let Some(reason) = &state.load_failure {
                return error(reason);
            }
            state.expire();
            return match request["id"].as_u64().and_then(|id| state.tasks.get(&id)) {
                Some(task) => serde_json::to_value(task)
                    .unwrap_or_else(|err| error(&format!("task serialization: {err}"))),
                None => error("unknown task"),
            };
        }
        self.execute_small(request)
    }
    #[allow(dead_code)]
    fn execute(&self, request: Value) -> String {
        self.execute_value(request).to_string()
    }
    fn execute_small(&self, request: Value) -> Value {
        let mut state = self.state.lock().unwrap();
        if request["op"] != "status" && request["op"] != "clean" {
            if let Some(reason) = &state.load_failure {
                return error(reason);
            }
        }
        state.expire();
        match request["op"].as_str().unwrap_or("") {
            "clean" => {
                state.tasks.retain(|_, t| t.state.is_active());
                state.load_failure = None;
                *self.failure.lock().unwrap() = None;
                if let Err(err) = self.save(&state) {
                    return error(&format!("cannot persist cleaned state: {err}"));
                }
                json!({"cleaned":true, "tasks":state.tasks.len()})
            }
            "status" => {
                json!({"compiled":true, "enabled":self.enabled && !self.stopped.load(Ordering::Acquire), "listen":self.listen, "path":self.path, "endpoint_id":self.endpoint_id,"auth":self.auth.is_some(), "devices":state.devices.len(), "tasks":state.tasks.len(), "http_requests":self.requests.load(Ordering::Relaxed), "error":self.failure.lock().unwrap().clone()})
            }
            "devices" => json!({"devices":state.devices,"connection_requests":state.connection_requests,"periodic_inform":state.periodic_inform}),
            "reset_device_tasks" => {
                let Some(device) = request["device"].as_str().filter(|id| !id.is_empty()) else {
                    return error("device required");
                };
                if let Err(err) = self.reset_tasks(&mut state, device) {
                    return error(&format!("cannot persist device task reset: {err}"));
                }
                json!({"reset":true})
            }
            "busy" => {
                json!({"busy":state.tasks.values().any(|t| t.device == request["device"].as_str().unwrap_or("") && t.state.is_active())})
            }
            "queue" => {
                if !self.enabled {
                    return error("ACS disabled by UCI");
                }
                let device = request["device"].as_str().unwrap_or("");
                if !state.devices.contains_key(device) {
                    return error("unknown device: a real Inform is required before queuing RPCs; use acs devices");
                }
                let method = request["method"].as_str().unwrap_or("");
                let args = request["args"].clone();
                if let Err(err) = validate(method, &args) {
                    return error(&err);
                }
                if let Err(err) = protect_management(&state, device, method, &args) {
                    return error(&err);
                }
                if state.tasks.values().filter(|t| t.state.is_active()).count() >= 128 {
                    return error("ACS queue full");
                }
                let Some(id) = state.next_id.checked_add(1) else {
                    return error("ACS task IDs exhausted");
                };
                let task = Task {
                    id,
                    device: device.into(),
                    method: method.into(),
                    args,
                    state: TaskState::Queued,
                    created: now(),
                    sent: None,
                    result: None,
                };
                self.enqueue(&mut state, task)
            }
            _ => error("unknown ACS operation"),
        }
    }
}
fn protect_management(
    state: &State,
    device: &str,
    method: &str,
    args: &Value,
) -> Result<(), String> {
    const ROOT: &str = "InternetGatewayDevice.WANDevice.1.WANConnectionDevice.";
    let paths: Vec<&str> = match method {
        "SetParameterValues" => args["parameters"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|p| p["name"].as_str())
            .collect(),
        "AddObject" | "DeleteObject" => vec![args["object"].as_str().unwrap_or("")],
        _ => return Ok(()),
    };
    if method == "SetParameterValues"
        && args["parameters"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|p| {
                p["name"].as_str().is_some_and(|n| {
                    n.starts_with("InternetGatewayDevice.ManagementServer.")
                        || n.starts_with("Device.ManagementServer.")
                        || ((n.ends_with("ServiceList") || n.ends_with("SERVICELIST"))
                            && crate::provisioning::contains_service_token(
                                p["value"].as_str().unwrap_or(""),
                                "TR069",
                            ))
                })
            })
    {
        return Err("TR069 management is owned by OMCI".into());
    }
    for path in paths {
        if method == "AddObject" && path == ROOT {
            continue;
        }
        if !path.starts_with("InternetGatewayDevice.WANDevice.") {
            continue;
        }
        let parent = path
            .strip_prefix(ROOT)
            .and_then(|p| p.split('.').next())
            .and_then(|p| p.parse::<u32>().ok())
            .ok_or("protected WAN object")?;
        let parts: Vec<_> = path.strip_prefix(ROOT).unwrap().split('.').collect();
        let connection = parts
            .get(1)
            .filter(|v| matches!(**v, "WANIPConnection" | "WANPPPConnection"))
            .zip(parts.get(2).and_then(|n| n.parse::<u32>().ok()))
            .map(|(kind, n)| (*kind, n));
        if !crate::provisioning::owns_business_wan(device, parent, connection) {
            return Err("WAN is not an authorized editable business connection".into());
        }
        if method == "AddObject" {
            continue;
        }
        if connection.is_none() && crate::provisioning::protected_wan_parent(device, parent) {
            return Err("WAN parent contains a protected fallback or management connection".into());
        }
        let prefix = connection
            .map(|(kind, n)| format!("{ROOT}{parent}.{kind}.{n}."))
            .unwrap_or_else(|| format!("{ROOT}{parent}."));
        let observed = readback::observed(state, device, &prefix);
        if observed.is_none_or(|management| management) {
            return Err("WAN write requires a fresh non-TR069 service readback".into());
        }
    }
    Ok(())
}
fn validate(method: &str, args: &Value) -> Result<(), String> {
    let path = |v: &Value| {
        v.as_str()
            .is_some_and(|s| s.len() <= 1024 && !s.chars().any(char::is_control))
    };
    match method {
        "GetRPCMethods" => (),
        "AddObject" | "DeleteObject"
            if path(&args["object"])
                && args["object"].as_str().is_some_and(|s| {
                    s.starts_with("InternetGatewayDevice.")
                        && s.ends_with('.')
                        && s.split('.').count() >= 5
                }) => {}
        "GetParameterNames" if path(&args["path"]) && args["next_level"].is_boolean() => (),
        "GetParameterValues"
            if args["names"].as_array().is_some_and(|a| {
                !a.is_empty()
                    && a.len() <= 64
                    && a.iter().all(|v| {
                        path(v)
                            && v != ""
                            && !v.as_str().is_some_and(|s| s.ends_with('.'))
                    })
            }) => {}
        "SetParameterValues"
            if args["parameters"].as_array().is_some_and(|a| {
                !a.is_empty()
                    && a.len() <= 32
                    && a.iter().all(|v| {
                        path(&v["name"])
                            && v["name"] != ""
                            && v["value"].as_str().is_some_and(|s| {
                                s.len() <= 4096
                                    && !s
                                        .chars()
                                        .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
                            })
                    })
            }) => {}
        _ => return Err("invalid method or parameter arguments".into()),
    }
    soap::request("urn:dslforum-org:cwmp-1-0", 1, method, args).map_err(|e| e.to_string())?;
    Ok(())
}
impl State {
    #[cfg(test)]
    fn forget_tasks(&mut self, device: &str) {
        self.tasks.retain(|_, task| task.device != device);
        self.sessions.retain(|_, (id, _, _)| id != device);
    }
    pub(super) fn prune(&mut self) {
        let time = now();
        self.tasks.retain(|_, task| {
            task.state.is_active() || time.saturating_sub(task.sent.unwrap_or(task.created)) < 180
        });
        for task in self.tasks.values_mut() {
            if !task.state.is_active() && time.saturating_sub(task.sent.unwrap_or(task.created)) >= 15 {
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
    fn expire(&mut self) {
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
    fn next(&mut self, device: &str, namespace: &str) -> io::Result<Option<transaction::PreparedRpc>> {
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
            let internal_interval = self.interval_authorizations.get(&task.id).is_some_and(|grant|grant.permits(task, self));
            if let Err(reason) = if internal_interval { Ok(()) } else { protect_management(self, device, &task.method, &task.args) } {
                let id = task.id;
                let task = self.tasks.get_mut(&id).unwrap();
                task.state = TaskState::Fault;
                task.result = Some(error_result(&reason));
                return Ok(None);
            }
        }
        let task = self
            .tasks
            .values_mut()
            .find(|t| t.device == device && t.state == TaskState::Queued);
        if let Some(task) = task {
            let xml = soap::request(namespace, task.id, &task.method, &task.args)?;
            let undo = transaction::DispatchUndo { id: task.id, sent: task.sent };
            task.state = TaskState::Sent;
            task.sent = Some(now());
            return Ok(Some(transaction::PreparedRpc { xml, undo }));
        }
        Ok(None)
    }
    fn response(&mut self, device: &str, message: &soap::Message) -> io::Result<()> {
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
pub fn control_value(request: Value) -> Value {
    init().execute_value(request)
}
pub fn control(request: &str) -> String {
    if request.len() > 64 * 1024 {
        error("ACS request too large").to_string()
    } else {
        serde_json::from_str(request)
            .map(|v| init().execute_value(v).to_string())
            .unwrap_or_else(|_| error("invalid ACS JSON").to_string())
    }
}
pub fn start_from_env() {
    let _ = init();
    connection_request::start();
    println!("[ACS] waiting for authorized management network");
}

/// Change only the endpoint. Device/task history is shared with draining sessions.
pub fn reconfigure(enabled: bool, listen: &str, path: &str, username: &str, password: &str,retain_previous:bool) -> Result<(), String> {
    let _guard=RECONFIGURE.lock().unwrap();
    let old = init();
    let next=transition(old,enabled,listen,path,username,password,retain_previous)?;
    *SERVER.get().unwrap().lock().unwrap()=next;
    Ok(())
}
pub fn configuration_matches(enabled:bool,listen:&str,path:&str,username:&str,password:&str)->bool {
    let s=init();s.enabled==enabled&&s.listen==listen&&s.path==path&&s.auth==(!username.is_empty()).then(||http::basic(username,password))&&s.failure.lock().unwrap().is_none()&&(!enabled||(!s.stopping.load(Ordering::Acquire)&&!s.stopped.load(Ordering::Acquire)))
}
pub fn retire_previous() {let _guard=RECONFIGURE.lock().unwrap();let s=init();for previous in std::mem::take(&mut *s.legacy.lock().unwrap()) {stop_listener(&previous);}}
fn stop_listener(server:&Server) {
    *server.drain_deadline.lock().unwrap()=Some(std::time::Instant::now()+Duration::from_secs(30));
    server.stopping.store(true,Ordering::Release);
}
fn transition(old:Arc<Server>,enabled:bool,listen:&str,path:&str,username:&str,password:&str,retain_previous:bool)->Result<Arc<Server>,String> {
    if let Some(reason) = &old.state.lock().unwrap().load_failure { return Err(reason.clone()); }
    let auth = (!username.is_empty()).then(|| http::basic(username, password));
    if old.enabled == enabled && old.listen == listen && old.path == path && old.auth == auth && old.failure.lock().unwrap().is_none() && (!enabled || !old.stopped.load(Ordering::Acquire)) { return Ok(old); }
    let listener = if enabled {
        if old.enabled && old.listen == listen {
            stop_listener(&old);
            let deadline=std::time::Instant::now()+Duration::from_secs(2);
            while !old.stopped.load(Ordering::Acquire) { if std::time::Instant::now()>=deadline {return Err("old ACS listener did not stop".into());} std::thread::sleep(Duration::from_millis(20)); }
        }
        Some(TcpListener::bind(listen).map_err(|e| format!("ACS listener bind failed: {e}"))?)
    } else { None };
    if let Some(listener)=&listener {listener.set_nonblocking(true).map_err(|e|format!("ACS listener setup failed: {e}"))?;}
    let endpoint_id=token().map_err(|_|"cannot allocate ACS endpoint identity")?;
    let actual_listen=listener.as_ref().map(|l|l.local_addr().map(|a|a.to_string())).transpose().map_err(|_|"cannot inspect ACS listener")?.unwrap_or_else(||listen.into());
    let mut legacy=old.legacy.lock().unwrap().clone();
    if retain_previous&&enabled&&old.enabled {legacy.push(old.clone());}else{for previous in &legacy{stop_listener(previous);}legacy.clear();}
    let server = Arc::new(Server { state:old.state.clone(), enabled, listen:actual_listen, path:path.into(), state_file:old.state_file.clone(), auth,
        failure:Mutex::new(None), requests:old.requests.clone(), endpoint_id, drain_deadline:Mutex::new(None),legacy:Mutex::new(legacy),stopping:AtomicBool::new(false), stopped:AtomicBool::new(!enabled) });
    if !retain_previous||!enabled||old.listen==listen {stop_listener(&old);}
    if let Some(listener)=listener {start_listener(server.clone(),listener);}
    Ok(server)
}
fn start_listener(server: Arc<Server>, listener: TcpListener) {
    println!("[ACS] listening on {}{}", server.listen, server.path);
    let (tx, rx) = mpsc::sync_channel::<TcpStream>(16);
    let rx = Arc::new(Mutex::new(rx));
    const WORKERS: usize = 3;
    for idx in 0..WORKERS {
        let server = server.clone();
        let rx = rx.clone();
        let _ = std::thread::Builder::new()
            .name(format!("acs-worker-{idx}"))
            .stack_size(256 * 1024)
            .spawn(move || {
                loop {
                    let stream = {
                        let Ok(rx_guard) = rx.lock() else { break; };
                        match rx_guard.recv_timeout(Duration::from_millis(200)) {
                            Ok(s) => s,
                            Err(mpsc::RecvTimeoutError::Timeout) => {
                                if server.stopping.load(Ordering::Acquire) {
                                    break;
                                }
                                continue;
                            }
                            Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        }
                    };
                    if let Err(e) = serve(&server, stream) {
                        eprintln!("[ACS] HTTP connection ended: {}", e.kind());
                    }
                }
            });
    }
    let _ = std::thread::Builder::new()
        .name("acs-listener".into())
        .stack_size(256 * 1024)
        .spawn(move || {
            while !server.stopping.load(Ordering::Acquire) {
                let stream = match listener.accept() {
                    Ok((stream, _)) => stream,
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(50));
                        continue;
                    }
                    Err(e) => {
                        *server.failure.lock().unwrap() = Some(format!("listener accept: {e}"));
                        break;
                    }
                };
                if let Err(mpsc::TrySendError::Full(_)) = tx.try_send(stream) {
                    eprintln!("[ACS] worker queue full; dropping incoming connection");
                }
            }
            drop(tx);
            drop(listener);
            server.stopped.store(true, Ordering::Release);
        });
}
fn token() -> io::Result<String> {
    let mut bytes = [0u8; 16];
    fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
fn serve(server: &Server, mut stream: TcpStream) -> io::Result<()> {
    let peer = stream.peer_addr()?.ip().to_string();
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut current: Option<String> = None;
    let mut current_session: Option<String> = None;
    for _ in 0..64 {
        if let Some(deadline)=*server.drain_deadline.lock().unwrap() {
            let Some(remaining)=deadline.checked_duration_since(std::time::Instant::now()) else {break;};
            reader.get_mut().set_read_timeout(Some(remaining.min(Duration::from_secs(30)).max(Duration::from_millis(1))))?;
        }
        let req = match http::read(&mut reader, &mut stream) {
            Ok(v) => v,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::UnexpectedEof
                        | io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                ) =>
            {
                break
            }
            Err(e) => {
                eprintln!("[ACS] peer={peer} HTTP request rejected: {e}");
                http::reply(&mut stream, 400, "", "", true)?;
                break;
            }
        };
        if let Some(deadline)=*server.drain_deadline.lock().unwrap() {
            let Some(remaining)=deadline.checked_duration_since(std::time::Instant::now()) else {break;};
            stream.set_write_timeout(Some(remaining.min(Duration::from_secs(10)).max(Duration::from_millis(1))))?;
        }
        server.requests.fetch_add(1, Ordering::Relaxed);
        let current_matches=req.path==server.path&&server.auth.as_ref().is_none_or(|a|req.headers.get("authorization")==Some(a));
        let legacy=if current_matches {None}else{server.legacy.lock().unwrap().iter().find(|s|req.path==s.path&&s.auth.as_ref().is_none_or(|a|req.headers.get("authorization")==Some(a))).cloned()};
        let policy=legacy.as_deref().unwrap_or(server);
        if req.path != policy.path {
            http::reply(&mut stream, 404, "", "", true)?;
            break;
        }
        if policy
            .auth
            .as_ref()
            .is_some_and(|auth| req.headers.get("authorization") != Some(auth))
        {
            http::reply(
                &mut stream,
                401,
                "",
                "WWW-Authenticate: Basic realm=\"FTTR ACS\"\r\n",
                false,
            )?;
            continue;
        }
        let mut state = server.state.lock().unwrap();
        state.expire();
        if current.is_none() {
            let cookie = req.headers.get("cookie").and_then(|s| {
                s.split(';')
                    .find_map(|part| part.trim().strip_prefix("fttr_session="))
            });
            current = cookie
                .and_then(|key| state.sessions.get_mut(key))
                .filter(|(_, address, _)| *address == peer)
                .map(|(device, _, at)| {
                    *at = now();
                    device.clone()
                });
            if current.is_some() { current_session = cookie.map(str::to_owned); }
        }
        let mut extra = String::new();
        let mut dispatch = None;
        let mut state_changed = false;
        let response = if req.body.is_empty() {
            if let Some(device) = &current {
                let namespace = state.devices[device].namespace.clone();
                state.next(device, &namespace)?.map(|rpc| {
                    dispatch = Some(rpc.undo);
                    state_changed = true;
                    rpc.xml
                })
            } else {
                drop(state);
                http::reply(&mut stream, 400, "", "", true)?;
                break;
            }
        } else {
            let message = match std::str::from_utf8(&req.body)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "SOAP body is not UTF-8"))
                .and_then(soap::parse)
            {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("[ACS] peer={peer} SOAP rejected bytes={}: {e}", req.body.len());
                    drop(state);
                    http::reply(&mut stream, 400, "", "", true)?;
                    break;
                }
            };
            match message.method.as_str() {
                "Inform" => {
                    if message.serial.is_empty() || message.serial.len() > 128 || message.oui.len() != 6 || !message.oui.bytes().all(|b| b.is_ascii_hexdigit()) || message.product.len() > 128 { drop(state); http::reply(&mut stream, 400, "", "", true)?; break; }
                    let device = format!("{}:{}:{}", message.oui.to_ascii_uppercase(), message.product, message.serial);
                    if state.devices.len() >= 64 && !state.devices.contains_key(&device) { drop(state); http::reply(&mut stream, 503, "", "", true)?; break; }
                    // A new Inform is a new session; uncertain outstanding writes are never replayed.
                    for task in state.tasks.values_mut().filter(|t| t.device == device && t.state == TaskState::Sent) { task.state = TaskState::Unknown; task.result = Some(error_result("new Inform before RPC response")); }
                    state.sessions.retain(|_,(id,_,_)| *id != device);
                    let key = token()?; state.sessions.insert(key.clone(), (device.clone(), peer.clone(), now()));
                    extra = format!("Set-Cookie: fttr_session={key}; Path={}; HttpOnly; SameSite=Strict\r\n", policy.path);
                    if server.stopping.load(Ordering::Acquire) {drop(state);http::reply(&mut stream,503,"","",true)?;break;}
                    state.devices.insert(device.clone(), Device { serial:message.serial, oui:message.oui, product_class:message.product, peer:peer.clone(), namespace:message.namespace.clone(), last_inform:now(), endpoint_id:policy.endpoint_id.clone(),inform:message.data });
                    if let Some(mut proof) = state.connection_request_proofs.remove(&device) {
                        proof.inform(&state.devices[&device], &key);
                        if proof.current(&state.devices[&device], &proof.config()) { state.connection_request_proofs.insert(device.clone(), proof); }
                    }
                    current_session = Some(key);
                    current = Some(device);
                    state_changed = true;
                    Some(soap::envelope(&message.namespace, &message.id, "<cwmp:InformResponse><MaxEnvelopes>1</MaxEnvelopes></cwmp:InformResponse>"))
                }
                "GetRPCMethods" => Some(soap::envelope(&message.namespace, &message.id, "<cwmp:GetRPCMethodsResponse><MethodList soap-enc:arrayType=\"xsd:string[3]\"><string>Inform</string><string>GetRPCMethods</string><string>TransferComplete</string></MethodList></cwmp:GetRPCMethodsResponse>")),
                "TransferComplete" if current.is_some() => Some(soap::envelope(&message.namespace, &message.id, "<cwmp:TransferCompleteResponse/>")),
                _ => {
                    if let Some(device) = &current {
                        if let Err(e) = state.response(device, &message) { eprintln!("[ACS] peer={peer} RPC response rejected method={} id={}: {e}", message.method, message.id); drop(state); http::reply(&mut stream, 400, "", "", true)?; break; }
                        if let Some(session) = &current_session { connection_request_proof::completed(&mut state, device, session); }
                        state_changed = true;
                        state.next(device, &message.namespace)?.map(|rpc| {
                            dispatch = Some(rpc.undo);
                            rpc.xml
                        })
                    } else { drop(state); http::reply(&mut stream, 400, "", "", true)?; break; }
                }
            }
        };
        if state_changed {
            if server.save(&state).is_err() {
                if let Some(undo) = dispatch {
                    undo.restore(&mut state);
                }
                drop(state);
                http::reply(&mut stream, 503, "", "", true)?;
                break;
            }
        }
        if let (Some(device), Some(session), Some(undo)) = (&current, &current_session, &dispatch) {
            connection_request_proof::dispatched(&mut state, device, session, undo.id);
        }
        drop(state);
        let close = response.is_none()
            || req
                .headers
                .get("connection")
                .is_some_and(|s| s.eq_ignore_ascii_case("close"));
        http::reply(
            &mut stream,
            if response.is_some() { 200 } else { 204 },
            response.as_deref().unwrap_or(""),
            &extra,
            close,
        )?;
        if close {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn save_preserves_completed_results_and_private_mode_when_replacing_state() {
        use std::os::unix::fs::PermissionsExt;
        let server = listener_test_server();
        fs::write(&server.state_file, b"previous state").unwrap();
        let mut state = State::default();
        state.next_id = 7;
        state.tasks.insert(7, Task {
            id: 7, device: "save-test".into(), method: "GetParameterValues".into(),
            args: json!({"names":["InternetGatewayDevice."]}), state: TaskState::Completed,
            created: 1, sent: Some(2), result: Some(serde_json::value::to_raw_value(&json!({"raw":"\"\\\n".repeat(100_000)})).unwrap()),
        });
        let result = server.save(&state);
        result.unwrap();
        let saved = fs::read(&server.state_file).unwrap();
        assert_eq!(saved, serde_json::to_vec(&state).unwrap());
        assert_eq!(fs::metadata(&server.state_file).unwrap().permissions().mode() & 0o777, 0o600);
        assert!(!std::path::Path::new(&format!("{}.tmp", server.state_file)).exists());
        fs::remove_file(&server.state_file).unwrap();
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn save_preserves_previous_state_when_buffer_flush_fails() {
        let server = listener_test_server();
        fs::write(&server.state_file, b"previous state").unwrap();
        let temp = format!("{}.tmp", server.state_file);
        std::os::unix::fs::symlink("/dev/full", &temp).unwrap();
        let result = server.save(&State::default());
        assert!(result.is_err());
        assert_eq!(fs::read(&server.state_file).unwrap(), b"previous state");
        assert!(server.failure.lock().unwrap().as_deref().unwrap().starts_with("state persistence:"));
        fs::remove_file(temp).unwrap();
        fs::remove_file(&server.state_file).unwrap();
    }
    #[test]
    fn management_writes_and_unowned_wan_are_blocked_even_when_queued() {
        let mut state = State::default();
        for name in ["InternetGatewayDevice.ManagementServer.URL", "InternetGatewayDevice.WANDevice.1.WANConnectionDevice.3.WANIPConnection.1.X_CU_ServiceList"] {
            let args=json!({"parameters":[{"name":name,"value":"TR069"}]});
            assert!(protect_management(&state,"guard-test","SetParameterValues",&args).is_err());
            state.tasks.insert(1,Task{id:1,device:"guard-test".into(),method:"SetParameterValues".into(),args,state:TaskState::Queued,created:now(),sent:None,result:None});
            assert!(state.next("guard-test","urn:dslforum-org:cwmp-1-0").unwrap().is_none());
            assert_eq!(state.tasks[&1].state,TaskState::Fault);
        }
        assert!(protect_management(
            &state,
            "guard-test",
            "DeleteObject",
            &json!({"object":"InternetGatewayDevice.WANDevice.1.WANConnectionDevice.3."})
        )
        .is_err());
        assert!(protect_management(
            &state,
            "guard-test",
            "AddObject",
            &json!({"object":"InternetGatewayDevice.WANDevice.1.WANConnectionDevice."})
        )
        .is_ok());
        assert!(protect_management(
            &state,
            "guard-test",
            "GetParameterValues",
            &json!({"names":["InternetGatewayDevice.ManagementServer.URL"]})
        )
        .is_ok());
    }
    #[test]
    fn task_identity_fault_and_no_write_replay() {
        let mut s = State::default();
        s.tasks.insert(
            4,
            Task {
                id: 4,
                device: "a".into(),
                method: "SetParameterValues".into(),
                args: json!({"parameters":[{"name":"x","value":"y"}]}),
                state: TaskState::Queued,
                created: now(),
                sent: None,
                result: None,
            },
        );
        assert!(s
            .next("a", "urn:dslforum-org:cwmp-1-0")
            .unwrap()
            .unwrap()
            .xml
            .contains("fttr-4"));
        assert!(s.next("a", "urn:dslforum-org:cwmp-1-0").unwrap().is_none());
        let msg = soap::parse(&soap::envelope(
            "urn:dslforum-org:cwmp-1-0",
            "fttr-4",
            "<cwmp:SetParameterValuesResponse><Status>1</Status></cwmp:SetParameterValuesResponse>",
        ))
        .unwrap();
        assert!(s.response("b", &msg).is_err());
        s.response("a", &msg).unwrap();
        assert_eq!(s.tasks[&4].state, TaskState::AcceptedPendingApply);
        s.tasks.get_mut(&4).unwrap().state = TaskState::Sent;
        s.tasks.get_mut(&4).unwrap().sent = Some(now() - 100);
        s.expire();
        assert_eq!(s.tasks[&4].state, TaskState::Unknown);
        assert!(s.next("a", "urn:dslforum-org:cwmp-1-0").unwrap().is_none());
        assert!(validate("SetParameterValues", &json!({"parameters":[]})).is_err());
    }
    #[test]
    fn reset_discards_device_tasks_and_sessions_without_replaying_writes() {
        let mut state = State::default();
        for (id, device, status) in [(1, "a", TaskState::Queued), (2, "a", TaskState::Sent), (3, "b", TaskState::Queued)] {
            state.tasks.insert(id, Task { id, device: device.into(), method: "SetParameterValues".into(), args: json!({}), state: status, created: now(), sent: None, result: None });
        }
        state.next_id = 3;
        state.sessions.insert("a-token".into(), ("a".into(), "peer".into(), now()));
        state.sessions.insert("b-token".into(), ("b".into(), "peer".into(), now()));
        state.forget_tasks("a");
        assert_eq!(state.tasks.keys().copied().collect::<Vec<_>>(), vec![3]);
        assert!(!state.sessions.contains_key("a-token"));
        assert!(state.sessions.contains_key("b-token"));
        assert_eq!(state.next_id, 3);
        assert!(state.next("a", "urn:dslforum-org:cwmp-1-0").unwrap().is_none());
        let reply = soap::parse(&soap::envelope("urn:dslforum-org:cwmp-1-0", "fttr-2", "<cwmp:SetParameterValuesResponse><Status>0</Status></cwmp:SetParameterValuesResponse>")).unwrap();
        assert!(state.response("a", &reply).is_err());
    }
    #[test]
    fn listener_bind_failure_preserves_the_existing_listener() {
        let server=listener_test_server();let old=transition(server,true,"127.0.0.1:0","/old","","",false).unwrap();
        let occupied=TcpListener::bind("127.0.0.1:0").unwrap();assert!(transition(old.clone(),true,&occupied.local_addr().unwrap().to_string(),"/new","","",true).is_err());
        assert!(!old.stopping.load(Ordering::Acquire));assert!(TcpStream::connect(&old.listen).is_ok());stop_listener(&old);
    }
    pub(super) fn listener_test_server()->Arc<Server> {
        Arc::new(Server{state:Arc::new(Mutex::new(State::default())),enabled:false,listen:String::new(),path:"/cwmp".into(),state_file:format!("/tmp/fttr-listener-test-{}-{}.json",std::process::id(),token().unwrap()),auth:None,failure:Mutex::new(None),requests:Arc::new(AtomicUsize::new(0)),endpoint_id:String::new(),drain_deadline:Mutex::new(None),legacy:Mutex::new(Vec::new()),stopping:AtomicBool::new(false),stopped:AtomicBool::new(true)})
    }
    #[test]
    fn listener_migration_shares_tasks_and_marks_inform_with_the_actual_endpoint() {
        use std::io::BufRead;
        let old=transition(listener_test_server(),true,"127.0.0.1:0","/old","old","secret",false).unwrap();
        let new=transition(old.clone(),true,"127.0.0.1:0","/new","new","secret",true).unwrap();assert!(Arc::ptr_eq(&old.state,&new.state));assert!(!old.stopping.load(Ordering::Acquire));
        let inform=soap::envelope("urn:dslforum-org:cwmp-1-0","cpe","<cwmp:Inform><DeviceId><OUI>ABCDEF</OUI><ProductClass>test</ProductClass><SerialNumber>migration</SerialNumber></DeviceId></cwmp:Inform>");
        let send=|address:&str,path:&str,user:&str| {
            let mut stream=TcpStream::connect(address).unwrap();stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();write!(stream,"POST {path} HTTP/1.1\r\nHost: localhost\r\nAuthorization: {}\r\nContent-Length: {}\r\n\r\n{inform}",http::basic(user,"secret"),inform.len()).unwrap();stream.flush().unwrap();let mut reader=BufReader::new(stream);let mut headers=String::new();loop {let mut line=String::new();assert_ne!(reader.read_line(&mut line).unwrap(),0);headers.push_str(&line);if line=="\r\n"{break;}}assert!(headers.starts_with("HTTP/1.1 200"));let length:usize=headers.lines().find_map(|l|l.strip_prefix("Content-Length: ")).unwrap().parse().unwrap();let mut body=vec![0;length];reader.read_exact(&mut body).unwrap();assert!(String::from_utf8(body).unwrap().contains("InformResponse"));
        };
        send(&old.listen,"/old","old");assert_eq!(new.state.lock().unwrap().devices["ABCDEF:test:migration"].endpoint_id,old.endpoint_id);
        send(&new.listen,"/new","new");assert_eq!(new.state.lock().unwrap().devices["ABCDEF:test:migration"].endpoint_id,new.endpoint_id);
        let same_address=transition(new.clone(),true,&new.listen,"/third","third","secret",true).unwrap();send(&same_address.listen,"/new","new");assert_eq!(same_address.state.lock().unwrap().devices["ABCDEF:test:migration"].endpoint_id,new.endpoint_id);send(&same_address.listen,"/third","third");assert_eq!(same_address.state.lock().unwrap().devices["ABCDEF:test:migration"].endpoint_id,same_address.endpoint_id);
        for previous in same_address.legacy.lock().unwrap().iter(){stop_listener(previous);}stop_listener(&same_address);fs::remove_file(&same_address.state_file).unwrap();
    }
    #[test]
    fn actual_http_inform_cookie_rpc_and_termination() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let state_file = format!("/tmp/fttr-acs-wire-test-{}.json", std::process::id());
        let server = Arc::new(Server {
            state: Arc::new(Mutex::new(State::default())),
            enabled: true,
            listen: address.to_string(),
            path: "/cwmp".into(),
            state_file: state_file.clone(),
            auth: Some(http::basic("test", "secret")),
            failure: Mutex::new(None),
            requests: Arc::new(AtomicUsize::new(0)),
            endpoint_id:"wire-test".into(),
            drain_deadline:Mutex::new(None),
            legacy:Mutex::new(Vec::new()),
            stopping: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
        });
        let handler = server.clone();
        let thread = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            serve(&handler, stream).unwrap();
        });
        let mut client = TcpStream::connect(address).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut reader = BufReader::new(client.try_clone().unwrap());
        fn exchange(
            client: &mut TcpStream,
            reader: &mut BufReader<TcpStream>,
            body: &str,
        ) -> (String, String) {
            use std::io::BufRead;
            write!(client, "POST /cwmp HTTP/1.1\r\nHost: localhost\r\nAuthorization: Basic dGVzdDpzZWNyZXQ=\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap();
            client.flush().unwrap();
            let mut headers = String::new();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                headers.push_str(&line);
                if line == "\r\n" {
                    break;
                }
            }
            let length: usize = headers
                .lines()
                .find_map(|l| l.strip_prefix("Content-Length: "))
                .unwrap()
                .parse()
                .unwrap();
            let mut data = vec![0; length];
            reader.read_exact(&mut data).unwrap();
            (headers, String::from_utf8(data).unwrap())
        }
        let inform = soap::envelope("urn:dslforum-org:cwmp-1-0", "cpe1", "<cwmp:Inform><DeviceId><OUI>ABCDEF</OUI><ProductClass>test</ProductClass><SerialNumber>mock</SerialNumber></DeviceId></cwmp:Inform>");
        let (headers, body) = exchange(&mut client, &mut reader, &inform);
        assert!(headers.contains("Set-Cookie: fttr_session="));
        assert!(body.contains("InformResponse"));
        let task: Value=serde_json::from_str(&server.execute(json!({"op":"queue","device":"ABCDEF:test:mock","method":"GetParameterValues","args":{"names":["InternetGatewayDevice.DeviceInfo.ModelName"]}}))).unwrap();
        assert_eq!(task["id"], 1);
        let (_, rpc) = exchange(&mut client, &mut reader, "");
        assert!(rpc.contains("GetParameterValues"));
        let response=soap::envelope("urn:dslforum-org:cwmp-1-0", "fttr-1", "<cwmp:GetParameterValuesResponse><ParameterList><ParameterValueStruct><Name>InternetGatewayDevice.DeviceInfo.ModelName</Name><Value xsi:type=\"xsd:string\">mock</Value></ParameterValueStruct></ParameterList></cwmp:GetParameterValuesResponse>");
        let (headers, body) = exchange(&mut client, &mut reader, &response);
        assert!(headers.starts_with("HTTP/1.1 204"));
        assert!(body.is_empty());
        thread.join().unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&server.execute(json!({"op":"task","id":1}))).unwrap()["result"]["parameters"][0]["value"],
            "mock"
        );
        fs::remove_file(state_file).unwrap();
    }
}
