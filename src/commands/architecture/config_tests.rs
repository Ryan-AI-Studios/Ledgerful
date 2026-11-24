use super::*;
use crate::commands::architecture::test_support::{COMPONENTS, FORBID_B_TO_A};

fn rejected(text: &str, item: &str) {
    let error = parse_architecture_toml(text).unwrap_err().to_string();
    assert!(error.contains("ARCHITECTURE_CONFIG_INVALID"), "{error}");
    assert!(error.contains(item), "expected {item:?} in {error}");
}

#[test]
fn architecture_config_full_fixture_parses() {
    let parsed = parse_architecture_toml(&format!("{COMPONENTS}{FORBID_B_TO_A}")).unwrap();
    assert_eq!(parsed.schema_version, 1);
    assert_eq!(parsed.components.len(), 3);
    assert_eq!(parsed.components[0].paths, ["src/a/**"]);
    assert_eq!(parsed.rules[0].effect, RuleEffect::Forbid);
    assert_eq!(parsed.rules[0].id, "b-to-a");
}

#[test]
fn architecture_config_ruleless_fixture_parses() {
    let parsed = parse_architecture_toml(COMPONENTS).unwrap();
    assert!(parsed.rules.is_empty());
}

#[test]
fn architecture_config_rejects_unknown_keys() {
    rejected(
        &format!("typo = true\n{COMPONENTS}"),
        "unknown top-level key",
    );
    rejected(
        &format!("{COMPONENTS}\npathz = []"),
        "unknown component key",
    );
    rejected(
        &format!("{COMPONENTS}{FORBID_B_TO_A}\nforbidden = false"),
        "unknown rule key",
    );
}

#[test]
fn architecture_config_rejects_schema_version_defects() {
    rejected(
        &COMPONENTS.replacen("schema_version = 1", "", 1),
        "schema_version field is required",
    );
    for replacement in ["schema_version = 0", "schema_version = 2"] {
        rejected(
            &COMPONENTS.replacen("schema_version = 1", replacement, 1),
            "schema_version",
        );
    }
}

#[test]
fn architecture_config_rejects_component_defects() {
    rejected("schema_version = 1", "components");
    for (name, offender) in [
        ("a", "duplicate"),
        ("*", "*"),
        ("Upper", "Upper"),
        ("has space", "has space"),
    ] {
        rejected(
            &COMPONENTS.replace("name = \"b\"", &format!("name = \"{name}\"")),
            offender,
        );
    }
    rejected(
        &COMPONENTS.replace("[\"src/a/**\"]", "[]"),
        "component \"a\"",
    );
}

#[test]
fn architecture_config_rejects_glob_defects() {
    for pattern in [
        "[",
        "",
        "/abs",
        "./here",
        r"src\a",
        "src/../out",
        "C:temp/**",
        "~/src",
        "~user/docs",
    ] {
        let pattern = serde_json::to_string(pattern).unwrap();
        let text = COMPONENTS.replace("[\"src/a/**\"]", &format!("[{pattern}]"));
        rejected(&text, &pattern);
    }
}

#[test]
fn architecture_config_rejects_rule_defects() {
    let initial = format!("{COMPONENTS}{FORBID_B_TO_A}");
    rejected(&format!("{initial}{FORBID_B_TO_A}"), "b-to-a");
    rejected(&initial.replace("id = \"b-to-a\"", "id = \"\""), "rule id");
    rejected(
        &initial.replace("from = \"b\"", "from = \"missing\""),
        "missing",
    );
    rejected(
        &initial.replace("to = \"a\"", "to = \"missing\""),
        "missing",
    );
    rejected(
        &initial.replace("effect = \"forbid\"", "effect = \"forbidden\""),
        "rule effect must be allow or forbid",
    );
    rejected(
        &initial.replace("from = \"b\"", "from = \"a\""),
        "same component",
    );
    rejected(
        &format!("{initial}{}", FORBID_B_TO_A.replace("b-to-a", "other")),
        "other",
    );
    let wildcard = initial
        .replace("from = \"b\"", "from = \"*\"")
        .replace("to = \"a\"", "to = \"*\"");
    assert_eq!(parse_architecture_toml(&wildcard).unwrap().rules.len(), 1);
}

#[test]
fn architecture_config_digest_stable_and_byte_sensitive() {
    let input = b"schema_version = 1\n";
    assert_eq!(config_digest(input), config_digest(input));
    assert_ne!(config_digest(input), config_digest(b"schema_version = 2\n"));
    assert_ne!(
        config_digest(input),
        config_digest(b"schema_version = 1\r\n")
    );
    assert!(config_digest(input).starts_with("blake3:"));
}

#[test]
fn architecture_config_name_rule_boundaries() {
    assert!(is_valid_component_name(&"x".repeat(64)));
    assert!(!is_valid_component_name(&"x".repeat(65)));
    assert!(is_valid_component_name("0"));
    assert!(is_valid_component_name("a.b_c-d"));
    for name in ["", ".a", "-a", "_a", "é", "A", "a/b"] {
        assert!(!is_valid_component_name(name), "{name}");
    }
}
