use fttr_api::*;
use serde_json::json;

#[test]
fn operation_parameters_and_job_transport_reject_wrong_types() {
    assert!(serde_json::from_value::<MibStartRequest>(
        json!({"onu_id":256,"optical_sn":"TEST_REDACTED","tables":false})
    )
    .is_err());
    assert!(serde_json::from_value::<JobRequest>(json!({"job":"test-job","chunk":-1})).is_err());
    assert!(serde_json::from_value::<HlUpdateRequest>(json!({"target":{"mqtt_mac":"REDACTED","binding_generation":"test"},"kind":"unknown","entry":1,"password":"","active":false})).is_err());
    assert!(
        serde_json::from_value::<NpuSetRequest>(json!({"enabled":"false","bridge":""})).is_err()
    );
    for value in [
        json!({"status":"pending","job":"test-job"}),
        json!({"error":"capacity"}),
    ] {
        let reply: JobStartReply = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(reply).unwrap(), value);
    }
    for value in [
        json!({"status":"pending"}),
        json!({"status":"ready","chunk":0,"chunks":1,"bytes":2,"data":"e30="}),
        json!({"error":"expired"}),
    ] {
        let reply: JobChunkReply = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(reply).unwrap(), value);
    }
    assert!(
        serde_json::from_value::<JobChunkReply>(json!({"status":"ready","data":"e30="})).is_err()
    );
    assert!(serde_json::from_value::<NpuSetReply>(json!({"status":"success"})).is_err());
}
