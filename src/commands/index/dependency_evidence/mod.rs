mod discovery;
mod files;
mod native;
mod relationships;

use crate::index::languages::Language;
use crate::scip::orchestrator::ScipToolchain;
use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use miette::{IntoDiagnostic, Result};
use rusqlite::{Connection, Transaction};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write};
use std::path::Path;

const LIMITATIONS: [&str; 4] = [
    "crossLanguageUnqualified",
    "ignoredOrUnseenFiles",
    "importsAliasesReExportsUnqualified",
    "retainedScipRowsUnattributed",
];

const LANGUAGES: &[(Language, &str, &[&str])] = &[
    (
        Language::Cpp,
        "cpp",
        &["c", "cc", "cpp", "cxx", "h", "h++", "hh", "hpp", "hxx"],
    ),
    (Language::Go, "go", &["go"]),
    (Language::Java, "java", &["java"]),
    (Language::Markdown, "markdown", &["md"]),
    (Language::Python, "python", &["py"]),
    (Language::Rust, "rust", &["rs"]),
    (
        Language::TypeScript,
        "typescript",
        &["js", "jsx", "ts", "tsx"],
    ),
];

fn language_sql(column: &str, missing: &str) -> String {
    // Column and language data are private constants, never repository input.
    let mut sql = format!("CASE WHEN {column} IS NULL THEN '{missing}'");
    for (_, label, extensions) in LANGUAGES {
        for extension in *extensions {
            use std::fmt::Write as _;
            let _ = write!(
                sql,
                " WHEN substr({column}, -{length}) COLLATE BINARY = '.{extension}'
                  AND substr({column}, -{prefix}, 1) NOT IN ('', '/', char(92)) THEN '{label}'",
                length = extension.len() + 1,
                prefix = extension.len() + 2,
            );
        }
    }
    sql.push_str(" ELSE 'other' END");
    sql
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Report {
    schema_version: u32,
    kind: &'static str,
    native: native::NativeEvidence,
    languages: Vec<LanguageEvidence>,
    relationships: relationships::Relationships,
    scip_artifact: Artifact,
    limitations: [&'static str; 4],
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LanguageEvidence {
    language: &'static str,
    native_parser: &'static str,
    indexed_files: Option<i64>,
    scip_toolchain: Option<&'static str>,
    root_markers: Vec<&'static str>,
    generation_eligibility: &'static str,
    capability: &'static str,
    setup_hint: Option<&'static str>,
}

fn file_counts(conn: &Connection) -> rusqlite::Result<BTreeMap<String, i64>> {
    let language = language_sql("file_path", "other");
    let mut stmt = conn.prepare(&format!(
        "SELECT {language} AS lane, COUNT(*) FROM project_files WHERE parse_status != 'DELETED' GROUP BY lane"
    ))?;
    stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect()
}

fn language_evidence(
    root: &Path,
    counts: Option<&BTreeMap<String, i64>>,
    other: bool,
) -> Vec<LanguageEvidence> {
    let count = |lang| counts.map(|map| map.get(lang).copied().unwrap_or_default());
    let mut languages = Vec::new();
    for &(language, label, _) in LANGUAGES {
        let tool = match language {
            Language::Python => Some(ScipToolchain::ScipPython),
            Language::Rust => Some(ScipToolchain::RustAnalyzer),
            Language::TypeScript => Some(ScipToolchain::ScipTypescript),
            _ => None,
        };
        let (root_markers, marker_error) =
            tool.map(|tool| tool.root_markers(root)).unwrap_or_default();
        let (generation_eligibility, capability) = match tool {
            None => ("notWired", "notApplicable"),
            Some(_) if marker_error => ("unknown", "notProbed"),
            Some(_) if root_markers.is_empty() => ("missingRootMarker", "notApplicable"),
            Some(_) => ("candidate", "notProbed"),
        };
        languages.push(LanguageEvidence {
            language: label,
            native_parser: if language == Language::Markdown {
                "documentation"
            } else {
                "source"
            },
            indexed_files: count(label),
            scip_toolchain: tool.map(ScipToolchain::exe_name),
            root_markers,
            generation_eligibility,
            capability,
            setup_hint: tool.map(ScipToolchain::install_hint),
        });
    }
    if other {
        languages.push(LanguageEvidence {
            language: "other",
            native_parser: "unsupported",
            indexed_files: count("other"),
            scip_toolchain: None,
            root_markers: Vec::new(),
            generation_eligibility: "notWired",
            capability: "notApplicable",
            setup_hint: None,
        });
    }
    languages.sort_by_key(|row| row.language);
    languages
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Artifact {
    state: &'static str,
    source_freshness: &'static str,
    reasons: BTreeSet<&'static str>,
}

impl Artifact {
    fn new(state: &'static str, reason: Option<&'static str>) -> Self {
        Self {
            state,
            source_freshness: "unknown",
            reasons: reason.into_iter().collect(),
        }
    }
}

fn assess_artifact(
    conn: Option<&Connection>,
    path: Option<&Path>,
    db_reason: Option<&'static str>,
) -> Artifact {
    use crate::scip::stale_detect::{ArtifactRegistration, read_artifact_registration};
    use files::ReadFailure;
    let Some(path) = path else {
        return Artifact::new("notChecked", None);
    };
    let Some(raw) = path.to_str().filter(|_| path.is_absolute()) else {
        return Artifact::new("unknown", Some("artifactSpellingUnverified"));
    };
    if !files::canonical_spelling(path) {
        return Artifact::new("unknown", Some("artifactSpellingUnverified"));
    }
    let Some(conn) = conn else {
        return Artifact::new("unknown", db_reason);
    };
    let registration = match read_artifact_registration(conn, raw) {
        Ok(registration) => registration,
        Err(_) => return Artifact::new("unknown", Some("registrationReadFailed")),
    };
    let (digest, _) = files::read_stable(path, 100 * 1024 * 1024, true, |input| {
        let mut digest = blake3::Hasher::new();
        digest.update_reader(input)?;
        Ok(digest.finalize().to_hex())
    });
    let digest = match digest {
        Ok(digest) => digest,
        Err(error) => {
            return Artifact::new(
                "unknown",
                Some(match error {
                    ReadFailure::Unstable => "artifactUnstable",
                    ReadFailure::Missing | ReadFailure::Unreadable => "artifactUnreadable",
                    ReadFailure::Capped => "artifactLimit",
                    ReadFailure::Spelling => "artifactSpellingUnverified",
                }),
            );
        }
    };
    if !files::canonical_spelling(path) {
        return Artifact::new("unknown", Some("artifactUnstable"));
    }
    match registration {
        ArtifactRegistration::Exact(hash)
            if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) =>
        {
            Artifact::new("unknown", Some("registrationInvalid"))
        }
        ArtifactRegistration::Exact(hash) if digest.as_str() == hash => {
            Artifact::new("matchesRegisteredBytes", None)
        }
        ArtifactRegistration::Exact(_) => Artifact::new("changedSinceRegistration", None),
        ArtifactRegistration::ExactKeyNotFound => Artifact::new("unregistered", None),
        ArtifactRegistration::RelativeKeyExists => {
            Artifact::new("unknown", Some("relativeRegistration"))
        }
    }
}

fn sqlite_reason(error: &rusqlite::Error) -> &'static str {
    use rusqlite::ErrorCode;
    match error.sqlite_error_code() {
        Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked) => "databaseBusy",
        Some(ErrorCode::NotADatabase | ErrorCode::DatabaseCorrupt) => "databaseCorrupt",
        _ => "databaseReadFailed",
    }
}

fn open_storage(layout: &Layout) -> std::result::Result<Connection, &'static str> {
    let db = layout.state_subdir().join("ledger.db");
    match db.as_std_path().metadata() {
        Ok(meta) if meta.len() == 0 => return Err("databaseEmpty"),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Err("databaseMissing"),
        _ => {}
    }
    StorageManager::open_sqlite_read_only(db.as_std_path(), 5000)
        .map_err(|error| sqlite_reason(&error))
}

fn snapshot(connection: &Connection) -> std::result::Result<Transaction<'_>, &'static str> {
    let conn = connection
        .unchecked_transaction()
        .map_err(|error| sqlite_reason(&error))?;
    // Older and newer schemas are both unknown for this version; querying
    // user_version also pins the deferred read transaction's snapshot.
    let version: i64 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|error| sqlite_reason(&error))?;
    if usize::try_from(version).ok() != Some(crate::state::migrations::get_migrations_count()) {
        return Err("databaseSchemaIncompatible");
    }
    Ok(conn)
}

pub(crate) fn execute(json: bool, artifact: Option<&Path>) -> Result<()> {
    let layout = crate::commands::helpers::get_layout()?;
    let root = layout.root.as_std_path();
    let storage = open_storage(&layout);
    let snapshot = storage
        .as_ref()
        .map_err(|reason| *reason)
        .and_then(snapshot);
    let database_reason = snapshot.as_ref().err().copied();
    let conn = snapshot.as_ref().ok().map(|snapshot| &**snapshot);
    let (native, other) = native::assess(root, conn, database_reason);
    let counts = conn.and_then(|conn| file_counts(conn).ok());
    let report = Report {
        schema_version: 1,
        kind: "dependencyEvidence",
        native,
        languages: language_evidence(root, counts.as_ref(), other),
        relationships: match conn {
            Some(conn) => relationships::collect(conn),
            None => relationships::Relationships::unknown(
                database_reason.unwrap_or("databaseReadFailed"),
            ),
        },
        scip_artifact: assess_artifact(conn, artifact, database_reason),
        limitations: LIMITATIONS,
    };
    let mut stdout = io::stdout().lock();
    if json {
        writeln!(stdout, "{}", crate::output::json::format_json(&report)?).into_diagnostic()?;
    } else {
        write_human(&mut stdout, &report)?;
    }
    stdout.flush().into_diagnostic()
}

fn write_human(out: &mut dyn Write, report: &Report) -> Result<()> {
    let value = serde_json::to_value(report).into_diagnostic()?;
    writeln!(out, "Dependency evidence (v{})", report.schema_version).into_diagnostic()?;
    for section in [
        "native",
        "languages",
        "relationships",
        "scipArtifact",
        "limitations",
    ] {
        writeln!(out, "{section}").into_diagnostic()?;
        if let Some(fields) = value[section].as_object() {
            for (label, field) in fields {
                if label == "witnesses" {
                    if let Some(witnesses) = field.as_array() {
                        for witness in witnesses {
                            writeln!(out, "  witness {witness}").into_diagnostic()?;
                        }
                    }
                } else {
                    writeln!(out, "  {label} {field}").into_diagnostic()?;
                }
            }
        } else if let Some(items) = value[section].as_array() {
            for item in items {
                writeln!(out, "  {item}").into_diagnostic()?;
            }
        }
    }
    Ok(())
}
