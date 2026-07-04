use crate::config::model::Provider;
use crate::state::layout::Layout;
use miette::Result;

/// Set a configuration value in `.ledgerful/config.toml` by dotted key.
///
/// `key_value` is a single `dotted.path = rhs` string (e.g.
/// `coverage.services.enabled=true`). The path is split on `.` and the
/// right-hand side is split on the FIRST `=`. Intermediate tables are created
/// if they do not exist; existing comments and formatting are preserved via
/// `toml_edit`.
///
/// RHS value inference: the right-hand side is first parsed as TOML (so
/// `true`/`42`/`3.14`/"quoted"/`[1,2]` become bool/int/float/string/array
/// respectively). If parsing fails and the RHS is a non-empty bareword with
/// no TOML-significant characters (i.e. it failed only because it is not a
/// valid TOML literal), it is stored as a string so users can write
/// `services.alias=mylabel` without quoting. Any other parse failure is
/// surfaced as a diagnostic.
pub fn execute_config_set(key_value: &str) -> Result<()> {
    let current_dir = std::env::current_dir()
        .map_err(|e| miette::miette!("Failed to get current directory: {e}"))?;
    let layout = Layout::new(current_dir.to_string_lossy().as_ref());
    execute_config_set_in(&layout, key_value)
}

/// Testable form of [`execute_config_set`] that operates against an explicit
/// [`Layout`] rather than the process current directory.
pub fn execute_config_set_in(layout: &Layout, key_value: &str) -> Result<()> {
    // Split on the FIRST `=`. Everything to the left is the dotted key path,
    // everything to the right is the TOML value literal.
    let eq_pos = key_value
        .find('=')
        .ok_or_else(|| miette::miette!("missing `=` in `key=value` argument: `{key_value}`"))?;
    let key_path_str = key_value[..eq_pos].trim();
    let rhs = key_value[eq_pos + 1..].trim();

    if key_path_str.is_empty() {
        return Err(miette::miette!("empty key path in `key=value` argument"));
    }

    let path = split_key_path(key_path_str)?;

    // TA22: special-case the `ask.providers.priority` array-of-tables path.
    if path.len() >= 3
        && path[0] == "ask"
        && path[1] == "providers"
        && (path[2] == "priority" || path[2].starts_with("priority["))
    {
        return set_provider_priority(layout, key_path_str, rhs);
    }

    if rhs.is_empty() {
        return Err(miette::miette!(
            "empty value in `key=value` argument: `{key_value}`"
        ));
    }

    let leaf_key = path[path.len() - 1].as_str();
    let parent_path: Vec<&str> = path[..path.len() - 1].iter().map(|s| s.as_str()).collect();

    let (mut doc, config_path) = load_config_doc(layout)?;

    let root = doc.as_table_mut();
    let parent = navigate_or_create(root, &parent_path)?;
    let new_item = parse_rhs(rhs)?;

    // Preserve inline comments on an existing value key. If the leaf key
    // already exists as a `toml_edit::Item::Value`, mutate its inner value in
    // place so the existing `Decor` (prefix/suffix, including inline comments
    // such as `enabled = false # keep this`) survives the edit. Only fall back
    // to `parent.insert` (which swaps the whole `Item` and drops decor) when
    // the key is missing or the existing item is not a Value (e.g. a Table),
    // matching the prior replace-whole-item behavior.
    let existing_is_value = matches!(parent.get(leaf_key), Some(toml_edit::Item::Value(_)));
    if existing_is_value
        && let Some(toml_edit::Item::Value(existing)) = parent.get_mut(leaf_key)
        && let Some(new_value) = new_item.as_value()
    {
        // Carry the existing value's decor (inline comment, spacing) onto the
        // replacement so the typed value changes while the surrounding
        // formatting is preserved.
        let preserved_decor = existing.decor().clone();
        let mut replacement = new_value.clone();
        *replacement.decor_mut() = preserved_decor;
        *existing = replacement;
    } else if !existing_is_value {
        // Missing key, or existing item is a Table/non-Value: insert/replace
        // the whole item (prior behavior).
        parent.insert(leaf_key, new_item);
    } else if new_item.as_value().is_none() {
        // Defensive fallback: existing was a Value but the parsed RHS is not
        // (currently unreachable — parse_rhs always returns an Item::Value).
        // Replace the whole item so the edit is not silently dropped. The
        // `get_mut` borrow from the `if let` chain has ended by this branch.
        parent.insert(leaf_key, new_item);
    }

    write_config_doc(&config_path, &doc)?;

    println!("Set {key_path_str} = {rhs} in {}", config_path);
    Ok(())
}

/// Entry point for the `ledgerful config unset` subcommand.
pub fn execute_config_unset(key: &str) -> Result<()> {
    let current_dir = std::env::current_dir()
        .map_err(|e| miette::miette!("Failed to get current directory: {e}"))?;
    let layout = Layout::new(current_dir.to_string_lossy().as_ref());
    execute_config_unset_in(&layout, key)
}

/// Testable form of [`execute_config_unset`] that operates against an explicit
/// [`Layout`].
pub fn execute_config_unset_in(layout: &Layout, key: &str) -> Result<()> {
    let key = key.trim();
    if key.is_empty() {
        return Err(miette::miette!("empty key path"));
    }

    let path = split_key_path(key)?;

    // TA32: Support both array-of-tables index removal and standard key removal.
    let (mut doc, config_path) = load_config_doc(layout)?;
    let root = doc.as_table_mut();

    match path.as_slice() {
        [prefix @ .., last] if last.ends_with(']') => {
            let open = last
                .rfind('[')
                .ok_or_else(|| miette::miette!("invalid key path `{key}`: unmatched `]`"))?;
            let index_str = &last[open + 1..last.len() - 1];
            let index: usize = index_str.parse().map_err(|_| {
                miette::miette!("invalid key path `{key}`: index must be a non-negative integer")
            })?;
            let mut prefix = prefix.to_vec();
            prefix.push(last[..open].to_string());

            let item = navigate_to_item(root, &prefix)?;
            let array = item.as_array_of_tables_mut().ok_or_else(|| {
                miette::miette!("key `{}` is not an array of tables", prefix.join("."))
            })?;

            let len = array.len();
            if index >= len {
                return Err(miette::miette!(
                    "Index {index} is out of bounds for array of length {len}. Use index {len} to append."
                ));
            }

            array.remove(index);

            if array.is_empty()
                && let Some(parent_table) = root
                    .get_mut("ask")
                    .and_then(|ask| ask.as_table_mut())
                    .and_then(|ask| ask.get_mut("providers"))
                    .and_then(|p| p.as_table_mut())
            {
                parent_table.remove("priority");
            }
        }
        [prefix @ .., last] => {
            let item = navigate_to_item(root, prefix)?;
            let table = item
                .as_table_mut()
                .ok_or_else(|| miette::miette!("key `{}` is not a table", prefix.join(".")))?;
            table.remove(last);
        }
        [] => unreachable!(),
    };

    write_config_doc(&config_path, &doc)?;
    println!("Unset {} in {}", key, config_path);
    Ok(())
}

/// Load the editable TOML document and the config file path for a `Layout`.
pub(crate) fn load_config_doc(
    layout: &Layout,
) -> Result<(toml_edit::DocumentMut, camino::Utf8PathBuf)> {
    layout.ensure_state_dir()?;
    let config_path = layout.config_file();

    let content = if config_path.exists() {
        std::fs::read_to_string(&config_path)
            .map_err(|e| miette::miette!("Failed to read {}: {e}", config_path))?
    } else {
        crate::config::defaults::default_config_contents()
            .map_err(|e| miette::miette!("Failed to materialize default config: {e}"))?
    };

    let doc: toml_edit::DocumentMut = content
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| miette::miette!("TOML parse error in {}: {e}", config_path))?;

    Ok((doc, config_path))
}

/// Serialize a TOML document back to the config file.
pub(crate) fn write_config_doc(
    config_path: &camino::Utf8PathBuf,
    doc: &toml_edit::DocumentMut,
) -> Result<()> {
    let serialized = doc_to_string(doc);
    std::fs::write(config_path, serialized)
        .map_err(|e| miette::miette!("Failed to write {}: {e}", config_path))
}

/// Serialize a TOML document to a string.
pub(crate) fn doc_to_string(doc: &toml_edit::DocumentMut) -> String {
    doc.to_string()
}

/// Split a dotted key path into segments, detecting `[N]` index markers that
/// are part of a segment (e.g. `ask.providers.priority[0].backend` becomes
/// `["ask", "providers", "priority[0]", "backend"]`).
pub(crate) fn split_key_path(key_path_str: &str) -> Result<Vec<String>> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut chars = key_path_str.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '.' => {
                if current.is_empty() {
                    return Err(miette::miette!(
                        "invalid key path `{key_path_str}`: empty segment after splitting on `.`"
                    ));
                }
                segments.push(std::mem::take(&mut current));
            }
            '[' => {
                current.push(c);
                let mut depth = 1usize;
                for inner in chars.by_ref() {
                    current.push(inner);
                    match inner {
                        '[' => depth += 1,
                        ']' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                if depth != 0 {
                    return Err(miette::miette!(
                        "invalid key path `{key_path_str}`: unmatched `[`"
                    ));
                }
            }
            _ => current.push(c),
        }
    }
    if current.is_empty() {
        return Err(miette::miette!(
            "invalid key path `{key_path_str}`: empty segment after splitting on `.`"
        ));
    }
    segments.push(current);
    Ok(segments)
}

/// Walk a TOML table along `path`, creating implicit intermediate tables as
/// needed. Returns a mutable reference to the (possibly newly created) parent
/// table. If any segment along the path exists but is not a table, returns a
/// diagnostic instead of clobbering it.
pub(crate) fn navigate_or_create<'a>(
    table: &'a mut toml_edit::Table,
    path: &[&str],
) -> Result<&'a mut toml_edit::Table> {
    if path.is_empty() {
        return Ok(table);
    }
    let key = path[0];
    let entry = table.entry(key);
    let item = entry.or_insert_with(|| {
        let mut t = toml_edit::Table::new();
        t.set_implicit(true);
        toml_edit::Item::Table(t)
    });
    match item {
        toml_edit::Item::Table(sub) => navigate_or_create(sub, &path[1..]),
        other => {
            let _ = other;
            Err(miette::miette!(
                "key `{key}` exists but is not a table; cannot descend into `{key}`"
            ))
        }
    }
}

/// Navigate to an existing `Item` along `path`, descending into tables.
/// Returns a mutable reference to the final item.
pub(crate) fn navigate_to_item<'a>(
    table: &'a mut toml_edit::Table,
    path: &[String],
) -> Result<&'a mut toml_edit::Item> {
    if path.is_empty() {
        return Err(miette::miette!("empty key path"));
    }
    if path.len() == 1 {
        let key = path[0].as_str();
        return table
            .get_mut(key)
            .ok_or_else(|| miette::miette!("key `{key}` not found"));
    }
    let key = path[0].as_str();
    let item = table
        .get_mut(key)
        .ok_or_else(|| miette::miette!("key `{key}` not found"))?;
    let sub = item.as_table_mut().ok_or_else(|| {
        miette::miette!("key `{key}` exists but is not a table; cannot descend into `{key}`")
    })?;
    navigate_to_item(sub, &path[1..])
}

/// TA22: handle `ask.providers.priority` paths.
///
/// Supports three forms:
/// - `ask.providers.priority[0].backend=ollama_cloud` — set a field inside an
///   existing or newly appended array-of-tables entry.
/// - `ask.providers.priority=ollama_cloud,gemini,local` — replace the entire
///   list with provider stubs.
/// - `ask.providers.priority=` — clear the array.
pub(crate) fn set_provider_priority(layout: &Layout, key_path_str: &str, rhs: &str) -> Result<()> {
    let (mut doc, config_path) = load_config_doc(layout)?;
    let root = doc.as_table_mut();

    // `ask.providers` table must exist; create it if missing.
    let providers = navigate_or_create(root, &["ask", "providers"])?;

    // Determine whether the key uses an indexed path (e.g. `priority[0].backend`).
    let priority_key = "priority";
    let raw_item = providers
        .entry(priority_key)
        .or_insert_with(|| toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new()));
    let array = raw_item.as_array_of_tables_mut().ok_or_else(|| {
        miette::miette!("ask.providers.priority exists but is not an array of tables")
    })?;

    // Detect index syntax in the original key path string.
    let indexed_field = parse_priority_index_path(key_path_str);

    // Guard against common path-typing mistakes that would otherwise fall
    // through to the shortcut/clear path and silently clobber the list:
    //   1. `ask.providers.priority.backend=…` — bare `.field` without `[N]`.
    //   2. `ask.providers.priority[0]=…` — `[N]` without a trailing `.field`.
    //   3. `ask.providers.priority[abc].backend=…` — non-numeric index.
    // `parse_priority_index_path` returns None for all three, so detect them
    // explicitly via the raw key-path suffix and surface a clear error.
    if indexed_field.is_none()
        && let Some(rest) = key_path_str.strip_prefix("ask.providers.priority")
    {
        if rest.starts_with('.') && !rest[1..].is_empty() {
            return Err(miette::miette!(
                "missing index in `{key_path_str}`; use `ask.providers.priority[N].backend=...` to set a single entry, or `ask.providers.priority=backend1,backend2` to replace the whole list"
            ));
        }
        if rest.starts_with('[') {
            // `[N]` present but parse_priority_index_path rejected it:
            // either no trailing `.field`, a non-numeric index, or an empty
            // index. Distinguish a bare `[N]` (missing field) from a malformed
            // index for a clearer message.
            let close = rest.find(']');
            let after_close = close.map(|c| &rest[c + 1..]).unwrap_or("");
            if after_close.is_empty() || !after_close.starts_with('.') {
                return Err(miette::miette!(
                    "missing field in `{key_path_str}`; use `ask.providers.priority[N].field=...` (e.g. `ask.providers.priority[0].backend=ollama_cloud`), or `ask.providers.priority=backend1,backend2` to replace the whole list"
                ));
            }
            // There is a `.field` suffix but the index didn't parse, so the
            // index itself is malformed (non-numeric or empty).
            return Err(miette::miette!(
                "invalid index in `{key_path_str}`; the index `[N]` must be a non-negative integer"
            ));
        }
    }

    if let Some((index, field)) = indexed_field {
        let len = array.len();
        if index > len {
            return Err(miette::miette!(
                "Index {index} is out of bounds for array of length {len}. Use index {len} to append."
            ));
        }

        // Append a new table if index == len.
        if index == len {
            let mut entry_table = toml_edit::Table::new();
            entry_table.insert("backend", toml_edit::value("local".to_string()));
            array.push(entry_table);
        }

        let entry = array.get_mut(index).ok_or_else(|| {
            miette::miette!("internal error: failed to access priority entry {index}")
        })?;

        // Validate the field name so we only write supported keys.
        if !is_valid_provider_entry_field(&field) {
            return Err(miette::miette!("invalid provider entry field `{field}`"));
        }

        let value_item = parse_rhs(rhs)?;
        // For `backend`, validate the provider variant immediately.
        if field == "backend" {
            let value_str = value_item
                .as_str()
                .ok_or_else(|| miette::miette!("backend must be a string"))?;
            let _ = Provider::from_str_fail_fast(
                value_str,
                &format!("ask.providers.priority[{index}].backend"),
            )
            .map_err(|e| miette::miette!("{e}"))?;
        }

        entry.insert(&field, value_item);

        write_config_doc(&config_path, &doc)?;
        println!("Set {key_path_str} = {rhs} in {}", config_path);
        return validate_and_print_priority(layout);
    }

    // Non-indexed `ask.providers.priority` — full list replacement or clear.
    if rhs.is_empty() {
        array.clear();
        if array.is_empty() {
            providers.remove(priority_key);
        }
        write_config_doc(&config_path, &doc)?;
        println!("{}", build_priority_clear_confirmation());
        return Ok(());
    }

    // Comma-separated backend shortcut syntax.
    let mut new_entries = Vec::new();
    let mut backend_names: Vec<&str> = Vec::new();
    for backend_name in rhs.split(',') {
        let backend_name = backend_name.trim();
        if backend_name.is_empty() {
            continue;
        }
        let provider = Provider::from_str_fail_fast(backend_name, "ask.providers.priority")
            .map_err(|e| miette::miette!("{e}"))?;
        let default_entry = default_provider_entry(provider);
        new_entries.push(default_entry);
        backend_names.push(backend_name);
    }

    if new_entries.is_empty() {
        return Err(miette::miette!("at least one provider must be configured"));
    }

    array.clear();
    for entry in new_entries {
        array.push(entry);
    }

    // Build the confirmation message before serializing so the mutable borrow
    // of `array` can end before we immutably borrow `doc`.
    let confirmation = build_priority_set_confirmation(&backend_names);

    write_config_doc(&config_path, &doc)?;

    println!("{confirmation}");

    validate_and_print_priority(layout)
}

/// Parse `ask.providers.priority[N].field` from a raw key path string.
/// Returns `Some((index, field))` when an index is present, or `None` for the
/// bare `ask.providers.priority` form.
pub(crate) fn parse_priority_index_path(key_path_str: &str) -> Option<(usize, String)> {
    let prefix = "ask.providers.priority";
    let rest = key_path_str.strip_prefix(prefix)?;
    if rest.is_empty() {
        return None;
    }
    if !rest.starts_with('[') {
        return None;
    }
    let close = rest.find(']')?;
    let index: usize = rest[1..close].parse().ok()?;
    let after = &rest[close + 1..];
    if !after.starts_with('.') {
        return None;
    }
    let field = after[1..].to_string();
    if field.is_empty() {
        return None;
    }
    Some((index, field))
}

/// Fields allowed inside a `[[ask.providers.priority]]` entry.
pub(crate) fn is_valid_provider_entry_field(field: &str) -> bool {
    matches!(
        field,
        "backend" | "model" | "timeout_secs" | "api_key_env" | "base_url"
    )
}

/// Build a default `ProviderEntry` table for the shortcut syntax.
pub(crate) fn default_provider_entry(provider: Provider) -> toml_edit::Table {
    let mut table = toml_edit::Table::new();
    let backend_name = match provider {
        Provider::OllamaCloud => "ollama_cloud",
        Provider::Gemini => "gemini",
        Provider::Local => "local",
        Provider::OpenRouter => "openrouter",
    };
    table.insert("backend", toml_edit::value(backend_name));
    // TA22 R2: create stubs with default model/timeout values.
    match provider {
        Provider::OllamaCloud => {
            table.insert("model", toml_edit::value("minimax-m3:cloud".to_string()));
            table.insert("timeout_secs", toml_edit::value(30i64));
        }
        Provider::Gemini => {
            table.insert(
                "model",
                toml_edit::value("gemini-3.1-flash-lite".to_string()),
            );
            table.insert("timeout_secs", toml_edit::value(30i64));
        }
        Provider::Local => {
            table.insert("model", toml_edit::value("local".to_string()));
            table.insert("timeout_secs", toml_edit::value(30i64));
        }
        Provider::OpenRouter => {
            table.insert("model", toml_edit::value("openrouter".to_string()));
            table.insert("timeout_secs", toml_edit::value(30i64));
        }
    }
    table
}

/// Human-readable display name for a backend string.
pub(crate) fn provider_display_name(backend: &str) -> String {
    match Provider::from_str_fail_fast(backend, "display") {
        Ok(p) => p.display_name().to_string(),
        Err(_) => {
            let mut s = backend.to_string();
            if let Some(c) = s.get_mut(0..1) {
                c.make_ascii_uppercase();
            }
            s
        }
    }
}

/// Build the human-readable confirmation message for the shortcut syntax
/// (R3). Pure function so it can be unit-tested without capturing stdout.
///
/// Example: `["ollama_cloud", "gemini", "local"]` →
/// `"Provider priority set: OllamaCloud → Gemini → Local"`.
pub(crate) fn build_priority_set_confirmation(backends: &[&str]) -> String {
    let names: Vec<String> = backends.iter().map(|b| provider_display_name(b)).collect();
    format!("Provider priority set: {}", names.join(" → "))
}

/// Build the clear-list confirmation message (R4).
pub(crate) fn build_priority_clear_confirmation() -> &'static str {
    "Provider priority list cleared. Legacy backend selection will be used."
}

/// Reload the config and validate that at least one provider is configured and
/// that every backend is a known variant.
pub(crate) fn validate_and_print_priority(layout: &Layout) -> Result<()> {
    let config = crate::config::load_config(layout)
        .map_err(|e| miette::miette!("Configuration is invalid after update: {e}"))?;
    let priority = &config.ask.providers.priority;
    if priority.is_empty() {
        return Err(miette::miette!("at least one provider must be configured"));
    }
    Ok(())
}

/// Parse a right-hand-side value literal into an editable TOML item.
///
/// First tries to parse the RHS as a TOML value (so `true`, `42`, `3.14`,
/// `"quoted"`, `[1,2]` are typed correctly). If that fails, treats the RHS as
/// a bare string literal (so `services.alias=mylabel` works without
/// quoting). If the RHS is structurally broken (e.g. `[1,` — unclosed array),
/// the original TOML parse error is surfaced rather than silently storing a
/// malformed string.
pub(crate) fn parse_rhs(s: &str) -> Result<toml_edit::Item> {
    // Parse `__x__ = <rhs>` as a toml_edit document so the RHS inherits the
    // correct typed form (bool/int/float/string/array) and is returned as a
    // `toml_edit::Item` directly — no cross-crate conversion needed.
    let candidate = format!("__x__ = {s}");
    match candidate.parse::<toml_edit::DocumentMut>() {
        Ok(doc) => {
            let root = doc.as_table();
            let item = root
                .get("__x__")
                .ok_or_else(|| miette::miette!("internal error: parsed RHS is missing"))?;
            Ok(item.clone())
        }
        Err(parse_err) => {
            // Fall back to treating the RHS as a bare string ONLY when it is
            // a clean bareword (non-empty, no TOML-significant punctuation that
            // would indicate the user intended a typed value but mistyped it).
            // An unclosed array like `[1,` is NOT a clean bareword, so its
            // original parse error is surfaced instead of being silently
            // stored as the literal string "[1,".
            if is_clean_bareword(s) {
                Ok(toml_edit::value(s.to_string()))
            } else {
                Err(miette::miette!("invalid value `{s}`: {parse_err}"))
            }
        }
    }
}

/// A "clean bareword" is a non-empty token that contains none of the
/// characters that would make it ambiguous whether the user intended a typed
/// TOML value (`[`, `]`, `{`, `}`, `=`, `,`, `"`, `'`, `#`). When the RHS
/// fails TOML parsing but is a clean bareword, we store it as a string so
/// `services.alias=mylabel` works without quoting.
pub(crate) fn is_clean_bareword(s: &str) -> bool {
    !s.is_empty()
        && !s.chars().any(|c| {
            matches!(
                c,
                '[' | ']' | '{' | '}' | '=' | ',' | '"' | '\'' | '#' | '\n' | '\r'
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- parse_rhs / is_clean_bareword (Track DX3) -----------------------

    #[test]
    fn parse_rhs_bool() {
        let item = parse_rhs("true").expect("bool");
        assert_eq!(item.as_value().and_then(|v| v.as_bool()), Some(true));
    }

    #[test]
    fn parse_rhs_int() {
        let item = parse_rhs("42").expect("int");
        assert_eq!(item.as_value().and_then(|v| v.as_integer()), Some(42));
    }

    #[test]
    fn parse_rhs_float() {
        let item = parse_rhs("2.5").expect("float");
        let f = item.as_value().and_then(|v| v.as_float()).expect("float");
        assert!((f - 2.5).abs() < f64::EPSILON);
    }

    #[test]
    fn parse_rhs_quoted_string() {
        let item = parse_rhs("\"bar-baz\"").expect("string");
        assert_eq!(item.as_value().and_then(|v| v.as_str()), Some("bar-baz"));
    }

    #[test]
    fn parse_rhs_array() {
        let item = parse_rhs("[1, 2]").expect("array");
        let arr = item.as_array().expect("array");
        assert_eq!(arr.len(), 2);
        assert_eq!(arr.get(0).and_then(|v| v.as_integer()), Some(1));
        assert_eq!(arr.get(1).and_then(|v| v.as_integer()), Some(2));
    }

    #[test]
    fn parse_rhs_bareword_falls_back_to_string() {
        let item = parse_rhs("mylabel").expect("bareword fallback");
        assert_eq!(item.as_value().and_then(|v| v.as_str()), Some("mylabel"));
    }

    #[test]
    fn parse_rhs_unclosed_array_errors() {
        assert!(
            parse_rhs("[1,").is_err(),
            "unclosed array must not be stored"
        );
    }

    #[test]
    fn is_clean_bareword_classification() {
        assert!(is_clean_bareword("mylabel"));
        assert!(is_clean_bareword("foo-bar_2"));
        assert!(!is_clean_bareword(""));
        assert!(!is_clean_bareword("[1,"));
        assert!(!is_clean_bareword("\"quoted\""));
        assert!(!is_clean_bareword("a # b"));
    }

    // TA22 R3: shortcut confirmation message matches spec exactly.
    #[test]
    fn build_priority_set_confirmation_matches_spec_format() {
        let msg = build_priority_set_confirmation(&["ollama_cloud", "gemini", "local"]);
        assert_eq!(
            msg, "Provider priority set: OllamaCloud → Gemini → Local",
            "R3 confirmation message must match spec exactly"
        );
    }

    #[test]
    fn build_priority_set_confirmation_single_backend() {
        let msg = build_priority_set_confirmation(&["local"]);
        assert_eq!(msg, "Provider priority set: Local");
    }

    // TA22 R4: clear confirmation message matches spec exactly.
    #[test]
    fn build_priority_clear_confirmation_matches_spec() {
        assert_eq!(
            build_priority_clear_confirmation(),
            "Provider priority list cleared. Legacy backend selection will be used."
        );
    }
}
