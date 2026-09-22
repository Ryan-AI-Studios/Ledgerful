//! Read-only `ledger diagnose` (0416).

mod classify;

use std::path::Path;

use classify::{Diagnosis, PROVENANCE_LIMITS, classify};
use miette::{Result, miette};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::commands::helpers::get_layout;
use crate::ledger::db::LedgerDb;
use crate::state::storage::StorageManager;

#[derive(Debug, Clone)]
pub struct DiagnoseOpts {
    pub json: bool,
    pub limit: u64,
    pub offset: u64,
    pub output: Option<std::path::PathBuf>,
    pub manifest: bool,
}

pub fn execute_ledger_diagnose(opts: DiagnoseOpts) -> Result<()> {
    let layout = get_layout()?;
    diagnose_layout(&layout, &opts)
}

pub fn diagnose_layout(layout: &crate::state::layout::Layout, opts: &DiagnoseOpts) -> Result<()> {
    if opts.limit == 0 || opts.limit > 1000 {
        return Err(miette!("--limit must be from 1 through 1000"));
    }
    if let Some(path) = &opts.output {
        refuse_ledgerful_path(path)?;
        if path.exists() {
            return Err(miette!("output file already exists: {}", path.display()));
        }
    }

    let storage = StorageManager::open_read_only_sqlite_only(layout)?;
    let db = LedgerDb::new(storage.get_connection());
    let entries = db
        .get_all_committed_ledger_entries()
        .map_err(|e| miette!("Failed to read ledger entries: {e}"))?;
    let head = db
        .get_chain_head()
        .map_err(|e| miette!("Failed to read chain head: {e}"))?;
    let diagnosis = classify(&entries, head.as_ref());

    if opts.manifest {
        let path = opts
            .output
            .as_ref()
            .ok_or_else(|| miette!("--manifest requires --output PATH for the proposal file"))?;
        let body = serde_json::to_string_pretty(&manifest_json(layout, &diagnosis))
            .map_err(|e| miette!("manifest json: {e}"))?;
        write_new(path, &body)?;
        if !opts.json {
            println!("Wrote recovery manifest proposal to {}", path.display());
            return Ok(());
        }
    } else if let Some(path) = &opts.output {
        let full = diagnosis_json(&diagnosis, 0, diagnosis.anomalies.len() as u64);
        let body = serde_json::to_string_pretty(&full).map_err(|e| miette!("json: {e}"))?;
        write_new(path, &body)?;
    }

    let page = diagnosis_json(&diagnosis, opts.offset, opts.limit);
    if opts.json {
        let body = serde_json::to_string_pretty(&page).map_err(|e| miette!("json: {e}"))?;
        println!("{body}");
    } else if !opts.manifest {
        println!(
            "linkedEntries={} extraGenesisCount={} orphanCount={} forkCount={} cycleCount={} duplicateHashCount={} invalidSignatureCount={} headDisagreement={}",
            diagnosis.linked_entries,
            diagnosis.extra_genesis_count,
            diagnosis.orphan_count,
            diagnosis.fork_count,
            diagnosis.cycle_count,
            diagnosis.duplicate_hash_count,
            diagnosis.invalid_signature_count,
            diagnosis.head_disagreement
        );
        println!(
            "{} anomalies. Pass --json to page them.",
            diagnosis.anomalies.len()
        );
    }
    Ok(())
}

fn refuse_ledgerful_path(path: &Path) -> Result<()> {
    if path.components().any(|c| c.as_os_str() == ".ledgerful") {
        return Err(miette!(
            "refusing to write diagnosis output under .ledgerful"
        ));
    }
    Ok(())
}

fn write_new(path: &Path, body: &str) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|e| miette!("create {}: {e}", path.display()))?;
    }
    std::fs::write(path, body).map_err(|e| miette!("write {}: {e}", path.display()))?;
    Ok(())
}

fn diagnosis_json(diagnosis: &Diagnosis, offset: u64, limit: u64) -> DiagnoseJson {
    let total = diagnosis.anomalies.len() as u64;
    let start = usize::try_from(offset).unwrap_or(usize::MAX);
    let len = usize::try_from(limit).unwrap_or(0);
    let page: Vec<&classify::AnomalyRow> =
        diagnosis.anomalies.iter().skip(start).take(len).collect();
    DiagnoseJson {
        schema_version: 1,
        kind: "ledgerChainDiagnosis",
        valid_local: diagnosis.valid_local,
        federated_skip: diagnosis.federated_skip,
        linked_entries: diagnosis.linked_entries,
        extra_genesis_count: diagnosis.extra_genesis_count,
        orphan_count: diagnosis.orphan_count,
        fork_count: diagnosis.fork_count,
        cycle_count: diagnosis.cycle_count,
        duplicate_hash_count: diagnosis.duplicate_hash_count,
        invalid_signature_count: diagnosis.invalid_signature_count,
        head_disagreement: diagnosis.head_disagreement,
        page: PageJson {
            limit,
            offset,
            returned: page.len() as u64,
            total_anomalies: total,
        },
        anomalies: page
            .into_iter()
            .map(|row| AnomalyJson {
                tx_id: row.tx_id.clone(),
                prev_hash: row.prev_hash.clone(),
                computed_hash: row.computed_hash.clone(),
                sig_version: row.sig_version,
                origin: row.origin.clone(),
                committed_at: row.committed_at.clone(),
                signature_status: row.signature_status,
                in_signed_segment: row.in_signed_segment,
                anomaly_class: row.anomaly_class,
            })
            .collect(),
    }
}

fn manifest_json(layout: &crate::state::layout::Layout, diagnosis: &Diagnosis) -> ManifestJson {
    let mut hasher = Sha256::new();
    hasher.update(diagnosis.candidate_tx_ids.join("\n").as_bytes());
    ManifestJson {
        schema_version: 1,
        kind: "ledgerRecoveryManifestProposal",
        manifest_version: 1,
        repo_identity: layout.root.to_string(),
        source_digest: hex::encode(hasher.finalize()),
        stored_head: diagnosis.stored_head.clone(),
        candidate_tx_ids: diagnosis.candidate_tx_ids.clone(),
        row_digests: diagnosis.row_digests.clone(),
        exclusions: diagnosis.exclusions.clone(),
        provenance_limits: PROVENANCE_LIMITS,
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DiagnoseJson {
    schema_version: u32,
    kind: &'static str,
    valid_local: usize,
    federated_skip: usize,
    linked_entries: usize,
    extra_genesis_count: usize,
    orphan_count: usize,
    fork_count: usize,
    cycle_count: usize,
    duplicate_hash_count: usize,
    invalid_signature_count: usize,
    head_disagreement: bool,
    page: PageJson,
    anomalies: Vec<AnomalyJson>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PageJson {
    limit: u64,
    offset: u64,
    returned: u64,
    total_anomalies: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AnomalyJson {
    tx_id: String,
    prev_hash: Option<String>,
    computed_hash: String,
    sig_version: u32,
    origin: String,
    committed_at: String,
    signature_status: &'static str,
    in_signed_segment: bool,
    anomaly_class: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ManifestJson {
    schema_version: u32,
    kind: &'static str,
    manifest_version: u32,
    repo_identity: String,
    source_digest: String,
    stored_head: Option<String>,
    candidate_tx_ids: Vec<String>,
    row_digests: std::collections::BTreeMap<String, String>,
    exclusions: Vec<String>,
    provenance_limits: &'static str,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::types::{Category, ChangeType, EntryType, LedgerEntry};

    fn bare(tx_id: &str) -> LedgerEntry {
        LedgerEntry {
            id: 0,
            tx_id: tx_id.to_string(),
            category: Category::Feature,
            entry_type: EntryType::Implementation,
            entity: "src/a.rs".into(),
            entity_normalized: "src/a.rs".into(),
            change_type: ChangeType::Modify,
            summary: "s".into(),
            reason: "r".into(),
            is_breaking: false,
            committed_at: "2020-01-01T00:00:00Z".into(),
            verification_status: None,
            verification_basis: None,
            outcome_notes: None,
            origin: "LOCAL".into(),
            trace_id: None,
            signature: None,
            public_key: None,
            risk: None,
            related_tickets: None,
            author: "t".into(),
            observed: None,
            prev_hash: None,
            sig_version: 1,
        }
    }

    #[test]
    fn diagnosis_page_totals_match_the_full_set() {
        let mut entries = vec![bare("tx-primary")];
        for i in 0..25 {
            entries.push(bare(&format!("tx-extra-{i:02}")));
        }
        let diagnosis = classify::classify(&entries, None);
        assert!(diagnosis.anomalies.len() > 20);
        let page = diagnosis_json(&diagnosis, 10, 10);
        assert_eq!(page.page.offset, 10);
        assert_eq!(page.page.limit, 10);
        assert_eq!(page.page.returned, 10);
        assert_eq!(page.page.total_anomalies, diagnosis.anomalies.len() as u64);
        assert!(page.page.total_anomalies > page.page.returned);
        assert_eq!(page.extra_genesis_count, diagnosis.extra_genesis_count);
        let ids: Vec<_> = page
            .anomalies
            .iter()
            .map(|row| row.tx_id.as_str())
            .collect();
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(ids, sorted);
        let next = diagnosis_json(&diagnosis, 20, 10);
        let overlap = page
            .anomalies
            .iter()
            .any(|row| next.anomalies.iter().any(|other| other.tx_id == row.tx_id));
        assert!(!overlap);
    }

    #[test]
    fn diagnose_rejects_limit_and_ledgerful_path_before_open() {
        let dir = tempfile::tempdir().unwrap();
        let root = camino::Utf8Path::from_path(dir.path())
            .unwrap()
            .to_path_buf();
        let layout = crate::state::layout::Layout::new(&root);
        let err = diagnose_layout(
            &layout,
            &DiagnoseOpts {
                json: true,
                limit: 0,
                offset: 0,
                output: None,
                manifest: false,
            },
        )
        .unwrap_err();
        assert!(format!("{err}").contains("1 through 1000"), "{err}");
        let err = diagnose_layout(
            &layout,
            &DiagnoseOpts {
                json: true,
                limit: 1001,
                offset: 0,
                output: None,
                manifest: false,
            },
        )
        .unwrap_err();
        assert!(format!("{err}").contains("1 through 1000"), "{err}");
        let out = dir.path().join(".ledgerful").join("diag.json");
        let err = diagnose_layout(
            &layout,
            &DiagnoseOpts {
                json: false,
                limit: 100,
                offset: 0,
                output: Some(out),
                manifest: false,
            },
        )
        .unwrap_err();
        assert!(format!("{err}").contains(".ledgerful"), "{err}");
    }
}
