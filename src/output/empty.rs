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

/// Always-object list envelope (`schemaVersion` 1 + collection key + `resultCount`).
/// Used by printers that never carry `emptyReason` (including empty catalogs).
pub fn format_json_list_envelope<T: Serialize>(items: Vec<T>, key: &str) -> serde_json::Value {
    let result_count = items.len();
    let mut map = serde_json::Map::new();
    map.insert("schemaVersion".to_string(), json!(1));
    map.insert(key.to_string(), json!(items));
    map.insert("resultCount".to_string(), json!(result_count));
    json!(map)
}

pub fn format_json_empty_state<T: Serialize>(
    items: Vec<T>,
    key: &str,
    reason_fn: impl FnOnce() -> (EmptyReason, String),
) -> serde_json::Value {
    if items.is_empty() {
        let (reason, message) = reason_fn();
        let mut map = serde_json::Map::new();
        map.insert("schemaVersion".to_string(), json!(1));
        map.insert(key.to_string(), json!(items));
        map.insert("resultCount".to_string(), json!(0));
        map.insert("emptyReason".to_string(), json!(reason));
        map.insert("message".to_string(), json!(message));
        json!(map)
    } else {
        format_json_list_envelope(items, key)
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

    #[test]
    fn empty_list_is_object_with_reason_and_result_count_zero() {
        let items: Vec<serde_json::Value> = vec![];
        let output = format_json_empty_state(items, "results", || {
            (EmptyReason::NoMatches, "no hits".to_string())
        });
        assert!(output.is_object(), "empty arm must be an object: {output}");
        assert_eq!(output["schemaVersion"], 1);
        assert_eq!(output["resultCount"], 0);
        assert_eq!(output["emptyReason"], "noMatches");
        assert_eq!(output["message"], "no hits");
        assert_eq!(output["results"], json!([]));
        assert!(!output.is_array());
    }

    #[test]
    fn populated_list_is_object_without_empty_reason() {
        let items = vec![serde_json::json!({"path": "a.rs"})];
        let output = format_json_empty_state(items, "results", || {
            panic!("reason_fn must not run for populated lists");
        });
        assert!(
            output.is_object(),
            "populated arm must be an object: {output}"
        );
        assert_eq!(output["schemaVersion"], 1);
        assert_eq!(output["resultCount"], 1);
        assert_eq!(output["results"].as_array().map(|a| a.len()), Some(1));
        assert!(
            output.get("emptyReason").is_none(),
            "populated arm must omit emptyReason: {output}"
        );
        assert!(
            output.get("message").is_none(),
            "populated arm must omit message: {output}"
        );
        assert!(!output.is_array());
    }

    #[test]
    fn list_envelope_omits_empty_reason_even_when_empty() {
        let items: Vec<serde_json::Value> = vec![];
        let output = format_json_list_envelope(items, "gates");
        assert!(output.is_object());
        assert_eq!(output["schemaVersion"], 1);
        assert_eq!(output["resultCount"], 0);
        assert_eq!(output["gates"], json!([]));
        assert!(output.get("emptyReason").is_none());
        assert!(output.get("message").is_none());
    }
}
