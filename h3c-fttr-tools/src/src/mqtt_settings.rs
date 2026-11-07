//! Startup MQTT policy. Init passes an immutable snapshot; standalone commands
//! read UCI so disabling an adapter also closes their transport entry points.
use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Settings {
    pub enabled: bool,
    pub hl202_enabled: bool,
    pub ty_fs803g_enabled: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: true,
            hl202_enabled: true,
            ty_fs803g_enabled: false,
        }
    }
}

fn boolean(value: &str) -> Result<bool, String> {
    match value {
        "1" | "on" | "true" | "yes" => Ok(true),
        "0" | "off" | "false" | "no" => Ok(false),
        _ => Err("invalid MQTT boolean".into()),
    }
}

impl Settings {
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut settings = Self::default();
        let mut selected = false;
        for line in text.lines() {
            let words = crate::provisioning::uci_words(line)?;
            match words.first().map(String::as_str) {
                Some("config") => {
                    selected = words.get(1).is_some_and(|v| v == "mqtt")
                        && words.get(2).is_some_and(|v| v == "mqtt")
                }
                Some("option") if selected && words.len() == 3 => {
                    let field = match words[1].as_str() {
                        "enabled" => &mut settings.enabled,
                        "hl202_enabled" => &mut settings.hl202_enabled,
                        "ty_fs803g_enabled" => &mut settings.ty_fs803g_enabled,
                        _ => continue,
                    };
                    *field = boolean(&words[2])?;
                }
                _ => (),
            }
        }
        Ok(settings)
    }

    pub fn load() -> Result<Self, String> {
        let mut settings = if let Ok(mut uci) = openwrt_uci::Uci::new() {
            Self::parse(&uci.export_package("fttr").map_err(|e| e.to_string())?)?
        } else {
            match std::fs::read_to_string("/etc/config/fttr") {
                Ok(text) => Self::parse(&text)?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
                Err(e) => return Err(e.to_string()),
            }
        };
        for (name, field) in [
            ("FTTR_MQTT_ENABLED", &mut settings.enabled),
            ("FTTR_MQTT_HL202_ENABLED", &mut settings.hl202_enabled),
            (
                "FTTR_MQTT_TY_FS803G_ENABLED",
                &mut settings.ty_fs803g_enabled,
            ),
        ] {
            if let Ok(value) = std::env::var(name) {
                *field = boolean(&value)?;
            }
        }
        Ok(settings)
    }

    pub fn hl202(self) -> bool {
        cfg!(feature = "mqtt-hl202") && self.enabled && self.hl202_enabled
    }
    pub fn ty_fs803g(self) -> bool {
        cfg!(feature = "mqtt-ty-fs803g") && self.enabled && self.ty_fs803g_enabled
    }
    pub fn any(self) -> bool {
        self.hl202() || self.ty_fs803g()
    }
}

pub fn hl202_enabled() -> bool {
    Settings::load().is_ok_and(Settings::hl202)
}

pub fn require_hl202() -> std::io::Result<()> {
    if hl202_enabled() {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "HL202 MQTT adapter disabled or not compiled",
        ))
    }
}

pub fn status() -> serde_json::Value {
    match Settings::load() {
        Ok(s) => serde_json::json!({"configured": s, "compiled": {
            "mqtt": cfg!(feature="mqtt"), "hl202": cfg!(feature="mqtt-hl202"),
            "ty_fs803g": cfg!(feature="mqtt-ty-fs803g")
        }, "effective": {"broker": s.any(), "hl202": s.hl202(), "ty_fs803g": s.ty_fs803g()}}),
        Err(e) => serde_json::json!({"error": e}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_defaults_and_switch_combinations() {
        assert_eq!(
            Settings::parse("config acs 'acs'\n option enabled '0'").unwrap(),
            Settings::default()
        );
        for mask in 0..8 {
            let s = Settings::parse(&format!("config mqtt 'mqtt'\n option enabled '{}'\n option hl202_enabled '{}'\n option ty_fs803g_enabled '{}'", mask & 1, (mask >> 1) & 1, (mask >> 2) & 1)).unwrap();
            assert_eq!(s.hl202(), cfg!(feature = "mqtt-hl202") && mask & 3 == 3);
            assert_eq!(
                s.ty_fs803g(),
                cfg!(feature = "mqtt-ty-fs803g") && mask & 5 == 5
            );
        }
        assert!(Settings::parse("config mqtt 'mqtt'\n option enabled 'bad'").is_err());
    }
}
