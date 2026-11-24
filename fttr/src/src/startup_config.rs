//! Configuration fixed for the lifetime of the daemon.
use std::sync::OnceLock;

pub(crate) struct StartupConfig {
    pub(crate) acs_state_file: String,
    pub(crate) acs_username: String,
    pub(crate) acs_password: String,
    pub(crate) rx_snapshot: bool,
}

impl Default for StartupConfig {
    fn default() -> Self {
        Self {
            acs_state_file: "/var/lib/fttr/acs-state.json".into(),
            acs_username: String::new(),
            acs_password: String::new(),
            rx_snapshot: false,
        }
    }
}

static CONFIG: OnceLock<StartupConfig> = OnceLock::new();

/// Read the same UCI package used by provisioning before starting the runtime.
pub fn initialize() -> Result<(), &'static str> {
    let text = crate::provisioning::load_uci_text().map_err(|_| "UCI read failed")?;
    let config = parse(&text)?;
    CONFIG.set(config).map_err(|_| "already initialized")
}

pub(crate) fn get() -> &'static StartupConfig {
    #[cfg(test)]
    return CONFIG.get_or_init(StartupConfig::default);
    #[cfg(not(test))]
    CONFIG.get().expect("startup configuration not initialized")
}

fn parse(text: &str) -> Result<StartupConfig, &'static str> {
    let mut config = StartupConfig::default();
    let mut section = "";
    let mut seen_sections = std::collections::BTreeSet::new();
    let mut seen_options = std::collections::BTreeSet::new();
    for line in text.lines() {
        let words = crate::provisioning::uci_words(line).map_err(|_| "invalid UCI syntax")?;
        if words.is_empty() || words[0].starts_with('#') {
            continue;
        }
        match words[0].as_str() {
            "package" if words.len() == 2 => (),
            "config" if words.len() == 2 || words.len() == 3 => {
                section = match words.get(2).map(String::as_str) {
                    Some("acs") => "acs",
                    Some("fiberhome") => "fiberhome",
                    _ => "",
                };
                if !section.is_empty() && !seen_sections.insert(section) {
                    return Err("duplicate startup configuration section");
                }
            }
            "option" | "list" if words.len() == 3 => {
                let key = words[1].as_str();
                let relevant = matches!(
                    (section, key),
                    ("acs", "state_file" | "username" | "password") | ("fiberhome", "rx_snapshot")
                );
                if !relevant {
                    continue;
                }
                if words[0] != "option" || !seen_options.insert((section, words[1].clone())) {
                    return Err("startup settings require unique scalar options");
                }
                match (section, key) {
                    ("acs", "state_file") => config.acs_state_file = words[2].clone(),
                    ("acs", "username") => config.acs_username = words[2].clone(),
                    ("acs", "password") => config.acs_password = words[2].clone(),
                    ("fiberhome", "rx_snapshot") => {
                        config.rx_snapshot = match words[2].as_str() {
                            "1" | "true" => true,
                            "" | "0" | "false" => false,
                            _ => return Err("invalid fiberhome rx_snapshot"),
                        }
                    }
                    _ => unreachable!(),
                }
            }
            _ => return Err("invalid UCI syntax"),
        }
    }
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn packaged_configuration_is_accepted() {
        let config = parse(include_str!("../../files/fttr.config")).unwrap();
        assert_eq!(config.acs_state_file, "/var/lib/fttr/acs-state.json");
        assert!(config.acs_username.is_empty());
        assert!(!config.rx_snapshot);
    }

    #[test]
    fn defaults_and_explicit_empty_values() {
        let defaults = parse("").unwrap();
        assert_eq!(defaults.acs_state_file, "/var/lib/fttr/acs-state.json");
        assert!(defaults.acs_username.is_empty());
        assert!(!defaults.rx_snapshot);
        let config = parse("config acs 'acs'\n option state_file ''\n option username ''\n option password 'a'\\''b'\n").unwrap();
        assert_eq!(config.acs_state_file, "");
        assert_eq!(config.acs_password, "a'b");
    }

    #[test]
    fn snapshot_boolean_and_invalid_values() {
        for (value, expected) in [
            ("1", true),
            ("true", true),
            ("", false),
            ("0", false),
            ("false", false),
        ] {
            let text = format!("config fiberhome 'fiberhome'\n option rx_snapshot '{value}'\n");
            assert_eq!(parse(&text).unwrap().rx_snapshot, expected);
        }
        assert!(parse("config fiberhome 'fiberhome'\n option rx_snapshot 'yes'\n").is_err());
        assert!(parse("config acs 'acs'\n list username 'user'\n").is_err());
        assert!(
            parse("config acs 'acs'\n option username 'one'\n option username 'two'\n").is_err()
        );
        assert!(parse("config acs 'acs'\n option password 'unterminated\n").is_err());
    }
}
