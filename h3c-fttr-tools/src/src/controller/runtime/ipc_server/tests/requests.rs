use super::*;

#[test]
fn production_dispatch_retains_status_and_adapter_routes() {
    let handler = super::super::super::ipc_handler(
        Arc::new(Mutex::new(Vec::new())),
        Arc::new(Mutex::new(crate::pon_engine::PonEngine::new())),
        Arc::new(Mutex::new(std::collections::BTreeMap::new())),
    );
    let server = Server::new(5, handler);

    let responses: Vec<serde_json::Value> =
        ["STATUS", "ACS {", "PROVISIONING {", "HL202 {", "MIB {"]
            .into_iter()
            .map(|command| {
                serde_json::from_str(&server.request(format!("{command}\n").as_bytes())).unwrap()
            })
            .collect();

    assert!(responses[0]["switch_ports"].is_array());
    for response in &responses[1..] {
        assert!(!response["error"].is_null(), "{response}");
    }
}

#[test]
fn request_boundary_rejects_eof_invalid_utf8_and_oversized_lines() {
    for input in [
        b"STATUS".to_vec(),
        vec![255, b'\n'],
        vec![b'x'; MAX_REQUEST_BYTES],
    ] {
        let (mut client, stream) = UnixStream::pair().unwrap();
        let writer = thread::spawn(move || {
            client.write_all(&input).unwrap();
            client.shutdown(Shutdown::Write).unwrap();
        });

        let result = read_request(Connection {
            stream,
            accepted: Instant::now(),
        });

        assert!(result.is_err());
        writer.join().unwrap();
    }
}

#[test]
fn request_at_byte_limit_retains_first_line_routing() {
    let (mut client, stream) = UnixStream::pair().unwrap();
    let writer = thread::spawn(move || {
        let mut line = vec![b'x'; MAX_REQUEST_BYTES - 1];
        line.push(b'\n');
        client.write_all(&line).unwrap();
    });

    let request = read_request(Connection {
        stream,
        accepted: Instant::now(),
    })
    .unwrap();

    assert_eq!(request.line.len(), MAX_REQUEST_BYTES);
    writer.join().unwrap();
}

#[test]
fn expired_request_deadline_rejects_queued_connections() {
    let (_client, stream) = UnixStream::pair().unwrap();

    let result = read_request(Connection {
        stream,
        accepted: Instant::now() - SOCKET_TIMEOUT,
    });

    assert_eq!(result.err().unwrap().kind(), io::ErrorKind::TimedOut);
}

#[test]
fn stalled_request_read_expires_at_the_total_deadline() {
    let (_client, stream) = UnixStream::pair().unwrap();

    let result = read_request(Connection {
        stream,
        accepted: Instant::now() - SOCKET_TIMEOUT + Duration::from_millis(50),
    });

    assert!(matches!(
        result.err().unwrap().kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    ));
}

#[test]
fn busy_is_returned_when_all_intake_slots_are_stalled() {
    let count = INTAKE_WORKERS * 2 + 1;
    let server = Server::new(count, |_, _| panic!("incomplete request reached handler"));
    let (responded, responses) = channel();
    let mut clients = Vec::new();
    let mut readers = Vec::new();
    for _ in 0..count {
        let client = server.connect();
        let stream = client.try_clone().unwrap();
        let responded = responded.clone();
        readers.push(thread::spawn(move || {
            responded.send(reply(stream)).unwrap()
        }));
        clients.push(client);
    }

    let response = responses.recv_timeout(Duration::from_secs(2)).unwrap();

    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&response).unwrap()["error"]["code"],
        "busy"
    );
    for client in clients {
        client.shutdown(Shutdown::Both).unwrap();
    }
    for reader in readers {
        reader.join().unwrap();
    }
}
