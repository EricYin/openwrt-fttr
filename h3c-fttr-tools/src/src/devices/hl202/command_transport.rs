use super::command::{framed_script, parse_reply, CommandResult, CommandStatus};
use crate::mqtt::session::{self, CommandSession};
use rumqttc::{Event, Packet};
use std::{io, net::SocketAddr, time::Duration};
use tokio::time::Instant;

pub(super) struct Request<'a> {
    pub target: String,
    pub script: &'a str,
    pub id: String,
    pub timeout: Duration,
}

/// The only execution implementation, shared by direct and identity-guarded
/// callers. The synchronous enqueue callback must not wait or change the target.
pub(super) async fn run(
    address: SocketAddr,
    request: Request<'_>,
    enqueue: impl FnOnce(&mut CommandSession, &str, &[u8]) -> io::Result<()>,
) -> io::Result<CommandResult> {
    let topic = format!("/fttr/subdev/exec/{}", request.target);
    let reply_topic = format!("{topic}/resp");
    let mut session = CommandSession::connect(address, &format!("fttr-exec-{}", request.id), Instant::now() + Duration::from_secs(5))
        .await.map_err(io::Error::other)?;
    session.subscribe(&reply_topic, Instant::now() + request.timeout.max(Duration::from_secs(1)))
        .await.map_err(|error| {
            let kind = if error.contains("SUBACK") { io::ErrorKind::PermissionDenied } else { io::ErrorKind::Other };
            io::Error::new(kind, error)
        })?;
    enqueue(&mut session, &topic, framed_script(request.script, &request.id).as_bytes())?;
    session.flushed(Instant::now() + Duration::from_secs(5)).await.map_err(io::Error::other)?;
    let deadline = Instant::now() + request.timeout;
    while Instant::now() < deadline {
        match session.next(deadline).await {
            Ok(Event::Incoming(Packet::Publish(message))) if !message.retain && message.topic == reply_topic => {
                if let Some((output, code, output_truncated)) = parse_reply(&message.payload, &request.id) {
                    return Ok(CommandResult { target: request.target, status: CommandStatus::Executed,
                        request_id: Some(request.id), exit_code: Some(code), output: Some(output),
                        output_truncated, error: None });
                }
            }
            Err(_) if Instant::now() >= deadline => break,
            Err(error) => return Err(io::Error::other(error)),
            _ => (),
        }
    }
    Ok(CommandResult { target: request.target, status: CommandStatus::Timeout,
        request_id: Some(request.id), exit_code: None, output: None, output_truncated: false,
        error: Some("no matching device response before timeout; execution outcome is unknown".into()) })
}

pub(super) fn enqueue(session: &mut CommandSession, topic: &str, payload: &[u8]) -> io::Result<()> {
    session.enqueue_publish(topic, payload).map_err(io::Error::other)
}

pub(super) fn blocking<T>(future: impl std::future::Future<Output = io::Result<T>>) -> io::Result<T> {
    session::blocking(async { Ok(future.await) }).map_err(io::Error::other)?
}

#[cfg(test)]
mod tests;
