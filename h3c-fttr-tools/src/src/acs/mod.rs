//! Optional, bounded CWMP endpoint. It never takes a PON/OMCI lock.
mod http;
mod soap;
mod task;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::OpenOptionsExt;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex, OnceLock,
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use task::{Task, TaskState};

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
    inform: Value,
}
#[derive(Default, Serialize, Deserialize)]
struct State {
    devices: BTreeMap<String, Device>,
    tasks: BTreeMap<u64, Task>,
    next_id: u64,
    #[serde(skip)]
    sessions: BTreeMap<String, (String, String, u64)>,
}
struct Server {
    state: Mutex<State>,
    enabled: bool,
    listen: String,
    path: String,
    state_file: String,
    auth: Option<String>,
    failure: Mutex<Option<String>>,
    requests: AtomicUsize,
}
static SERVER: OnceLock<Arc<Server>> = OnceLock::new();
fn env(name: &str, fallback: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| fallback.into())
}
fn init() -> Arc<Server> {
    SERVER.get_or_init(|| {
        let enabled = env("FTTR_ACS_ENABLED", "0") == "1";
        let state_file = env("FTTR_ACS_STATE", "/var/lib/fttr/acs-state.json");
        let mut state: State = fs::metadata(&state_file).ok().filter(|m| m.len() <= 8 * 1024 * 1024).and_then(|_| fs::read(&state_file).ok()).and_then(|data| serde_json::from_slice(&data).ok()).unwrap_or_default();
        for task in state.tasks.values_mut().filter(|t| t.state == TaskState::Sent) { task.state = TaskState::Unknown; task.result = Some(error("daemon restarted after dispatch; execution is unknown, not automatically retried")); }
        state.next_id = state.next_id.max(state.tasks.keys().next_back().copied().unwrap_or(0));
        let user = env("FTTR_ACS_USERNAME", ""); let password = env("FTTR_ACS_PASSWORD", "");
        Arc::new(Server { state: Mutex::new(state), enabled, listen: env("FTTR_ACS_LISTEN", "127.0.0.1:7547"), path: env("FTTR_ACS_PATH", "/cwmp"), state_file,
            auth: (!user.is_empty()).then(|| http::basic(&user, &password)), failure: Mutex::new(None), requests:AtomicUsize::new(0) })
    }).clone()
}
impl Server {
    fn save(&self, state: &State) {
        let result = (|| -> io::Result<()> {
            let path = std::path::Path::new(&self.state_file);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            let temp = format!("{}.tmp", self.state_file);
            let mut file = OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .mode(0o600)
                .open(&temp)?;
            let data = serde_json::to_vec(state)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            file.write_all(&data)?;
            file.flush()?;
            fs::rename(temp, path)
        })();
        if let Err(err) = result {
            *self.failure.lock().unwrap() = Some(format!("state persistence: {err}"));
        }
    }
    fn execute(&self, request: Value) -> Value {
        let mut state = self.state.lock().unwrap();
        state.expire();
        match request["op"].as_str().unwrap_or("") {
            "status" => {
                json!({"compiled":true, "enabled":self.enabled, "listen":self.listen, "path":self.path, "auth":self.auth.is_some(), "devices":state.devices.len(), "tasks":state.tasks.len(), "http_requests":self.requests.load(Ordering::Relaxed), "error":self.failure.lock().unwrap().clone()})
            }
            "devices" => json!({"devices":state.devices}),
            "busy" => {
                json!({"busy":state.tasks.values().any(|t| t.device == request["device"].as_str().unwrap_or("") && t.state.is_active())})
            }
            "task" => request["id"]
                .as_u64()
                .and_then(|id| state.tasks.get(&id))
                .map(|t| json!(t))
                .unwrap_or_else(|| error("unknown task")),
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
                while state.tasks.len() >= 256 {
                    let expired = state
                        .tasks
                        .iter()
                        .find(|(_, t)| !t.state.is_active())
                        .map(|(id, _)| *id);
                    if let Some(id) = expired {
                        state.tasks.remove(&id);
                    } else {
                        return error("ACS task history full");
                    }
                }
                state.next_id += 1;
                let id = state.next_id;
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
                state.tasks.insert(id, task.clone());
                self.save(&state);
                json!(task)
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
                        || ((n.ends_with("ServiceList") || n.ends_with("SERVICELIST"))
                            && p["value"].as_str().unwrap_or("").contains("TR069"))
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
        let observed = state
            .tasks
            .values()
            .rev()
            .filter(|t| {
                t.device == device
                    && t.method == "GetParameterValues"
                    && t.state == TaskState::Completed
                    && now().saturating_sub(t.sent.unwrap_or(0)) <= 90
            })
            .find_map(|t| {
                t.result.as_ref()?["parameters"]
                    .as_array()?
                    .iter()
                    .find(|p| {
                        p["name"].as_str().is_some_and(|n| {
                            n.starts_with(&prefix)
                                && (n.ends_with("ServiceList") || n.ends_with("SERVICELIST"))
                        })
                    })
                    .and_then(|p| p["value"].as_str())
            });
        if observed.is_none_or(|v| v.contains("TR069")) {
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
                !a.is_empty() && a.len() <= 64 && a.iter().all(|v| path(v) && v != "")
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
    fn expire(&mut self) {
        let time = now();
        self.sessions
            .retain(|_, (_, _, at)| time.saturating_sub(*at) < 120);
        for t in self.tasks.values_mut() {
            if t.state == TaskState::Sent && time.saturating_sub(t.sent.unwrap_or(time)) >= 90 {
                t.state = TaskState::Unknown;
                t.result = Some(error(
                    "response timeout: device execution is unknown; no automatic write retry",
                ));
            } else if t.state == TaskState::Queued && time.saturating_sub(t.created) >= 600 {
                t.state = TaskState::Expired;
                t.result = Some(error("no device session within 600 seconds"));
            }
        }
    }
    fn next(&mut self, device: &str, namespace: &str) -> io::Result<Option<String>> {
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
            if let Err(reason) = protect_management(self, device, &task.method, &task.args) {
                let id = task.id;
                let task = self.tasks.get_mut(&id).unwrap();
                task.state = TaskState::Fault;
                task.result = Some(error(&reason));
                return Ok(None);
            }
        }
        let task = self
            .tasks
            .values_mut()
            .find(|t| t.device == device && t.state == TaskState::Queued);
        if let Some(task) = task {
            let xml = soap::request(namespace, task.id, &task.method, &task.args)?;
            task.state = TaskState::Sent;
            task.sent = Some(now());
            return Ok(Some(xml));
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
        task.result = Some(message.data.clone());
        Ok(())
    }
}
pub fn control(request: &str) -> String {
    let reply = if request.len() > 64 * 1024 {
        error("ACS request too large")
    } else {
        serde_json::from_str(request)
            .map(|v| init().execute(v))
            .unwrap_or_else(|_| error("invalid ACS JSON"))
    };
    reply.to_string()
}
pub fn start_from_env() {
    let server = init();
    if !server.enabled {
        println!("[ACS] compiled, disabled by UCI");
        return;
    }
    let listener = match TcpListener::bind(&server.listen) {
        Ok(v) => v,
        Err(e) => {
            *server.failure.lock().unwrap() = Some(format!("listener: {e}"));
            eprintln!("[ACS] listener failed: {e}");
            return;
        }
    };
    println!("[ACS] listening on {}{}", server.listen, server.path);
    let connections = Arc::new(AtomicUsize::new(0));
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            if connections.fetch_add(1, Ordering::AcqRel) >= 16 {
                connections.fetch_sub(1, Ordering::AcqRel);
                continue;
            }
            let server = server.clone();
            let connections = connections.clone();
            std::thread::spawn(move || {
                if let Err(e) = serve(&server, stream) {
                    eprintln!("[ACS] HTTP connection ended: {}", e.kind());
                }
                connections.fetch_sub(1, Ordering::AcqRel);
            });
        }
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
    for _ in 0..64 {
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
            Err(_) => {
                http::reply(&mut stream, 400, "", "", true)?;
                break;
            }
        };
        server.requests.fetch_add(1, Ordering::Relaxed);
        if req.path != server.path {
            http::reply(&mut stream, 404, "", "", true)?;
            break;
        }
        if server
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
        }
        let mut extra = String::new();
        let response = if req.body.is_empty() {
            if let Some(device) = &current {
                let namespace = state.devices[device].namespace.clone();
                state.next(device, &namespace)?
            } else {
                http::reply(&mut stream, 400, "", "", true)?;
                break;
            }
        } else {
            let message = match std::str::from_utf8(&req.body)
                .ok()
                .and_then(|text| soap::parse(text).ok())
            {
                Some(v) => v,
                None => {
                    http::reply(&mut stream, 400, "", "", true)?;
                    break;
                }
            };
            match message.method.as_str() {
                "Inform" => {
                    if message.serial.is_empty() || message.serial.len() > 128 || message.oui.len() != 6 || !message.oui.bytes().all(|b| b.is_ascii_hexdigit()) || message.product.len() > 128 { http::reply(&mut stream, 400, "", "", true)?; break; }
                    let device = format!("{}:{}:{}", message.oui.to_ascii_uppercase(), message.product, message.serial);
                    if state.devices.len() >= 64 && !state.devices.contains_key(&device) { http::reply(&mut stream, 503, "", "", true)?; break; }
                    // A new Inform is a new session; uncertain outstanding writes are never replayed.
                    for task in state.tasks.values_mut().filter(|t| t.device == device && t.state == TaskState::Sent) { task.state = TaskState::Unknown; task.result = Some(error("new Inform before RPC response")); }
                    state.sessions.retain(|_,(id,_,_)| *id != device);
                    let key = token()?; state.sessions.insert(key.clone(), (device.clone(), peer.clone(), now()));
                    extra = format!("Set-Cookie: fttr_session={key}; Path={}; HttpOnly; SameSite=Strict\r\n", server.path);
                    state.devices.insert(device.clone(), Device { serial:message.serial, oui:message.oui, product_class:message.product, peer:peer.clone(), namespace:message.namespace.clone(), last_inform:now(), inform:message.data });
                    current = Some(device);
                    Some(soap::envelope(&message.namespace, &message.id, "<cwmp:InformResponse><MaxEnvelopes>1</MaxEnvelopes></cwmp:InformResponse>"))
                }
                "GetRPCMethods" => Some(soap::envelope(&message.namespace, &message.id, "<cwmp:GetRPCMethodsResponse><MethodList soap-enc:arrayType=\"xsd:string[3]\"><string>Inform</string><string>GetRPCMethods</string><string>TransferComplete</string></MethodList></cwmp:GetRPCMethodsResponse>")),
                "TransferComplete" if current.is_some() => Some(soap::envelope(&message.namespace, &message.id, "<cwmp:TransferCompleteResponse/>")),
                _ => {
                    if let Some(device) = &current {
                        if state.response(device, &message).is_err() { http::reply(&mut stream, 400, "", "", true)?; break; }
                        state.next(device, &message.namespace)?
                    } else { http::reply(&mut stream, 400, "", "", true)?; break; }
                }
            }
        };
        server.save(&state);
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
    fn actual_http_inform_cookie_rpc_and_termination() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let state_file = format!("/tmp/fttr-acs-wire-test-{}.json", std::process::id());
        let server = Arc::new(Server {
            state: Mutex::new(State::default()),
            enabled: true,
            listen: address.to_string(),
            path: "/cwmp".into(),
            state_file: state_file.clone(),
            auth: Some(http::basic("test", "secret")),
            failure: Mutex::new(None),
            requests: AtomicUsize::new(0),
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
        let task=server.execute(json!({"op":"queue","device":"ABCDEF:test:mock","method":"GetParameterValues","args":{"names":["InternetGatewayDevice.DeviceInfo.ModelName"]}}));
        assert_eq!(task["id"], 1);
        let (_, rpc) = exchange(&mut client, &mut reader, "");
        assert!(rpc.contains("GetParameterValues"));
        let response=soap::envelope("urn:dslforum-org:cwmp-1-0", "fttr-1", "<cwmp:GetParameterValuesResponse><ParameterList><ParameterValueStruct><Name>InternetGatewayDevice.DeviceInfo.ModelName</Name><Value xsi:type=\"xsd:string\">mock</Value></ParameterValueStruct></ParameterList></cwmp:GetParameterValuesResponse>");
        let (headers, body) = exchange(&mut client, &mut reader, &response);
        assert!(headers.starts_with("HTTP/1.1 204"));
        assert!(body.is_empty());
        thread.join().unwrap();
        assert_eq!(
            server.execute(json!({"op":"task","id":1}))["result"]["parameters"][0]["value"],
            "mock"
        );
        fs::remove_file(state_file).unwrap();
    }
}
