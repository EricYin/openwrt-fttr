use super::*;
use serde_json::value::{to_raw_value, RawValue};

fn server() -> Arc<Server> {
    super::tests::listener_test_server()
}

fn task(id: u64) -> Task {
    Task {
        id, device: "compact-test".into(), method: "GetParameterValues".into(),
        args: json!({"names":["InternetGatewayDevice."]}), state: TaskState::Sent,
        created: now(), sent: Some(now()), result: None,
    }
}

#[test]
fn large_result_survives_response_save_load_and_direct_query() {
    let server = server();
    let parameters: Vec<_> = (0..24_000).map(|n| json!({
        "name":format!("InternetGatewayDevice.LANDevice.1.Test.{n}"),
        "value":format!("{n}:\"\\\n{}", "x".repeat(170)), "type":"xsd:string"
    })).collect();
    let expected = json!({"parameters":parameters});
    let mut state = State::default();
    state.tasks.insert(1, task(1));
    let mut message = soap::parse(&soap::envelope("urn:dslforum-org:cwmp-1-0", "fttr-1", "<cwmp:GetParameterValuesResponse/>")).unwrap();
    message.data = expected.clone();

    state.response("compact-test", &message).unwrap();
    server.save(&state).unwrap();
    let loaded = State::load(&server.state_file).unwrap();
    *server.state.lock().unwrap() = loaded;
    let reply = server.execute(json!({"op":"task","id":1}));

    assert!(reply.len() > 6 * 1024 * 1024);
    let reply: Value = serde_json::from_str(&reply).unwrap();
    assert_eq!(reply["result"], expected);
    assert_eq!(reply["state"], "completed");
    fs::remove_file(&server.state_file).unwrap();
}

#[test]
fn query_serializes_raw_result_without_a_dom_round_trip() {
    let server = server();
    let raw = r#"{ "parameters" : [ ], "number" : 1e+02 }"#;
    let mut record = task(1);
    record.state = TaskState::Completed;
    record.result = Some(RawValue::from_string(raw.into()).unwrap());
    server.state.lock().unwrap().tasks.insert(1, record);

    let reply = server.execute(json!({"op":"task","id":1}));

    assert!(reply.contains(raw));
}

#[test]
fn malformed_or_oversized_state_blocks_queries_mutation_and_replacement() {
    for contents in [b"{invalid".to_vec(), vec![b' '; 8 * 1024 * 1024 + 1]] {
        let server = server();
        fs::write(&server.state_file, &contents).unwrap();
        *server.state.lock().unwrap() = State::load_or_block(&server.state_file);

        let query: Value = serde_json::from_str(&server.execute(json!({"op":"task","id":1}))).unwrap();
        let reset: Value = serde_json::from_str(&server.execute(json!({"op":"reset_device_tasks","device":"compact-test"}))).unwrap();
        let saved = server.save(&server.state.lock().unwrap());
        let activated = transition(server.clone(), true, "127.0.0.1:0", "/cwmp", "", "", false);

        assert!(query["error"].as_str().unwrap().starts_with("state load:"));
        assert!(reset["error"].as_str().unwrap().starts_with("state load:"));
        assert!(saved.is_err());
        assert!(activated.is_err());
        assert_eq!(fs::read(&server.state_file).unwrap(), contents);
        fs::remove_file(&server.state_file).unwrap();
    }
}

#[test]
fn restart_marks_sent_unknown_without_losing_completed_results() {
    let server = server();
    let mut state = State::default();
    state.tasks.insert(7, task(7));
    let mut completed = task(8);
    completed.state = TaskState::Completed;
    completed.result = Some(to_raw_value(&json!({"status":"1","instance_number":"12"})).unwrap());
    state.tasks.insert(8, completed);
    server.save(&state).unwrap();

    let loaded = State::load(&server.state_file).unwrap();

    assert_eq!(loaded.tasks[&7].state, TaskState::Unknown);
    assert!(loaded.tasks[&7].result.as_ref().unwrap().get().contains("execution is unknown"));
    assert_eq!(loaded.tasks[&8].result.as_ref().unwrap().get(), state.tasks[&8].result.as_ref().unwrap().get());
    assert_eq!(loaded.next_id, 8);
    fs::remove_file(&server.state_file).unwrap();
}

#[test]
fn fault_and_add_object_fields_survive_raw_retention() {
    for (method, body, expected_state) in [
        ("SetParameterValues", "<soap:Fault><faultcode>Client</faultcode><faultstring>CWMP fault</faultstring><detail><cwmp:Fault><FaultCode>9003</FaultCode><FaultString>Invalid arguments</FaultString><SetParameterValuesFault><ParameterName>bad.name</ParameterName><FaultCode>9005</FaultCode><FaultString>Invalid parameter</FaultString></SetParameterValuesFault></cwmp:Fault></detail></soap:Fault>", TaskState::Fault),
        ("AddObject", "<cwmp:AddObjectResponse><InstanceNumber>12</InstanceNumber><Status>1</Status></cwmp:AddObjectResponse>", TaskState::AcceptedPendingApply),
    ] {
        let mut state = State::default();
        let mut record = task(1);
        record.method = method.into();
        state.tasks.insert(1, record);
        let message = soap::parse(&soap::envelope("urn:dslforum-org:cwmp-1-0", "fttr-1", body)).unwrap();
        let expected = message.data.clone();

        state.response("compact-test", &message).unwrap();

        assert_eq!(state.tasks[&1].state, expected_state);
        assert_eq!(serde_json::from_str::<Value>(state.tasks[&1].result.as_ref().unwrap().get()).unwrap(), expected);
    }
}

#[test]
fn wan_readback_guard_requires_eligible_fresh_non_management_result() {
    let prefix = "InternetGatewayDevice.WANDevice.1.WANConnectionDevice.3.WANIPConnection.1.";
    let mut state = State::default();
    let mut record = task(1);
    record.state = TaskState::Completed;
    record.result = Some(to_raw_value(&json!({"parameters":[{"name":format!("{prefix}X_CU_ServiceList"),"value":"INTERNET"}]})).unwrap());
    state.tasks.insert(1, record.clone());

    let observed = readback::observed(&state, "compact-test", prefix);

    assert_eq!(observed, Some(false));
    for (device, method, status, sent) in [
        ("other", "GetParameterValues", TaskState::Completed, now()),
        ("compact-test", "GetParameterNames", TaskState::Completed, now()),
        ("compact-test", "GetParameterValues", TaskState::Fault, now()),
        ("compact-test", "GetParameterValues", TaskState::Completed, now() - 91),
    ] {
        let mut ineligible = record.clone();
        ineligible.device = device.into(); ineligible.method = method.into();
        ineligible.state = status; ineligible.sent = Some(sent);
        state.tasks.insert(1, ineligible);
        assert_eq!(readback::observed(&state, "compact-test", prefix), None);
    }
}

#[test]
fn wan_readback_guard_fails_closed_on_management_or_malformed_results() {
    let prefix = "InternetGatewayDevice.WANDevice.1.WANConnectionDevice.3.WANIPConnection.1.";
    for data in [
        json!({"parameters":[{"name":format!("{prefix}X_CU_ServiceList"),"value":"INTERNET_TR069"}]}),
        json!({"parameters":[{"name":format!("{prefix}X_CU_ServiceList"),"value":false}]}),
        json!({"parameters":null}),
    ] {
        let mut state = State::default();
        let mut record = task(1);
        record.state = TaskState::Completed;
        record.result = Some(to_raw_value(&data).unwrap());
        state.tasks.insert(1, record);

        let observed = readback::observed(&state, "compact-test", prefix);

        assert_eq!(observed, Some(true));
    }
}

#[test]
fn test_prune_bounds_inactive_tasks_and_preserves_active() {
    let mut state = State::default();
    let current_time = now();

    // 50 completed tasks, half old (> 180s)
    for i in 1..=50 {
        let mut t = task(i);
        t.state = TaskState::Completed;
        t.created = if i <= 25 { current_time - 2000 } else { current_time - 100 };
        t.sent = Some(t.created);
        state.tasks.insert(i, t);
    }
    // 2 active tasks (one queued, one sent, both old)
    let mut active1 = task(101);
    active1.state = TaskState::Queued;
    active1.created = current_time - 2000;
    state.tasks.insert(101, active1);

    let mut active2 = task(102);
    active2.state = TaskState::Sent;
    active2.created = current_time - 2000;
    active2.sent = Some(current_time - 2000);
    state.tasks.insert(102, active2);

    state.prune();

    // Active tasks must remain
    assert!(state.tasks.contains_key(&101));
    assert!(state.tasks.contains_key(&102));

    // Old tasks (1..=25) must be pruned by TTL
    for i in 1..=25 {
        assert!(!state.tasks.contains_key(&i));
    }

    // Fresh inactive tasks (26..=50) remain
    let inactive_count = state.tasks.values().filter(|t| !t.state.is_active()).count();
    assert_eq!(inactive_count, 25);
}

#[test]
fn test_prune_bounds_total_result_bytes_dropping_older_inactive_tasks() {
    let mut state = State::default();
    let current_time = now();

    let mut t1 = task(1);
    t1.state = TaskState::Completed;
    t1.created = current_time - 20;
    t1.sent = Some(t1.created);
    t1.result = Some(RawValue::from_string(format!("\"{}\"", "a".repeat(200 * 1024))).unwrap());
    state.tasks.insert(1, t1);

    let mut t2 = task(2);
    t2.state = TaskState::Completed;
    t2.created = current_time - 5;
    t2.sent = Some(t2.created);
    t2.result = Some(RawValue::from_string(format!("\"{}\"", "b".repeat(100 * 1024))).unwrap());
    state.tasks.insert(2, t2);

    state.prune();

    assert!(state.tasks[&1].result.is_none());
    assert!(state.tasks[&2].result.is_some());
}

#[test]
fn test_get_parameter_values_rejects_trailing_dot_wildcards() {
    let wildcard = json!({"names": ["InternetGatewayDevice."]});
    assert!(validate("GetParameterValues", &wildcard).is_err());

    let leaf = json!({"names": ["InternetGatewayDevice.DeviceInfo.SoftwareVersion"]});
    assert!(validate("GetParameterValues", &leaf).is_ok());
}

#[test]
fn test_clean_op_clears_inactive_tasks_and_resets_load_failure() {
    let server = server();
    let mut state = State::default();
    state.load_failure = Some("state load: ACS state exceeds 8 MiB".into());

    let mut inactive = task(1);
    inactive.state = TaskState::Completed;
    state.tasks.insert(1, inactive);

    let mut active = task(2);
    active.state = TaskState::Queued;
    state.tasks.insert(2, active);

    *server.state.lock().unwrap() = state;

    let res: Value = serde_json::from_str(&server.execute(json!({"op": "clean"}))).unwrap();
    assert_eq!(res["cleaned"], true);
    assert_eq!(res["tasks"], 1);

    let state = server.state.lock().unwrap();
    assert!(state.load_failure.is_none());
    assert!(!state.tasks.contains_key(&1));
    assert!(state.tasks.contains_key(&2));
    let _ = fs::remove_file(&server.state_file);
}
