use super::command::{framed_script, parse_reply, CommandResult, CommandStatus};
use crate::mqtt::{MqttClient, MqttEvent};
use std::io;
use std::time::{Duration, Instant};

pub(super) trait Transport {
    fn subscribe(&mut self, topic: &str, timeout: Duration) -> io::Result<()>;
    fn publish(&mut self, topic: &str, payload: &[u8]) -> io::Result<()>;
    fn receive(&mut self, timeout: Duration) -> io::Result<Option<MqttEvent>>;
    fn ping(&mut self) -> io::Result<()>;
}

impl Transport for MqttClient {
    fn subscribe(&mut self, topic: &str, timeout: Duration) -> io::Result<()> {
        self.subscribe_confirmed(topic, timeout)
    }
    fn publish(&mut self, topic: &str, payload: &[u8]) -> io::Result<()> {
        MqttClient::publish(self, topic, payload)
    }
    fn receive(&mut self, timeout: Duration) -> io::Result<Option<MqttEvent>> {
        self.set_read_timeout(timeout)?;
        self.read_event()
    }
    fn ping(&mut self) -> io::Result<()> { MqttClient::ping(self) }
}

pub(super) struct Request<'a> {
    pub target: String,
    pub script: &'a str,
    pub id: String,
    pub timeout: Duration,
}

pub(super) fn run(transport: &mut impl Transport, request: Request<'_>) -> io::Result<CommandResult> {
    let topic = format!("/fttr/subdev/exec/{}", request.target);
    let reply_topic = format!("{topic}/resp");
    transport.subscribe(&reply_topic, request.timeout)?;
    transport.publish(&topic, framed_script(request.script, &request.id).as_bytes())?;
    let deadline = Instant::now() + request.timeout;
    let mut heartbeat = Instant::now();
    while Instant::now() < deadline {
        if heartbeat.elapsed() >= Duration::from_secs(20) {
            transport.ping()?;
            heartbeat = Instant::now();
        }
        if let Some(MqttEvent::Publish { topic, payload, retain: false }) =
            transport.receive(deadline.saturating_duration_since(Instant::now()))? {
            if topic == reply_topic {
                if let Some((output, code, output_truncated)) = parse_reply(&payload, &request.id) {
                    return Ok(CommandResult { target: request.target, status: CommandStatus::Executed,
                        request_id: Some(request.id), exit_code: Some(code), output: Some(output),
                        output_truncated, error: None });
                }
            }
        }
    }
    Ok(CommandResult { target: request.target, status: CommandStatus::Timeout,
        request_id: Some(request.id), exit_code: None, output: None, output_truncated: false,
        error: Some("no matching device response before timeout; execution outcome is unknown".into()) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    #[derive(Default)]
    struct Broker {
        subscribed: bool,
        publications: Vec<(String, Vec<u8>)>,
        replies: VecDeque<MqttEvent>,
        reject_subscription: bool,
    }

    impl Transport for Broker {
        fn subscribe(&mut self, _: &str, _: Duration) -> io::Result<()> {
            if self.reject_subscription {
                return Err(io::Error::new(io::ErrorKind::PermissionDenied, "rejected"));
            }
            self.subscribed = true;
            Ok(())
        }
        fn publish(&mut self, topic: &str, payload: &[u8]) -> io::Result<()> {
            assert!(self.subscribed);
            self.publications.push((topic.into(), payload.to_vec()));
            Ok(())
        }
        fn receive(&mut self, _: Duration) -> io::Result<Option<MqttEvent>> {
            Ok(self.replies.pop_front())
        }
        fn ping(&mut self) -> io::Result<()> { Ok(()) }
    }

    fn reply(topic: &str, id: &str, retained: bool) -> MqttEvent {
        MqttEvent::Publish { topic: topic.into(), retain: retained,
            payload: format!("__FTTR_BEGIN_{id}__\n猫\n__FTTR_END_{id}__=7\n").into_bytes() }
    }

    fn request(timeout: Duration) -> Request<'static> {
        Request { target: "1A2B3C4DA111".into(), script: "printf '猫';\n$(id); `id`",
            id: "current".into(), timeout }
    }

    #[test]
    fn mqtt_command_transport_ignores_retained_stale_and_wrong_topic_replies() {
        let topic = "/fttr/subdev/exec/1A2B3C4DA111/resp";
        let mut broker = Broker { replies: VecDeque::from([
            reply(topic, "current", true), reply(topic, "old", false),
            reply("/different/resp", "current", false), reply(topic, "current", false),
        ]), ..Broker::default() };
        let result = run(&mut broker, request(Duration::from_secs(1))).unwrap();
        assert_eq!(result.output.as_deref(), Some("猫"));
        assert_eq!(result.exit_code, Some(7));
        assert!(!result.success());
        assert_eq!(broker.publications.len(), 1);
        assert_eq!(broker.publications[0].0, "/fttr/subdev/exec/1A2B3C4DA111");
        assert!(String::from_utf8_lossy(&broker.publications[0].1).contains("$(id); `id`"));
    }

    #[test]
    fn mqtt_command_transport_timeout_does_not_republish() {
        let mut broker = Broker::default();
        let result = run(&mut broker, request(Duration::ZERO)).unwrap();
        assert_eq!(result.status, CommandStatus::Timeout);
        assert!(!result.success());
        assert_eq!(broker.publications.len(), 1);
        assert!(result.error.unwrap().contains("outcome is unknown"));
    }

    #[test]
    fn mqtt_command_transport_subscription_rejection_prevents_publication() {
        let mut broker = Broker { reject_subscription: true, ..Broker::default() };
        assert_eq!(run(&mut broker, request(Duration::from_secs(1))).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied);
        assert!(broker.publications.is_empty());
    }

    #[test]
    fn mqtt_command_transport_round_trips_fragmented_tcp_packets() {
        use std::io::{Read, Write};
        use std::net::{TcpListener, TcpStream};
        use mqttrs2::{decode_slice_with_len, encode_slice, Packet, Publish, QosPid};

        fn packet(stream: &mut TcpStream) -> Vec<u8> {
            let mut first = [0];
            stream.read_exact(&mut first).unwrap();
            let mut bytes = vec![first[0]];
            let mut remaining = 0usize;
            for index in 0..4 {
                stream.read_exact(&mut first).unwrap();
                bytes.push(first[0]);
                remaining += usize::from(first[0] & 0x7f) << (7 * index);
                if first[0] & 0x80 == 0 { break; }
            }
            assert!(remaining < 4096);
            let header = bytes.len();
            bytes.resize(header + remaining, 0);
            stream.read_exact(&mut bytes[header..]).unwrap();
            bytes
        }

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            assert_eq!(packet(&mut stream)[0] >> 4, 1);
            stream.write_all(&[0x20, 2, 0, 0]).unwrap();
            assert_eq!(packet(&mut stream)[0] >> 4, 8);
            stream.write_all(&[0x90, 3, 0, 1, 0]).unwrap();
            let published = packet(&mut stream);
            let (_, Packet::Publish(published)) = decode_slice_with_len(&published).unwrap().unwrap()
                else { panic!("expected command publication"); };
            assert_eq!(published.topic_name, "/fttr/subdev/exec/1A2B3C4DA111");
            assert!(std::str::from_utf8(published.payload).unwrap().contains("$(id); `id`"));
            let response = Packet::Publish(Publish { dup: false, retain: false,
                qospid: QosPid::AtMostOnce, topic_name: "/fttr/subdev/exec/1A2B3C4DA111/resp",
                payload: "__FTTR_BEGIN_current__\n猫\n__FTTR_END_current__=7\n".as_bytes() });
            let mut encoded = [0; 256];
            let size = encode_slice(&response, &mut encoded).unwrap();
            for chunk in encoded[..size].chunks(3) { stream.write_all(chunk).unwrap(); }
        });
        let mut client = MqttClient::connect(&address.to_string(), "command-wire-test").unwrap();
        let result = run(&mut client, request(Duration::from_secs(2))).unwrap();
        assert_eq!(result.output.as_deref(), Some("猫"));
        assert_eq!(result.exit_code, Some(7));
        worker.join().unwrap();
    }
}
