use std::{
    io::{self, Read},
    os::fd::AsRawFd,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

/// Maximum bytes read from synchronous command output to prevent memory blowup.
const MAX_SYNC_OUTPUT_BYTES: u64 = 65536;

/// Bounded synchronous process execution with timeout and output limit.
/// Drain a nonblocking pipe while the child runs, including after it exits.
pub fn run_sync(program: &str, args: &[&str], timeout: Duration) -> io::Result<String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;

    let result = (|| {
        let mut stdout = child.stdout.take().unwrap();
        let fd = stdout.as_raw_fd();
        // SAFETY: stdout owns a live pipe descriptor throughout these calls.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return Err(io::Error::last_os_error());
        }
        let deadline = Instant::now() + timeout;
        let mut bytes = Vec::new();
        let mut buffer = [0; 4096];
        let mut eof = false;
        loop {
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("{program} timed out after {timeout:?}"),
                ));
            }
            if !eof {
                match stdout.read(&mut buffer) {
                    Ok(0) => eof = true,
                    Ok(n) => {
                        bytes.extend_from_slice(&buffer[..n]);
                        if bytes.len() as u64 > MAX_SYNC_OUTPUT_BYTES {
                            return Err(io::Error::other("command exceeds output limit"));
                        }
                        continue;
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => (),
                    Err(error) => return Err(error),
                }
            }
            if let Some(status) = child.try_wait()? {
                if !status.success() {
                    return Err(io::Error::other(format!("{program}: exited with {status}")));
                }
                if eof {
                    return String::from_utf8(bytes).map_err(io::Error::other);
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    })();
    if result.is_err() {
        let _ = child.kill();
    }
    child.wait()?;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_drains_pipe_and_rejects_truncation() {
        let output = run_sync(
            "head",
            &["-c", "60000", "/dev/zero"],
            Duration::from_secs(2),
        )
        .unwrap();
        assert_eq!(output.len(), 60000);
        let error = run_sync(
            "head",
            &["-c", "65537", "/dev/zero"],
            Duration::from_secs(2),
        )
        .unwrap_err();
        assert!(error.to_string().contains("output limit"));
    }

    #[test]
    fn sync_bounds_quiet_and_inherited_pipe_hangs() {
        for args in [&["-c", "exec sleep 2"][..], &["-c", "sleep 2 & exit 0"][..]] {
            let start = Instant::now();
            let error = run_sync("sh", args, Duration::from_millis(100)).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::TimedOut);
            assert!(start.elapsed() < Duration::from_secs(1));
        }
        assert!(run_sync("sh", &["-c", "exit 1"], Duration::from_secs(1)).is_err());
    }
}

/// Bounded asynchronous process execution with timeout.
/// Killing a dropped child ensures task cancellation doesn't leak orphan processes.
pub async fn run_async(program: &str, args: &[&str], timeout: Duration) -> io::Result<String> {
    let output = tokio::time::timeout(
        timeout,
        tokio::process::Command::new(program)
            .args(args)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| {
        io::Error::new(
            io::ErrorKind::TimedOut,
            format!("{program} timed out after {timeout:?}"),
        )
    })??;

    if !output.status.success() {
        return Err(io::Error::other(format!("{program}: {}", output.status)));
    }
    String::from_utf8(output.stdout).map_err(io::Error::other)
}
