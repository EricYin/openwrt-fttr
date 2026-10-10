//! Shell-over-MQTT command transport. Separate connections from fttrd telemetry.
use crate::mqtt::{MqttClient, MqttEvent};
use serde::Serialize;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Debug, Serialize)]
pub struct CommandResult {
    pub target: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl CommandResult {
    pub fn failed(target: &str, error: impl ToString) -> Self {
        Self { target: target.into(), status: "error".into(), request_id: None,
               exit_code: None, output: None, error: Some(error.to_string()) }
    }
    pub fn success(&self) -> bool {
        self.error.is_none() && self.exit_code.unwrap_or(0) == 0
    }
}

pub fn normalize_mac(value: &str) -> io::Result<String> {
    let mac = if value.len() == 17 && value.split(':').count() == 6
        && value.split(':').all(|part| part.len() == 2) {
        value.replace(':', "")
    } else { value.to_string() };
    if mac.len() != 12 || !mac.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput,
            "target must be the 12-digit br0 MAC (e.g. 1A2B3C4DA111), not the optical SN"));
    }
    Ok(mac.to_ascii_uppercase())
}

fn request_id() -> String {
    format!("{:x}_{:x}", std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos())
}

fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\"'\"'"))
}

fn framed_script(command: &str, id: &str) -> String {
    format!("printf '__FTTR_BEGIN_{id}__\\n'; sh -c {} 2>&1; fttr_rc=$?; printf '\\n__FTTR_END_{id}__=%s\\n' \"$fttr_rc\"",
        shell_quote(command))
}

fn parse_reply(payload: &[u8], id: &str) -> Option<(String, i32)> {
    let text = String::from_utf8_lossy(payload);
    let body = text.strip_prefix(&format!("__FTTR_BEGIN_{id}__\n"))?;
    let (output, code) = body.rsplit_once(&format!("\n__FTTR_END_{id}__="))?;
    let code = code.strip_suffix('\n')?.parse::<i32>().ok()?;
    Some((output.to_string(), code))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn shell_frame_preserves_script_and_reports_stderr_and_exit() {
        let script = "printf '%s\\n' \"single'quote\"; printf 'error\\n' >&2; exit 7";
        let result = Command::new("sh").args(["-c", &framed_script(script, "test123")]).output().unwrap();
        assert!(result.status.success());
        assert_eq!(parse_reply(&result.stdout, "test123"), Some(("single'quote\nerror\n".into(), 7)));
        assert!(parse_reply(&result.stdout, "previous_request").is_none());
        assert!(result.stderr.is_empty());
    }

    #[test]
    fn empty_output_still_has_a_response_and_binary_output_is_lossy_text() {
        let result = Command::new("sh").args(["-c", &framed_script("exit 0", "empty")]).output().unwrap();
        assert_eq!(parse_reply(&result.stdout, "empty"), Some((String::new(), 0)));
        assert!(parse_reply(b"__FTTR_BEGIN_empty__\nbody\n__FTTR_END_empty__=bad\n", "empty").is_none());
        assert_eq!(parse_reply(b"__FTTR_BEGIN_empty__\n\xff\n__FTTR_END_empty__=0\n", "empty"), Some(("\u{fffd}".into(), 0)));
    }

    #[test]
    fn identity_and_advisory_lock_reject_ambiguous_commands() {
        assert_eq!(normalize_mac("1c:94:68:5d:f9:98").unwrap(), "1A2B3C4DA111");
        assert!(normalize_mac("H3CT1A2B3C4D").is_err());
        assert!(normalize_mac("../1A2B3C4DA111").is_err());
        let target = format!("TEST{}", request_id());
        let first = lock_target(&target).unwrap();
        assert_eq!(lock_target(&target).unwrap_err().kind(), io::ErrorKind::WouldBlock);
        drop(first);
        assert!(lock_target(&target).is_ok());
        std::fs::remove_file(format!("/tmp/fttr-command-{target}.lock")).unwrap();
    }
}

// Advisory locks survive file reuse but are released automatically on process exit.
// Do not unlink the lock file: doing so would let clients lock different inodes.
fn lock_target(target: &str) -> io::Result<File> {
    let file = OpenOptions::new().read(true).write(true).create(true).truncate(false)
        .open(format!("/tmp/fttr-command-{target}.lock"))?;
    extern "C" { fn flock(fd: i32, operation: i32) -> i32; }
    if unsafe { flock(file.as_raw_fd(), 2 | 4) } != 0 {
        return Err(io::Error::new(io::ErrorKind::WouldBlock,
            format!("another fttrctl command is active for {target}")));
    }
    Ok(file)
}

/// QoS 0 dispatch only. Success reports a socket write, not device execution.
pub fn dispatch(target: &str, command: &str) -> io::Result<CommandResult> {
    let target = if target == "all" { "all".to_string() } else { normalize_mac(target)? };
    if command.trim().is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "command must not be empty"));
    }
    let _lock = lock_target(&target)?;
    let mut mqtt = MqttClient::connect("127.0.0.1:1883", &format!("fttr-cmd-{}", request_id()))?;
    mqtt.publish(&format!("/fttr/subdev/cmd/{target}"), command.as_bytes())?;
    Ok(CommandResult { target, status: "dispatched".into(), request_id: None,
        exit_code: None, output: None, error: None })
}

/// Wait for a fresh, matching response frame containing combined output and exit code.
pub fn execute(target: &str, command: &str, timeout: Duration) -> io::Result<CommandResult> {
    let target = normalize_mac(target)?;
    if command.trim().is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "command must not be empty"));
    }
    let _lock = lock_target(&target)?;
    let id = request_id();
    let topic = format!("/fttr/subdev/exec/{target}");
    let reply_topic = format!("{topic}/resp");
    let mut mqtt = MqttClient::connect("127.0.0.1:1883", &format!("fttr-exec-{id}"))?;
    mqtt.subscribe_confirmed(&reply_topic, timeout)?;
    mqtt.publish(&topic, framed_script(command, &id).as_bytes())?;
    let deadline = Instant::now() + timeout;
    let mut heartbeat = Instant::now();
    while Instant::now() < deadline {
        if heartbeat.elapsed() >= Duration::from_secs(20) {
            mqtt.ping()?;
            heartbeat = Instant::now();
        }
        mqtt.set_read_timeout(deadline.saturating_duration_since(Instant::now()))?;
        if let Some(MqttEvent::Publish { topic, payload, retain: false }) = mqtt.read_event()? {
            if topic == reply_topic {
                if let Some((output, code)) = parse_reply(&payload, &id) {
                    return Ok(CommandResult { target, status: "executed".into(), request_id: Some(id),
                        exit_code: Some(code), output: Some(output), error: None });
                }
            }
        }
    }
    Ok(CommandResult { target, status: "timeout".into(), request_id: Some(id), exit_code: None,
        output: None, error: Some("no matching device response before timeout; execution outcome is unknown".into()) })
}
