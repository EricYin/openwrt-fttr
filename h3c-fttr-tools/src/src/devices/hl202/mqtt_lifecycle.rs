//! HL202-only TCAPI work triggered by the generic MQTT connected event.
use super::command;
use crate::devices::{MqttConnectedEvent, MqttLifecycleAdapter};
use std::time::Duration;

const TELEMETRY_SUFFIX: &str = "/fttr_data.tar.gz";

pub fn mqtt_target(device_id: &str) -> Option<String> {
    command::normalize_mac(
        device_id
            .strip_suffix(TELEMETRY_SUFFIX)
            .unwrap_or(device_id),
    )
    .ok()
}

/// Read only the settings we manage; never return credentials or the full TCAPI dump.
pub fn read_settings(device_id: &str, acs_url: &str) -> Result<bool, String> {
    let target = mqtt_target(device_id).ok_or("invalid MQTT command target")?;
    let script = format!(
        "if {}; then printf 'FTTR_CWMP_MATCH\\n'; else printf 'FTTR_CWMP_DIFF\\n'; fi",
        settings_tests(Some(acs_url)).join(" && ")
    );
    let result = command::execute(&target, &script, Duration::from_secs(30))
        .map_err(|_| "CWMP settings read transport failed")?;
    if !result.success() {
        return Err(format!(
            "CWMP settings read failed (status={}, exit={:?}); outcome unknown",
            result.status, result.exit_code
        ));
    }
    match result.output.as_deref().map(str::trim) {
        Some("FTTR_CWMP_MATCH") => Ok(true),
        Some("FTTR_CWMP_DIFF") => Ok(false),
        _ => Err("CWMP settings read returned no valid result".into()),
    }
}

pub struct MqttLifecycle;

impl MqttLifecycleAdapter for MqttLifecycle {
    fn matches_mqtt_device(&self, device_id: &str) -> bool {
        mqtt_target(device_id).is_some()
    }

    fn on_mqtt_connected(&self, event: &MqttConnectedEvent) -> Result<(), String> {
        let script = config_script(event.acs_url.as_deref());
        let target = mqtt_target(&event.device_id).ok_or("invalid MQTT command target")?;
        let result = command::execute(&target, &script, Duration::from_secs(90))
            .map_err(|_| "HL202 MQTT command transport failed")?;
        if !result.success()
            || !result
                .output
                .as_deref()
                .is_some_and(|output| output.contains("HL202_CWMP_SETTINGS_VERIFIED"))
        {
            return Err(format!(
                "HL202 CWMP settings were not verified (status={}, exit={:?}); execution outcome may be unknown",
                result.status, result.exit_code
            ));
        }
        Ok(())
    }
}

fn config_script(acs_url: Option<&str>) -> String {
    let mut commands = Vec::new();
    commands.push("changed=0".to_string());
    if let Some(acs_url) = acs_url {
        commands.push(format!(
            "if [ \"$(tcapi get Cwmp_Entry acsUrl)\" != {0} ]; then tcapi set Cwmp_Entry acsUrl {0} || exit 1; changed=1; fi",
            shell_quote(acs_url)
        ));
        commands.push(
            "case \"$(tcapi get Cwmp_Entry SyncAcsUrl)\" in No) tcapi set Cwmp_Entry SyncAcsUrl Yes || exit 1;; esac".into(),
        );
    }
    commands.push(
        "if [ \"$(tcapi get Cwmp_Entry periodInterval)\" != 5 ]; then tcapi set Cwmp_Entry periodInterval 5 || exit 1; changed=1; fi".into(),
    );
    for (key, value) in [("Active", "Yes"), ("periodActive", "Yes")] {
        commands.push(format!("if [ \"$(tcapi get Cwmp_Entry {key})\" != {value} ]; then tcapi set Cwmp_Entry {key} {value} || exit 1; changed=1; fi"));
    }
    // Also persist matching runtime values after a previous command with an unknown outcome.
    commands.push("tcapi commit Cwmp && tcapi save".into());
    commands.extend(settings_tests(acs_url));
    commands.push("printf 'HL202_CWMP_SETTINGS_VERIFIED\\n'".into());
    commands.join(" && ")
}

fn settings_tests(acs_url: Option<&str>) -> Vec<String> {
    let mut commands = Vec::new();
    if let Some(acs_url) = acs_url {
        commands.push(format!(
            "test \"$(tcapi get Cwmp_Entry acsUrl)\" = {}",
            shell_quote(acs_url)
        ));
        // Some stock profiles do not expose this optional vendor field. Verify it only
        // when its recognized Yes/No values establish that the field exists.
        commands.push(
            "case \"$(tcapi get Cwmp_Entry SyncAcsUrl)\" in No) false;; *) true;; esac".into(),
        );
    }
    commands.push("test \"$(tcapi get Cwmp_Entry periodInterval)\" = 5".into());
    commands.push("test \"$(tcapi get Cwmp_Entry Active)\" = Yes".into());
    commands.push("test \"$(tcapi get Cwmp_Entry periodActive)\" = Yes".into());
    commands
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_actual_mqtt_mac_without_a_test_device_allowlist() {
        let adapter = MqttLifecycle;
        assert!(adapter.matches_mqtt_device("02AABBCCDDEE"));
        assert!(adapter.matches_mqtt_device("02aabbccddee/fttr_data.tar.gz"));
        assert!(adapter.matches_mqtt_device("02:11:22:33:44:55"));
        assert!(!adapter.matches_mqtt_device("HWTC3C6DE5B3/fttr_data.tar.gz"));
        assert!(!adapter.matches_mqtt_device("H3CT12345678"));
        assert!(!adapter.matches_mqtt_device("../02AABBCCDDEE"));
    }

    #[test]
    fn shell_quote_preserves_single_quotes() {
        assert_eq!(
            shell_quote("http://acs.example/a'b"),
            "'http://acs.example/a'\"'\"'b'"
        );
    }

    #[test]
    fn config_script_sets_both_acs_url_fields_and_five_second_interval() {
        let script = config_script(Some("http://192.168.1.1:7547/cwmp"));
        assert!(script.contains("tcapi set Cwmp_Entry acsUrl 'http://192.168.1.1:7547/cwmp'"));
        assert!(script.contains("tcapi set Cwmp_Entry SyncAcsUrl Yes"));
        assert!(script.contains("tcapi set Cwmp_Entry periodInterval 5"));
        assert!(script.contains(
            "if [ \"$(tcapi get Cwmp_Entry acsUrl)\" != 'http://192.168.1.1:7547/cwmp' ]"
        ));
        assert!(script.contains("case \"$(tcapi get Cwmp_Entry SyncAcsUrl)\" in No) tcapi set"));
        assert!(script.contains("if [ \"$(tcapi get Cwmp_Entry periodInterval)\" != 5 ]"));
        assert!(script.contains("tcapi commit Cwmp && tcapi save"));
        assert!(script.contains("tcapi get Cwmp_Entry acsUrl"));
        assert!(script.contains("tcapi get Cwmp_Entry SyncAcsUrl)\" in No) false"));
        assert!(script.contains("tcapi get Cwmp_Entry periodInterval"));
    }

    #[test]
    fn missing_optional_sync_field_does_not_prevent_url_commit_and_readback() {
        let mock = r#"
url=old; interval=43200; active=No; periodic=No; committed=0
tcapi() {
    case "$1:$3" in
        get:acsUrl) printf '%s' "$url";;
        get:periodInterval) printf '%s' "$interval";;
        get:Active) printf '%s' "$active";;
        get:periodActive) printf '%s' "$periodic";;
        get:SyncAcsUrl) printf 'no node information'; return 1;;
        set:acsUrl) url="$4";;
        set:periodInterval) interval="$4";;
        set:Active) active="$4";;
        set:periodActive) periodic="$4";;
        set:SyncAcsUrl) return 3;;
        commit:*) committed=1;;
        save:*) test "$committed" = 1;;
        *) return 4;;
    esac
}
"#;
        let script = format!(
            "{mock}\n{}",
            config_script(Some("http://192.0.2.1:7547/cwmp"))
        );
        let output = std::process::Command::new("sh")
            .args(["-c", &script])
            .output()
            .unwrap();
        assert!(output.status.success(), "{:?}", output);
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            "HL202_CWMP_SETTINGS_VERIFIED\n"
        );
    }
}
