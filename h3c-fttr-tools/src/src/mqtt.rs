use std::convert::TryFrom;
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};
use mqttrs2::*;
use crate::mqtt_limits;

const IDLE_READ_CAPACITY: usize = 4096;
const LARGE_READ_CAPACITY: usize = 64 * 1024;

#[cfg(test)]
#[path = "mqtt/buffer_tests.rs"]
mod buffer_tests;

/// A lightweight, robust MQTT 3.1.1 client using `mqttrs2` protocol codec
/// over a standard synchronous `TcpStream`.
pub struct MqttClient {
    stream: TcpStream,
    read_buf: Vec<u8>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn subscribe_requires_successful_fragmented_suback() {
        for code in [0u8, 0x80] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let server = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                let mut bytes = [0u8; 1024];
                let mut received = Vec::new();
                for expected in [1, 8] {
                    loop {
                        let n = stream.read(&mut bytes).unwrap();
                        assert_ne!(n, 0);
                        received.extend_from_slice(&bytes[..n]);
                        if let Some((used, _)) = decode_slice_with_len(&received).unwrap() {
                            assert_eq!(received[0] >> 4, expected);
                            received.drain(..used);
                            break;
                        }
                    }
                    if expected == 1 { stream.write_all(&[0x20, 2, 0, 0]).unwrap(); }
                    else {
                        stream.write_all(&[0x90, 3]).unwrap();
                        thread::sleep(Duration::from_millis(10));
                        stream.write_all(&[0, 1, code]).unwrap();
                    }
                }
            });
            let mut client = MqttClient::connect(&addr.to_string(), "subscribe-test").unwrap();
            let result = client.subscribe_confirmed("/test/resp", Duration::from_secs(1));
            if code == 0 { assert!(result.is_ok()); }
            else { assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied); }
            server.join().unwrap();
        }
    }
}

pub enum MqttEvent {
    Publish { topic: String, payload: Vec<u8>, retain: bool },
    Suback { pid: Pid, accepted: bool },
    Other,
}

impl MqttClient {
    fn reclaim_read_buffer(&mut self) {
        if self.read_buf.capacity() > LARGE_READ_CAPACITY && self.read_buf.len() <= IDLE_READ_CAPACITY {
            self.read_buf.shrink_to(IDLE_READ_CAPACITY);
        }
    }

    /// Connects to the local/remote MQTT broker and completes the MQTT 3.1.1 handshake.
    pub fn connect(addr: &str, client_id: &str) -> io::Result<Self> {
        let mut stream = TcpStream::connect(addr)?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;

        // 1. Build and serialize CONNECT packet
        let connect = Connect {
            protocol: Protocol::MQTT311,
            keep_alive: 60,
            client_id,
            clean_session: true,
            last_will: None,
            username: None,
            password: None,
        };

        let mut send_buf = vec![0u8; 128 + client_id.len()];
        let n = encode_slice(&Packet::Connect(connect), &mut send_buf)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("MQTT connect encode error: {e:?}")))?;
        stream.write_all(&send_buf[..n])?;

        // 2. Read and validate CONNACK packet
        let mut ack_buf = [0u8; 4];
        stream.read_exact(&mut ack_buf)?;
        let (_, pkt) = decode_slice_with_len(&ack_buf)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("MQTT connack decode error: {e:?}")))?
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "Incomplete CONNACK packet"))?;

        match pkt {
            Packet::Connack(connack) => {
                if connack.code != ConnectReturnCode::Accepted {
                    return Err(io::Error::new(
                        io::ErrorKind::ConnectionRefused,
                        format!("MQTT connection rejected, return code: {:?}", connack.code),
                    ));
                }
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Expected CONNACK packet from broker",
                ));
            }
        }

        // Configure short timeout for polling in main worker loop
        stream.set_read_timeout(Some(Duration::from_millis(500)))?;

        Ok(MqttClient {
            stream,
            read_buf: Vec::with_capacity(4096),
        })
    }

    /// Clones the underlying socket handle (useful for concurrent publishing while listening)
    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(MqttClient {
            stream: self.stream.try_clone()?,
            read_buf: Vec::new(),
        })
    }

    /// Subscribes to a given MQTT topic path with QoS 0
    pub fn subscribe(&mut self, topic: &str) -> io::Result<()> {
        let sub = Subscribe {
            pid: Pid::try_from(1).unwrap_or_default(),
            topics: vec![SubscribeTopic {
                topic_path: topic.to_string(),
                qos: QoS::AtMostOnce,
            }],
        };

        let mut send_buf = vec![0u8; 128 + topic.len()];
        let n = encode_slice(&Packet::Subscribe(sub), &mut send_buf)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("MQTT subscribe encode error: {e:?}")))?;
        self.stream.write_all(&send_buf[..n])
    }

    /// Dedicated request clients must confirm the subscription before publishing.
    pub fn subscribe_confirmed(&mut self, topic: &str, timeout: Duration) -> io::Result<()> {
        self.subscribe(topic)?;
        let deadline = Instant::now() + timeout;
        loop {
            self.set_read_timeout(deadline.saturating_duration_since(Instant::now()))?;
            if let Some(MqttEvent::Suback { pid, accepted }) = self.read_event()? {
                if pid == Pid::try_from(1).unwrap() {
                    return if accepted { Ok(()) } else {
                        Err(io::Error::new(io::ErrorKind::PermissionDenied, "MQTT subscription rejected"))
                    };
                }
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "MQTT SUBACK timeout"));
            }
        }
    }

    pub fn set_read_timeout(&self, timeout: Duration) -> io::Result<()> {
        self.stream.set_read_timeout(Some(timeout.max(Duration::from_millis(1)).min(Duration::from_millis(500))))
    }

    /// Publishes a payload to the designated topic with QoS 0
    pub fn publish(&mut self, topic: &str, data: &[u8]) -> io::Result<()> {
        let publish = Publish {
            dup: false,
            qospid: QosPid::AtMostOnce,
            retain: false,
            topic_name: topic,
            payload: data,
        };

        let mut send_buf = vec![0u8; mqtt_limits::publish_size(topic.len(), data.len())?];
        let n = encode_slice(&Packet::Publish(publish), &mut send_buf)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("MQTT publish encode error: {e:?}")))?;
        self.stream.write_all(&send_buf[..n])
    }

    /// Sends a PINGREQ heartbeat packet (2 bytes)
    pub fn ping(&mut self) -> io::Result<()> {
        let mut send_buf = [0u8; 4];
        let n = encode_slice(&Packet::Pingreq, &mut send_buf)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("MQTT pingreq encode error: {e:?}")))?;
        self.stream.write_all(&send_buf[..n])
    }

    /// Streams and decodes incoming packets with full TCP framing / fragmentation handling.
    /// Returns `Ok(Some((topic, payload)))` when a PUBLISH packet arrives.
    /// Returns `Ok(None)` on read timeout (no data available right now).
    pub fn read_packet(&mut self) -> io::Result<Option<(String, Vec<u8>)>> {
        loop {
            match self.read_event()? {
                Some(MqttEvent::Publish { topic, payload, .. }) => return Ok(Some((topic, payload))),
                Some(_) => continue,
                None => return Ok(None),
            }
        }
    }

    pub fn read_event(&mut self) -> io::Result<Option<MqttEvent>> {
        loop {
            // 1. Process any complete packets already buffered in `read_buf`
            if !self.read_buf.is_empty() {
                if let Err(error) = mqtt_limits::declared_size(&self.read_buf) {
                    self.read_buf.clear();
                    self.reclaim_read_buffer();
                    return Err(error);
                }
                match decode_slice_with_len(&self.read_buf) {
                    Ok(Some((consumed_len, packet))) => {
                        let extracted_msg = match packet {
                            Packet::Publish(p) => MqttEvent::Publish {
                                topic: p.topic_name.to_string(), payload: p.payload.to_vec(), retain: p.retain,
                            },
                            Packet::Suback(s) => MqttEvent::Suback {
                                pid: s.pid,
                                accepted: s.return_codes.len() == 1 && matches!(s.return_codes[0], SubscribeReturnCodes::Success(_)),
                            },
                            _ => MqttEvent::Other,
                        };
                        // Now packet borrow ends, we can drain the consumed bytes
                        self.read_buf.drain(..consumed_len);
                        self.reclaim_read_buffer();

                        return Ok(Some(extracted_msg));
                    }
                    Ok(None) => {
                        // Incomplete packet in buffer; proceed to read more bytes from socket
                    }
                    Err(e) => {
                        // Protocol framing error; clear buffer to prevent deadlocks
                        self.read_buf.clear();
                        self.reclaim_read_buffer();
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!("MQTT stream decode error: {e:?}"),
                        ));
                    }
                }
            }

            // 2. Read incoming chunk from the TCP stream
            let mut chunk = [0u8; 2048];
            let available = mqtt_limits::MAX_PACKET_BYTES.saturating_sub(self.read_buf.len());
            if available == 0 {
                self.read_buf.clear();
                self.reclaim_read_buffer();
                return Err(io::Error::new(io::ErrorKind::InvalidData, "MQTT packet buffer limit exceeded"));
            }
            let capacity = available.min(chunk.len());
            match self.stream.read(&mut chunk[..capacity]) {
                Ok(0) => {
                    self.read_buf.clear();
                    self.reclaim_read_buffer();
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "MQTT broker closed connection",
                    ));
                }
                Ok(n) => {
                    self.read_buf.extend_from_slice(&chunk[..n]);
                    // Loop back to decode the newly accumulated bytes
                }
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::TimedOut => {
                    return Ok(None);
                }
                Err(e) => {
                    self.read_buf.clear();
                    self.reclaim_read_buffer();
                    return Err(e);
                }
            }
        }
    }
}
