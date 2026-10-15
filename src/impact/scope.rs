//! Change-scoped hotspot rows and the prospective `scopeNote` (0503).

use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::path::Path;

use crate::impact::packet::{Hotspot, ImpactPacket};
use crate::impact::path_class::normalize_path;
use crate::index::surface_freshness::PROSPECTIVE_SCOPE_NOTE;

/// Set `scopeNote` only for a prospective packet whose `treeClean` is false.
pub fn apply_prospective_scope_note(packet: &mut ImpactPacket) {
    if packet.analysis_mode == "prospective" && !packet.tree_clean {
        packet.scope_note = Some(PROSPECTIVE_SCOPE_NOTE.to_string());
    } else {
        packet.scope_note = None;
    }
}

/// Human line for a set `scopeNote`. Callers print it unchanged.
pub fn scope_note_line(note: Option<&str>) -> Option<String> {
    note.map(|text| format!("  {text}"))
}

/// Normalized seed paths: every change path and `old_path`.
pub fn seed_paths(packet: &ImpactPacket) -> BTreeSet<String> {
    let mut seeds = BTreeSet::new();
    for change in &packet.changes {
        insert_path(&mut seeds, &change.path);
        if let Some(old) = &change.old_path {
            insert_path(&mut seeds, old);
        }
    }
    seeds
}

/// Normalized consequence paths with every seed removed.
pub fn affected_paths(packet: &ImpactPacket, seeds: &BTreeSet<String>) -> BTreeSet<String> {
    let mut affected = BTreeSet::new();
    for pair in &packet.temporal_couplings {
        consider(&mut affected, seeds, &pair.file_a);
        consider(&mut affected, seeds, &pair.file_b);
    }
    if let Some(blast) = &packet.blast_radius {
        for edge in &blast.edges {
            consider_str(&mut affected, seeds, &edge.from_file);
            consider_str(&mut affected, seeds, &edge.to_file);
        }
        for path in &blast.must_touch_files {
            consider_str(&mut affected, seeds, path);
        }
    }
    for coupling in &packet.structural_couplings {
        consider(&mut affected, seeds, &coupling.caller_file_path);
    }
    for coverage in &packet.test_coverage {
        for test in &coverage.covering_tests {
            consider_str(&mut affected, seeds, &test.test_file);
        }
    }
    if let Some(flows) = &packet.affected_flows {
        for flow in &flows.flows {
            if let Some(file) = &flow.handler_file {
                consider_str(&mut affected, seeds, file);
            }
        }
    }
    affected
}

/// Keep seed ∪ affected rows. `limit` is the emit cap from config, applied
/// after the shared scorer has already returned an untruncated candidate list.
pub fn retain_scoped_hotspots(
    hotspots: Vec<Hotspot>,
    packet: &ImpactPacket,
    limit: usize,
) -> Vec<Hotspot> {
    let seeds = seed_paths(packet);
    let affected = affected_paths(packet, &seeds);
    let mut best: Vec<(String, Hotspot)> = Vec::new();
    for hotspot in hotspots {
        let key = normalize_path(&hotspot.path.to_string_lossy());
        if key.is_empty() || (!seeds.contains(&key) && !affected.contains(&key)) {
            continue;
        }
        if let Some(existing) = best.iter_mut().find(|(path, _)| path == &key) {
            if prefer_hotspot(&hotspot, &existing.1) {
                existing.1 = hotspot;
            }
        } else {
            best.push((key, hotspot));
        }
    }

    let mut seed_rows = Vec::new();
    let mut affected_rows = Vec::new();
    for (key, hotspot) in best {
        if seeds.contains(&key) {
            seed_rows.push(hotspot);
        } else {
            affected_rows.push(hotspot);
        }
    }
    sort_rows(&mut seed_rows);
    sort_rows(&mut affected_rows);

    if seed_rows.len() >= limit {
        return seed_rows;
    }
    let room = limit - seed_rows.len();
    seed_rows.extend(affected_rows.into_iter().take(room));
    seed_rows
}

fn insert_path(set: &mut BTreeSet<String>, path: &Path) {
    let key = normalize_path(&path.to_string_lossy());
    if !key.is_empty() {
        set.insert(key);
    }
}

fn consider(set: &mut BTreeSet<String>, seeds: &BTreeSet<String>, path: &Path) {
    consider_str(set, seeds, &path.to_string_lossy());
}

fn consider_str(set: &mut BTreeSet<String>, seeds: &BTreeSet<String>, path: &str) {
    let key = normalize_path(path);
    if !key.is_empty() && !seeds.contains(&key) {
        set.insert(key);
    }
}

fn sort_rows(rows: &mut [Hotspot]) {
    rows.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| {
                normalize_path(&a.path.to_string_lossy())
                    .cmp(&normalize_path(&b.path.to_string_lossy()))
            })
    });
}

/// True when `candidate` should replace `current` (higher score, then path).
fn prefer_hotspot(candidate: &Hotspot, current: &Hotspot) -> bool {
    match candidate
        .score
        .partial_cmp(&current.score)
        .unwrap_or(Ordering::Equal)
    {
        Ordering::Greater => true,
        Ordering::Less => false,
        Ordering::Equal => candidate.path < current.path,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::impact::budget::{CompletenessFilter, HotspotProvenance, HotspotProvenanceSource};
    use crate::impact::packet::{BlastRadius, ChangedFile, FileAnalysisStatus, TemporalCoupling};
    use std::path::PathBuf;

    fn hotspot(path: &str, score: f32) -> Hotspot {
        Hotspot {
            path: PathBuf::from(path),
            score,
            display_score: score,
            complexity: 1,
            frequency: 1.0,
            centrality: None,
        }
    }

    fn change(path: &str, old: Option<&str>) -> ChangedFile {
        ChangedFile {
            path: PathBuf::from(path),
            status: "Modified".to_string(),
            old_path: old.map(PathBuf::from),
            is_staged: false,
            symbols: None,
            imports: None,
            runtime_usage: None,
            analysis_status: FileAnalysisStatus::default(),
            analysis_warnings: Vec::new(),
            api_routes: Vec::new(),
            data_models: Vec::new(),
            ci_gates: Vec::new(),
        }
    }

    fn scoped_packet() -> ImpactPacket {
        let mut packet = ImpactPacket {
            changes: vec![change("README.md", Some("old/README.md"))],
            temporal_couplings: vec![TemporalCoupling {
                file_a: PathBuf::from("README.md"),
                file_b: PathBuf::from("src/temporal.rs"),
                score: 0.5,
            }],
            temporal_couplings_audit: vec![TemporalCoupling {
                file_a: PathBuf::from("README.md"),
                file_b: PathBuf::from("src/audit.rs"),
                score: 0.2,
            }],
            discarded_tree_paths: vec![PathBuf::from("src/tree")],
            ..ImpactPacket::default()
        };
        packet.blast_radius = Some(BlastRadius {
            must_touch_files: vec!["README.md".to_string(), "src/partner.rs".to_string()],
            ..BlastRadius::default()
        });
        packet
    }

    fn candidates() -> Vec<Hotspot> {
        vec![
            hotspot("src/unrelated.rs", 0.99),
            hotspot("README.md", 0.1),
            hotspot("README.md", 0.1),
            hotspot(r"src\temporal.rs", 0.4),
            hotspot("src/temporal.rs", 0.2),
            hotspot("old/README.md", 0.3),
            hotspot("src/partner.rs", 0.8),
            hotspot("src/audit.rs", 0.7),
            hotspot("src/tree", 0.6),
        ]
    }

    fn paths(rows: &[Hotspot]) -> Vec<String> {
        rows.iter()
            .map(|row| normalize_path(&row.path.to_string_lossy()))
            .collect()
    }

    #[test]
    fn retain_keeps_seeds_outside_a_global_top_1_and_drops_the_rest() {
        let packet = scoped_packet();
        let kept = retain_scoped_hotspots(candidates(), &packet, 1);
        let got = paths(&kept);
        assert!(got.contains(&"README.md".to_string()));
        assert!(got.contains(&"old/README.md".to_string()));
        assert!(!got.contains(&"src/unrelated.rs".to_string()));
        assert!(!got.contains(&"src/temporal.rs".to_string()));
        assert!(!got.contains(&"src/partner.rs".to_string()));
        assert!(!got.contains(&"src/audit.rs".to_string()));
        assert!(!got.contains(&"src/tree".to_string()));
        assert_eq!(got.len(), 2);
        assert!(got.len() > 1);
    }

    #[test]
    fn retain_keeps_partners_dedupes_and_omits_audit_paths() {
        let packet = scoped_packet();
        let kept = retain_scoped_hotspots(candidates(), &packet, 10);
        let got = paths(&kept);
        assert_eq!(
            got.iter()
                .filter(|path| path.as_str() == "README.md")
                .count(),
            1
        );
        assert_eq!(
            got.iter()
                .filter(|path| path.as_str() == "src/temporal.rs")
                .count(),
            1
        );
        assert!(got.contains(&"src/partner.rs".to_string()));
        assert!(got.contains(&"old/README.md".to_string()));
        assert!(!got.contains(&"src/unrelated.rs".to_string()));
        assert!(!got.contains(&"src/audit.rs".to_string()));
        assert!(!got.contains(&"src/tree".to_string()));
        let temporal = kept
            .iter()
            .find(|row| normalize_path(&row.path.to_string_lossy()) == "src/temporal.rs")
            .expect("temporal partner");
        assert!((temporal.score - 0.4).abs() < f32::EPSILON);
    }

    #[test]
    fn prospective_note_is_exact_and_absent_on_a_dirty_tree() {
        let mut prospective = ImpactPacket {
            analysis_mode: "prospective".to_string(),
            tree_clean: false,
            ..ImpactPacket::default()
        };
        apply_prospective_scope_note(&mut prospective);
        assert_eq!(
            prospective.scope_note.as_deref(),
            Some(PROSPECTIVE_SCOPE_NOTE)
        );
        let line = scope_note_line(prospective.scope_note.as_deref()).expect("line");
        assert_eq!(line, format!("  {PROSPECTIVE_SCOPE_NOTE}"));

        let mut dirty = ImpactPacket {
            analysis_mode: "working_tree".to_string(),
            tree_clean: false,
            ..ImpactPacket::default()
        };
        apply_prospective_scope_note(&mut dirty);
        assert!(dirty.scope_note.is_none());
        assert!(scope_note_line(None).is_none());

        let mut clean_prospective = ImpactPacket {
            analysis_mode: "prospective".to_string(),
            tree_clean: true,
            ..ImpactPacket::default()
        };
        apply_prospective_scope_note(&mut clean_prospective);
        assert!(clean_prospective.scope_note.is_none());
    }

    #[test]
    fn scoped_fields_encode_twice_to_the_same_bytes() {
        let mut packet = ImpactPacket {
            analysis_mode: "prospective".to_string(),
            tree_clean: false,
            hotspot_provenance: Some(HotspotProvenance {
                source: HotspotProvenanceSource::Live,
                commits_requested: Some(500),
                limit: Some(10),
                filter: Some(CompletenessFilter::Unfiltered),
                head: Some("abc".to_string()),
                ..HotspotProvenance::default()
            }),
            ..ImpactPacket::default()
        };
        apply_prospective_scope_note(&mut packet);
        let first = serde_json::to_vec(&serde_json::json!({
            "scopeNote": packet.scope_note,
            "hotspotProvenance": packet.hotspot_provenance,
        }))
        .expect("encode");
        let second = serde_json::to_vec(&serde_json::json!({
            "scopeNote": packet.scope_note,
            "hotspotProvenance": packet.hotspot_provenance,
        }))
        .expect("encode");
        assert_eq!(first, second);
        assert!(packet.scope_note.is_some());
        assert_eq!(
            packet.hotspot_provenance.as_ref().and_then(|row| row.limit),
            Some(10)
        );
    }
}
