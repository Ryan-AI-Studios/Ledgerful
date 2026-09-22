//! Canonical recovery manifest. The digest is not the 0416 `sourceDigest`.

use crate::commands::ledger_diagnose::classify::{self, Diagnosis};
use crate::ledger::crypto::nfc_normalize;
use crate::ledger::types::{ChainHead, LedgerEntry};
use miette::{Result, miette};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub const MANIFEST_DOMAIN: &str = "ledger-recovery-manifest-v1";
pub const HISTORICAL_CONTINUITY: &str = "notEstablished";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdoptedRow {
    pub tx_id: String,
    pub row_digest: String,
    pub committed_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryManifest {
    pub repo_identity: String,
    pub stored_head: String,
    pub manifest_digest: String,
    pub adopted: Vec<AdoptedRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManifestFile {
    kind: String,
    manifest_version: u32,
    repo_identity: String,
    stored_head: String,
    manifest_digest: String,
    adopted: Vec<ManifestAdopted>,
    provenance_limits: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManifestAdopted {
    tx_id: String,
    row_digest: String,
    committed_at: String,
}

pub fn canonical_payload(repo_identity: &str, stored_head: &str, adopted: &[AdoptedRow]) -> String {
    let mut lines = Vec::with_capacity(adopted.len() + 4);
    lines.push(MANIFEST_DOMAIN.to_string());
    lines.push(format!("repo:{}", nfc_normalize(repo_identity)));
    let head = if stored_head.is_empty() {
        "-".to_string()
    } else {
        nfc_normalize(stored_head)
    };
    lines.push(format!("storedHead:{head}"));
    lines.push("adopted:".to_string());
    let mut rows = adopted.to_vec();
    rows.sort_by(|a, b| a.tx_id.cmp(&b.tx_id));
    for row in rows {
        lines.push(format!(
            "{}\t{}\t{}",
            row.tx_id,
            row.row_digest,
            nfc_normalize(&row.committed_at)
        ));
    }
    lines.join("\n")
}

pub fn digest_of(payload: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(payload.as_bytes());
    hex::encode(hasher.finalize())
}

pub fn build_manifest(
    repo_identity: &str,
    stored_head: &str,
    adopted: Vec<AdoptedRow>,
) -> Result<RecoveryManifest> {
    let mut seen = BTreeMap::<String, ()>::new();
    for row in &adopted {
        if seen.insert(row.tx_id.clone(), ()).is_some() {
            return Err(miette!("duplicate adopted tx id {}", row.tx_id));
        }
    }
    let payload = canonical_payload(repo_identity, stored_head, &adopted);
    Ok(RecoveryManifest {
        repo_identity: repo_identity.to_string(),
        stored_head: stored_head.to_string(),
        manifest_digest: digest_of(&payload),
        adopted,
    })
}

pub fn eligible_rows(diagnosis: &Diagnosis) -> Result<Vec<AdoptedRow>> {
    if diagnosis.head_disagreement {
        return Err(miette!(
            "adoption refused: stored chain head is missing or disagrees with the signed segment"
        ));
    }
    if diagnosis.fork_count > 0 {
        return Err(miette!(
            "adoption refused: fork count={}",
            diagnosis.fork_count
        ));
    }
    if diagnosis.orphan_count > 0 {
        return Err(miette!(
            "adoption refused: orphan count={}",
            diagnosis.orphan_count
        ));
    }
    if diagnosis.cycle_count > 0 {
        return Err(miette!(
            "adoption refused: cycle count={}",
            diagnosis.cycle_count
        ));
    }
    if diagnosis.duplicate_hash_count > 0 {
        return Err(miette!(
            "adoption refused: duplicate hash count={}",
            diagnosis.duplicate_hash_count
        ));
    }
    if diagnosis.invalid_signature_count > 0 {
        return Err(miette!(
            "adoption refused: invalid or unsigned signature count={}",
            diagnosis.invalid_signature_count
        ));
    }
    if diagnosis.candidate_tx_ids.is_empty() {
        return Err(miette!("adoption refused: no eligible extra-genesis rows"));
    }
    let mut adopted = Vec::with_capacity(diagnosis.candidate_tx_ids.len());
    for tx_id in &diagnosis.candidate_tx_ids {
        let row_digest = diagnosis
            .row_digests
            .get(tx_id)
            .ok_or_else(|| miette!("missing row digest for {tx_id}"))?
            .clone();
        let committed_at = diagnosis
            .anomalies
            .iter()
            .find(|row| row.tx_id == *tx_id)
            .map(|row| row.committed_at.clone())
            .ok_or_else(|| miette!("missing committedAt for {tx_id}"))?;
        adopted.push(AdoptedRow {
            tx_id: tx_id.clone(),
            row_digest,
            committed_at,
        });
    }
    Ok(adopted)
}

pub fn diagnose(entries: &[LedgerEntry], head: Option<&ChainHead>) -> Diagnosis {
    classify::classify(entries, head)
}

pub fn to_json(manifest: &RecoveryManifest) -> Result<String> {
    let file = ManifestFile {
        kind: "ledgerRecoveryManifest".to_string(),
        manifest_version: 1,
        repo_identity: manifest.repo_identity.clone(),
        stored_head: manifest.stored_head.clone(),
        manifest_digest: manifest.manifest_digest.clone(),
        adopted: manifest
            .adopted
            .iter()
            .map(|row| ManifestAdopted {
                tx_id: row.tx_id.clone(),
                row_digest: row.row_digest.clone(),
                committed_at: row.committed_at.clone(),
            })
            .collect(),
        provenance_limits: classify::PROVENANCE_LIMITS.to_string(),
    };
    serde_json::to_string_pretty(&file).map_err(|e| miette!("manifest json: {e}"))
}

pub fn from_json(body: &str) -> Result<RecoveryManifest> {
    let file: ManifestFile = serde_json::from_str(body)
        .map_err(|e| miette!("recovery manifest is not valid JSON: {e}"))?;
    if file.kind != "ledgerRecoveryManifest" || file.manifest_version != 1 {
        return Err(miette!(
            "recovery manifest kind or manifestVersion is not ledgerRecoveryManifest 1"
        ));
    }
    if file.repo_identity.chars().nth(1) == Some(':') {
        return Err(miette!(
            "recovery manifest repoIdentity must be an origin URL, not a local path"
        ));
    }
    let adopted = file
        .adopted
        .into_iter()
        .map(|row| AdoptedRow {
            tx_id: row.tx_id,
            row_digest: row.row_digest,
            committed_at: row.committed_at,
        })
        .collect();
    let rebuilt = build_manifest(&file.repo_identity, &file.stored_head, adopted)?;
    if rebuilt.manifest_digest != file.manifest_digest {
        return Err(miette!(
            "recovery manifest digest does not match its adopted rows"
        ));
    }
    Ok(rebuilt)
}

pub fn attestation_reason(
    manifest_digest: &str,
    adopted_count: usize,
    original_head: &str,
) -> String {
    format!(
        "{{\"adoptedCount\":{adopted_count},\"historicalContinuity\":\"{HISTORICAL_CONTINUITY}\",\"kind\":\"ledgerRecoveryAttestation\",\"manifestDigest\":\"{manifest_digest}\",\"originalHead\":\"{original_head}\"}}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_digest_is_stable_and_sorted() {
        let rows = vec![
            AdoptedRow {
                tx_id: "b".into(),
                row_digest: "bb".into(),
                committed_at: "t2".into(),
            },
            AdoptedRow {
                tx_id: "a".into(),
                row_digest: "aa".into(),
                committed_at: "t1".into(),
            },
        ];
        let manifest = build_manifest("https://example.invalid/repo.git", "head", rows).unwrap();
        let again = canonical_payload(
            "https://example.invalid/repo.git",
            "head",
            &manifest.adopted,
        );
        assert!(again.contains("\na\taa\tt1\n") || again.ends_with("a\taa\tt1"));
        assert!(again.find("a\taa\tt1").unwrap() < again.find("b\tbb\tt2").unwrap());
        assert_eq!(manifest.manifest_digest, digest_of(&again));
        assert!(!manifest.manifest_digest.is_empty());
    }

    #[test]
    fn adopt_rejects_duplicate_row() {
        let rows = vec![
            AdoptedRow {
                tx_id: "a".into(),
                row_digest: "aa".into(),
                committed_at: "t".into(),
            },
            AdoptedRow {
                tx_id: "a".into(),
                row_digest: "bb".into(),
                committed_at: "t".into(),
            },
        ];
        let err = build_manifest("https://example.invalid/repo.git", "head", rows).unwrap_err();
        assert!(format!("{err}").contains("duplicate"), "{err}");
    }
}
