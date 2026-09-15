use serde::{Deserialize, Serialize};

fn default_audit_overall_budget_secs() -> u64 {
    25
}

/// `[audit]` configuration — overall emit budget for unscoped `audit` /
/// `ledger audit` (0350). Distinct from `[hotspots] history_budget_secs`.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct AuditConfig {
    /// Overall unscoped-audit emit budget in seconds. `0` disables the wall clock.
    #[serde(default = "default_audit_overall_budget_secs")]
    pub overall_budget_secs: u64,
}

impl Default for AuditConfig {
    fn default() -> Self {
        Self {
            overall_budget_secs: default_audit_overall_budget_secs(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::model::Config;

    #[test]
    #[allow(non_snake_case)]
    fn audit_config_default__overall_25() {
        assert_eq!(Config::default().audit.overall_budget_secs, 25);
        assert_eq!(AuditConfig::default().overall_budget_secs, 25);
    }

    #[test]
    #[allow(non_snake_case)]
    fn audit_config_default__serializes_audit_overall_budget_secs() {
        let v = serde_json::to_value(Config::default()).expect("config json");
        assert_eq!(v["audit"]["overall_budget_secs"], 25);
    }
}
