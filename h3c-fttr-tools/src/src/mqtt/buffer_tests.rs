use super::*;
use std::net::TcpListener;

fn client_with_buffer(buffer: Vec<u8>) -> (MqttClient, TcpStream) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let stream = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (peer, _) = listener.accept().unwrap();
    (MqttClient { stream, read_buf: buffer }, peer)
}

#[test]
fn large_buffer_is_reclaimed_when_packet_drain_leaves_small_partial_packet() {
    let mut buffer = Vec::with_capacity(2 * 1024 * 1024);
    buffer.extend_from_slice(&[0xd0, 0, 0x30]);
    let (mut client, _peer) = client_with_buffer(buffer);

    assert!(matches!(client.read_event().unwrap(), Some(MqttEvent::Other)));

    assert_eq!(client.read_buf, [0x30]);
    assert!(client.read_buf.capacity() <= IDLE_READ_CAPACITY);
}

#[test]
fn normal_buffer_is_reused_when_packet_is_drained() {
    let mut buffer = Vec::with_capacity(8192);
    buffer.extend_from_slice(&[0xd0, 0]);
    let (mut client, _peer) = client_with_buffer(buffer);
    let capacity = client.read_buf.capacity();

    client.read_event().unwrap();

    assert_eq!(client.read_buf.capacity(), capacity);
}

#[test]
fn large_buffer_is_reclaimed_when_decoder_rejects_packet() {
    let mut buffer = Vec::with_capacity(2 * 1024 * 1024);
    buffer.extend_from_slice(&[0, 0]);
    let (mut client, _peer) = client_with_buffer(buffer);

    assert_eq!(client.read_event().err().unwrap().kind(), io::ErrorKind::InvalidData);

    assert!(client.read_buf.is_empty());
    assert!(client.read_buf.capacity() <= IDLE_READ_CAPACITY);
}

#[test]
fn large_buffer_is_reclaimed_when_declared_size_is_rejected() {
    let mut buffer = Vec::with_capacity(2 * 1024 * 1024);
    buffer.extend_from_slice(&[0x30, 0xff, 0xff, 0xff, 0x7f]);
    let (mut client, _peer) = client_with_buffer(buffer);

    assert_eq!(client.read_event().err().unwrap().kind(), io::ErrorKind::InvalidData);

    assert!(client.read_buf.is_empty());
    assert!(client.read_buf.capacity() <= IDLE_READ_CAPACITY);
}

#[test]
fn large_buffer_is_reclaimed_when_connection_closes_mid_packet() {
    let mut buffer = Vec::with_capacity(2 * 1024 * 1024);
    buffer.push(0x30);
    let (mut client, peer) = client_with_buffer(buffer);
    drop(peer);

    assert_eq!(client.read_event().err().unwrap().kind(), io::ErrorKind::UnexpectedEof);

    assert!(client.read_buf.is_empty());
    assert!(client.read_buf.capacity() <= IDLE_READ_CAPACITY);
}

#[test]
fn live_publish_reclaims_buffer_and_preserves_following_packet() {
    let (mut client, mut peer) = client_with_buffer(Vec::new());
    let sender = std::thread::spawn(move || {
        let data = vec![b'x'; 256 * 1024];
        let packet = Packet::Publish(Publish {
            dup: false,
            qospid: QosPid::AtMostOnce,
            retain: false,
            topic_name: "test/report",
            payload: &data,
        });
        let mut encoded = vec![0; data.len() + 128];
        let size = encode_slice(&packet, &mut encoded).unwrap();
        peer.write_all(&encoded[..size]).unwrap();
        peer.write_all(&[0xd0, 0]).unwrap();
    });

    let event = client.read_event().unwrap().unwrap();

    match event {
        MqttEvent::Publish { topic, payload, retain } => {
            assert_eq!(topic, "test/report");
            assert_eq!(payload, vec![b'x'; 256 * 1024]);
            assert!(!retain);
        }
        _ => panic!("expected publish"),
    }
    assert!(client.read_buf.capacity() <= IDLE_READ_CAPACITY);
    assert!(matches!(client.read_event().unwrap(), Some(MqttEvent::Other)));
    sender.join().unwrap();
}
