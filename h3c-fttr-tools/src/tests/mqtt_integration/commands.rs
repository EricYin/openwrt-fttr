use super::*;
use h3c_fttr_tools::mqtt::session;
use rumqttc::{AsyncClient, Event, Outgoing, Packet, QoS, SubscribeReasonCode};

static CLI_PORT: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn mqtt_commands_cli_preserves_output_json_exit_and_sequential_broadcast() {
    let _port = CLI_PORT.lock().unwrap();
    let mut addrs = addresses();
    addrs[0] = "127.0.0.1:1883".parse().unwrap();
    let (_fixture, rx) = child(&addrs);
    assert!(ready(&rx));
    let (tx, subscribed) = mpsc::sync_channel(1);
    let address = addrs[1];
    let peer = std::thread::spawn(move || session::blocking(async {
        let (client, mut events) = AsyncClient::new(session::options(address, "simulated-stock-command-peer"), 1);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        let mut expected = None;
        let mut topics = Vec::new();
        let mut pending_reply = false;
        loop {
            let event = tokio::time::timeout_at(deadline, events.poll()).await.map_err(|e| e.to_string())?.map_err(|e| e.to_string())?;
            match event {
                Event::Incoming(Packet::ConnAck(_)) => client.try_subscribe("/fttr/subdev/#", QoS::AtMostOnce).unwrap(),
                Event::Outgoing(Outgoing::Subscribe(id)) => expected = Some(id),
                Event::Incoming(Packet::SubAck(ack)) => {
                    assert_eq!(Some(ack.pkid), expected);
                    assert_eq!(ack.return_codes, [SubscribeReasonCode::Success(QoS::AtMostOnce)]);
                    tx.send(()).unwrap();
                }
                Event::Incoming(Packet::Publish(message)) if !message.topic.ends_with("/resp") => {
                    topics.push(message.topic.clone());
                    let script = std::str::from_utf8(&message.payload).unwrap();
                    if message.topic.contains("/exec/") {
                        let id = script.split("__FTTR_BEGIN_").nth(1).unwrap().split("__").next().unwrap();
                        let response = format!("__FTTR_BEGIN_{id}__\nfixture stdout\nfixture stderr\n\n__FTTR_END_{id}__=7\n");
                        client.try_publish(format!("{}/resp", message.topic), QoS::AtMostOnce, false, response).unwrap();
                        pending_reply = true;
                    } else {
                        assert_eq!(script, "fixture-only");
                    }
                }
                Event::Outgoing(Outgoing::Publish(0)) => {
                    assert!(pending_reply);
                    pending_reply = false;
                    if topics.len() == 5 { return Ok(topics); }
                }
                _ => (),
            }
        }
    }).unwrap());
    subscribed.recv_timeout(Duration::from_secs(5)).unwrap();
    let cli = |args: &[&str]| {
        let output = Command::new(env!("CARGO_BIN_EXE_fttrctl")).args(args).output().unwrap();
        println!("CLI {args:?}: exit={:?}, stdout={}, stderr={}", output.status.code(), String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        output
    };
    let json = cli(&["exec", "--json", "1a:2b:3c:4d:a1:11", "fixture-only"]);
    assert_eq!(json.status.code(), Some(7));
    let result: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(result["target"], "1A2B3C4DA111");
    assert_eq!(result["exit_code"], 7);
    assert_eq!(result["output"], "fixture stdout\nfixture stderr\n");
    let plain = cli(&["exec", "1A2B3C4DA111", "fixture-only"]);
    assert_eq!(plain.status.code(), Some(7));
    assert_eq!(plain.stdout, b"fixture stdout\nfixture stderr\n");
    let dispatched = cli(&["cmd", "1A2B3C4DA111", "fixture-only"]);
    assert!(dispatched.status.success());
    assert!(String::from_utf8_lossy(&dispatched.stdout).contains("dispatched (device execution not confirmed)"));
    let broadcast = cli(&["broadcast", "--wait", "--json", "--targets", "1A2B3C4DA111,1A2B3C4DA112", "fixture-only"]);
    assert_eq!(broadcast.status.code(), Some(1));
    let results: serde_json::Value = serde_json::from_slice(&broadcast.stdout).unwrap();
    assert_eq!(results.as_array().unwrap().len(), 2);
    let topics = peer.join().unwrap();
    assert_eq!(topics, ["/fttr/subdev/exec/1A2B3C4DA111", "/fttr/subdev/exec/1A2B3C4DA111", "/fttr/subdev/cmd/1A2B3C4DA111", "/fttr/subdev/exec/1A2B3C4DA111", "/fttr/subdev/exec/1A2B3C4DA112"]);
}

#[test]
fn mqtt_commands_process_locks_reject_same_target_allow_other_target() {
    let _port = CLI_PORT.lock().unwrap();
    let mut addrs = addresses();
    addrs[0] = "127.0.0.1:1883".parse().unwrap();
    let (_fixture, rx) = child(&addrs);
    assert!(ready(&rx));
    let (observed_tx, observed) = mpsc::sync_channel(2);
    let (ready_tx, subscribed) = mpsc::sync_channel(1);
    let (release, release_rx) = mpsc::sync_channel(1);
    let address = addrs[1];
    let peer = std::thread::spawn(move || session::blocking(async {
        let (client, mut events) = AsyncClient::new(session::options(address, "simulated-lock-peer"), 1);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        let mut replies = Vec::new();
        let mut flushed = 0;
        loop {
            match tokio::time::timeout_at(deadline, events.poll()).await.map_err(|e| e.to_string())?.map_err(|e| e.to_string())? {
                Event::Incoming(Packet::ConnAck(_)) => client.try_subscribe("/fttr/subdev/exec/+", QoS::AtMostOnce).unwrap(),
                Event::Incoming(Packet::SubAck(ack)) => {
                    assert_eq!(ack.return_codes, [SubscribeReasonCode::Success(QoS::AtMostOnce)]);
                    ready_tx.send(()).unwrap();
                }
                Event::Incoming(Packet::Publish(message)) => {
                    let script = std::str::from_utf8(&message.payload).unwrap();
                    let id = script.split("__FTTR_BEGIN_").nth(1).unwrap().split("__").next().unwrap();
                    replies.push((format!("{}/resp", message.topic), format!("__FTTR_BEGIN_{id}__\nlock fixture\n\n__FTTR_END_{id}__=0\n")));
                    observed_tx.send(message.topic).unwrap();
                    if replies.len() == 2 {
                        release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                        let (topic, payload) = replies.remove(0);
                        client.try_publish(topic, QoS::AtMostOnce, false, payload).unwrap();
                    }
                }
                Event::Outgoing(Outgoing::Publish(0)) => {
                    flushed += 1;
                    if flushed == 2 { return Ok(()); }
                    let (topic, payload) = replies.remove(0);
                    client.try_publish(topic, QoS::AtMostOnce, false, payload).unwrap();
                }
                _ => (),
            }
        }
    }).unwrap());
    subscribed.recv_timeout(Duration::from_secs(5)).unwrap();
    let spawn = |target: &str| Fixture(Command::new(env!("CARGO_BIN_EXE_fttrctl"))
        .args(["exec", "--json", target, "fixture-only"]).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap());
    let first = spawn("02AABBCCDD01");
    assert_eq!(observed.recv_timeout(Duration::from_secs(5)).unwrap(), "/fttr/subdev/exec/02AABBCCDD01");
    let duplicate = Command::new(env!("CARGO_BIN_EXE_fttrctl")).args(["exec", "02AABBCCDD01", "fixture-only"]).output().unwrap();
    assert!(!duplicate.status.success());
    assert!(String::from_utf8_lossy(&duplicate.stderr).contains("another fttrctl command is active"));
    let other = spawn("02AABBCCDD02");
    assert_eq!(observed.recv_timeout(Duration::from_secs(5)).unwrap(), "/fttr/subdev/exec/02AABBCCDD02");
    release.send(()).unwrap();
    for mut process in [first, other] {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = process.0.try_wait().unwrap() { assert!(status.success()); break; }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    peer.join().unwrap();
    assert!(observed.try_recv().is_err(), "duplicate process published a command");
}
