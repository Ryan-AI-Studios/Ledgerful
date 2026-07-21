use crate::config::ConfigError;
use crate::config::redact::{contains_structured_url_secret, is_secret_field_name};
use std::fs;
use std::io::Write;
use std::path::Path;
use std::str::FromStr;
use toml_edit::{Array, ArrayOfTables, DocumentMut, InlineTable, Item, Table, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StarterConfigSource {
    Environment(camino::Utf8PathBuf),
    Home(camino::Utf8PathBuf),
    BuiltIn,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StarterConfig {
    pub contents: String,
    pub removed_secret_paths: Vec<String>,
    pub source: StarterConfigSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SanitizedStarterConfig {
    pub contents: String,
    pub removed_secret_paths: Vec<String>,
}

pub fn starter_config_contents() -> Result<StarterConfig, ConfigError> {
    let (raw, source) = match crate::config::defaults::default_config_template_path() {
        Some(path) if path.exists() => {
            let raw = fs::read_to_string(path.as_std_path()).map_err(|source| {
                ConfigError::ReadFailed {
                    path: path.to_string(),
                    source,
                }
            })?;
            let source = if std::env::var_os(crate::config::defaults::DEFAULT_CONFIG_TEMPLATE_ENV)
                .is_some()
            {
                StarterConfigSource::Environment(path)
            } else {
                StarterConfigSource::Home(path)
            };
            (raw, source)
        }
        _ => (
            crate::config::defaults::DEFAULT_CONFIG.to_string(),
            StarterConfigSource::BuiltIn,
        ),
    };
    let sanitized = sanitize_starter_config(&raw)?;
    Ok(StarterConfig {
        contents: sanitized.contents,
        removed_secret_paths: sanitized.removed_secret_paths,
        source,
    })
}

/// Atomically publish a new config without replacing a concurrent/existing one.
///
/// The same-directory hard link is the create-if-absent publication primitive:
/// it either creates the destination as a complete file or reports that the
/// destination already exists. The staging name is operation-owned and is
/// removed on every handled path.
pub(crate) fn publish_starter_config(
    destination: &Path,
    contents: &str,
) -> Result<bool, ConfigError> {
    publish_starter_config_with_operations(
        destination,
        contents,
        &mut SystemStarterPublicationOperations { file: None },
    )
}

trait StarterPublicationOperations {
    fn create_stage(&mut self, stage: &Path) -> std::io::Result<()>;
    fn write_stage(&mut self, stage: &Path, contents: &[u8]) -> std::io::Result<()>;
    fn flush_stage(&mut self, stage: &Path) -> std::io::Result<()>;
    fn sync_stage(&mut self, stage: &Path) -> std::io::Result<()>;
    fn publish_no_replace(&mut self, stage: &Path, destination: &Path) -> std::io::Result<()>;
    fn cleanup_stage(&mut self, stage: &Path) -> std::io::Result<()>;
}

struct SystemStarterPublicationOperations {
    file: Option<fs::File>,
}

impl StarterPublicationOperations for SystemStarterPublicationOperations {
    fn create_stage(&mut self, stage: &Path) -> std::io::Result<()> {
        self.file = Some(
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(stage)?,
        );
        Ok(())
    }

    fn write_stage(&mut self, _stage: &Path, contents: &[u8]) -> std::io::Result<()> {
        self.file
            .as_mut()
            .ok_or_else(|| std::io::Error::other("starter stage is not open"))?
            .write_all(contents)
    }

    fn flush_stage(&mut self, _stage: &Path) -> std::io::Result<()> {
        self.file
            .as_mut()
            .ok_or_else(|| std::io::Error::other("starter stage is not open"))?
            .flush()
    }

    fn sync_stage(&mut self, _stage: &Path) -> std::io::Result<()> {
        self.file
            .as_ref()
            .ok_or_else(|| std::io::Error::other("starter stage is not open"))?
            .sync_all()
    }

    fn publish_no_replace(&mut self, stage: &Path, destination: &Path) -> std::io::Result<()> {
        self.file.take();
        publish_no_replace(stage, destination)
    }

    fn cleanup_stage(&mut self, stage: &Path) -> std::io::Result<()> {
        self.file.take();
        match fs::remove_file(stage) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}

#[cfg(windows)]
fn publish_no_replace(stage: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_WRITE_THROUGH, MoveFileExW};
    let source: Vec<u16> = stage.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    // Legitimate: Windows MoveFileExW for atomic no-replace publish of starter config.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    let success = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_WRITE_THROUGH,
        )
    };
    if success == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn publish_no_replace(stage: &Path, destination: &Path) -> std::io::Result<()> {
    let directory = fs::File::open(
        stage
            .parent()
            .ok_or_else(|| std::io::Error::other("stage has no parent"))?,
    )?;
    nix::fcntl::renameat2(
        &directory,
        stage
            .file_name()
            .ok_or_else(|| std::io::Error::other("stage has no name"))?,
        &directory,
        destination
            .file_name()
            .ok_or_else(|| std::io::Error::other("destination has no name"))?,
        nix::fcntl::RenameFlags::RENAME_NOREPLACE,
    )
    .map_err(std::io::Error::from)
}

#[cfg(all(not(windows), not(all(target_os = "linux", target_env = "gnu"))))]
fn publish_no_replace(stage: &Path, destination: &Path) -> std::io::Result<()> {
    match fs::hard_link(stage, destination) {
        Ok(()) => Ok(()),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::Unsupported | std::io::ErrorKind::CrossesDevices
            ) =>
        {
            Err(no_replace_unavailable(error))
        }
        Err(error) => Err(error),
    }
}

#[cfg_attr(not(test), allow(dead_code))]
fn no_replace_unavailable(_source: std::io::Error) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "starter config publication requires an atomic no-clobber filesystem primitive; \
         this platform/filesystem provides none, so config creation was refused safely",
    )
}

#[cfg(test)]
fn publish_starter_config_with<P, C>(
    destination: &Path,
    contents: &str,
    publish: P,
    cleanup: C,
) -> Result<bool, ConfigError>
where
    P: FnOnce(&Path, &Path) -> std::io::Result<()>,
    C: FnOnce(&Path) -> std::io::Result<()>,
{
    struct ClosureStarterPublicationOperations<P, C> {
        system: SystemStarterPublicationOperations,
        publish: Option<P>,
        cleanup: Option<C>,
    }

    impl<P, C> StarterPublicationOperations for ClosureStarterPublicationOperations<P, C>
    where
        P: FnOnce(&Path, &Path) -> std::io::Result<()>,
        C: FnOnce(&Path) -> std::io::Result<()>,
    {
        fn create_stage(&mut self, stage: &Path) -> std::io::Result<()> {
            self.system.create_stage(stage)
        }

        fn write_stage(&mut self, stage: &Path, contents: &[u8]) -> std::io::Result<()> {
            self.system.write_stage(stage, contents)
        }

        fn flush_stage(&mut self, stage: &Path) -> std::io::Result<()> {
            self.system.flush_stage(stage)
        }

        fn sync_stage(&mut self, stage: &Path) -> std::io::Result<()> {
            self.system.sync_stage(stage)
        }

        fn publish_no_replace(&mut self, stage: &Path, destination: &Path) -> std::io::Result<()> {
            self.system.file.take();
            self.publish
                .take()
                .ok_or_else(|| std::io::Error::other("publish operation already consumed"))?(
                stage,
                destination,
            )
        }

        fn cleanup_stage(&mut self, stage: &Path) -> std::io::Result<()> {
            self.system.file.take();
            self.cleanup
                .take()
                .ok_or_else(|| std::io::Error::other("cleanup operation already consumed"))?(
                stage
            )
        }
    }

    publish_starter_config_with_operations(
        destination,
        contents,
        &mut ClosureStarterPublicationOperations {
            system: SystemStarterPublicationOperations { file: None },
            publish: Some(publish),
            cleanup: Some(cleanup),
        },
    )
}

fn publish_starter_config_with_operations<O>(
    destination: &Path,
    contents: &str,
    operations: &mut O,
) -> Result<bool, ConfigError>
where
    O: StarterPublicationOperations,
{
    let parent = destination
        .parent()
        .ok_or_else(|| ConfigError::ValidationFailed {
            reason: "starter config destination has no parent directory".to_string(),
        })?;
    let stage = parent.join(format!(
        ".ledgerful-starter-{}.tmp",
        uuid::Uuid::new_v4().simple()
    ));
    let result = (|| -> std::io::Result<bool> {
        operations.create_stage(&stage)?;
        operations.write_stage(&stage, contents.as_bytes())?;
        operations.flush_stage(&stage)?;
        operations.sync_stage(&stage)?;

        match operations.publish_no_replace(&stage, destination) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
            Err(error) => Err(error),
        }
    })();
    let cleanup_result = operations.cleanup_stage(&stage);
    match (result, cleanup_result) {
        (Ok(published), Ok(())) => Ok(published),
        (Err(source), Ok(())) => Err(map_starter_publication_error(destination, source)),
        (Ok(_), Err(source)) => Err(ConfigError::WriteFailed {
            path: stage.display().to_string(),
            source,
        }),
        (Err(_), Err(source)) => Err(ConfigError::WriteFailed {
            path: stage.display().to_string(),
            source: std::io::Error::new(
                source.kind(),
                "starter publication failed and operation-owned staging cleanup also failed",
            ),
        }),
    }
}

fn map_starter_publication_error(destination: &Path, source: std::io::Error) -> ConfigError {
    if source.kind() == std::io::ErrorKind::Unsupported {
        ConfigError::AtomicPublicationUnavailable {
            path: destination.display().to_string(),
        }
    } else {
        ConfigError::WriteFailed {
            path: destination.display().to_string(),
            source,
        }
    }
}

pub(crate) fn sanitize_starter_config(input: &str) -> Result<SanitizedStarterConfig, ConfigError> {
    let mut document = DocumentMut::from_str(input).map_err(|_| ConfigError::ValidationFailed {
        reason: "starter config contains malformed TOML".to_string(),
    })?;
    let mut removed = Vec::new();
    sanitize_table(document.as_table_mut(), "", &mut removed);
    removed.sort();
    removed.dedup();

    let contents = document.to_string();
    let config: crate::config::Config =
        toml::from_str(&contents).map_err(|_| ConfigError::ValidationFailed {
            reason: "sanitized starter config is not a valid Ledgerful configuration".to_string(),
        })?;
    crate::config::validate_config(&config).map_err(|_| ConfigError::ValidationFailed {
        reason: "sanitized starter config failed validation".to_string(),
    })?;

    let invariant_document =
        DocumentMut::from_str(&contents).map_err(|_| ConfigError::ValidationFailed {
            reason: "sanitized starter config could not be re-read".to_string(),
        })?;
    let invariant_violations = find_secret_assignments(invariant_document.as_table(), "");
    if !invariant_violations.is_empty() {
        return Err(ConfigError::ValidationFailed {
            reason: "sanitized starter config still contains secret-bearing assignments"
                .to_string(),
        });
    }

    Ok(SanitizedStarterConfig {
        contents,
        removed_secret_paths: removed,
    })
}

fn sanitize_table(table: &mut Table, prefix: &str, removed: &mut Vec<String>) {
    let keys: Vec<String> = table.iter().map(|(key, _)| key.to_string()).collect();
    for key in keys {
        let path = join_path(prefix, &key);
        let remove = table
            .get(&key)
            .is_some_and(|item| assignment_is_secret(&key, item.as_value()));
        if remove {
            table.remove(&key);
        } else if let Some(item) = table.get_mut(&key) {
            sanitize_item(item, &path, removed);
            continue;
        } else {
            continue;
        }
        removed.push(path);
    }
}

fn sanitize_item(item: &mut Item, path: &str, removed: &mut Vec<String>) {
    match item {
        Item::Table(table) => sanitize_table(table, path, removed),
        Item::ArrayOfTables(tables) => sanitize_array_of_tables(tables, path, removed),
        Item::Value(value) => sanitize_value(value, path, removed),
        Item::None => {}
    }
}

fn sanitize_array_of_tables(tables: &mut ArrayOfTables, path: &str, removed: &mut Vec<String>) {
    for (index, table) in tables.iter_mut().enumerate() {
        sanitize_table(table, &format!("{path}[{index}]"), removed);
    }
}

fn sanitize_value(value: &mut Value, path: &str, removed: &mut Vec<String>) {
    match value {
        Value::InlineTable(table) => sanitize_inline_table(table, path, removed),
        Value::Array(array) => sanitize_array(array, path, removed),
        _ => {}
    }
}

fn sanitize_array(array: &mut Array, path: &str, removed: &mut Vec<String>) {
    for (index, value) in array.iter_mut().enumerate() {
        sanitize_value(value, &format!("{path}[{index}]"), removed);
    }
}

fn sanitize_inline_table(table: &mut InlineTable, prefix: &str, removed: &mut Vec<String>) {
    let keys: Vec<String> = table.iter().map(|(key, _)| key.to_string()).collect();
    for key in keys {
        let path = join_path(prefix, &key);
        let remove = table
            .get(&key)
            .is_some_and(|value| assignment_is_secret(&key, Some(value)));
        if remove {
            table.remove(&key);
            removed.push(path);
        } else if let Some(value) = table.get_mut(&key) {
            sanitize_value(value, &path, removed);
        }
    }
}

fn assignment_is_secret(key: &str, value: Option<&Value>) -> bool {
    is_secret_field_name(key) || value.is_some_and(value_contains_structured_secret)
}

fn value_contains_structured_secret(value: &Value) -> bool {
    match value {
        Value::String(value) => contains_structured_url_secret(value.value()),
        Value::Array(values) => values.iter().any(value_contains_structured_secret),
        Value::InlineTable(table) => table.iter().any(|(key, value)| {
            is_secret_field_name(key) || value_contains_structured_secret(value)
        }),
        _ => false,
    }
}

fn find_secret_assignments(table: &Table, prefix: &str) -> Vec<String> {
    let mut found = Vec::new();
    for (key, item) in table.iter() {
        let path = join_path(prefix, key);
        if assignment_is_secret(key, item.as_value()) {
            found.push(path);
            continue;
        }
        match item {
            Item::Table(child) => found.extend(find_secret_assignments(child, &path)),
            Item::ArrayOfTables(children) => {
                for (index, child) in children.iter().enumerate() {
                    found.extend(find_secret_assignments(child, &format!("{path}[{index}]")));
                }
            }
            _ => {}
        }
    }
    found
}

fn join_path(prefix: &str, key: &str) -> String {
    if prefix.is_empty() {
        key.to_string()
    } else {
        format!("{prefix}.{key}")
    }
}

#[cfg(test)]
mod tests {
    mod env_guard {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/integration/common/env_guard.rs"
        ));
    }

    use env_guard::TempEnv;

    const SENTINEL: &str = "TA33-SENTINEL-MUST-NEVER-LEAK";

    #[test]
    #[serial_test::serial(env)]
    fn missing_explicit_template_preserves_builtin_fallback_contract() {
        let temp = tempfile::tempdir().expect("tempdir");
        let missing = temp.path().join("missing.toml");
        let key = crate::config::defaults::DEFAULT_CONFIG_TEMPLATE_ENV;
        let original = std::env::var_os(key);
        let missing = missing.to_string_lossy();
        let starter = {
            let _template = TempEnv::set(key, &missing);
            super::starter_config_contents().expect("built-in fallback")
        };
        assert_eq!(std::env::var_os(key), original);
        assert_eq!(starter.source, super::StarterConfigSource::BuiltIn);
        assert!(starter.contents.contains("[core]"));
    }

    #[test]
    #[serial_test::serial(env)]
    fn template_environment_is_restored_after_early_error() {
        fn return_early_after_removing(key: &str) -> Result<(), &'static str> {
            let _template = TempEnv::remove(key);
            assert_eq!(std::env::var_os(key), None);
            Err("injected early error")
        }

        let key = crate::config::defaults::DEFAULT_CONFIG_TEMPLATE_ENV;
        let original = std::env::var_os(key);
        let temporary = format!("missing-{}.toml", uuid::Uuid::new_v4());
        {
            let _baseline = TempEnv::set(key, &temporary);
            let result = return_early_after_removing(key);

            assert_eq!(result, Err("injected early error"));
            assert_eq!(std::env::var(key).as_deref(), Ok(temporary.as_str()));
        }
        assert_eq!(std::env::var_os(key), original);
    }

    #[test]
    fn sanitize_starter_config_nested_containers_removes_secrets() {
        let input = format!(
            r#"# api_key in a comment is preserved
[gemini]
api_key = "{SENTINEL}"
model = "gemini"

[database]
database_url = "not even a valid URL"
ordinary_url = "https://example.com/docs"
credential_url = "postgres://public:{SENTINEL}@localhost/db"
query_url = "https://example.com/api?token={SENTINEL}"
username_only = "https://public@example.com/docs"
inline = {{ name = "safe", password = "{SENTINEL}" }}
items = [{{ name = "safe", api_key = "{SENTINEL}" }}]

[[providers]]
name = "first"
secret = "{SENTINEL}"

[[providers]]
name = "second"
endpoint = "https://example.com"
"#
        );

        let sanitized = super::sanitize_starter_config(&input).expect("template should sanitize");

        assert!(!sanitized.contents.contains(SENTINEL));
        assert!(
            sanitized
                .contents
                .contains("# api_key in a comment is preserved")
        );
        assert!(sanitized.contents.contains("model = \"gemini\""));
        assert!(
            sanitized
                .contents
                .contains("ordinary_url = \"https://example.com/docs\"")
        );
        assert!(
            sanitized
                .contents
                .contains("username_only = \"https://public@example.com/docs\"")
        );
        assert_eq!(
            sanitized.removed_secret_paths,
            vec![
                "database.credential_url",
                "database.database_url",
                "database.inline",
                "database.items",
                "database.query_url",
                "gemini.api_key",
                "providers[0].secret",
            ]
        );
    }

    #[test]
    fn sanitize_starter_config_malformed_toml_fails_without_echoing_value() {
        let input = format!("[gemini]\napi_key = \"{SENTINEL}");
        let error = super::sanitize_starter_config(&input)
            .expect_err("malformed templates must fail closed")
            .to_string();

        assert!(!error.contains(SENTINEL));
        assert!(error.contains("starter config"));
    }

    #[test]
    fn sanitize_starter_config_invalid_ledgerful_config_fails_closed() {
        let error = super::sanitize_starter_config("[watch]\ndebounce_ms = 0\n")
            .expect_err("invalid Ledgerful config must fail closed")
            .to_string();

        assert!(error.contains("sanitized starter config failed validation"));
    }

    #[test]
    fn sanitize_starter_config_non_secret_formatting_is_stable() {
        let input = "# retained\n[core]\nstrict  =  true # retained spacing\n";
        let sanitized = super::sanitize_starter_config(input).expect("clean template should pass");

        assert_eq!(sanitized.contents, input);
        assert!(sanitized.removed_secret_paths.is_empty());
    }

    #[test]
    fn structured_url_classifier_handles_allowlist_and_false_positives() {
        assert!(crate::config::redact::contains_structured_url_secret(
            "postgres://user:pass@example.com/db"
        ));
        assert!(crate::config::redact::contains_structured_url_secret(
            "https://example.com/path?api_key=value"
        ));
        assert!(!crate::config::redact::contains_structured_url_secret(
            "https://public@example.com/docs"
        ));
        assert!(!crate::config::redact::contains_structured_url_secret(
            "ftp://user:pass@example.com/file"
        ));
        assert!(!crate::config::redact::contains_structured_url_secret(
            "not-a-url"
        ));
    }

    #[test]
    fn starter_and_display_structured_url_policy_stay_in_parity() {
        for (url, secret) in crate::config::redact::structured_url_test_cases() {
            let input = format!("[core]\nstrict=true\n[probe]\nvalue={url:?}\n");
            let starter = super::sanitize_starter_config(&input);
            let mut display = serde_json::json!({"nested": [[url]]});
            crate::config::redact::redact_config_value(&mut display);
            assert_eq!(display["nested"][0][0] == "[REDACTED]", secret, "{url}");
            match starter {
                Ok(sanitized) => {
                    assert_eq!(!sanitized.contents.contains("value"), secret, "{url}");
                }
                Err(error) => {
                    assert!(!error.to_string().contains("TA33-SENTINEL"));
                    panic!("valid parity fixture failed: {error}");
                }
            }
        }
    }

    #[test]
    fn scalar_and_nested_array_secret_urls_remove_the_owning_assignment() {
        let input = "[core]\nstrict = true\nendpoints = [\"https://safe.example\", [\"postgres://u:p@db/x\"]]\nordinary = [\"https://example.com?a=b\"]\n";
        let sanitized = super::sanitize_starter_config(input).expect("sanitize");
        assert!(!sanitized.contents.contains("postgres://"));
        assert!(!sanitized.contents.contains("endpoints"));
        assert!(sanitized.contents.contains("ordinary"));
        assert_eq!(sanitized.removed_secret_paths, ["core.endpoints"]);
    }

    #[test]
    fn independent_invariant_finds_secret_descendants() {
        let document: toml_edit::DocumentMut =
            "[x]\nvalues = [[\"https://example.test?a=1\"], [\"redis://u:p@host\"]]\n"
                .parse()
                .expect("fixture");
        assert_eq!(
            super::find_secret_assignments(document.as_table(), ""),
            ["x.values"]
        );
    }

    #[test]
    fn publish_starter_config_existing_destination_wins_without_temp_debris() {
        let temp = tempfile::tempdir().expect("tempdir");
        let destination = temp.path().join("config.toml");
        std::fs::write(&destination, "existing").expect("fixture");

        let created =
            super::publish_starter_config(&destination, "replacement").expect("publish result");

        assert!(!created);
        assert_eq!(
            std::fs::read_to_string(&destination).expect("existing config"),
            "existing"
        );
        let debris: Vec<_> = std::fs::read_dir(temp.path())
            .expect("directory")
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".ledgerful-starter-")
            })
            .collect();
        assert!(debris.is_empty(), "temporary files remain: {debris:?}");
    }

    #[test]
    fn publish_starter_config_new_destination_is_complete_and_cleans_stage() {
        let temp = tempfile::tempdir().expect("tempdir");
        let destination = temp.path().join("config.toml");

        let created =
            super::publish_starter_config(&destination, "complete").expect("publish result");

        assert!(created);
        assert_eq!(
            std::fs::read_to_string(&destination).expect("published config"),
            "complete"
        );
        assert_eq!(
            std::fs::read_dir(temp.path())
                .expect("directory")
                .filter_map(Result::ok)
                .count(),
            1
        );
    }

    #[test]
    fn publish_reports_cleanup_failure_after_destination_is_complete() {
        let temp = tempfile::tempdir().expect("tempdir");
        let destination = temp.path().join("config.toml");
        let error = super::publish_starter_config_with(
            &destination,
            "complete",
            |stage, destination| std::fs::hard_link(stage, destination),
            |_| {
                Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "injected",
                ))
            },
        )
        .expect_err("cleanup failure is observable");
        assert!(destination.exists());
        assert!(error.to_string().contains("ledgerful-starter"));
    }

    #[test]
    fn unavailable_no_replace_primitive_fails_closed_without_publishing_partial_config() {
        let temp = tempfile::tempdir().expect("tempdir");
        let destination = temp.path().join("config.toml");
        let error = super::publish_starter_config_with(
            &destination,
            "complete",
            |_stage, _destination| {
                Err(super::no_replace_unavailable(
                    std::io::Error::from_raw_os_error(18),
                ))
            },
            |path| std::fs::remove_file(path),
        )
        .expect_err("unsupported primitive must fail closed");

        assert!(!destination.exists());
        assert!(error.to_string().contains("atomic no-clobber"));
        assert_eq!(
            std::fs::read_dir(temp.path())
                .expect("directory")
                .filter_map(Result::ok)
                .count(),
            0,
            "staging file must be cleaned after a failed publication"
        );
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum StarterFault {
        Create,
        WritePartial,
        Flush,
        Sync,
        Publish,
        Cleanup,
    }

    struct FaultingStarterOperations {
        fault: StarterFault,
        file: Option<std::fs::File>,
        cleanup_called: bool,
        cleanup_also_fails: bool,
    }

    impl super::StarterPublicationOperations for FaultingStarterOperations {
        fn create_stage(&mut self, stage: &std::path::Path) -> std::io::Result<()> {
            if self.fault == StarterFault::Create {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "injected create",
                ));
            }
            self.file = Some(
                std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(stage)?,
            );
            Ok(())
        }

        fn write_stage(
            &mut self,
            _stage: &std::path::Path,
            contents: &[u8],
        ) -> std::io::Result<()> {
            use std::io::Write;
            let file = self.file.as_mut().expect("stage created");
            if self.fault == StarterFault::WritePartial {
                file.write_all(&contents[..contents.len().min(3)])?;
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WriteZero,
                    "injected partial write",
                ));
            }
            file.write_all(contents)
        }

        fn flush_stage(&mut self, _stage: &std::path::Path) -> std::io::Result<()> {
            use std::io::Write;
            if self.fault == StarterFault::Flush {
                return Err(std::io::Error::other("injected flush"));
            }
            self.file.as_mut().expect("stage created").flush()
        }

        fn sync_stage(&mut self, _stage: &std::path::Path) -> std::io::Result<()> {
            if self.fault == StarterFault::Sync {
                return Err(std::io::Error::other("injected sync"));
            }
            self.file.as_ref().expect("stage created").sync_all()
        }

        fn publish_no_replace(
            &mut self,
            _stage: &std::path::Path,
            _destination: &std::path::Path,
        ) -> std::io::Result<()> {
            self.file.take();
            Err(std::io::Error::new(
                if self.fault == StarterFault::Publish {
                    std::io::ErrorKind::PermissionDenied
                } else {
                    std::io::ErrorKind::AlreadyExists
                },
                "injected publish",
            ))
        }

        fn cleanup_stage(&mut self, stage: &std::path::Path) -> std::io::Result<()> {
            self.cleanup_called = true;
            self.file.take();
            if self.fault == StarterFault::Cleanup || self.cleanup_also_fails {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "injected cleanup",
                ));
            }
            match std::fs::remove_file(stage) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error),
            }
        }
    }

    #[test]
    fn starter_publication_operation_faults_never_publish_partial_destination() {
        for fault in [
            StarterFault::Create,
            StarterFault::WritePartial,
            StarterFault::Flush,
            StarterFault::Sync,
            StarterFault::Publish,
        ] {
            let temp = tempfile::tempdir().expect("tempdir");
            let destination = temp.path().join("config.toml");
            let mut operations = FaultingStarterOperations {
                fault,
                file: None,
                cleanup_called: false,
                cleanup_also_fails: false,
            };

            let error = super::publish_starter_config_with_operations(
                &destination,
                "complete",
                &mut operations,
            )
            .expect_err("injected operation must fail");

            assert!(!destination.exists(), "{fault:?}");
            assert!(operations.cleanup_called, "{fault:?}");
            assert!(
                std::fs::read_dir(temp.path())
                    .expect("directory")
                    .filter_map(Result::ok)
                    .next()
                    .is_none(),
                "{fault:?}: {error}"
            );
        }
    }

    #[test]
    fn starter_publication_existing_destination_and_cleanup_failure_preserve_prior_bytes() {
        let temp = tempfile::tempdir().expect("tempdir");
        let destination = temp.path().join("config.toml");
        std::fs::write(&destination, b"prior").expect("fixture");
        let mut operations = FaultingStarterOperations {
            fault: StarterFault::Cleanup,
            file: None,
            cleanup_called: false,
            cleanup_also_fails: false,
        };

        let error = super::publish_starter_config_with_operations(
            &destination,
            "replacement",
            &mut operations,
        )
        .expect_err("cleanup failure is observable");

        assert_eq!(std::fs::read(&destination).unwrap(), b"prior");
        assert!(operations.cleanup_called);
        assert!(error.to_string().contains("Failed to write config file"));
    }

    #[test]
    fn starter_publication_fault_matrix_preserves_every_existing_destination() {
        for fault in [
            StarterFault::Create,
            StarterFault::WritePartial,
            StarterFault::Flush,
            StarterFault::Sync,
            StarterFault::Publish,
            StarterFault::Cleanup,
        ] {
            let temp = tempfile::tempdir().expect("tempdir");
            let destination = temp.path().join("config.toml");
            std::fs::write(&destination, b"prior").expect("fixture");
            let mut operations = FaultingStarterOperations {
                fault,
                file: None,
                cleanup_called: false,
                cleanup_also_fails: false,
            };

            let _ = super::publish_starter_config_with_operations(
                &destination,
                "replacement",
                &mut operations,
            );

            assert_eq!(std::fs::read(&destination).unwrap(), b"prior", "{fault:?}");
            assert!(operations.cleanup_called, "{fault:?}");
        }
    }

    #[test]
    fn starter_publication_primary_and_cleanup_failures_are_both_observable_and_safe() {
        let temp = tempfile::tempdir().expect("tempdir");
        let destination = temp.path().join("config.toml");
        let mut operations = FaultingStarterOperations {
            fault: StarterFault::WritePartial,
            file: None,
            cleanup_called: false,
            cleanup_also_fails: true,
        };

        let error = super::publish_starter_config_with_operations(
            &destination,
            "replacement",
            &mut operations,
        )
        .expect_err("double failure must be observable");

        assert!(!destination.exists());
        assert!(operations.cleanup_called);
        assert!(error.to_string().contains("Failed to write config file"));
    }
}
