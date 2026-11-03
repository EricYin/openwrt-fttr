use fttr_api::*;
use fttr_omci::{
    mib::{MibAssembler, MibRecord, TableSnapshot},
    schema::MeRegistry,
};
use serde_json::{json, Value};

#[path = "../../../src/devices/hl202/telemetry/model.rs"]
mod telemetry;

#[test]
fn mib_producer_preserves_decoded_unknown_and_table_evidence() {
    let mut assembler = MibAssembler::new(2);
    assembler.push(MibRecord {
        sequence: 0,
        class_id: 256,
        entity_id: 0,
        attribute_mask: 0x8000,
        data: [0; 26],
    });
    assembler.push(MibRecord {
        sequence: 1,
        class_id: 65534,
        entity_id: 1,
        attribute_mask: 0x8000,
        data: [0xab; 26],
    });
    let mut report = assembler.finish();
    let attribute = MeRegistry::get(171)
        .unwrap()
        .attributes
        .iter()
        .find(|a| a.index == 6)
        .unwrap();
    report.tables.push(TableSnapshot::from_result(
        171,
        1,
        attribute,
        Ok(vec![0; 16]),
    ));
    let wire = serde_json::to_value(report).unwrap();
    let shared: MibReport = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(serde_json::to_value(shared).unwrap(), wire);
    let mut malformed = wire;
    malformed["schema_version"] = json!(2);
    assert!(serde_json::from_value::<MibReport>(malformed).is_err());
}

#[test]
fn telemetry_producer_preserves_open_values_inside_typed_envelopes() {
    let report = telemetry::Report {
        schema_version: 1,
        compressed_bytes: 12,
        decompressed_bytes: 24,
        fields: [(
            "extension".into(),
            telemetry::Field {
                value: json!({"vendor": [false, null, 3]}),
                member: "test.json".into(),
                unit: None,
            },
        )]
        .into(),
        members: vec![telemetry::Member {
            name: "test.json".into(),
            bytes: 24,
            encoding: "utf8".into(),
            text: None,
            text_omitted: true,
        }],
        errors: vec![],
    };
    let wire = serde_json::to_value(report).unwrap();
    let shared: HlTelemetry = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(serde_json::to_value(shared).unwrap(), wire);
    let mut malformed = wire;
    malformed["members"][0]["text_omitted"] = json!("true");
    assert!(serde_json::from_value::<HlTelemetry>(malformed).is_err());
}

#[test]
fn provisioning_preserves_optional_observations_and_omci_results() {
    let unavailable = json!({"state":"pending","reason":"observation not started"});
    assert!(serde_json::from_value::<AcsBootstrap>(unavailable).is_ok());
    let omci = json!({"state":"association_verified","enabled":true,"reason":null,"revision":"test_revision","vlan":46,"updated_at":12});
    let shared: DeviceOmciManagement = serde_json::from_value(omci.clone()).unwrap();
    let normalized = serde_json::to_value(shared).unwrap();
    for key in ["state", "enabled", "revision", "vlan", "updated_at"] {
        assert_eq!(normalized[key], omci[key]);
    }
    let pending = json!({"state":"not_started","items":[]});
    let shared: ProvisioningStatus = serde_json::from_value(pending.clone()).unwrap();
    assert_eq!(serde_json::to_value(shared).unwrap(), pending);
    assert!(
        serde_json::from_value::<ProvisioningStatus>(json!({"state":"typo","items":[]})).is_err()
    );
    assert!(serde_json::from_value::<ProvisioningStatus>(
        json!({"state":"not_started","items":[{"id":"test"}]})
    )
    .is_err());
}

#[test]
fn asynchronous_result_contracts_keep_sparse_command_failures_and_literal_guards() {
    let target = json!({"mqtt_mac":"REDACTED","optical_sn":null,"onu_id":null,"binding_generation":"test","last_seen":0,"reason":"test"});
    let wire = json!({"schema_version":1,"operation":{"target":target,"result":{"target":"REDACTED","status":"timeout","output_truncated":false,"error":"outcome unknown"},"binding_current":false},"accounts":null,"write_status":"unconfirmed"});
    let shared: HlAccountReply = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(serde_json::to_value(shared).unwrap(), wire);
    assert!(serde_json::from_value::<HlAccount>(
        json!({"entry":3,"username":"test","active":false})
    )
    .is_err());
    assert!(serde_json::from_value::<RestartReply>(json!({"restarting":false})).is_err());
    let source = json!({"schema_version":1,"target":target,"report":null,"read_only":true});
    assert!(serde_json::from_value::<HlReportReply>(source.clone()).is_ok());
    let mut bad: Value = source;
    bad["read_only"] = json!(false);
    assert!(serde_json::from_value::<HlReportReply>(bad).is_err());
}
