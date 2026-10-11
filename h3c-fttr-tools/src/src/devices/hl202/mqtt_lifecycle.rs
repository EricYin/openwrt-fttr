//! HL202-only TCAPI work triggered by the generic MQTT connected event.
use super::command;
use crate::devices::{MqttConnectedEvent, MqttLifecycleAdapter};
use std::time::Duration;

const MQTT_MAC: &str = "1C94685DF998";
const TELEMETRY_SUFFIX: &str = "/fttr_data.tar.gz";

pub struct MqttLifecycle;

impl MqttLifecycleAdapter for MqttLifecycle {
    fn matches_mqtt_device(&self, device_id: &str) -> bool {
        device_id.eq_ignore_ascii_case(MQTT_MAC)
            || device_id.eq_ignore_ascii_case(&format!("{MQTT_MAC}{TELEMETRY_SUFFIX}"))
    }

    fn on_mqtt_connected(&self, event: &MqttConnectedEvent) -> Result<(), String> {
        let script = config_script(event.acs_url.as_deref());
        let result = command::execute(MQTT_MAC, &script, Duration::from_secs(90))
            .map_err(|error| format!("HL202 MQTT command transport failed: {error}"))?;
        if !result.success()
            || !result
                .output
                .as_deref()
                .is_some_and(|output| output.contains("HL202_CWMP_SETTINGS_VERIFIED"))
        {
            return Err(format!(
                "HL202 CWMP settings were not verified (status={}, exit={:?}, output={:?}, error={:?})",
                result.status, result.exit_code, result.output, result.error
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
            "if [ \"$(tcapi get Cwmp_Entry SyncAcsUrl)\" != Yes ]; then tcapi set Cwmp_Entry SyncAcsUrl Yes || exit 1; changed=1; fi".into(),
        );
    }
    commands.push(
        "if [ \"$(tcapi get Cwmp_Entry periodInterval)\" != 5 ]; then tcapi set Cwmp_Entry periodInterval 5 || exit 1; changed=1; fi".into(),
    );
    commands.push("if [ \"$changed\" = 1 ]; then tcapi commit Cwmp && tcapi save || exit 1; fi".into());
    if let Some(acs_url) = acs_url {
        commands.push(format!(
            "test \"$(tcapi get Cwmp_Entry acsUrl)\" = {}",
            shell_quote(acs_url)
        ));
        commands.push("test \"$(tcapi get Cwmp_Entry SyncAcsUrl)\" = Yes".into());
    }
    commands.push("test \"$(tcapi get Cwmp_Entry periodInterval)\" = 5".into());
    commands.push("printf 'HL202_CWMP_SETTINGS_VERIFIED\\n'".into());
    commands.join(" && ")
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_only_the_known_hl202_mqtt_identity() {
        let adapter = MqttLifecycle;
        assert!(adapter.matches_mqtt_device(MQTT_MAC));
        assert!(adapter.matches_mqtt_device("1c94685df998/fttr_data.tar.gz"));
        assert!(!adapter.matches_mqtt_device("HWTC3C6DE5B3/fttr_data.tar.gz"));
        assert!(!adapter.matches_mqtt_device("H3CT685DF998"));
    }

    #[test]
    fn shell_quote_preserves_single_quotes() {
        assert_eq!(shell_quote("http://acs.example/a'b"), "'http://acs.example/a'\"'\"'b'");
    }

    #[test]
    fn config_script_sets_both_acs_url_fields_and_five_second_interval() {
        let script = config_script(Some("http://192.168.1.1:7547/cwmp"));
        assert!(script.contains("tcapi set Cwmp_Entry acsUrl 'http://192.168.1.1:7547/cwmp'"));
        assert!(script.contains("tcapi set Cwmp_Entry SyncAcsUrl Yes"));
        assert!(script.contains("tcapi set Cwmp_Entry periodInterval 5"));
        assert!(script.contains("if [ \"$(tcapi get Cwmp_Entry acsUrl)\" != 'http://192.168.1.1:7547/cwmp' ]"));
        assert!(script.contains("if [ \"$(tcapi get Cwmp_Entry SyncAcsUrl)\" != Yes ]"));
        assert!(script.contains("if [ \"$(tcapi get Cwmp_Entry periodInterval)\" != 5 ]"));
        assert!(script.contains("if [ \"$changed\" = 1 ]; then tcapi commit Cwmp && tcapi save"));
        assert!(script.contains("tcapi get Cwmp_Entry acsUrl"));
        assert!(script.contains("tcapi get Cwmp_Entry SyncAcsUrl)\" = Yes"));
        assert!(script.contains("tcapi get Cwmp_Entry periodInterval"));
    }
}
