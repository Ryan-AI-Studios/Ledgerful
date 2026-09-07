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
    /// Operator id: non-empty `annotations["id"]`, else `Policy::id()` (`policy0` when no `@id`).
    pub cedar_id: String,
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

/// Resolve the operator-facing Cedar policy id. cedar-policy 4.x stores
/// `@id("…")` as the annotation key `"id"`; `Policy::id()` is only the
/// auto-assigned PolicyId (`policy0`, …) when no explicit id was parsed.
fn cedar_operator_id(annotations: &HashMap<String, String>, auto_id: String) -> String {
    annotations
        .get("id")
        .map(String::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or(auto_id)
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
            let cedar_id = cedar_operator_id(&annotations, policy.id().to_string());

            policies.push(CedarPolicy {
                effect,
                principal: Some(principal),
                action: Some(action),
                resource: Some(resource),
                conditions,
                annotations: Some(annotations),
                cedar_id,
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
            let cedar_id = cedar_operator_id(&annotations, template.id().to_string());

            policies.push(CedarPolicy {
                effect,
                principal: Some(principal),
                action: Some(action),
                resource: Some(resource),
                conditions,
                annotations: Some(annotations),
                cedar_id,
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

/// Parse `Action::"GET /api/session"` or `GET /api/session` into `(METHOD, path)`.
pub fn parse_cedar_action(action: &str) -> Option<(String, String)> {
    let trimmed = action.trim();
    let inner = trimmed
        .strip_prefix("Action::\"")
        .and_then(|rest| rest.strip_suffix('"'))
        .unwrap_or(trimmed)
        .trim();
    // Reject list-form / malformed constraints (`in [Action::"GET …", …]`)
    // instead of splitting on the first whitespace into a garbage method.
    if inner.starts_with('[') || inner.contains('"') {
        return None;
    }
    let (method, path) = inner.split_once(|c: char| c.is_whitespace())?;
    let method = method.trim().to_uppercase();
    let path = path.trim().to_string();
    if method.is_empty() || path.is_empty() {
        return None;
    }
    Some((method, path))
}

/// First `Action::"METHOD path"` in Cedar policy source.
pub fn parse_action_from_policy_raw(raw: &str) -> Option<(String, String)> {
    let marker = "Action::\"";
    let mut search = raw;
    while let Some(idx) = search.find(marker) {
        let rest = &search[idx + marker.len()..];
        let Some(end) = rest.find('"') else {
            break;
        };
        let inner = &rest[..end];
        if let Some(parsed) = parse_cedar_action(&format!("Action::\"{inner}\"")) {
            return Some(parsed);
        }
        search = &rest[end + 1..];
    }
    None
}

/// `@id("route_get_api_config")` from policy source.
pub fn extract_at_id(raw: &str) -> Option<String> {
    let marker = "@id(\"";
    let start = raw.find(marker)?;
    let rest = &raw[start + marker.len()..];
    let end = rest.find('"')?;
    let id = rest[..end].trim();
    if id.is_empty() {
        None
    } else {
        Some(id.to_string())
    }
}

/// Operator policy label: explicit `@id` annotation wins, then stored
/// `cedar_id`, then raw `@id`, then the stored label. Never `Policy: permit N`
/// when an id exists (spec B3.4: annotations → raw → non-legacy label).
pub fn policy_operator_label(
    stored_label: &str,
    cedar_id: Option<&str>,
    annotations_id: Option<&str>,
    raw: Option<&str>,
) -> String {
    if let Some(id) = annotations_id.map(str::trim).filter(|s| !s.is_empty()) {
        return id.to_string();
    }
    if let Some(id) = cedar_id.map(str::trim).filter(|s| !s.is_empty()) {
        return id.to_string();
    }
    if let Some(id) = raw.and_then(extract_at_id) {
        return id;
    }
    stored_label.to_string()
}

/// Cedar action path matches a route row: same method, and path equal or nest-relative of `/api`.
pub fn cedar_action_matches_route(action: &str, route_method: &str, route_path: &str) -> bool {
    let Some((action_method, action_path)) = parse_cedar_action(action) else {
        return false;
    };
    if !action_method.eq_ignore_ascii_case(route_method) {
        return false;
    }
    action_path == route_path
        || action_path
            .strip_prefix("/api")
            .is_some_and(|rest| rest == route_path && route_path.starts_with('/'))
}

/// When both `/session` and `/api/session` exist for the same method, keep `/api`.
pub fn prefer_api_endpoint_paths(hits: &[(String, String)]) -> Vec<(String, String)> {
    let set: std::collections::HashSet<(String, String)> = hits.iter().cloned().collect();
    let mut out: Vec<(String, String)> = hits
        .iter()
        .filter(|(method, path)| {
            if path == "/api" || path.starts_with("/api/") {
                return true;
            }
            let api_form = format!("/api{path}");
            !set.contains(&(method.clone(), api_form))
        })
        .cloned()
        .collect();
    out.sort();
    out.dedup();
    out
}

/// One boundary-link row used by CLI JSON and REST value assembly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundaryLink {
    pub policy_id: String,
    pub policy_label: String,
    pub relation: String,
    pub target_id: String,
    pub target_label: String,
    pub target_category: String,
}

fn parse_endpoint_method_path(target_id: &str, target_label: &str) -> Option<(String, String)> {
    const PREFIX: &str = "urn:ledgerful:endpoint:";
    if let Some(rest) = target_id.strip_prefix(PREFIX) {
        let (method, path) = rest.split_once(':')?;
        if !method.is_empty() && !path.is_empty() {
            return Some((method.to_uppercase(), path.to_string()));
        }
    }
    let (method, path) = target_label.split_once(|c: char| c.is_whitespace())?;
    let method = method.trim().to_uppercase();
    let path = path.trim().to_string();
    if method.is_empty() || path.is_empty() {
        None
    } else {
        Some((method, path))
    }
}

/// Drop cross-method false-hits on endpoints; prefer `/api` spelling when both exist.
pub fn refine_boundary_links(
    links: Vec<BoundaryLink>,
    policy_actions: &HashMap<String, (String, String)>,
) -> Vec<BoundaryLink> {
    let mut kept: Vec<BoundaryLink> = links
        .into_iter()
        .filter(|link| {
            if link.target_category != "endpoint" {
                return true;
            }
            let Some((want_method, _)) = policy_actions.get(&link.policy_id) else {
                return true;
            };
            let Some((got_method, _)) =
                parse_endpoint_method_path(&link.target_id, &link.target_label)
            else {
                return true;
            };
            got_method.eq_ignore_ascii_case(want_method)
        })
        .collect();

    let mut api_pairs: std::collections::HashSet<(String, String, String)> =
        std::collections::HashSet::new();
    for link in &kept {
        if link.target_category != "endpoint" {
            continue;
        }
        if let Some((method, path)) =
            parse_endpoint_method_path(&link.target_id, &link.target_label)
            && (path == "/api" || path.starts_with("/api/"))
        {
            api_pairs.insert((link.policy_id.clone(), method, path));
        }
    }
    kept.retain(|link| {
        if link.target_category != "endpoint" {
            return true;
        }
        let Some((method, path)) = parse_endpoint_method_path(&link.target_id, &link.target_label)
        else {
            return true;
        };
        if path == "/api" || path.starts_with("/api/") {
            return true;
        }
        let api_form = format!("/api{path}");
        !api_pairs.contains(&(link.policy_id.clone(), method, api_form))
    });

    kept.sort_by(|a, b| {
        (
            a.policy_label.as_str(),
            a.relation.as_str(),
            a.target_label.as_str(),
            a.target_category.as_str(),
            a.policy_id.as_str(),
            a.target_id.as_str(),
        )
            .cmp(&(
                b.policy_label.as_str(),
                b.relation.as_str(),
                b.target_label.as_str(),
                b.target_category.as_str(),
                b.policy_id.as_str(),
                b.target_id.as_str(),
            ))
    });
    kept
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
            assert_eq!(
                policy.cedar_id, want_id,
                "cedar_id must equal @id for {action}"
            );
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

    #[test]
    fn policy_operator_label_prefers_at_id_over_permit_index() {
        let raw = r#"@id("route_get_api_config")
permit (
    principal,
    action == Action::"GET /api/config",
    resource
);"#;
        let label = policy_operator_label("Policy: permit 6", None, None, Some(raw));
        assert_eq!(label, "route_get_api_config");
        let from_ann =
            policy_operator_label("Policy: permit 6", None, Some("route_get_api_config"), None);
        assert_eq!(from_ann, "route_get_api_config");
        let from_id = policy_operator_label(
            "Policy: permit 6",
            Some("route_get_api_hotspots"),
            None,
            None,
        );
        assert_eq!(from_id, "route_get_api_hotspots");
        // Explicit annotation wins over a conflicting stored cedar_id (B3.4).
        let ann_over_cedar = policy_operator_label(
            "Policy: permit 6",
            Some("stale_auto_id"),
            Some("route_get_api_config"),
            None,
        );
        assert_eq!(ann_over_cedar, "route_get_api_config");
        let empty_ann = policy_operator_label("Policy: permit 6", Some("policy0"), Some(""), None);
        assert_eq!(empty_ann, "policy0", "empty @id is missing");
    }

    #[test]
    fn cedar_action_matches_nest_relative_and_requires_method() {
        let action = r#"Action::"POST /api/session/exchange""#;
        assert!(cedar_action_matches_route(
            action,
            "POST",
            "/api/session/exchange"
        ));
        assert!(cedar_action_matches_route(
            action,
            "POST",
            "/session/exchange"
        ));
        assert!(
            !cedar_action_matches_route(action, "GET", "/session"),
            "POST policy must not match GET /session"
        );
        assert!(cedar_action_matches_route(
            r#"Action::"GET /api/session""#,
            "GET",
            "/session"
        ));
        assert!(
            !cedar_action_matches_route(r#"Action::"GET /apifoo""#, "GET", "foo"),
            "prefix-strip must not match a non-slash route path"
        );
    }

    #[test]
    fn prefer_api_endpoint_paths_keeps_api_spelling() {
        let hits = vec![
            ("POST".to_string(), "/session/exchange".to_string()),
            ("POST".to_string(), "/api/session/exchange".to_string()),
            ("GET".to_string(), "/session".to_string()),
        ];
        let kept = prefer_api_endpoint_paths(&hits);
        assert_eq!(
            kept,
            vec![
                ("GET".to_string(), "/session".to_string()),
                ("POST".to_string(), "/api/session/exchange".to_string()),
            ]
        );
    }

    #[test]
    fn refine_boundary_links_drops_cross_method_and_nest_duplicate() {
        let mut actions = HashMap::new();
        actions.insert(
            "p2".to_string(),
            ("POST".to_string(), "/api/session/exchange".to_string()),
        );
        let links = vec![
            BoundaryLink {
                policy_id: "p2".into(),
                policy_label: "route_post_api_session_exchange".into(),
                relation: "protected_by".into(),
                target_id: "urn:ledgerful:endpoint:GET:/session".into(),
                target_label: "GET /session".into(),
                target_category: "endpoint".into(),
            },
            BoundaryLink {
                policy_id: "p2".into(),
                policy_label: "route_post_api_session_exchange".into(),
                relation: "protected_by".into(),
                target_id: "urn:ledgerful:endpoint:POST:/session/exchange".into(),
                target_label: "POST /session/exchange".into(),
                target_category: "endpoint".into(),
            },
            BoundaryLink {
                policy_id: "p2".into(),
                policy_label: "route_post_api_session_exchange".into(),
                relation: "protected_by".into(),
                target_id: "urn:ledgerful:endpoint:POST:/api/session/exchange".into(),
                target_label: "POST /api/session/exchange".into(),
                target_category: "endpoint".into(),
            },
        ];
        let kept = refine_boundary_links(links, &actions);
        assert_eq!(kept.len(), 1, "{kept:?}");
        assert_eq!(kept[0].target_label, "POST /api/session/exchange");
    }

    #[test]
    fn parse_cedar_action_rejects_list_form_instead_of_garbage_split() {
        // `action in [Action::"GET /api/users", Action::"POST /api/users"]`
        // serializes as "[Action::\"GET /api/users\", Action::\"POST /api/users\"]".
        // Must parse to None (not a garbage method token), so callers fall back
        // to raw-source extraction instead of silently killing links.
        let list = "[Action::\"GET /api/users\", Action::\"POST /api/users\"]";
        assert_eq!(parse_cedar_action(list), None);
        let inner_quotes = "Action::\"GET /api/users\", Action::\"POST /api/users\"";
        assert_eq!(parse_cedar_action(inner_quotes), None);
        // Eq form still parses.
        assert_eq!(
            parse_cedar_action(r#"Action::"GET /api/users""#),
            Some(("GET".to_string(), "/api/users".to_string()))
        );
    }

    #[test]
    fn parse_action_from_policy_raw_recovers_list_form_first_member() {
        // List-form policy: metadata.action is unparseable; raw carries the
        // quoted members. First member recovers the link.
        let raw = r#"@id("route_list_users")
permit (
    principal,
    action in [Action::"GET /api/users", Action::"POST /api/users"],
    resource
);"#;
        assert_eq!(
            parse_action_from_policy_raw(raw),
            Some(("GET".to_string(), "/api/users".to_string()))
        );
        // Unterminated quote must not loop or fabricate a value.
        assert_eq!(
            parse_action_from_policy_raw(r#"Action::"GET /api/users"#),
            None
        );
        // Non-method member skipped; later valid member still found.
        let mixed = r#"x Action::"view" y Action::"GET /api/users" z"#;
        assert_eq!(
            parse_action_from_policy_raw(mixed),
            Some(("GET".to_string(), "/api/users".to_string()))
        );
    }

    #[test]
    fn extract_at_id_bounds_and_rejects_empty() {
        assert_eq!(
            extract_at_id(
                r#"@id("route_get_api_config")
permit (principal, action, resource);"#
            ),
            Some("route_get_api_config".to_string())
        );
        // Unterminated string.
        assert_eq!(extract_at_id("@id(\"never_closed"), None);
        // No marker.
        assert_eq!(extract_at_id("permit (principal, action, resource);"), None);
        // Empty id is rejected.
        assert_eq!(extract_at_id("@id(\"\"), x"), None);
    }
}
