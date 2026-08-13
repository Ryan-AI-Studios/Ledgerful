use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::LazyLock;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CedarPolicy {
    pub effect: String,
    pub principal: Option<String>,
    pub action: Option<String>,
    pub resource: Option<String>,
    pub conditions: Option<String>,
    pub annotations: Option<HashMap<String, String>>,
    pub is_template: bool,
    pub template_id: Option<String>,
    pub raw: String,
}

pub struct CedarImporter;

static CONDITION_PAT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?s)(when|unless)\s*\{([^}]*)\}"#).unwrap());

fn extract_conditions_from_raw(raw: &str) -> Option<String> {
    let mut conditions_list = Vec::new();
    for cond_cap in CONDITION_PAT.captures_iter(raw) {
        let cond_type = cond_cap[1].to_string();
        let cond_body = cond_cap[2].trim().to_string();
        conditions_list.push(format!("{} {{ {} }}", cond_type, cond_body));
    }
    if !conditions_list.is_empty() {
        Some(conditions_list.join(" "))
    } else {
        None
    }
}

// Format PrincipalConstraint
fn format_principal_constraint(c: &cedar_policy::PrincipalConstraint) -> String {
    match c {
        cedar_policy::PrincipalConstraint::Any => "any".to_string(),
        cedar_policy::PrincipalConstraint::Eq(uid) => uid.to_string(),
        cedar_policy::PrincipalConstraint::In(uid) => format!("in {}", uid),
        _ => format!("{:?}", c),
    }
}

// Format TemplatePrincipalConstraint
fn format_template_principal_constraint(c: &cedar_policy::TemplatePrincipalConstraint) -> String {
    match c {
        cedar_policy::TemplatePrincipalConstraint::Any => "any".to_string(),
        cedar_policy::TemplatePrincipalConstraint::Eq(uid) => uid
            .as_ref()
            .map(|u| u.to_string())
            .unwrap_or_else(|| "?principal".to_string()),
        cedar_policy::TemplatePrincipalConstraint::In(uid) => uid
            .as_ref()
            .map(|u| format!("in {}", u))
            .unwrap_or_else(|| "in ?principal".to_string()),
        _ => format!("{:?}", c),
    }
}

// Format ActionConstraint
fn format_action_constraint(c: &cedar_policy::ActionConstraint) -> String {
    match c {
        cedar_policy::ActionConstraint::Any => "any".to_string(),
        cedar_policy::ActionConstraint::Eq(uid) => uid.to_string(),
        cedar_policy::ActionConstraint::In(uids) => {
            let ids: Vec<String> = uids.iter().map(|uid| uid.to_string()).collect();
            if ids.len() == 1 {
                ids[0].clone()
            } else {
                format!("[{}]", ids.join(", "))
            }
        }
    }
}

// Format ResourceConstraint
fn format_resource_constraint(c: &cedar_policy::ResourceConstraint) -> String {
    match c {
        cedar_policy::ResourceConstraint::Any => "any".to_string(),
        cedar_policy::ResourceConstraint::Eq(uid) => uid.to_string(),
        cedar_policy::ResourceConstraint::In(uid) => format!("in {}", uid),
        _ => format!("{:?}", c),
    }
}

// Format TemplateResourceConstraint
fn format_template_resource_constraint(c: &cedar_policy::TemplateResourceConstraint) -> String {
    match c {
        cedar_policy::TemplateResourceConstraint::Any => "any".to_string(),
        cedar_policy::TemplateResourceConstraint::Eq(uid) => uid
            .as_ref()
            .map(|u| u.to_string())
            .unwrap_or_else(|| "?resource".to_string()),
        cedar_policy::TemplateResourceConstraint::In(uid) => uid
            .as_ref()
            .map(|u| format!("in {}", u))
            .unwrap_or_else(|| "in ?resource".to_string()),
        _ => format!("{:?}", c),
    }
}

impl CedarImporter {
    pub fn new() -> Self {
        Self
    }

    pub fn parse(&self, content: &str) -> Vec<CedarPolicy> {
        let mut policies = Vec::new();

        let policy_set = match cedar_policy::PolicySet::from_str(content) {
            Ok(ps) => ps,
            Err(e) => {
                tracing::warn!("Failed to parse Cedar policies: {:?}", e);
                return Vec::new();
            }
        };

        // 1. Process static and template-linked policies
        for policy in policy_set.policies() {
            let effect = match policy.effect() {
                cedar_policy::Effect::Permit => "permit".to_string(),
                cedar_policy::Effect::Forbid => "forbid".to_string(),
            };

            let principal = format_principal_constraint(&policy.principal_constraint());
            let action = format_action_constraint(&policy.action_constraint());
            let resource = format_resource_constraint(&policy.resource_constraint());

            let mut annotations = HashMap::new();
            for (key, val) in policy.annotations() {
                annotations.insert(key.to_string(), val.to_string());
            }

            let is_template = policy.template_id().is_some();
            let template_id = policy.template_id().map(|id| id.to_string());

            let raw = policy.to_string();
            let conditions = extract_conditions_from_raw(&raw);

            policies.push(CedarPolicy {
                effect,
                principal: Some(principal),
                action: Some(action),
                resource: Some(resource),
                conditions,
                annotations: Some(annotations),
                is_template,
                template_id,
                raw,
            });
        }

        // 2. Process templates
        for template in policy_set.templates() {
            let effect = match template.effect() {
                cedar_policy::Effect::Permit => "permit".to_string(),
                cedar_policy::Effect::Forbid => "forbid".to_string(),
            };

            let principal = format_template_principal_constraint(&template.principal_constraint());
            let action = format_action_constraint(&template.action_constraint());
            let resource = format_template_resource_constraint(&template.resource_constraint());

            let mut annotations = HashMap::new();
            for (key, val) in template.annotations() {
                annotations.insert(key.to_string(), val.to_string());
            }

            let raw = template.to_string();
            let conditions = extract_conditions_from_raw(&raw);

            policies.push(CedarPolicy {
                effect,
                principal: Some(principal),
                action: Some(action),
                resource: Some(resource),
                conditions,
                annotations: Some(annotations),
                is_template: true,
                template_id: None,
                raw,
            });
        }

        policies
    }
}

impl Default for CedarImporter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_policy() {
        let content = r#"permit(principal == User::"alice", action == Action::"view", resource == Photo::"vacation.jpg");"#;
        let importer = CedarImporter::new();
        let policies = importer.parse(content);
        assert_eq!(policies.len(), 1);
        assert_eq!(policies[0].effect, "permit");
        assert_eq!(policies[0].principal, Some("User::\"alice\"".to_string()));
        assert_eq!(policies[0].action, Some("Action::\"view\"".to_string()));
        assert_eq!(
            policies[0].resource,
            Some("Photo::\"vacation.jpg\"".to_string())
        );
    }

    #[test]
    fn test_parse_unconstrained_policy() {
        let content = r#"permit(principal, action, resource);"#;
        let importer = CedarImporter::new();
        let policies = importer.parse(content);
        assert_eq!(policies.len(), 1);
        assert_eq!(policies[0].principal, Some("any".to_string()));
        assert_eq!(policies[0].action, Some("any".to_string()));
        assert_eq!(policies[0].resource, Some("any".to_string()));
    }

    #[test]
    fn test_parse_action_list_policy() {
        let content = r#"permit(principal, action in [Action::"view", Action::"edit"], resource);"#;
        let importer = CedarImporter::new();
        let policies = importer.parse(content);
        assert_eq!(policies.len(), 1);
        assert_eq!(
            policies[0].action,
            Some("[Action::\"view\", Action::\"edit\"]".to_string())
        );
    }

    #[test]
    fn test_parse_multiple_conditions() {
        let content = r#"permit(principal, action, resource) when { principal.age > 18 } unless { resource.is_private };"#;
        let importer = CedarImporter::new();
        let policies = importer.parse(content);
        assert_eq!(policies.len(), 1);
        assert_eq!(
            policies[0].conditions,
            Some("when { principal.age > 18 } unless { resource.is_private }".to_string())
        );
    }

    #[test]
    fn test_parse_multiple_policies() {
        let content = r#"
            permit(principal == User::"alice", action == Action::"view", resource == Photo::"vacation.jpg");
            forbid(principal == User::"bob", action == Action::"delete", resource == Photo::"vacation.jpg");
        "#;
        let importer = CedarImporter::new();
        let policies = importer.parse(content);
        assert_eq!(policies.len(), 2);
    }

    #[test]
    fn committed_daemon_api_cedar_parses_eight_dx1_permits() {
        use crate::commands::dx1_templates::sanitize_cedar_id;

        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("policies")
            .join("daemon-api.cedar");
        let content = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("committed pack must be readable at {path:?}: {e}"));
        assert!(
            !content.contains("Feature::\"dogfood\""),
            "product pack must not copy the fixture Feature::dogfood"
        );
        assert!(
            !content.contains("Generated by `ledgerful security boundaries`"),
            "must not be the DX1 all-routes dump"
        );
        assert_eq!(
            content.matches("permit (").count(),
            8,
            "0186-G is 8 permits, not the ~26-route DX1 dump"
        );

        let importer = CedarImporter::new();
        let policies = importer.parse(&content);
        assert_eq!(
            policies.len(),
            8,
            "0186-G is 8 core routes, got {policies:?}"
        );

        let expected: [(&str, &str); 8] = [
            ("GET", "/api/status"),
            ("GET", "/api/session"),
            ("POST", "/api/session/exchange"),
            ("GET", "/api/snapshot"),
            ("GET", "/api/ledger"),
            ("GET", "/api/hotspots"),
            ("GET", "/api/config"),
            ("GET", "/api/security/boundaries"),
        ];
        let mut seen = std::collections::BTreeSet::new();
        for policy in &policies {
            assert_eq!(policy.effect, "permit");
            assert_eq!(policy.principal.as_deref(), Some("any"));
            assert_eq!(policy.resource.as_deref(), Some("any"));
            let action = policy.action.as_deref().unwrap_or("");
            let (method, path) = expected
                .iter()
                .find(|(m, p)| action == format!("Action::\"{m} {p}\""))
                .unwrap_or_else(|| panic!("unexpected action {action}"));
            let want_id = format!("route_{}", sanitize_cedar_id(method, path));
            let id = policy
                .annotations
                .as_ref()
                .and_then(|a| a.get("id"))
                .map(String::as_str)
                .unwrap_or("");
            assert_eq!(id, want_id, "action {action} @id must match sanitizer");
            seen.insert(*path);
        }
        assert_eq!(seen.len(), 8);
    }

    #[test]
    fn fixture_dogfood_policy_is_not_under_repo_root_policies() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        assert!(
            root.join("tests/fixtures/policies/dogfood_policy.cedar")
                .is_file(),
            "hermetic fixture must remain under tests/fixtures/policies/"
        );
        assert!(
            !root.join("policies/dogfood_policy.cedar").exists(),
            "fixture must not be copied to repo-root policies/"
        );
        assert!(
            root.join("policies/daemon-api.cedar").is_file(),
            "product pack lives at policies/daemon-api.cedar"
        );
    }
}
