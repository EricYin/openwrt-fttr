use crate::mqtt_identity::Selection;
use serde::{Deserialize, Serialize};
use std::io;

pub const MAX_REQUEST_BYTES: usize = 512 * 1024;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub schema_version: u8,
    pub target: Selection,
    pub action: Action,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    Exec {
        script: String,
        timeout_seconds: u16,
    },
    AccountsRead,
    AccountSetPassword {
        entry: u8,
        password: String,
    },
    AccountSetActive {
        entry: u8,
        active: bool,
    },
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

pub fn read_file(path: &std::path::Path) -> io::Result<Request> {
    parse(&super::script::read_text_file(path, MAX_REQUEST_BYTES)?)
}

pub fn parse(input: &str) -> io::Result<Request> {
    if input.len() > MAX_REQUEST_BYTES {
        return Err(invalid("HL202 request exceeds the byte limit"));
    }
    let mut request: Request = serde_json::from_str(input)
        .map_err(|_| invalid("invalid HL202 request JSON or action schema"))?;
    if request.schema_version != 1 {
        return Err(invalid("unsupported HL202 request schema version"));
    }
    if request.target.optical_sn.is_empty()
        || request.target.binding_generation.is_empty()
        || !(0..=15).contains(&request.target.onu_id)
    {
        return Err(invalid("invalid HL202 device selection"));
    }
    match &mut request.action {
        Action::Exec {
            script,
            timeout_seconds,
        } => {
            if !(1..=300).contains(timeout_seconds) {
                return Err(invalid("command timeout must be between 1 and 300 seconds"));
            }
            *script = super::script::validate(std::mem::take(script))?;
        }
        Action::AccountsRead => (),
        Action::AccountSetPassword { entry, password } => {
            if *entry > 2
                || password.len() > super::script::MAX_SCRIPT_BYTES
                || password.contains('\0')
            {
                return Err(invalid("invalid account entry or password encoding/size"));
            }
        }
        Action::AccountSetActive { entry, .. } => {
            if *entry > 2 {
                return Err(invalid("invalid account entry"));
            }
        }
    }
    Ok(request)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hl202_request_preserves_remote_shell_as_json_data() {
        let script = "printf '%s\\n' \"猫\"\n$(touch /tmp/never-local); `id` | cat";
        let input = serde_json::json!({
            "schema_version": 1,
            "target": {"optical_sn":"H3CT12345678","onu_id":0,"binding_generation":"epoch"},
            "action": {"kind":"exec","script":script,"timeout_seconds":20}
        });
        let request = parse(&input.to_string()).unwrap();
        match request.action {
            Action::Exec {
                script: actual,
                timeout_seconds,
            } => {
                assert_eq!(actual, script);
                assert_eq!(timeout_seconds, 20);
            }
            _ => panic!("wrong action"),
        }
    }

    #[test]
    fn hl202_request_accepts_protocol_slot_boundaries() {
        for (id, valid) in [(0, true), (15, true), (16, false)] {
            let input = serde_json::json!({
                "schema_version": 1,
                "target": {"optical_sn":"H3CT12345678","onu_id":id,"binding_generation":"epoch"},
                "action": {"kind":"accounts_read"}
            });
            assert_eq!(parse(&input.to_string()).is_ok(), valid);
        }
    }

    #[test]
    fn hl202_request_rejects_invalid_schema_without_echoing_secrets() {
        let input = serde_json::json!({
            "schema_version": 2,
            "target": {"optical_sn":"H3CT12345678","onu_id":2,"binding_generation":"epoch"},
            "action": {"kind":"account_set_password","entry":1,"password":"private-credential"}
        });
        let error = parse(&input.to_string()).err().unwrap();
        assert!(!error.to_string().contains("private-credential"));
        assert!(parse("{broken private-credential").is_err());
    }
}
