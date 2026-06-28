use serde::Serialize;
use serde_json::json;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum EmptyReason {
    CleanDiff,
    DisabledByConfig,
    NoIndexedData,
    StaleIndex,
    MissingSourceFiles,
    NoMatches,
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum JsonEmptyState<T: Serialize> {
    Results(Vec<T>),
    Empty {
        results: Vec<T>,
        empty_reason: EmptyReason,
        message: String,
    },
}

impl<T: Serialize> JsonEmptyState<T> {
    pub fn new_empty(reason: EmptyReason, message: String) -> Self {
        JsonEmptyState::Empty {
            results: Vec::new(),
            empty_reason: reason,
            message,
        }
    }
}

pub fn format_json_empty_state<T: Serialize>(
    items: Vec<T>,
    key: &str,
    reason_fn: impl FnOnce() -> (EmptyReason, String),
) -> serde_json::Value {
    if items.is_empty() {
        let (reason, message) = reason_fn();
        let mut map = serde_json::Map::new();
        map.insert(key.to_string(), json!(items));
        map.insert("emptyReason".to_string(), json!(reason));
        map.insert("message".to_string(), json!(message));
        json!(map)
    } else {
        // If not empty, return array
        json!(items)
    }
}

/// Builds the standardized `config set` enablement hint appended to empty-state
/// messages when emptiness is caused by a disabled config gate. Produces
/// identical phrasing across every CLI surface so users always see the same
/// copy-pasteable instruction.
///
/// - 1 key:  `To enable, run: \`ledgerful config set <k>=true\`.`
/// - 2 keys: `To enable, run: \`ledgerful config set <k1>=true\` (then
///   \`ledgerful config set <k2>=true\`).`
/// - empty:  empty string (caller should not append a hint).
pub fn config_enable_hint(keys: &[&str]) -> String {
    match keys {
        [] => String::new(),
        [k] => {
            format!("To enable, run: `ledgerful config set {k}=true`.")
        }
        [k1, k2] => {
            format!(
                "To enable, run: `ledgerful config set {k1}=true` (then \
                 `ledgerful config set {k2}=true`)."
            )
        }
        _ => {
            let mut out = String::from("To enable, run: ");
            for (i, k) in keys.iter().enumerate() {
                if i == 0 {
                    out.push_str(&format!("`ledgerful config set {k}=true`"));
                } else {
                    out.push_str(&format!(" (then `ledgerful config set {k}=true`)"));
                }
            }
            out.push('.');
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hint_single_key() {
        let hint = config_enable_hint(&["coverage.services.enabled"]);
        assert_eq!(
            hint,
            "To enable, run: `ledgerful config set coverage.services.enabled=true`."
        );
    }

    #[test]
    fn hint_two_keys_parenthetical() {
        let hint = config_enable_hint(&["coverage.enabled", "coverage.services.enabled"]);
        assert_eq!(
            hint,
            "To enable, run: `ledgerful config set coverage.enabled=true` (then \
             `ledgerful config set coverage.services.enabled=true`)."
        );
    }

    #[test]
    fn hint_empty_keys_returns_empty_string() {
        let hint = config_enable_hint(&[]);
        assert!(hint.is_empty());
    }

    #[test]
    fn hint_three_keys_chains_parentheticals() {
        // The >2-keys branch renders the first key, then a `(then ...)` clause
        // for every subsequent key. This pins the deterministic phrasing so
        // callers do not regress the shape.
        let hint = config_enable_hint(&[
            "coverage.enabled",
            "coverage.deploy.enabled",
            "coverage.services.enabled",
        ]);
        assert_eq!(
            hint,
            "To enable, run: `ledgerful config set coverage.enabled=true` \
             (then `ledgerful config set coverage.deploy.enabled=true`) \
             (then `ledgerful config set coverage.services.enabled=true`)."
        );
    }
}
