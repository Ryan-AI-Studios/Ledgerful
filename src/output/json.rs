use miette::{IntoDiagnostic, Result};
use serde::Serialize;
use std::fs;
use std::path::Path;

/// Pretty-print a serializable value as JSON (no trailing newline).
pub fn format_json<T: Serialize>(value: &T) -> Result<String> {
    serde_json::to_string_pretty(value).into_diagnostic()
}

/// Pretty-serialize `value` and write it to stdout with a trailing newline.
pub fn emit<T: Serialize>(value: &T) -> Result<()> {
    let json = format_json(value)?;
    println!("{json}");
    Ok(())
}

/// Pretty-serialize `value`. `None` infers stdout (`emit`, trailing newline).
/// `Some(path)` uses `fs::write` with no extra newline.
pub fn emit_to<T: Serialize>(value: &T, out: Option<&Path>) -> Result<()> {
    match out {
        None => emit(value),
        Some(path) => {
            let json = format_json(value)?;
            fs::write(path, json).into_diagnostic()?;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn format_json_has_no_trailing_newline() {
        let s = format_json(&json!({"a": 1})).expect("serialize");
        assert!(
            !s.ends_with('\n'),
            "pretty JSON body must not end with newline"
        );
        assert!(
            s.contains('\n'),
            "pretty JSON should contain internal newlines"
        );
    }

    #[test]
    fn format_json_does_not_inject_schema_version() {
        let s = format_json(&json!({"ok": true})).expect("serialize");
        let v: serde_json::Value = serde_json::from_str(&s).expect("parse");
        assert!(
            v.get("schemaVersion").is_none(),
            "helper must serialize the value as-is: {v}"
        );
        assert_eq!(v, json!({"ok": true}));
    }

    #[test]
    fn emit_to_file_has_no_extra_newline() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("out.json");
        let value = json!({"a": 1});
        emit_to(&value, Some(path.as_path())).expect("write");
        let body = fs::read_to_string(&path).expect("read");
        let expected = format_json(&value).expect("serialize");
        assert_eq!(
            body, expected,
            "--out body must be format_json with no extra newline"
        );
        assert!(
            !body.ends_with('\n'),
            "--out must not append a trailing newline"
        );
    }

    #[test]
    fn emit_stdout_adds_trailing_newline_vs_emit_to_file() {
        let value = json!({"k": true});
        let formatted = format_json(&value).expect("serialize");
        assert!(!formatted.ends_with('\n'));

        // stdout: println!(format_json) → trailing newline. Invoke emit so this
        // helper cannot false-pass if emit panics or is removed.
        emit(&value).expect("emit stdout");
        emit_to(&value, None).expect("emit_to None is stdout");
        let stdout_shape = format!("{formatted}\n");
        assert!(stdout_shape.ends_with('\n'));
        assert_eq!(&stdout_shape[..stdout_shape.len() - 1], formatted.as_str());

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("out.json");
        emit_to(&value, Some(path.as_path())).expect("write");
        let file_body = fs::read_to_string(&path).expect("read");
        assert_eq!(file_body, formatted);
        assert_ne!(
            file_body, stdout_shape,
            "stdout has a trailing newline; --out must not"
        );
    }
}
