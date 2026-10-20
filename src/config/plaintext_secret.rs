//! File-sourced plaintext provider-key hygiene for `doctor` (0422, 0554).
//!
//! Inspects the raw selected config table so serde aliases cannot collapse
//! `ollama_key` into `ollama_cloud_api_key`. Env / `.env` fills are ignored.
//!
//! Selection mirrors the runtime loader: an existing project config wins and
//! no template is inspected; otherwise the default template candidate chosen
//! by [`crate::config::defaults::select_template_candidate`] is inspected.
//! Inactive templates are never opened. Findings never carry credential
//! values, resolved paths, parser excerpts or OS error text.

use crate::config::defaults::{
    TemplateEnv, TemplateOrigin, TemplateSelection, select_template_candidate,
};
use crate::doctor::finding::{DoctorCategory, DoctorFinding};
use crate::state::layout::Layout;
use camino::Utf8Path;
use std::io;

/// Machine id for a file-assigned Gemini / Ollama key (warn / other).
pub const CONFIG_PLAINTEXT_SECRET_CODE: &str = "config-plaintext-secret";

/// Machine id when the selected source could not be fully inspected (warn / other).
pub const CONFIG_INSPECTION_INCOMPLETE_CODE: &str = "config-inspection-incomplete";

/// Fixed source label for the existing project config. Never a resolved path.
const PROJECT_SOURCE: &str = "project config in the Ledgerful state directory";
const OVERRIDE_SOURCE: &str = "template selected by LEDGERFUL_DEFAULT_CONFIG";
const USERPROFILE_SOURCE: &str = "user template USERPROFILE/.ledgerful/default-config.toml";
const HOME_SOURCE: &str = "user template HOME/.ledgerful/default-config.toml";

const REASON_METADATA: &str = "metadata";
const REASON_READ: &str = "read";
const REASON_ENCODING: &str = "encoding";
const REASON_PARSE: &str = "parse";
const REASON_INVALID_KEY_TYPE: &str = "invalid-key-type";

const INCOMPLETE_REMEDIATION: &str = "Inspect the selected source's accessibility/read, encoding, TOML and provider-key types. Doctor may have continued with default configuration after a config-load failure. Rerun ledgerful doctor --json.";

/// Container tables and the allowlisted key names inside them.
const ALLOWLIST: &[(&str, &str)] = &[
    ("gemini", "api_key"),
    ("local_model", "ollama_key"),
    ("local_model", "ollama_cloud_api_key"),
];

/// Kind of a candidate path as seen by the metadata seam.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CandidateKind {
    RegularFile,
    Directory,
    Other,
}

/// Doctor-facing findings for the selected config source.
///
/// Captures one process [`TemplateEnv`] snapshot and uses real filesystem
/// operations. The private seam below keeps the same logic testable.
pub fn doctor_plaintext_secret_findings(layout: &Layout) -> Vec<DoctorFinding> {
    doctor_plaintext_secret_findings_with_env(
        layout,
        &TemplateEnv::from_process(),
        real_metadata,
        real_read,
    )
}

/// Private seam: explicit environment snapshot plus injected metadata and
/// byte-read operations. Failures and invalid bytes are synthesized in tests
/// without touching the process environment.
pub(crate) fn doctor_plaintext_secret_findings_with_env<M, R>(
    layout: &Layout,
    env: &TemplateEnv,
    metadata_fn: M,
    read_fn: R,
) -> Vec<DoctorFinding>
where
    M: Fn(&Utf8Path) -> io::Result<CandidateKind>,
    R: Fn(&Utf8Path) -> io::Result<Vec<u8>>,
{
    let project_path = layout.config_file();
    let mut findings = Vec::new();

    // Project wins whenever it exists, even if malformed; templates are then
    // not inspected at all. Metadata failures other than NotFound are an
    // uncertainty warning, and the runtime-equivalent default is inspected.
    // An existing project suppresses template inspection entirely, even when
    // malformed. Uncertain project metadata warns and falls through to the
    // runtime-equivalent default candidate.
    match metadata_fn(&project_path) {
        Ok(kind) => {
            findings.extend(
                inspect_candidate(PROJECT_SOURCE, Ok(kind), &project_path, &read_fn)
                    .unwrap_or_default(),
            );
            sort_findings(&mut findings);
            return findings;
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(_) => findings.push(incomplete(PROJECT_SOURCE, REASON_METADATA)),
    }

    match select_template_candidate(env) {
        TemplateSelection::Absent => {}
        TemplateSelection::Candidate { origin, path } => {
            let source = origin_source(origin);
            match path {
                None => findings.push(incomplete(source, REASON_ENCODING)),
                Some(path) => {
                    let meta = metadata_fn(&path);
                    if let Some(found) = inspect_candidate(source, meta, &path, &read_fn) {
                        findings.extend(found);
                    }
                }
            }
        }
    }

    sort_findings(&mut findings);
    findings
}

fn sort_findings(findings: &mut [DoctorFinding]) {
    findings.sort_by(|a, b| a.code.cmp(&b.code).then(a.message.cmp(&b.message)));
}

fn origin_source(origin: TemplateOrigin) -> &'static str {
    match origin {
        TemplateOrigin::Override => OVERRIDE_SOURCE,
        TemplateOrigin::UserProfile => USERPROFILE_SOURCE,
        TemplateOrigin::Home => HOME_SOURCE,
    }
}

/// Inspect one candidate whose metadata result is already known.
///
/// Returns `None` when the candidate is absent (NotFound), which is ordinary
/// fallback and not a finding. Otherwise returns the findings for it.
fn inspect_candidate<R>(
    source: &str,
    meta: io::Result<CandidateKind>,
    path: &Utf8Path,
    read_fn: &R,
) -> Option<Vec<DoctorFinding>>
where
    R: Fn(&Utf8Path) -> io::Result<Vec<u8>>,
{
    let kind = match meta {
        Ok(kind) => kind,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return None,
        Err(_) => return Some(vec![incomplete(source, REASON_METADATA)]),
    };
    if kind != CandidateKind::RegularFile {
        return Some(vec![incomplete(source, REASON_METADATA)]);
    }

    let Ok(bytes) = read_fn(path) else {
        return Some(vec![incomplete(source, REASON_READ)]);
    };
    let Ok(content) = String::from_utf8(bytes) else {
        return Some(vec![incomplete(source, REASON_ENCODING)]);
    };
    let Ok(root) = content.parse::<toml::Table>() else {
        return Some(vec![incomplete(source, REASON_PARSE)]);
    };

    Some(collect_findings(source, &root))
}

fn collect_findings(source: &str, root: &toml::Table) -> Vec<DoctorFinding> {
    let (mut valid, mut invalid) = (Vec::new(), Vec::new());

    for (table, key) in ALLOWLIST {
        let Some(container) = root.get(*table) else {
            continue;
        };
        let Some(inner) = container.as_table() else {
            // Non-table container: report the container name once.
            invalid.push((*table).to_string());
            continue;
        };
        let Some(value) = inner.get(*key) else {
            continue;
        };
        let path = format!("{table}.{key}");
        match value.as_str() {
            Some(text) if text.trim().is_empty() => {}
            Some(_) => valid.push(path),
            None => invalid.push(path),
        }
    }

    valid.sort();
    valid.dedup();
    invalid.sort();
    invalid.dedup();

    let mut findings = Vec::new();
    if !valid.is_empty() {
        findings.push(secret_finding(source, &valid));
    }
    if !invalid.is_empty() {
        findings.push(invalid_type_finding(source, &invalid));
    }
    findings
}

fn secret_finding(source: &str, paths: &[String]) -> DoctorFinding {
    let named = paths.join(", ");
    DoctorFinding::warn(
        CONFIG_PLAINTEXT_SECRET_CODE,
        DoctorCategory::Other,
        format!(
            "Plaintext provider key(s) in {source}: {named}. Move them to GEMINI_API_KEY / OLLAMA_CLOUD_API_KEY (or .env) and rotate."
        ),
    )
    .with_remediation(secret_remediation(source, &named))
}

fn secret_remediation(source: &str, named: &str) -> String {
    [
        format!("Delete the assigned {named} lines from {source}"),
        "Set GEMINI_API_KEY and/or OLLAMA_CLOUD_API_KEY in the process environment (or .env)"
            .to_string(),
        "An existing non-empty file key is not superseded by setting env".to_string(),
        "Rotate those keys at the provider consoles".to_string(),
        "ledgerful doctor --json".to_string(),
    ]
    .join("\n")
}

fn invalid_type_finding(source: &str, paths: &[String]) -> DoctorFinding {
    let named = paths.join(", ");
    DoctorFinding::warn(
        CONFIG_INSPECTION_INCOMPLETE_CODE,
        DoctorCategory::Other,
        format!(
            "Plaintext provider-key inspection incomplete for {source}: {REASON_INVALID_KEY_TYPE}. Invalid provider key or container type(s): {named}."
        ),
    )
    .with_remediation(INCOMPLETE_REMEDIATION)
}

fn incomplete(source: &str, reason: &str) -> DoctorFinding {
    DoctorFinding::warn(
        CONFIG_INSPECTION_INCOMPLETE_CODE,
        DoctorCategory::Other,
        format!("Plaintext provider-key inspection incomplete for {source}: {reason}."),
    )
    .with_remediation(INCOMPLETE_REMEDIATION)
}

fn real_metadata(path: &Utf8Path) -> io::Result<CandidateKind> {
    let meta = std::fs::metadata(path.as_std_path())?;
    Ok(if meta.is_file() {
        CandidateKind::RegularFile
    } else if meta.is_dir() {
        CandidateKind::Directory
    } else {
        CandidateKind::Other
    })
}

fn real_read(path: &Utf8Path) -> io::Result<Vec<u8>> {
    std::fs::read(path.as_std_path())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::doctor::plan_doctor_fix;
    use crate::commands::doctor::{DoctorCategory, DoctorSeverity};
    use camino::Utf8Path;
    use std::ffi::OsString;
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

    /// Explicit no-template snapshot: no override, no home.
    fn no_template_env() -> TemplateEnv {
        TemplateEnv::default()
    }

    fn scan(layout: &Layout) -> Vec<DoctorFinding> {
        doctor_plaintext_secret_findings_with_env(
            layout,
            &no_template_env(),
            real_metadata,
            real_read,
        )
    }

    fn assert_one_secret_finding(layout: &Layout, expected_paths: &str) {
        let findings = scan(layout);
        assert_eq!(
            findings.len(),
            1,
            "expected one config-plaintext-secret finding, got {findings:?}"
        );
        let f = &findings[0];
        assert_eq!(f.code, CONFIG_PLAINTEXT_SECRET_CODE);
        assert_eq!(f.severity, DoctorSeverity::Warn);
        assert_eq!(f.category, DoctorCategory::Other);
        assert_eq!(
            f.message,
            format!(
                "Plaintext provider key(s) in {PROJECT_SOURCE}: {expected_paths}. Move them to GEMINI_API_KEY / OLLAMA_CLOUD_API_KEY (or .env) and rotate."
            )
        );
        assert!(!f.message.contains(SENTINEL), "message leaked sentinel");
        let rem = f.remediation.as_deref().expect("remediation required");
        assert!(
            rem.contains("ledgerful doctor --json"),
            "remediation must name doctor --json: {rem}"
        );
        assert!(!rem.contains(SENTINEL), "remediation leaked sentinel");
        assert!(
            !rem.contains("config set"),
            "remediation must not suggest config set: {rem}"
        );
    }

    #[test]
    fn user_template_secret_project_file_gemini_key_emits_exact_message() {
        let (_tmp, layout) = layout_with_config(&format!("[gemini]\napi_key = \"{SENTINEL}\"\n"));
        assert_one_secret_finding(&layout, "gemini.api_key");
    }

    #[test]
    fn user_template_secret_project_file_ollama_key_emits_exact_message() {
        let (_tmp, layout) =
            layout_with_config(&format!("[local_model]\nollama_key = \"{SENTINEL}\"\n"));
        assert_one_secret_finding(&layout, "local_model.ollama_key");
    }

    #[test]
    fn user_template_secret_both_ollama_spellings_detected_sorted() {
        let (_tmp, layout) = layout_with_config(&format!(
            "[local_model]\nollama_key = \"{SENTINEL}\"\nollama_cloud_api_key = \"{SENTINEL}\"\n"
        ));
        assert_one_secret_finding(
            &layout,
            "local_model.ollama_cloud_api_key, local_model.ollama_key",
        );
    }

    #[test]
    fn user_template_secret_one_finding_for_all_keys() {
        let (_tmp, layout) = layout_with_config(&format!(
            "[gemini]\napi_key = \"{SENTINEL}\"\n\n[local_model]\nollama_key = \"{SENTINEL}\"\n"
        ));
        assert_one_secret_finding(&layout, "gemini.api_key, local_model.ollama_key");
    }

    #[test]
    fn user_template_secret_remediation_is_five_lines_no_trailing_newline() {
        let (_tmp, layout) = layout_with_config(&format!("[gemini]\napi_key = \"{SENTINEL}\"\n"));
        let findings = scan(&layout);
        let rem = findings[0].remediation.as_deref().unwrap();
        assert_eq!(
            rem,
            format!(
                "Delete the assigned gemini.api_key lines from {PROJECT_SOURCE}\n\
                 Set GEMINI_API_KEY and/or OLLAMA_CLOUD_API_KEY in the process environment (or .env)\n\
                 An existing non-empty file key is not superseded by setting env\n\
                 Rotate those keys at the provider consoles\n\
                 ledgerful doctor --json"
            )
        );
        assert!(!rem.ends_with('\n'));
    }

    #[test]
    fn user_template_secret_ignores_empty_and_comments() {
        let (_tmp, layout) = layout_with_config(
            "[gemini]\n# api_key = \"test-key-not-a-secret\"\napi_key = \"\"\n\n[local_model]\n# ollama_key = \"test-key-not-a-secret\"\n",
        );
        assert!(scan(&layout).is_empty());
    }

    #[test]
    fn user_template_secret_ignores_whitespace_only() {
        let (_tmp, layout) = layout_with_config("[gemini]\napi_key = \"   \"\n");
        assert!(scan(&layout).is_empty());
    }

    #[test]
    fn user_template_secret_missing_project_no_home_is_clean() {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);
        layout.ensure_state_dir().unwrap();
        assert!(scan(&layout).is_empty());
    }

    #[test]
    fn user_template_secret_malformed_project_reports_parse_incomplete() {
        let (_tmp, layout) = layout_with_config("not = [ valid toml");
        let findings = scan(&layout);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].code, CONFIG_INSPECTION_INCOMPLETE_CODE);
        assert_eq!(
            findings[0].message,
            format!("Plaintext provider-key inspection incomplete for {PROJECT_SOURCE}: parse.")
        );
    }

    #[test]
    fn user_template_secret_wrong_container_type_is_invalid_key_type() {
        let (_tmp, layout) = layout_with_config("gemini = \"x\"\n");
        let findings = scan(&layout);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].code, CONFIG_INSPECTION_INCOMPLETE_CODE);
        assert_eq!(
            findings[0].message,
            format!(
                "Plaintext provider-key inspection incomplete for {PROJECT_SOURCE}: invalid-key-type. Invalid provider key or container type(s): gemini."
            )
        );
    }

    #[test]
    fn user_template_secret_mixed_valid_and_invalid_yields_both() {
        let (_tmp, layout) = layout_with_config(&format!(
            "[gemini]\napi_key = \"{SENTINEL}\"\n\n[local_model]\nollama_key = 7\n"
        ));
        let findings = scan(&layout);
        let codes: Vec<&str> = findings.iter().map(|f| f.code.as_str()).collect();
        assert_eq!(
            codes,
            [
                CONFIG_INSPECTION_INCOMPLETE_CODE,
                CONFIG_PLAINTEXT_SECRET_CODE
            ]
        );
        assert!(findings[0].message.ends_with(
            "invalid-key-type. Invalid provider key or container type(s): local_model.ollama_key."
        ));
    }

    #[test]
    #[serial_test::serial(env)]
    fn user_template_secret_env_only_fills_are_ignored() {
        let (_tmp, layout) = layout_with_config("[core]\nstrict = false\n");
        let _env = TempEnv::set("OLLAMA_CLOUD_API_KEY", SENTINEL);
        assert!(scan(&layout).is_empty());
    }

    #[test]
    fn user_template_secret_project_directory_is_metadata_incomplete() {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);
        layout.ensure_state_dir().unwrap();
        fs::create_dir(layout.config_file()).unwrap();
        let findings = scan(&layout);
        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings[0].message,
            format!("Plaintext provider-key inspection incomplete for {PROJECT_SOURCE}: metadata.")
        );
    }

    #[cfg(unix)]
    #[test]
    fn user_template_secret_symlink_regular_target_inspected_directory_refused() {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);
        layout.ensure_state_dir().unwrap();
        // Metadata follows the link: a regular target is inspected like a plain file.
        let target = root.join("real-config.toml");
        fs::write(&target, format!("[gemini]\napi_key = \"{SENTINEL}\"\n")).unwrap();
        std::os::unix::fs::symlink(&target, layout.config_file()).unwrap();
        assert_one_secret_finding(&layout, "gemini.api_key");

        // A link to a directory is refused as metadata uncertainty, not inspected.
        fs::remove_file(layout.config_file()).unwrap();
        let dir_target = root.join("dir-target");
        fs::create_dir(&dir_target).unwrap();
        std::os::unix::fs::symlink(&dir_target, layout.config_file()).unwrap();
        let findings = scan(&layout);
        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings[0].message,
            format!("Plaintext provider-key inspection incomplete for {PROJECT_SOURCE}: metadata.")
        );
    }

    #[test]
    fn user_template_secret_project_read_reason_via_injected_read() {
        let (_tmp, layout) = layout_with_config("[core]\nstrict = false\n");
        let findings = doctor_plaintext_secret_findings_with_env(
            &layout,
            &no_template_env(),
            |_| Ok(CandidateKind::RegularFile),
            |_| Err(io::Error::new(io::ErrorKind::PermissionDenied, "denied")),
        );
        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings[0].message,
            format!("Plaintext provider-key inspection incomplete for {PROJECT_SOURCE}: read.")
        );
    }

    #[test]
    fn user_template_secret_project_metadata_permission_denied_inspects_default() {
        let tmp = tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let layout = Layout::new(root);
        layout.ensure_state_dir().unwrap();
        let template = tmp.path().join("default-config.toml");
        let template_str = template.to_string_lossy().into_owned();
        fs::write(&template, format!("[gemini]\napi_key = \"{SENTINEL}\"\n")).unwrap();
        let env = TemplateEnv {
            override_path: Some(template_str.into()),
            ..TemplateEnv::default()
        };
        let findings = doctor_plaintext_secret_findings_with_env(
            &layout,
            &env,
            |path| {
                if path == layout.config_file().as_path() {
                    Err(io::Error::new(io::ErrorKind::PermissionDenied, "denied"))
                } else {
                    real_metadata(path)
                }
            },
            real_read,
        );
        let codes: Vec<&str> = findings.iter().map(|f| f.code.as_str()).collect();
        assert_eq!(
            codes,
            [
                CONFIG_INSPECTION_INCOMPLETE_CODE,
                CONFIG_PLAINTEXT_SECRET_CODE
            ]
        );
        assert_eq!(
            findings[0].message,
            format!("Plaintext provider-key inspection incomplete for {PROJECT_SOURCE}: metadata.")
        );
        assert_eq!(
            findings[1].message,
            format!(
                "Plaintext provider key(s) in {OVERRIDE_SOURCE}: gemini.api_key. Move them to GEMINI_API_KEY / OLLAMA_CLOUD_API_KEY (or .env) and rotate."
            )
        );
    }

    #[test]
    fn user_template_secret_override_missing_is_clean_no_home_search() {
        let (_tmp, layout) = layout_with_config("[core]\nstrict = false\n");
        let env = TemplateEnv {
            override_path: Some("definitely-missing-template.toml".into()),
            userprofile: Some("/should/not/be/read".into()),
            home: None,
        };
        let findings = doctor_plaintext_secret_findings_with_env(
            &layout,
            &env,
            |_| Err(io::Error::new(io::ErrorKind::NotFound, "missing")),
            |_| panic!("no template may be read when the override is missing"),
        );
        assert!(findings.is_empty());
    }

    #[test]
    fn user_template_secret_non_utf8_userprofile_shadows_home_with_encoding_warning() {
        #[cfg(unix)]
        use std::os::unix::ffi::OsStringExt;
        #[cfg(unix)]
        let bad: OsString = OsString::from_vec(vec![0xff, b'/', b'h']);
        #[cfg(not(unix))]
        let bad: OsString = {
            use std::os::windows::ffi::OsStringExt;
            OsString::from_wide(&[0xD800, 0x0061])
        };
        let (_tmp, layout) = layout_with_config("[core]\nstrict = false\n");
        let env = TemplateEnv {
            override_path: None,
            userprofile: Some(bad),
            home: Some("/valid/home".into()),
        };
        let findings = doctor_plaintext_secret_findings_with_env(
            &layout,
            &env,
            |_| Err(io::Error::new(io::ErrorKind::NotFound, "missing")),
            |_| panic!("HOME must never be inspected when USERPROFILE is present"),
        );
        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings[0].message,
            format!(
                "Plaintext provider-key inspection incomplete for {USERPROFILE_SOURCE}: encoding."
            )
        );
    }

    #[test]
    fn user_template_secret_template_invalid_utf8_bytes_report_encoding() {
        let (_tmp, layout) = layout_with_config("[core]\nstrict = false\n");
        let env = TemplateEnv {
            override_path: Some("template.toml".into()),
            ..TemplateEnv::default()
        };
        let findings = doctor_plaintext_secret_findings_with_env(
            &layout,
            &env,
            |path| {
                if path == layout.config_file().as_path() {
                    Err(io::Error::new(io::ErrorKind::NotFound, "no project config"))
                } else {
                    Ok(CandidateKind::RegularFile)
                }
            },
            |_| Ok(vec![0xff, 0xfe, 0x00]),
        );
        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings[0].message,
            format!(
                "Plaintext provider-key inspection incomplete for {OVERRIDE_SOURCE}: encoding."
            )
        );
    }

    #[test]
    fn user_template_secret_fix_plan_empty_for_diagnostic_only() {
        let (_tmp, layout) = layout_with_config(&format!("[gemini]\napi_key = \"{SENTINEL}\"\n"));
        let findings = scan(&layout);
        let plan = plan_doctor_fix(&findings, None, None, true);
        assert!(
            plan.actions.is_empty(),
            "doctor --fix must not strip keys: {:?}",
            plan.actions
        );
    }
}
