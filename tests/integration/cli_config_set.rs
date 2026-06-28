//! Integration tests for `ledgerful config set` (Track DX3).
//!
//! These exercise the comment/format-preserving `execute_config_set_in`
//! surface directly (no subprocess) against a temp `Layout`, then reload via
//! `crate::config::load_config` and/or re-parse the file with `toml_edit` to
//! assert typed values, comment preservation, intermediate-table creation,
//! and error handling.

use camino::Utf8Path;
use ledgerful::commands::config::execute_config_set_in;
use ledgerful::config::load_config;
use ledgerful::state::layout::Layout;
use std::fs;
use tempfile::tempdir;

/// Build a `Layout` rooted at the temp dir.
fn layout_at(root: &std::path::Path) -> Layout {
    let utf8 = Utf8Path::from_path(root).expect("temp dir must be UTF-8");
    Layout::new(utf8)
}

/// Write an initial `.ledgerful/config.toml` with a comment + nested table.
fn write_initial_config(layout: &Layout) {
    layout.ensure_state_dir().expect("ensure_state_dir");
    let content = "# my comment\n[coverage]\n[coverage.services]\nenabled = false\n";
    fs::write(layout.config_file(), content).expect("write config");
}

// T1: existing nested key flipped to true; comment preserved; reload sees it.
#[test]
fn set_flips_existing_nested_bool_and_preserves_comment() {
    let tmp = tempdir().expect("tempdir");
    let layout = layout_at(tmp.path());
    write_initial_config(&layout);

    execute_config_set_in(&layout, "coverage.services.enabled=true").expect("set should succeed");

    let on_disk = fs::read_to_string(layout.config_file()).expect("read config");
    assert!(
        on_disk.contains("# my comment"),
        "comment must be preserved; got:\n{on_disk}"
    );
    assert!(
        on_disk.contains("enabled = true"),
        "value must be flipped to true; got:\n{on_disk}"
    );

    let config = load_config(&layout).expect("reload config");
    assert!(
        config.coverage.services.enabled,
        "reload must see enabled=true"
    );
}

// T2: a brand-new nested key path creates intermediate tables.
#[test]
fn set_creates_intermediate_tables_for_new_path() {
    let tmp = tempdir().expect("tempdir");
    let layout = layout_at(tmp.path());
    write_initial_config(&layout);

    execute_config_set_in(&layout, "coverage.metrics.enabled=true").expect("set should succeed");

    let on_disk = fs::read_to_string(layout.config_file()).expect("read config");
    let doc: toml_edit::DocumentMut = on_disk
        .parse::<toml_edit::DocumentMut>()
        .expect("reparse on disk");
    // coverage.metrics.enabled should exist as a bool true.
    let val = doc
        .get("coverage")
        .and_then(|c| c.as_table())
        .and_then(|c| c.get("metrics"))
        .and_then(|m| m.as_table())
        .and_then(|m| m.get("enabled"))
        .and_then(|e| e.as_value())
        .and_then(|v| v.as_bool())
        .expect("coverage.metrics.enabled must exist as bool");
    assert!(val, "coverage.metrics.enabled must be true");
}

// T3: value types int / float / string / bool round-trip through reload.
#[test]
fn set_handles_int_float_string_bool_value_types() {
    let tmp = tempdir().expect("tempdir");
    let layout = layout_at(tmp.path());
    // Start from the default config so typed fields exist.
    layout.ensure_state_dir().expect("ensure_state_dir");
    let default = ledgerful::config::defaults::default_config_contents().expect("default config");
    fs::write(layout.config_file(), default).expect("write default config");

    // int
    execute_config_set_in(&layout, "hotspots.limit=42").expect("set int");
    // float
    execute_config_set_in(&layout, "temporal.coupling_threshold=0.75").expect("set float");
    // quoted string
    execute_config_set_in(&layout, "intent.required=\"custom-value\"").expect("set string");
    // bool
    execute_config_set_in(&layout, "coverage.services.enabled=true").expect("set bool");

    let config = load_config(&layout).expect("reload config");
    assert_eq!(config.hotspots.limit, 42, "int round-trip");
    assert!(
        (config.temporal.coupling_threshold - 0.75).abs() < f32::EPSILON,
        "float round-trip: {}",
        config.temporal.coupling_threshold
    );
    assert_eq!(config.intent.required, "custom-value", "string round-trip");
    assert!(config.coverage.services.enabled, "bool round-trip");
}

// T4: an unquoted bareword that is not valid TOML is stored as a string.
#[test]
fn set_bareword_falls_back_to_string() {
    let tmp = tempdir().expect("tempdir");
    let layout = layout_at(tmp.path());
    // Use a typed string field so we can reload-assert.
    layout.ensure_state_dir().expect("ensure_state_dir");
    let default = ledgerful::config::defaults::default_config_contents().expect("default config");
    fs::write(layout.config_file(), default).expect("write default config");

    execute_config_set_in(&layout, "intent.required=mylabel").expect("set bareword");

    let config = load_config(&layout).expect("reload config");
    assert_eq!(
        config.intent.required, "mylabel",
        "bareword stored as string"
    );

    // Also verify the on-disk literal is stored as a quoted string, not a
    // bareword (TOML serializes strings quoted).
    let on_disk = fs::read_to_string(layout.config_file()).expect("read config");
    assert!(
        on_disk.contains("required = \"mylabel\""),
        "bareword must be serialized as a quoted string; got:\n{on_disk}"
    );
}

// T5: error cases return Err with clear messages.
#[test]
fn set_errors_on_missing_equals() {
    let tmp = tempdir().expect("tempdir");
    let layout = layout_at(tmp.path());
    write_initial_config(&layout);

    let result = execute_config_set_in(&layout, "coverage.services.enabled");
    assert!(result.is_err(), "missing `=` must error");
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains('=') || msg.contains("missing"),
        "error should mention the missing `=`: {msg}"
    );
}

#[test]
fn set_errors_on_invalid_value() {
    let tmp = tempdir().expect("tempdir");
    let layout = layout_at(tmp.path());
    write_initial_config(&layout);

    // `[1,` is an unclosed array — NOT a clean bareword — so it must error
    // rather than being silently stored as the literal string "[1,".
    let result = execute_config_set_in(&layout, "x=[1,");
    assert!(result.is_err(), "invalid value must error");
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("invalid value") || msg.contains('['),
        "error should mention the invalid value: {msg}"
    );
}

// T6: explicit comment-preservation check, independent of value assertions.
#[test]
fn set_preserves_original_comment() {
    let tmp = tempdir().expect("tempdir");
    let layout = layout_at(tmp.path());
    write_initial_config(&layout);

    // Perform an unrelated set on a different key.
    execute_config_set_in(&layout, "coverage.enabled=true").expect("set should succeed");

    let on_disk = fs::read_to_string(layout.config_file()).expect("read config");
    let comment_count = on_disk
        .lines()
        .filter(|l| l.trim_start().starts_with('#'))
        .count();
    assert!(
        on_disk.contains("# my comment"),
        "original comment must still be present; got:\n{on_disk}"
    );
    assert!(
        comment_count >= 1,
        "at least one comment must survive; got {comment_count}"
    );
}

// Extra: when the config file does not exist, `set` materializes it from
// defaults rather than failing.
#[test]
fn set_materializes_default_config_when_missing() {
    let tmp = tempdir().expect("tempdir");
    let layout = layout_at(tmp.path());
    layout.ensure_state_dir().expect("ensure_state_dir");
    assert!(
        !layout.config_file().exists(),
        "precondition: no config file"
    );

    execute_config_set_in(&layout, "coverage.services.enabled=true")
        .expect("set on missing file should materialize default");

    let config = load_config(&layout).expect("reload config");
    assert!(config.coverage.services.enabled, "value must persist");
}

// Extra: descending into a non-table value is rejected, not clobbered.
#[test]
fn set_rejects_descending_into_non_table() {
    let tmp = tempdir().expect("tempdir");
    let layout = layout_at(tmp.path());
    layout.ensure_state_dir().expect("ensure_state_dir");
    // `enabled` is a bool; trying to treat it as a table must fail.
    fs::write(layout.config_file(), "[coverage]\nenabled = false\n").expect("write config");

    let result = execute_config_set_in(&layout, "coverage.enabled.nested=true");
    assert!(result.is_err(), "descending into a non-table must error");
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("not a table") || msg.contains("enabled"),
        "error should explain the non-table collision: {msg}"
    );
}

// T7 (Finding 2): mutating an existing value key MUST preserve that key's own
// inline comment. Previously `parent.insert(leaf_key, ...)` replaced the whole
// `toml_edit::Item`, dropping the `Decor` (prefix/suffix incl. inline comments)
// attached to the existing value. The fix mutates the `Value` in place so the
// inline comment survives the typed-value change.
#[test]
fn set_preserves_inline_comment_on_edited_key() {
    let tmp = tempdir().expect("tempdir");
    let layout = layout_at(tmp.path());
    layout.ensure_state_dir().expect("ensure_state_dir");
    // The leaf key carries its own inline comment on the same line.
    fs::write(
        layout.config_file(),
        "[coverage]\nenabled = false # keep this\n",
    )
    .expect("write config");

    execute_config_set_in(&layout, "coverage.enabled=true").expect("set should succeed");

    let on_disk = fs::read_to_string(layout.config_file()).expect("read config");
    assert!(
        on_disk.contains("enabled = true"),
        "value must flip to true; got:\n{on_disk}"
    );
    assert!(
        on_disk.contains("# keep this"),
        "the edited key's own inline comment must be preserved; got:\n{on_disk}"
    );

    // Reload to confirm the typed value round-trips.
    let config = load_config(&layout).expect("reload config");
    assert!(config.coverage.enabled, "reload must see enabled=true");
}

// T7 follow-up: editing a value key must preserve its inline comment even when
// the replacement is a different TOML type (bool -> int). Guards against any
// decor being dropped due to a type-switch in the replacement path.
#[test]
fn set_preserves_inline_comment_across_value_type_change() {
    let tmp = tempdir().expect("tempdir");
    let layout = layout_at(tmp.path());
    layout.ensure_state_dir().expect("ensure_state_dir");
    fs::write(
        layout.config_file(),
        "[hotspots]\nlimit = 10 # cap for unit tests\n",
    )
    .expect("write config");

    execute_config_set_in(&layout, "hotspots.limit=42").expect("set should succeed");

    let on_disk = fs::read_to_string(layout.config_file()).expect("read config");
    assert!(
        on_disk.contains("limit = 42"),
        "value must change to 42; got:\n{on_disk}"
    );
    assert!(
        on_disk.contains("# cap for unit tests"),
        "inline comment must survive the type-preserving edit; got:\n{on_disk}"
    );
}

// ---------------------------------------------------------------------------
// TA22: Provider priority array-of-tables UX
// ---------------------------------------------------------------------------

fn default_config_without_priority(layout: &Layout) {
    layout.ensure_state_dir().expect("ensure_state_dir");
    let default = ledgerful::config::defaults::default_config_contents().expect("default config");
    fs::write(layout.config_file(), default).expect("write default config");
}

// R1: array-of-tables index syntax creates a `[[ask.providers.priority]]` entry.
#[test]
fn set_priority_index_creates_provider_entry() {
    let tmp = tempdir().expect("tempdir");
    let layout = layout_at(tmp.path());
    default_config_without_priority(&layout);

    execute_config_set_in(&layout, "ask.providers.priority[0].backend=ollama_cloud")
        .expect("set priority entry should succeed");

    let config = load_config(&layout).expect("reload config");
    assert_eq!(config.ask.providers.priority.len(), 1);
    assert_eq!(
        config.ask.providers.priority[0].backend,
        ledgerful::config::model::Provider::OllamaCloud
    );

    let on_disk = fs::read_to_string(layout.config_file()).expect("read config");
    assert!(
        on_disk.contains("[[ask.providers.priority]]"),
        "on-disk config must contain array-of-tables header; got:\n{on_disk}"
    );
    assert!(
        on_disk.contains("backend = \"ollama_cloud\""),
        "on-disk entry must serialize backend; got:\n{on_disk}"
    );
}

// R1: appending at exactly len() works.
#[test]
fn set_priority_index_append_at_len() {
    let tmp = tempdir().expect("tempdir");
    let layout = layout_at(tmp.path());
    default_config_without_priority(&layout);

    execute_config_set_in(&layout, "ask.providers.priority[0].backend=ollama_cloud")
        .expect("set first");
    execute_config_set_in(&layout, "ask.providers.priority[1].backend=gemini")
        .expect("append at len");

    let config = load_config(&layout).expect("reload config");
    assert_eq!(config.ask.providers.priority.len(), 2);
    assert_eq!(
        config.ask.providers.priority[0].backend,
        ledgerful::config::model::Provider::OllamaCloud
    );
    assert_eq!(
        config.ask.providers.priority[1].backend,
        ledgerful::config::model::Provider::Gemini
    );
}

// R1: out-of-bounds index > len() returns a helpful error.
#[test]
fn set_priority_index_out_of_bounds_errors() {
    let tmp = tempdir().expect("tempdir");
    let layout = layout_at(tmp.path());
    default_config_without_priority(&layout);

    let result = execute_config_set_in(&layout, "ask.providers.priority[9].backend=local");
    assert!(result.is_err(), "out-of-bounds index must error");
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("out of bounds") && msg.contains("length 0") && msg.contains("index 0"),
        "error should mention bounds and append index 0: {msg}"
    );
}

// R2: shortcut syntax replaces the entire priority list.
#[test]
fn set_priority_shortcut_replaces_list() {
    let tmp = tempdir().expect("tempdir");
    let layout = layout_at(tmp.path());
    default_config_without_priority(&layout);

    execute_config_set_in(&layout, "ask.providers.priority=ollama_cloud,gemini,local")
        .expect("shortcut set should succeed");

    let config = load_config(&layout).expect("reload config");
    assert_eq!(config.ask.providers.priority.len(), 3);
    let backends: Vec<_> = config
        .ask
        .providers
        .priority
        .iter()
        .map(|e| e.backend)
        .collect();
    assert_eq!(
        backends,
        vec![
            ledgerful::config::model::Provider::OllamaCloud,
            ledgerful::config::model::Provider::Gemini,
            ledgerful::config::model::Provider::Local,
        ]
    );
}

// R3: invalid backend name returns an error instead of panicking.
#[test]
fn set_priority_shortcut_invalid_backend_errors() {
    let tmp = tempdir().expect("tempdir");
    let layout = layout_at(tmp.path());
    default_config_without_priority(&layout);

    let result = execute_config_set_in(&layout, "ask.providers.priority=ollama_cloud,unknown");
    assert!(result.is_err(), "invalid backend must error");
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("unknown") || msg.contains("Invalid provider"),
        "error should mention invalid provider: {msg}"
    );
}

// R4: empty RHS clears the priority list.
#[test]
fn set_priority_empty_rhs_clears_list() {
    let tmp = tempdir().expect("tempdir");
    let layout = layout_at(tmp.path());
    default_config_without_priority(&layout);

    execute_config_set_in(&layout, "ask.providers.priority=ollama_cloud,gemini")
        .expect("populate list");
    execute_config_set_in(&layout, "ask.providers.priority=").expect("clear list");

    let config = load_config(&layout).expect("reload config");
    assert!(config.ask.providers.priority.is_empty());

    let on_disk = fs::read_to_string(layout.config_file()).expect("read config");
    assert!(
        !on_disk.contains("[[ask.providers.priority]]"),
        "array-of-tables header should be removed; got:\n{on_disk}"
    );
}

// R5: `config unset` removes a single array-of-tables entry.
#[test]
fn unset_priority_entry_removes_index_and_shifts() {
    let tmp = tempdir().expect("tempdir");
    let layout = layout_at(tmp.path());
    default_config_without_priority(&layout);

    execute_config_set_in(&layout, "ask.providers.priority=ollama_cloud,gemini,local")
        .expect("populate list");

    ledgerful::commands::config::execute_config_unset_in(&layout, "ask.providers.priority[1]")
        .expect("unset index 1");

    let config = load_config(&layout).expect("reload config");
    assert_eq!(config.ask.providers.priority.len(), 2);
    assert_eq!(
        config.ask.providers.priority[0].backend,
        ledgerful::config::model::Provider::OllamaCloud
    );
    assert_eq!(
        config.ask.providers.priority[1].backend,
        ledgerful::config::model::Provider::Local
    );
}

// R5: `config unset` with out-of-bounds index errors.
#[test]
fn unset_priority_entry_out_of_bounds_errors() {
    let tmp = tempdir().expect("tempdir");
    let layout = layout_at(tmp.path());
    default_config_without_priority(&layout);

    // Pre-populate an empty priority array so the path exists and the unset
    // command can report an out-of-bounds index consistently with set.
    execute_config_set_in(&layout, "ask.providers.priority[0].backend=ollama_cloud")
        .expect("seed priority array");

    let result =
        ledgerful::commands::config::execute_config_unset_in(&layout, "ask.providers.priority[9]");
    assert!(result.is_err(), "unset on empty list must error");
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("out of bounds"),
        "error should mention out of bounds: {msg}"
    );
}

// The `config set` command does not accept an empty value for ordinary keys.
#[test]
fn set_rejects_empty_value_for_non_priority_keys() {
    let tmp = tempdir().expect("tempdir");
    let layout = layout_at(tmp.path());
    default_config_without_priority(&layout);

    let result = execute_config_set_in(&layout, "coverage.enabled=");
    assert!(result.is_err(), "empty value for ordinary key must error");
}

// TA22 R5: unsetting the last entry removes the `[[ask.providers.priority]]`
// header entirely instead of leaving a stale empty array-of-tables.
#[test]
fn unset_priority_last_entry_removes_empty_array_header() {
    let tmp = tempdir().expect("tempdir");
    let layout = layout_at(tmp.path());
    default_config_without_priority(&layout);

    execute_config_set_in(&layout, "ask.providers.priority=ollama_cloud")
        .expect("seed single entry");
    let on_disk = fs::read_to_string(layout.config_file()).expect("read config");
    assert!(
        on_disk.contains("[[ask.providers.priority]]"),
        "precondition: header should exist; got:\n{on_disk}"
    );

    ledgerful::commands::config::execute_config_unset_in(&layout, "ask.providers.priority[0]")
        .expect("unset last entry");

    let on_disk = fs::read_to_string(layout.config_file()).expect("read config after unset");
    assert!(
        !on_disk.contains("[[ask.providers.priority]]"),
        "empty array-of-tables header should be removed; got:\n{on_disk}"
    );
}

// TA22: missing index in key path (e.g. `ask.providers.priority.backend=…`)
// must error instead of silently clobbering the list via the shortcut path.
#[test]
fn set_priority_missing_index_errors_instead_of_clobbering() {
    let tmp = tempdir().expect("tempdir");
    let layout = layout_at(tmp.path());
    default_config_without_priority(&layout);

    // Seed an existing list so we can prove it is NOT replaced.
    execute_config_set_in(&layout, "ask.providers.priority=ollama_cloud,gemini")
        .expect("seed list");

    let result = execute_config_set_in(&layout, "ask.providers.priority.backend=local");
    assert!(
        result.is_err(),
        "missing index must error, not silently clobber; got {:?}",
        result
    );
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("missing index"),
        "error should explain the missing index: {msg}"
    );

    // Confirm the existing list is untouched.
    let config = load_config(&layout).expect("reload config");
    assert_eq!(config.ask.providers.priority.len(), 2);
}

// TA22: `[N]` without a trailing `.field` must error instead of clobbering.
#[test]
fn set_priority_index_without_field_errors() {
    let tmp = tempdir().expect("tempdir");
    let layout = layout_at(tmp.path());
    default_config_without_priority(&layout);

    execute_config_set_in(&layout, "ask.providers.priority=ollama_cloud,gemini")
        .expect("seed list");

    let result = execute_config_set_in(&layout, "ask.providers.priority[0]=local");
    assert!(result.is_err(), "index without field must error");
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("missing field"),
        "error should explain the missing field: {msg}"
    );

    let config = load_config(&layout).expect("reload config");
    assert_eq!(config.ask.providers.priority.len(), 2);
}

// TA22: non-numeric index must error instead of clobbering.
#[test]
fn set_priority_non_numeric_index_errors() {
    let tmp = tempdir().expect("tempdir");
    let layout = layout_at(tmp.path());
    default_config_without_priority(&layout);

    execute_config_set_in(&layout, "ask.providers.priority=ollama_cloud").expect("seed list");

    let result = execute_config_set_in(&layout, "ask.providers.priority[abc].backend=local");
    assert!(result.is_err(), "non-numeric index must error");
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("invalid index"),
        "error should explain the invalid index: {msg}"
    );

    let config = load_config(&layout).expect("reload config");
    assert_eq!(config.ask.providers.priority.len(), 1);
}
