use serde_json::Value;
use std::process::Command;

#[test]
fn capabilities_offline_queries_agree_without_controller_initialization() {
    let query = |binary: &str, args: &[&str]| {
        let output = Command::new(binary)
            .args(args)
            // An enabled MQTT daemon would fail startup with this value.
            .env("FTTR_MQTT_LAN_ADDR", "not-an-ip")
            .output()
            .unwrap();
        assert!(output.status.success(), "{:?}", output);
        assert!(output.stderr.is_empty(), "{:?}", output);
        serde_json::from_slice::<Value>(&output.stdout).unwrap()
    };
    let daemon = query(env!("CARGO_BIN_EXE_fttrd"), &["--capabilities"]);
    let cli = query(env!("CARGO_BIN_EXE_fttrctl"), &["capabilities", "--json"]);
    assert_eq!(cli, daemon);
    assert_eq!(cli["schema_version"], 1);
    assert_eq!(cli["features"]["mqtt"], cfg!(feature = "mqtt"));
    assert_eq!(cli["features"]["acs"], cfg!(feature = "acs"));
    assert_eq!(
        cli["features"]["device-hl202"],
        cfg!(feature = "device-hl202")
    );
    assert_eq!(
        cli["features"]["device-hwtc"],
        cfg!(feature = "device-hwtc")
    );
    assert_eq!(
        cli["pages"]["hl202"],
        cfg!(all(feature = "device-hl202", feature = "mqtt"))
    );
    assert_eq!(
        cli["pages"]["config"],
        cfg!(feature = "acs") || (cfg!(feature = "device-hl202") && cfg!(feature = "mqtt"))
    );
    assert_eq!(
        cli["pages"]["sync"],
        cfg!(feature = "acs") || (cfg!(feature = "device-hl202") && cfg!(feature = "mqtt"))
    );
    for page in ["status", "hardware", "calibration", "about"] {
        assert_eq!(cli["pages"][page], true);
    }
    let adapters = cli["adapters"].as_array().expect("adapters array");
    assert_eq!(
        cli["features"]["device-scty"],
        cfg!(feature = "device-scty")
    );
    assert_eq!(
        adapters.iter().any(|a| a["id"] == "scty"),
        cfg!(feature = "device-scty")
    );
    assert!(adapters.iter().any(|a| a["id"] == "generic"));
    if cfg!(feature = "device-hl202") {
        assert!(adapters.iter().any(|a| a["id"] == "hl202"));
    }
    if cfg!(feature = "device-hwtc") {
        assert!(adapters.iter().any(|a| a["id"] == "hwtc"));
    }
    if cfg!(feature = "device-star") {
        assert!(adapters.iter().any(|a| a["id"] == "star"));
    }
}
