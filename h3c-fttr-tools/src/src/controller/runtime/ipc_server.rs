use std::io::{self, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

pub(super) mod metrics;

const INTAKE_WORKERS: usize = 2;
const HANDLER_WORKERS: usize = 2;
const STATUS_WORKERS: usize = 1;
const SOCKET_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_REQUEST_BYTES: usize = 524296;
const BUSY: &[u8] =
    b"{\"schema_version\":1,\"error\":{\"code\":\"busy\",\"message\":\"IPC workers busy\"}}\n";

struct Connection {
    stream: UnixStream,
    accepted: Instant,
}

struct Request {
    stream: UnixStream,
    line: String,
}

pub(super) fn start(
    listener: UnixListener,
    handler: impl Fn(UnixStream, &str) + Send + Sync + 'static,
) {
    if let Err(error) = thread::Builder::new()
        .name("ipc-accept".into())
        .stack_size(256 * 1024)
        .spawn(move || {
            serve(listener.incoming(), handler);
        })
    {
        eprintln!("[-] IPC accept startup failed: {error}");
    }
}

fn serve(
    streams: impl Iterator<Item = io::Result<UnixStream>>,
    handler: impl Fn(UnixStream, &str) + Send + Sync + 'static,
) {
    let handler = Arc::new(handler);
    let dispatch = {
        let handler = Arc::clone(&handler);
        move |request: Request| {
            let started = Instant::now();
            handler(request.stream, request.line.trim());
            metrics::handled(started.elapsed());
        }
    };
    let handlers = match pool(HANDLER_WORKERS, dispatch) {
        Ok(pool) => pool,
        Err(error) => {
            eprintln!("[-] IPC handler startup failed: {error}");
            return;
        }
    };
    let status = match pool(STATUS_WORKERS, move |request: Request| {
        let started = Instant::now();
        handler(request.stream, request.line.trim());
        metrics::handled(started.elapsed());
    }) {
        Ok(pool) => pool,
        Err(error) => {
            eprintln!("[-] IPC status startup failed: {error}");
            return;
        }
    };
    let intake = match pool(
        INTAKE_WORKERS,
        move |connection: Connection| match read_request(connection) {
            Ok(request) => {
                let target = if request.line.trim() == "STATUS" {
                    &status
                } else {
                    &handlers
                };
                match target.try_send(request) {
                    Ok(()) => {}
                    Err(TrySendError::Full(request) | TrySendError::Disconnected(request)) => {
                        reject(request.stream)
                    }
                }
            }
            Err(error) => {
                metrics::read_error();
                eprintln!("[-] IPC request read failed: {error}");
            }
        },
    ) {
        Ok(pool) => pool,
        Err(error) => {
            eprintln!("[-] IPC intake startup failed: {error}");
            return;
        }
    };
    for stream in streams {
        match stream {
            Ok(stream) => {
                metrics::accepted();
                match intake.try_send(Connection {
                    stream,
                    accepted: Instant::now(),
                }) {
                    Ok(()) => {}
                    Err(
                        TrySendError::Full(connection) | TrySendError::Disconnected(connection),
                    ) => reject(connection.stream),
                }
            }
            Err(error) => eprintln!("[-] IPC accept failed: {error}"),
        }
    }
}

fn pool<Job: Send + 'static>(
    workers: usize,
    handle: impl Fn(Job) + Send + Sync + 'static,
) -> io::Result<SyncSender<Job>> {
    let (sender, receiver) = mpsc::sync_channel(workers);
    let receiver = Arc::new(Mutex::new(receiver));
    let handle = Arc::new(handle);
    for _ in 0..workers {
        let receiver = Arc::clone(&receiver);
        let handle = Arc::clone(&handle);
        thread::Builder::new()
            .name("ipc-worker".into())
            .stack_size(256 * 1024)
            .spawn(move || loop {
                let job = match receiver.lock() {
                    Ok(receiver) => receiver.recv(),
                    Err(_) => return,
                };
                match job {
                    Ok(job) => handle(job),
                    Err(_) => return,
                }
            })?;
    }
    Ok(sender)
}

fn read_request(connection: Connection) -> io::Result<Request> {
    let Connection {
        mut stream,
        accepted,
    } = connection;
    let deadline = accepted + SOCKET_TIMEOUT;
    let mut bytes = Vec::new();
    let mut buffer = [0; 8192];
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::TimedOut, "IPC request deadline exceeded")
            })?;
        stream.set_read_timeout(Some(remaining))?;
        let available = buffer.len().min(MAX_REQUEST_BYTES - bytes.len());
        let count = stream.read(&mut buffer[..available])?;
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "incomplete IPC request",
            ));
        }
        let received = &buffer[..count];
        if let Some(end) = received.iter().position(|byte| *byte == b'\n') {
            bytes.extend_from_slice(&received[..=end]);
            let line = String::from_utf8(bytes)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            stream.set_write_timeout(Some(SOCKET_TIMEOUT))?;
            metrics::intake(accepted.elapsed());
            return Ok(Request { stream, line });
        }
        bytes.extend_from_slice(received);
        if bytes.len() == MAX_REQUEST_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "IPC request exceeds byte limit",
            ));
        }
    }
}

fn reject(mut stream: UnixStream) {
    metrics::rejected();
    if let Err(error) = stream
        .set_nonblocking(true)
        .and_then(|()| stream.write_all(BUSY))
    {
        eprintln!("[-] IPC busy response failed: {error}");
    }
}

#[cfg(test)]
mod tests;
