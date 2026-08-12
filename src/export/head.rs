//! Thin chain-head checkpoint export (`ledgerful export head`).

use crate::ledger::types::ChainHead;
use crate::state::layout::Layout;
use miette::Result;
use std::path::PathBuf;

/// Where `export head` should write the serialized chain head.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeadExportDest {
    /// Write pretty JSON to a filesystem path (path-safety + force apply).
    File { path: PathBuf, force: bool },
    /// Write exact pretty JSON bytes to stdout (no SUCCESS banner).
    Stdout,
}

/// Resolve CLI flags to a destination.
///
/// Path `-` always means stdout (fixes the footgun where `-` was a legal file
/// name). `--stdout` with a non-dash `--out` is an ambiguous dest and errors
/// (stricter than bridge export, which silently ignores `--out`).
pub fn resolve_head_export_dest(
    out: Option<PathBuf>,
    stdout: bool,
    force: bool,
) -> Result<HeadExportDest> {
    let out_is_dash = out.as_ref().is_some_and(|p| p.as_os_str() == "-");
    if stdout && out.as_ref().is_some_and(|p| p.as_os_str() != "-") {
        return Err(miette::miette!(
            "export head: --stdout cannot be combined with --out <path>; \
             use --stdout alone, or -o - for stdout, or --out <file> for a file"
        ));
    }
    if stdout || out_is_dash {
        return Ok(HeadExportDest::Stdout);
    }
    let path = out.unwrap_or_else(|| PathBuf::from("./ledgerful-chain-head.json"));
    Ok(HeadExportDest::File { path, force })
}

/// Read the live chain head and enforce unsigned-export policy.
///
/// - Missing head → hard error
/// - Unsigned + `require_signing` → hard refuse
/// - Unsigned + `!require_signing` → warn + return head
/// - Signed → return head
pub fn prepare_chain_head_export(layout: &Layout) -> Result<ChainHead> {
    let mut storage = crate::state::storage::StorageManager::init_with_layout(layout)?;
    let db = crate::ledger::db::LedgerDb::new(storage.get_connection_mut());
    let head = db
        .get_chain_head()
        .map_err(|e| miette::miette!("Failed to read chain head: {e}"))?
        .ok_or_else(|| {
            miette::miette!(
                "No chain head to export. Commit at least one ledger entry first, or use a ledger that has a chain_head row."
            )
        })?;

    let sig = head.head_signature.as_deref().unwrap_or("");
    let pub_key = head.head_public_key.as_deref().unwrap_or("");
    let signed = !sig.is_empty() && !pub_key.is_empty();

    // Only evaluate require_signing for unsigned heads. Fail closed on config
    // load errors so a malformed config cannot silently allow an unsigned export
    // via Config::default() (require_signing=false).
    if !signed {
        let config = crate::config::load::load_config(layout).map_err(|e| {
            miette::miette!(
                "Refusing to export an unsigned chain head: cannot load config to evaluate \
                 require_signing ({e}). Fix .ledgerful/config.toml or ensure the chain head is signed."
            )
        })?;
        if config.intent.require_signing {
            return Err(miette::miette!(
                "Refusing to export an unsigned chain head while intent.require_signing is true. \
                 Ensure keys exist under .ledgerful/keys and new commits sign the head \
                 (or re-sign / repair signing), then retry `export head`."
            ));
        }
        tracing::warn!(
            target: "cli_summary",
            "Exported chain head is unsigned (require_signing=false); retain as an honest unsigned checkpoint."
        );
    }

    Ok(head)
}

/// Serialize a chain head to pretty JSON bytes (same shape as SOC2 zip entry).
///
/// Uses `to_vec_pretty` (not compact `to_vec`). File path and stdout write the
/// same bytes; the SOC2 zip `chain_head.json` entry may be compact — field
/// shape matches, whitespace may differ. `verify --against-export` parses both.
pub fn serialize_chain_head(head: &ChainHead) -> Result<Vec<u8>> {
    serde_json::to_vec_pretty(head)
        .map_err(|e| miette::miette!("Failed to serialize chain head: {e}"))
}

#[cfg(test)]
#[allow(non_snake_case)] // project test naming: feature__condition__expected
mod resolve_dest_tests {
    use super::{HeadExportDest, resolve_head_export_dest};
    use std::path::PathBuf;

    #[test]
    fn resolve_head_export_dest__default__file_default_path() {
        let dest = resolve_head_export_dest(None, false, false).expect("default ok");
        assert_eq!(
            dest,
            HeadExportDest::File {
                path: PathBuf::from("./ledgerful-chain-head.json"),
                force: false,
            }
        );
    }

    #[test]
    fn resolve_head_export_dest__explicit_path__file() {
        let dest =
            resolve_head_export_dest(Some(PathBuf::from("x.json")), false, true).expect("path ok");
        assert_eq!(
            dest,
            HeadExportDest::File {
                path: PathBuf::from("x.json"),
                force: true,
            }
        );
    }

    #[test]
    fn resolve_head_export_dest__stdout_flag__stdout() {
        let dest = resolve_head_export_dest(None, true, false).expect("stdout ok");
        assert_eq!(dest, HeadExportDest::Stdout);
    }

    #[test]
    fn resolve_head_export_dest__out_dash__stdout() {
        let dest =
            resolve_head_export_dest(Some(PathBuf::from("-")), false, false).expect("dash ok");
        assert_eq!(dest, HeadExportDest::Stdout);
    }

    #[test]
    fn resolve_head_export_dest__stdout_plus_dash__stdout() {
        let dest =
            resolve_head_export_dest(Some(PathBuf::from("-")), true, true).expect("redundant ok");
        assert_eq!(dest, HeadExportDest::Stdout);
    }

    #[test]
    fn resolve_head_export_dest__stdout_plus_force__stdout_ignores_force() {
        let dest = resolve_head_export_dest(None, true, true).expect("force ignored");
        assert_eq!(dest, HeadExportDest::Stdout);
    }

    #[test]
    fn resolve_head_export_dest__stdout_plus_path__err() {
        let err = resolve_head_export_dest(Some(PathBuf::from("some.json")), true, false)
            .expect_err("ambiguous dest");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("--stdout") && msg.contains("--out"),
            "error should mention both flags, got: {msg}"
        );
    }
}
