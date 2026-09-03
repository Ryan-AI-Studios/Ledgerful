use serde::{Deserialize, Serialize};

/// Repo-local doctor operator intent (0226).
///
/// Ack is the same class as `intent.trusted_public_keys`: persist in
/// `.ledgerful/config.toml`, never in `.ledgerful/state/`. Stale codes (no
/// finding this run) are inert — no GC, no extra finding.
#[derive(Debug, Serialize, Deserialize, Clone, Default, PartialEq, Eq)]
pub struct DoctorConfig {
    /// Finding codes whose human bodies are suppressed until the finding
    /// disappears. Sorted + deduped on load. Unknown / leftover codes stay.
    #[serde(default)]
    pub acknowledged_codes: Vec<String>,
}

impl DoctorConfig {
    pub(crate) fn contains(&self, code: &str) -> bool {
        self.acknowledged_codes.iter().any(|c| c == code)
    }

    /// Sort + dedup in place (deterministic config).
    pub(crate) fn normalize(&mut self) {
        self.acknowledged_codes.sort();
        self.acknowledged_codes.dedup();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::model::Config;

    #[test]
    fn doctor_config_default_empty() {
        let config = Config::default();
        assert!(config.doctor.acknowledged_codes.is_empty());
    }

    #[test]
    fn doctor_config_deserializes_acknowledged_codes() {
        let toml_str = r#"
            [doctor]
            acknowledged_codes = ["sig-version", "sig-pin", "PHANTOM_PROMOTED_WITHOUT_VERIFY"]
        "#;
        let mut config: Config = toml::from_str(toml_str).unwrap();
        config.doctor.normalize();
        assert_eq!(
            config.doctor.acknowledged_codes,
            vec!["PHANTOM_PROMOTED_WITHOUT_VERIFY", "sig-pin", "sig-version"]
        );
    }

    #[test]
    fn doctor_config_omitted_section_is_empty() {
        let config: Config = toml::from_str("[intent]\nrequire_signing = false\n").unwrap();
        assert!(config.doctor.acknowledged_codes.is_empty());
        assert!(!config.doctor.contains("sig-pin"));
    }

    #[test]
    fn stale_ack_contains_is_inert_not_an_error() {
        let mut cfg = DoctorConfig {
            acknowledged_codes: vec!["not-a-live-finding".to_string()],
        };
        cfg.normalize();
        assert!(cfg.contains("not-a-live-finding"));
    }
}
