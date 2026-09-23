//! File-sourced plaintext provider-key hygiene for `doctor` (0422).
//!
//! Inspects the raw `config.toml` table so serde aliases cannot collapse
//! `ollama_key` into `ollama_cloud_api_key`. Env / `.env` fills are ignored.

use crate::commands::doctor::{DoctorCategory, DoctorFinding};
use crate::state::layout::Layout;
use std::fs;

/// Machine id for a file-assigned Gemini / Ollama key (warn / other).
pub const CONFIG_PLAINTEXT_SECRET_CODE: &str = "config-plaintext-secret";

const REMEDIATION: &str = "Delete the assigned gemini.api_key / local_model.ollama_key / local_model.ollama_cloud_api_key lines from .ledgerful/config.toml\nSet GEMINI_API_KEY and/or OLLAMA_CLOUD_API_KEY in the process environment (or .env)\nRotate those keys at the Gemini and Ollama provider consoles\nledgerful doctor --json";

/// Doctor-facing finding when `config.toml` assigns a non-empty provider key.
///
/// Missing file, empty / whitespace-only / comment-only values, env-only
/// fills, and TOML parse failure all return an empty vec (`legacy-config`
/// remains the parse-failure reporter).
pub fn doctor_plaintext_secret_findings(layout: &Layout) -> Vec<DoctorFinding> {
    let path = layout.config_file();
    if !path.exists() {
        return Vec::new();
    }

    let Ok(content) = fs::read_to_string(path.as_std_path()) else {
        return Vec::new();
    };
    let Ok(root) = content.parse::<toml::Table>() else {
        return Vec::new();
    };

    let mut paths = Vec::new();
    push_assigned_path(&root, "gemini", "api_key", &mut paths);
    push_assigned_path(&root, "local_model", "ollama_key", &mut paths);
    push_assigned_path(&root, "local_model", "ollama_cloud_api_key", &mut paths);
    paths.sort();
    paths.dedup();
    if paths.is_empty() {
        return Vec::new();
    }

    let named = paths.join(", ");
    vec![
        DoctorFinding::warn(
            CONFIG_PLAINTEXT_SECRET_CODE,
            DoctorCategory::Other,
            format!(
                "Plaintext provider key(s) in config.toml: {named}. Move them to GEMINI_API_KEY / OLLAMA_CLOUD_API_KEY (or .env) and rotate."
            ),
        )
        .with_remediation(REMEDIATION),
    ]
}

fn push_assigned_path(root: &toml::Table, table: &str, key: &str, paths: &mut Vec<String>) {
    let Some(value) = root
        .get(table)
        .and_then(toml::Value::as_table)
        .and_then(|inner| inner.get(key))
        .and_then(toml::Value::as_str)
    else {
        return;
    };
    if !value.trim().is_empty() {
        paths.push(format!("{table}.{key}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::doctor::plan_doctor_fix;
    use crate::commands::doctor::{DoctorCategory, DoctorSeverity};
    use camino::Utf8Path;
    use std::fs;
    use tempfile::tempdir;

    const SENTINEL: &str = "test-key-not-a-secret";

    mod env_guard {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/integration/common/env_guard.rs"
        ));
    }
    use env_guard::TempEnv;

    fn layout_with_config(contents: &str) -> (tempfile::TempDir, Layout) {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);
        layout.ensure_state_dir().unwrap();
        fs::write(layout.config_file(), contents).unwrap();
        (tmp, layout)
    }

    fn assert_one_secret_finding(layout: &Layout, expected_paths: &str) {
        let findings = doctor_plaintext_secret_findings(layout);
        assert_eq!(
            findings.len(),
            1,
            "expected one config-plaintext-secret finding, got {findings:?}"
        );
        let f = &findings[0];
        assert_eq!(f.code, CONFIG_PLAINTEXT_SECRET_CODE);
        assert_eq!(f.severity, DoctorSeverity::Warn);
        assert_eq!(f.category, DoctorCategory::Other);
        assert!(
            f.message.contains(expected_paths),
            "message must name {expected_paths}: {}",
            f.message
        );
        assert!(
            !f.message.contains(SENTINEL),
            "message leaked sentinel: {}",
            f.message
        );
        let rem = f.remediation.as_deref().expect("remediation required");
        assert!(
            rem.contains("ledgerful doctor --json"),
            "remediation must name doctor --json: {rem}"
        );
        assert!(
            !rem.contains(SENTINEL),
            "remediation leaked sentinel: {rem}"
        );
        assert!(
            !rem.contains("config set"),
            "remediation must not suggest config set: {rem}"
        );
    }

    #[test]
    fn doctor_plaintext_secret_emits_for_file_gemini_key() {
        let (_tmp, layout) = layout_with_config(&format!("[gemini]\napi_key = \"{SENTINEL}\"\n"));
        assert_one_secret_finding(&layout, "gemini.api_key");
    }

    #[test]
    fn doctor_plaintext_secret_emits_for_file_ollama_key() {
        let (_tmp, layout) =
            layout_with_config(&format!("[local_model]\nollama_key = \"{SENTINEL}\"\n"));
        assert_one_secret_finding(&layout, "local_model.ollama_key");
    }

    #[test]
    fn doctor_plaintext_secret_one_finding_both_keys_sorted() {
        let (_tmp, layout) = layout_with_config(&format!(
            "[gemini]\napi_key = \"{SENTINEL}\"\n\n[local_model]\nollama_key = \"{SENTINEL}\"\n"
        ));
        assert_one_secret_finding(&layout, "gemini.api_key, local_model.ollama_key");
        let findings = doctor_plaintext_secret_findings(&layout);
        assert_eq!(findings.len(), 1);
    }

    #[test]
    fn doctor_plaintext_secret_ignores_empty_and_comments() {
        let (_tmp, layout) = layout_with_config(
            "[gemini]\n# api_key = \"test-key-not-a-secret\"\napi_key = \"\"\n\n[local_model]\n# ollama_key = \"test-key-not-a-secret\"\n",
        );
        assert!(doctor_plaintext_secret_findings(&layout).is_empty());
    }

    #[test]
    fn doctor_plaintext_secret_ignores_whitespace_only() {
        let (_tmp, layout) = layout_with_config("[gemini]\napi_key = \"   \"\n");
        assert!(doctor_plaintext_secret_findings(&layout).is_empty());
    }

    #[test]
    fn doctor_plaintext_secret_ignores_missing_file() {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);
        layout.ensure_state_dir().unwrap();
        assert!(doctor_plaintext_secret_findings(&layout).is_empty());
    }

    #[test]
    fn doctor_plaintext_secret_ignores_malformed_toml() {
        let (_tmp, layout) = layout_with_config("not = [ valid toml");
        assert!(doctor_plaintext_secret_findings(&layout).is_empty());
    }

    #[test]
    #[serial_test::serial(env)]
    fn doctor_plaintext_secret_ignores_env_only_ollama_cloud() {
        let _env = TempEnv::set("OLLAMA_CLOUD_API_KEY", SENTINEL);
        let (_tmp, layout) = layout_with_config("[core]\nstrict = false\n");
        assert!(doctor_plaintext_secret_findings(&layout).is_empty());
    }

    #[test]
    #[serial_test::serial(env)]
    fn doctor_plaintext_secret_ignores_env_only_ollama_api() {
        let _env = TempEnv::set("OLLAMA_API_KEY", SENTINEL);
        let (_tmp, layout) = layout_with_config("[core]\nstrict = false\n");
        assert!(doctor_plaintext_secret_findings(&layout).is_empty());
    }

    #[test]
    #[serial_test::serial(env)]
    fn doctor_plaintext_secret_ignores_dotenv_only() {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);
        layout.ensure_state_dir().unwrap();
        fs::write(layout.config_file(), "[core]\nstrict = false\n").unwrap();
        fs::write(
            tmp.path().join(".env"),
            format!("OLLAMA_CLOUD_API_KEY={SENTINEL}\nOLLAMA_API_KEY={SENTINEL}\n"),
        )
        .unwrap();
        let _cwd = crate::tests::DirGuard::new(tmp.path());
        assert!(doctor_plaintext_secret_findings(&layout).is_empty());
    }

    #[test]
    #[serial_test::serial(env)]
    fn doctor_plaintext_secret_ignores_env_only_gemini() {
        let _env = TempEnv::set("GEMINI_API_KEY", SENTINEL);
        let (_tmp, layout) = layout_with_config("[core]\nstrict = false\n");
        assert!(doctor_plaintext_secret_findings(&layout).is_empty());
    }

    #[test]
    fn doctor_plaintext_secret_remediation_present_no_sentinel() {
        let (_tmp, layout) = layout_with_config(&format!("[gemini]\napi_key = \"{SENTINEL}\"\n"));
        let findings = doctor_plaintext_secret_findings(&layout);
        let rem = findings[0]
            .remediation
            .as_deref()
            .expect("remediation.is_some()");
        assert!(rem.contains("ledgerful doctor --json"));
        assert!(!rem.contains(SENTINEL));
    }

    #[test]
    fn doctor_plaintext_secret_fix_plan_empty() {
        let (_tmp, layout) = layout_with_config(&format!("[gemini]\napi_key = \"{SENTINEL}\"\n"));
        let findings = doctor_plaintext_secret_findings(&layout);
        let plan = plan_doctor_fix(&findings, None, None, true);
        assert!(
            plan.actions.is_empty(),
            "doctor --fix must not strip keys: {:?}",
            plan.actions
        );
    }
}
