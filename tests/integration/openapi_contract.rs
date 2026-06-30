//! Track 0011 — Machine-readable API contract drift test.
//!
//! Regenerates the OpenAPI document from the annotated Rust DTOs/handlers and
//! compares it to the committed artifact at `docs/api/openapi.json`.
//! Any divergence ⇒ test failure.

use ledgerful::commands::web::api::generate_openapi_json;

/// Location of the committed OpenAPI artifact relative to the crate root.
const OPENAPI_PATH: &str = "docs/api/openapi.json";

/// Parse a JSON string into a `serde_json::Value` with sorted object keys so
/// that semantically-equal documents compare equal regardless of key order.
fn canonicalize(input: &str) -> serde_json::Value {
    let mut value: serde_json::Value =
        serde_json::from_str(input).unwrap_or_else(|e| panic!("failed to parse JSON: {e}"));
    sort_json(&mut value);
    value
}

fn sort_json(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            // serde_json::Map preserves insertion order by default; collect into
            // a BTreeMap to get sorted keys, then rebuild.
            let mut sorted: std::collections::BTreeMap<String, serde_json::Value> =
                std::collections::BTreeMap::new();
            for (k, mut v) in std::mem::take(map) {
                sort_json(&mut v);
                sorted.insert(k, v);
            }
            map.clear();
            for (k, v) in sorted {
                map.insert(k, v);
            }
        }
        serde_json::Value::Array(arr) => {
            for v in arr {
                sort_json(v);
            }
        }
        _ => {}
    }
}

#[test]
fn openapi_artifact_exists() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let path = std::path::Path::new(manifest_dir).join(OPENAPI_PATH);
    assert!(
        path.exists(),
        "OpenAPI artifact not found at {}. Run the `openapi_contract_generate` test to create it.",
        path.display()
    );
}

#[test]
fn openapi_drift_check() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let artifact_path = std::path::Path::new(manifest_dir).join(OPENAPI_PATH);

    let committed = std::fs::read_to_string(&artifact_path).unwrap_or_else(|e| {
        panic!(
            "failed to read {}: {e}\nRun `openapi_contract_generate` to create the artifact.",
            artifact_path.display()
        )
    });

    let generated = generate_openapi_json();

    let committed_canon = canonicalize(&committed);
    let generated_canon = canonicalize(&generated);

    if committed_canon != generated_canon {
        // Produce a helpful diff-style message.
        let committed_str = serde_json::to_string_pretty(&committed_canon).unwrap_or_default();
        let generated_str = serde_json::to_string_pretty(&generated_canon).unwrap_or_default();
        panic!(
            "OpenAPI drift detected: the committed artifact does not match the schema generated \
             from the current Rust DTOs.\n\n\
             This means a DTO or handler annotation changed without regenerating \
             `docs/api/openapi.json`.\n\n\
             To fix: run `cargo test --test integration --all-features -- \
             openapi_contract::openapi_contract_generate --ignored` to regenerate the artifact, \
             then commit it.\n\n\
             --- committed (canonicalized)\n{}\n\n--- generated (canonicalized)\n{}",
            committed_str, generated_str
        );
    }
}

/// Generate (or regenerate) the committed OpenAPI artifact.
///
/// Run with `--ignored` to write `docs/api/openapi.json`:
/// ```text
/// cargo test --test integration --all-features -- openapi_contract_generate --ignored
/// ```
#[test]
#[ignore]
fn openapi_contract_generate() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let artifact_path = std::path::Path::new(manifest_dir).join(OPENAPI_PATH);

    let json = generate_openapi_json();
    std::fs::create_dir_all(
        artifact_path
            .parent()
            .unwrap_or_else(|| panic!("invalid artifact path: {}", artifact_path.display())),
    )
    .unwrap_or_else(|e| panic!("failed to create docs/api/: {e}"));
    std::fs::write(&artifact_path, &json)
        .unwrap_or_else(|e| panic!("failed to write {}: {e}", artifact_path.display()));
    eprintln!("OpenAPI artifact written to {}", artifact_path.display());
}

/// Assert that all property names in the generated OpenAPI schemas use
/// snake_case or camelCase consistently — no Rust-style PascalCase field names
/// leaking through (DoD-3 wire fidelity).
#[test]
fn openapi_wire_casing_fidelity() {
    let generated = generate_openapi_json();
    let value: serde_json::Value =
        serde_json::from_str(&generated).expect("generated OpenAPI is valid JSON");

    let schemas = value["components"]["schemas"]
        .as_object()
        .expect("OpenAPI has components.schemas");

    let mut violations: Vec<String> = Vec::new();

    for (schema_name, schema_def) in schemas {
        let properties = schema_def["properties"].as_object();
        if let Some(props) = properties {
            for prop_name in props.keys() {
                // Check for PascalCase violations (Rust field names leaking).
                // Wire names should be snake_case or camelCase.
                let first = prop_name.chars().next();
                if first.is_some_and(|c| c.is_uppercase()) {
                    violations.push(format!(
                        "{schema_name}.{prop_name}: starts with uppercase (PascalCase leak)"
                    ));
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "Wire casing violations found (DoD-3):\n{}",
        violations.join("\n")
    );
}
