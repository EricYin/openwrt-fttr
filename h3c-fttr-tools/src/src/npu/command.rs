use std::{
    io,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

/// Maximum bytes read from synchronous command output to prevent memory blowup.
const MAX_SYNC_OUTPUT_BYTES: u64 = 65536;

/// Bounded synchronous process execution with timeout and output limit.
/// Safe against hangs without requiring Unix domain socket hacks.
pub fn run_sync(program: &str, args: &[&str], timeout: Duration) -> io::Result<String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;

    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                return Err(io::Error::other(format!("{program}: exited with {status}")));
            }
            break;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("{program} timed out after {timeout:?}"),
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    let mut output = String::new();
    if let Some(stdout) = child.stdout.take() {
        use std::io::Read;
        stdout
            .take(MAX_SYNC_OUTPUT_BYTES)
            .read_to_string(&mut output)?;
    }
    Ok(output)
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
