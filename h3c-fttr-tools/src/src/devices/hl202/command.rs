//! Shell-over-MQTT command transport. Separate connections from fttrd telemetry.
#[cfg(feature = "mqtt")]
use crate::mqtt::session::CommandSession;
use serde::Serialize;
#[cfg(any(feature = "mqtt", test))]
use std::fs::{File, OpenOptions};
use std::io;
#[cfg(any(feature = "mqtt", test))]
use std::os::fd::AsRawFd;
use std::time::Duration;
#[cfg(any(feature = "mqtt", test))]
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandStatus {
    Error,
    Executed,
    Dispatched,
    Timeout,
}

impl std::fmt::Display for CommandStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Error => "error",
            Self::Executed => "executed",
            Self::Dispatched => "dispatched",
            Self::Timeout => "timeout",
        })
    }
}

#[derive(Debug, Serialize)]
pub struct CommandResult {
    pub target: String,
    pub status: CommandStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    pub output_truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl CommandResult {
    pub fn failed(target: &str, error: impl ToString) -> Self {
        Self {
            target: target.into(),
            status: CommandStatus::Error,
            request_id: None,
            exit_code: None,
            output: None,
            output_truncated: false,
            error: Some(error.to_string()),
        }
    }
    pub fn success(&self) -> bool {
        self.error.is_none()
            && match self.status {
                CommandStatus::Executed => self.exit_code == Some(0),
                CommandStatus::Dispatched => self.exit_code.is_none(),
                CommandStatus::Error | CommandStatus::Timeout => false,
            }
    }
}

pub fn normalize_mac(value: &str) -> io::Result<String> {
    let mac = if value.len() == 17
        && value.split(':').count() == 6
        && value.split(':').all(|part| part.len() == 2)
    {
        value.replace(':', "")
    } else {
        value.to_string()
    };
    if mac.len() != 12 || !mac.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "target must be the 12-digit br0 MAC (e.g. 1A2B3C4DA111), not the optical SN",
        ));
    }
    Ok(mac.to_ascii_uppercase())
}

#[cfg(any(feature = "mqtt", test))]
pub(super) fn request_id() -> String {
    format!(
        "{:x}_{:x}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    )
}

#[cfg(any(feature = "mqtt", test))]
fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\"'\"'"))
}

#[cfg(any(feature = "mqtt", test))]
pub(super) fn framed_script(command: &str, id: &str) -> String {
    format!("printf '__FTTR_BEGIN_{id}__\\n'; sh -c {} 2>&1; fttr_rc=$?; printf '\\n__FTTR_END_{id}__=%s\\n' \"$fttr_rc\"",
        shell_quote(command))
}

#[cfg(any(feature = "mqtt", test))]
pub(super) fn parse_reply(payload: &[u8], id: &str) -> Option<(String, i32, bool)> {
    let text = String::from_utf8_lossy(payload);
    let body = text.strip_prefix(&format!("__FTTR_BEGIN_{id}__\n"))?;
    let (output, code) = body.rsplit_once(&format!("\n__FTTR_END_{id}__="))?;
    let code = code.strip_suffix('\n')?.parse::<i32>().ok()?;
    let mut end = output.len().min(1024 * 1024);
    while !output.is_char_boundary(end) {
        end -= 1;
    }
    Some((output[..end].to_string(), code, end < output.len()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn mqtt_command_output_limit_preserves_utf8_and_exit_status() {
        // Given output crossing the byte limit inside a UTF-8 character.
        let output = format!("{}猫", "a".repeat(1024 * 1024 - 1));
        let payload = format!("__FTTR_BEGIN_limit__\n{output}\n__FTTR_END_limit__=7\n");
        // When parsing the matching response, then retain valid bounded text and exit proof.
        let (text, code, truncated) = parse_reply(payload.as_bytes(), "limit").unwrap();
        assert_eq!(text.len(), 1024 * 1024 - 1);
        assert_eq!(code, 7);
        assert!(truncated);
    }

    #[test]
    fn mqtt_command_output_limit_marks_only_overflow() {
        for size in [1024 * 1024, 1024 * 1024 + 1] {
            let payload = format!(
                "__FTTR_BEGIN_limit__\n{}\n__FTTR_END_limit__=0\n",
                "a".repeat(size)
            );
            let (text, code, truncated) = parse_reply(payload.as_bytes(), "limit").unwrap();
            assert_eq!(text.len(), 1024 * 1024);
            assert_eq!(code, 0);
            assert_eq!(truncated, size > 1024 * 1024);
        }
    }

    #[test]
    fn mqtt_command_success_requires_a_confirmed_outcome() {
        // Given a result without a transport error but without execution proof.
        let mut result = CommandResult {
            target: "1A2B3C4DA111".into(),
            status: CommandStatus::Timeout,
            request_id: None,
            exit_code: None,
            output: None,
            output_truncated: false,
            error: None,
        };
        // When classifying each externally observable result; then unknown is not success.
        assert!(!result.success());
        result.status = CommandStatus::Executed;
        assert!(!result.success());
        result.exit_code = Some(0);
        assert!(result.success());
        result.exit_code = Some(7);
        assert!(!result.success());
        result.status = CommandStatus::Dispatched;
        result.exit_code = None;
        assert!(result.success());
    }

    #[test]
    fn shell_frame_preserves_script_and_reports_stderr_and_exit() {
        let script = "printf '%s\\n' \"single'quote\"; printf 'error\\n' >&2; exit 7";
        let result = Command::new("sh")
            .args(["-c", &framed_script(script, "test123")])
            .output()
            .unwrap();
        assert!(result.status.success());
        assert_eq!(
            parse_reply(&result.stdout, "test123"),
            Some(("single'quote\nerror\n".into(), 7, false))
        );
        assert!(parse_reply(&result.stdout, "previous_request").is_none());
        assert!(result.stderr.is_empty());
    }

    #[test]
    fn empty_output_still_has_a_response_and_binary_output_is_lossy_text() {
        let result = Command::new("sh")
            .args(["-c", &framed_script("exit 0", "empty")])
            .output()
            .unwrap();
        assert_eq!(
            parse_reply(&result.stdout, "empty"),
            Some((String::new(), 0, false))
        );
        assert!(parse_reply(
            b"__FTTR_BEGIN_empty__\nbody\n__FTTR_END_empty__=bad\n",
            "empty"
        )
        .is_none());
        assert_eq!(
            parse_reply(
                b"__FTTR_BEGIN_empty__\n\xff\n__FTTR_END_empty__=0\n",
                "empty"
            ),
            Some(("\u{fffd}".into(), 0, false))
        );
    }

    #[test]
    fn identity_and_advisory_lock_reject_ambiguous_commands() {
        assert_eq!(normalize_mac("1a:2b:3c:4d:a1:11").unwrap(), "1A2B3C4DA111");
        assert!(normalize_mac("H3CT1A2B3C4D").is_err());
        assert!(normalize_mac("../1A2B3C4DA111").is_err());
        let target = format!("TEST{}", request_id());
        let first = lock_target(&target).unwrap();
        assert_eq!(
            lock_target(&target).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        drop(first);
        lock_target(&target).unwrap();
        std::fs::remove_file(format!("/tmp/fttr-command-{target}.lock")).unwrap();
    }

    #[test]
    fn command_locks_are_isolated_per_device() {
        let suffix = request_id();
        let first_target = format!("TESTA{suffix}");
        let second_target = format!("TESTB{suffix}");
        let first = lock_target(&first_target).unwrap();
        let second = lock_target(&second_target).unwrap();

        assert!(lock_target(&first_target).is_err());
        assert!(lock_target(&second_target).is_err());

        drop((first, second));
        std::fs::remove_file(format!("/tmp/fttr-command-{first_target}.lock")).unwrap();
        std::fs::remove_file(format!("/tmp/fttr-command-{second_target}.lock")).unwrap();
    }
}

// Explicit unlock also releases descriptors briefly inherited by concurrent fork/exec.
#[cfg(any(feature = "mqtt", test))]
#[derive(Debug)]
pub(super) struct CommandLock(File);

#[cfg(any(feature = "mqtt", test))]
impl Drop for CommandLock {
    fn drop(&mut self) {
        extern "C" {
            fn flock(fd: i32, operation: i32) -> i32;
        }
        unsafe {
            flock(self.0.as_raw_fd(), 8);
        }
    }
}

// Advisory locks survive file reuse and are released automatically on process exit.
// Do not unlink the lock file: doing so would let clients lock different inodes.
#[cfg(any(feature = "mqtt", test))]
pub(super) fn lock_target(target: &str) -> io::Result<CommandLock> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(format!("/tmp/fttr-command-{target}.lock"))?;
    extern "C" {
        fn flock(fd: i32, operation: i32) -> i32;
    }
    if unsafe { flock(file.as_raw_fd(), 2 | 4) } != 0 {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            format!("another fttrctl command is active for {target}"),
        ));
    }
    Ok(CommandLock(file))
}

/// QoS 0 dispatch only. Success reports a socket write, not device execution.
#[cfg(feature = "mqtt")]
pub fn dispatch(target: &str, command: &str) -> io::Result<CommandResult> {
    let target = if target == "all" {
        "all".to_string()
    } else {
        normalize_mac(target)?
    };
    if command.trim().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "command must not be empty",
        ));
    }
    let _lock = lock_target(&target)?;
    super::command_transport::blocking(async {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let mut session = CommandSession::connect(
            "127.0.0.1:1883".parse().unwrap(),
            &format!("fttr-cmd-{}", request_id()),
            deadline,
        )
        .await
        .map_err(io::Error::other)?;
        session
            .enqueue_publish(&format!("/fttr/subdev/cmd/{target}"), command.as_bytes())
            .map_err(io::Error::other)?;
        session.flushed(deadline).await.map_err(io::Error::other)
    })?;
    Ok(CommandResult {
        target,
        status: CommandStatus::Dispatched,
        request_id: None,
        exit_code: None,
        output: None,
        output_truncated: false,
        error: None,
    })
}

/// Wait for a fresh, matching response frame containing combined output and exit code.
#[cfg(feature = "mqtt")]
pub fn execute(target: &str, command: &str, timeout: Duration) -> io::Result<CommandResult> {
    crate::runtime::block_on(execute_async(target, command, timeout))
}

#[cfg(feature = "mqtt")]
pub async fn execute_async(
    target: &str,
    command: &str,
    timeout: Duration,
) -> io::Result<CommandResult> {
    let target = normalize_mac(target)?;
    if command.trim().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "command must not be empty",
        ));
    }
    let _lock = lock_target(&target)?;
    let id = request_id();
    super::command_transport::run(
        "127.0.0.1:1883".parse().unwrap(),
        super::command_transport::Request {
            target,
            script: command,
            id,
            timeout,
        },
        super::command_transport::enqueue,
    )
    .await
}

#[cfg(not(feature = "mqtt"))]
pub fn dispatch(_: &str, _: &str) -> io::Result<CommandResult> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "MQTT commands unavailable: build requires mqtt",
    ))
}

#[cfg(not(feature = "mqtt"))]
pub fn execute(_: &str, _: &str, _: Duration) -> io::Result<CommandResult> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "MQTT commands unavailable: build requires mqtt",
    ))
}
