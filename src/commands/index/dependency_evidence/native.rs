use super::discovery::{Discovery, discover};
use super::files::{ReadFailure, path_is_missing, read_stamped};
use rusqlite::Connection;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

const ENTRY_LIMIT: usize = 50_000;
const FILE_LIMIT: u64 = 8 * 1024 * 1024;
const CONTENT_LIMIT: u64 = 256 * 1024 * 1024;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct NativeEvidence {
    pub(super) state: &'static str,
    pub(super) assessed_head: Option<String>,
    pub(super) indexed_head: Option<String>,
    pub(super) reasons: BTreeSet<&'static str>,
}

type StoredFiles = BTreeMap<String, (Option<String>, bool)>;

enum StoredInventory {
    Complete(StoredFiles),
    Capped,
}

fn stored_sources(conn: &Connection, limit: usize) -> rusqlite::Result<StoredInventory> {
    let kind = super::language_sql("file_path", "other");
    let sql = format!(
        "SELECT
        CASE WHEN length(CAST(file_path AS BLOB)) BETWEEN 1 AND 4096 THEN file_path END,
        CASE WHEN length(CAST(content_hash AS BLOB)) = 64 THEN content_hash END,
        parse_status = 'OK'
        FROM project_files
        WHERE parse_status != 'DELETED' AND ({kind}) NOT IN ('other', 'markdown')
        LIMIT ?1"
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query([limit.saturating_add(1) as i64])?;
    let mut sources = BTreeMap::new();
    while let Some(row) = rows.next()? {
        let path: Option<String> = row.get(0)?;
        let Some(path) = path.filter(|_| sources.len() < limit) else {
            return Ok(StoredInventory::Capped);
        };
        let hash: Option<String> = row.get(1)?;
        let ok: bool = row.get(2)?;
        sources.insert(path, (hash, ok));
    }
    Ok(StoredInventory::Complete(sources))
}

fn clean_head(head: String) -> Option<String> {
    (matches!(head.len(), 40 | 64) && head.bytes().all(|c| c.is_ascii_hexdigit()))
        .then(|| head.to_ascii_lowercase())
}

fn live_head(root: &Path) -> Option<String> {
    let repo = crate::git::repo::open_repo(root).ok()?;
    crate::git::repo::get_head_info(&repo)
        .ok()?
        .0
        .and_then(clean_head)
}

pub(super) fn assess(
    root: &Path,
    transaction: Option<&Connection>,
    db_reason: Option<&'static str>,
) -> (NativeEvidence, bool) {
    assess_with_content_budget(root, transaction, db_reason, CONTENT_LIMIT, || {})
}

fn assess_with_content_budget(
    root: &Path,
    transaction: Option<&Connection>,
    db_reason: Option<&'static str>,
    mut content_budget: u64,
    before_boundary: impl FnOnce(),
) -> (NativeEvidence, bool) {
    let assessed_head = live_head(root);
    let indexed_head = transaction
        .and_then(crate::index::surface_freshness::read_indexed_head)
        .and_then(clean_head);
    let repo = crate::git::repo::open_repo(root).ok();
    let Discovery {
        sources,
        mut stamps,
        missing_inputs,
        other,
        mut reasons,
    } = discover(
        root,
        repo.as_ref(),
        ENTRY_LIMIT,
        FILE_LIMIT,
        &mut content_budget,
    );
    let discovery_complete = reasons.is_empty();
    reasons.extend(db_reason);
    let mut stale = false;
    match (&assessed_head, &indexed_head) {
        (Some(current), Some(indexed)) if current != indexed => {
            stale = true;
            reasons.insert("headMismatch");
        }
        (None, _) | (_, None) => {
            reasons.insert("headUnavailable");
        }
        _ => {}
    }
    if let Some(conn) = transaction.filter(|_| discovery_complete || !sources.is_empty()) {
        match stored_sources(conn, ENTRY_LIMIT) {
            Ok(StoredInventory::Complete(stored)) => {
                if sources.is_empty() && discovery_complete {
                    reasons.insert("emptySourceInventory");
                }
                if discovery_complete {
                    for path in stored.keys().filter(|path| !sources.contains_key(*path)) {
                        if path.contains([':', '\\'])
                            || path.split('/').any(|part| matches!(part, "" | "." | ".."))
                        {
                            reasons.insert("inventoryUncertain");
                            continue;
                        }
                        if path_is_missing(&root.join(path)) {
                            stale = true;
                            reasons.insert("deletedSource");
                        } else {
                            reasons.insert("inventoryUncertain");
                        }
                    }
                }
                let mut remaining = content_budget;
                for (relative, path) in &sources {
                    let Some((stored_hash, parsed)) = stored.get(relative) else {
                        if discovery_complete {
                            stale = true;
                            reasons.insert("unindexedSource");
                        }
                        continue;
                    };
                    if !parsed {
                        reasons.insert("failedParse");
                    }
                    let Some(stored_hash) = stored_hash else {
                        reasons.insert("missingContentHash");
                        continue;
                    };
                    if remaining == 0 {
                        reasons.insert("contentLimit");
                        continue;
                    }
                    let limit = FILE_LIMIT.min(remaining);
                    let (read, used) = read_stamped(path, limit, false, |input| {
                        let mut bytes = Vec::new();
                        input.read_to_end(&mut bytes)?;
                        let content = crate::util::fs::decode_bytes_with_encoding(&bytes)?;
                        Ok(blake3::hash(content.as_bytes()).to_hex())
                    });
                    remaining = remaining.saturating_sub(used);
                    match read {
                        Ok((digest, stamp)) => {
                            if digest.as_str() != stored_hash {
                                stale = true;
                                reasons.insert("changedSource");
                            }
                            stamps.push((path.to_owned(), stamp));
                        }
                        Err(ReadFailure::Capped) => {
                            reasons.insert(if limit == FILE_LIMIT {
                                "fileLimit"
                            } else {
                                "contentLimit"
                            });
                        }
                        Err(
                            ReadFailure::Missing | ReadFailure::Unreadable | ReadFailure::Spelling,
                        ) => {
                            reasons.insert("unreadableSource");
                        }
                        Err(ReadFailure::Unstable) => {
                            reasons.insert("unstableSource");
                        }
                    }
                }
            }
            Ok(StoredInventory::Capped) => {
                reasons.insert("storedInventoryLimit");
            }
            Err(_) => {
                reasons.insert("inventoryUnavailable");
            }
        }
    }
    before_boundary();
    let final_head = live_head(root);
    if final_head != assessed_head {
        reasons.insert("headChangedDuringAssessment");
    }
    if final_head
        .as_ref()
        .zip(indexed_head.as_ref())
        .is_some_and(|(live, indexed)| live != indexed)
    {
        stale = true;
        reasons.insert("headMismatch");
    }
    if stamps.iter().any(|(path, stamp)| !stamp.matches_path(path))
        || missing_inputs.iter().any(|path| !path_is_missing(path))
    {
        reasons.insert("inventoryUnstable");
    }

    let state = if stale {
        "stale"
    } else if reasons.is_empty() {
        "indexInputsMatch"
    } else {
        "unknown"
    };
    (
        NativeEvidence {
            state,
            assessed_head,
            indexed_head,
            reasons,
        },
        other,
    )
}
#[cfg(all(test, any(target_os = "linux", windows)))]
mod tests {
    use super::*;

    #[test]
    fn dependency_evidence_drops_provisional_other_and_sources_on_entry_limit() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("source.rs"), "fn a() {}").unwrap();
        std::fs::write(root.path().join("other.unknown_extension"), "unopened").unwrap();
        std::fs::write(root.path().join(".hidden"), "still counted").unwrap();
        let complete = discover(root.path(), None, 4, FILE_LIMIT, &mut CONTENT_LIMIT.clone());
        assert_eq!(
            complete
                .sources
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["source.rs"]
        );
        assert!(complete.other);
        assert!(complete.reasons.is_empty());
        let partial = discover(root.path(), None, 3, FILE_LIMIT, &mut CONTENT_LIMIT.clone());
        assert_eq!(partial.reasons, BTreeSet::from(["entryLimit"]));
        assert!(partial.sources.is_empty());
        assert!(!partial.other);
    }

    #[test]
    fn dependency_evidence_invalid_ignore_file_makes_inventory_uncertain() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join(".ignore"), "**{invalid\n").unwrap();
        let result = discover(
            root.path(),
            None,
            ENTRY_LIMIT,
            FILE_LIMIT,
            &mut CONTENT_LIMIT.clone(),
        );
        assert_eq!(result.reasons, BTreeSet::from(["ignoreInputError"]));
    }
    #[test]
    fn dependency_evidence_content_budget_retains_independent_proven_diff() {
        let root = tempfile::tempdir().unwrap();
        let ordinary = "fn a() {}";
        let changed = "fn c() {}";
        std::fs::write(root.path().join("a.rs"), changed).unwrap();
        std::fs::write(root.path().join("b.rs"), ordinary).unwrap();
        let mut conn = Connection::open_in_memory().unwrap();
        crate::state::migrations::get_migrations()
            .to_latest(&mut conn)
            .unwrap();
        let original_hash = blake3::hash(ordinary.as_bytes()).to_hex().to_string();
        conn.execute(
            "INSERT INTO project_files (file_path, content_hash, last_indexed_at) VALUES
            ('a.rs', ?1, ''), ('b.rs', ?1, '')",
            [original_hash],
        )
        .unwrap();
        let (native, _) = assess_with_content_budget(root.path(), Some(&conn), None, 9, || {});
        assert_eq!(native.state, "stale");
        assert_eq!(
            native.reasons,
            BTreeSet::from(["changedSource", "contentLimit", "headUnavailable"])
        );
    }

    #[test]
    fn dependency_evidence_legacy_hash_and_failed_parse_are_distinct() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("a.rs"), "fn parse_failure()").unwrap();
        let mut conn = Connection::open_in_memory().unwrap();
        crate::state::migrations::get_migrations()
            .to_latest(&mut conn)
            .unwrap();
        conn.execute(
            "INSERT INTO project_files (file_path, content_hash, last_indexed_at, parse_status)
          VALUES ('a.rs', NULL, '', 'PARSE_FAILED')",
            [],
        )
        .unwrap();
        let (native, _) = assess(root.path(), Some(&conn), None);
        assert_eq!(native.state, "unknown");
        assert_eq!(
            native.reasons,
            BTreeSet::from(["failedParse", "headUnavailable", "missingContentHash"])
        );
    }
    #[test]
    fn dependency_evidence_rechecks_early_source_at_the_assessment_boundary() {
        let root = tempfile::tempdir().unwrap();
        gix::init(root.path()).unwrap();
        let head = "0123456789012345678901234567890123456789";
        std::fs::write(root.path().join(".git/HEAD"), format!("{head}\n")).unwrap();
        let first = root.path().join("a.rs");
        let original = b"fn a() {}";
        std::fs::write(&first, original).unwrap();
        std::fs::write(root.path().join("b.rs"), original).unwrap();
        let mut conn = Connection::open_in_memory().unwrap();
        crate::state::migrations::get_migrations()
            .to_latest(&mut conn)
            .unwrap();
        conn.execute(
            "INSERT INTO index_metadata(key,value) VALUES ('head_hash',?1)",
            [head],
        )
        .unwrap();
        let digest = blake3::hash(original).to_hex().to_string();
        conn.execute(
            "INSERT INTO project_files (file_path, content_hash, last_indexed_at)
            VALUES ('a.rs', ?1, ''), ('b.rs', ?1, '')",
            [digest],
        )
        .unwrap();
        let (before, _) = assess(root.path(), Some(&conn), None);
        assert_eq!(before.state, "indexInputsMatch", "{:?}", before.reasons);
        let (after, _) =
            assess_with_content_budget(root.path(), Some(&conn), None, CONTENT_LIMIT, || {
                let metadata = std::fs::metadata(&first).unwrap();
                std::fs::write(&first, b"fn z() {}").unwrap();
                filetime::set_file_mtime(
                    &first,
                    filetime::FileTime::from_last_modification_time(&metadata),
                )
                .unwrap();
            });
        assert_eq!(after.state, "unknown");
        assert_eq!(after.reasons, BTreeSet::from(["inventoryUnstable"]));
    }

    #[test]
    fn dependency_evidence_stored_inventory_discards_overflow_and_oversized_paths() {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::state::migrations::get_migrations()
            .to_latest(&mut conn)
            .unwrap();
        conn.execute_batch(
            "INSERT INTO project_files(file_path,last_indexed_at)
            VALUES ('a.rs',''), ('b.rs',''), ('ab.md',''), ('unsupported.zz','');",
        )
        .unwrap();
        assert!(matches!(
            stored_sources(&conn, 1).unwrap(),
            StoredInventory::Capped
        ));
        assert!(
            matches!(stored_sources(&conn, 2).unwrap(), StoredInventory::Complete(rows) if rows.len() == 2)
        );
        let long = format!("{}.rs", "b".repeat(4097));
        conn.execute(
            "INSERT INTO project_files(file_path,last_indexed_at) VALUES (?1,'')",
            [long],
        )
        .unwrap();
        assert!(matches!(
            stored_sources(&conn, 100).unwrap(),
            StoredInventory::Capped
        ));
    }

    #[test]
    fn dependency_evidence_rejects_oversized_ignore_without_reading_content() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join(".ignore");
        std::fs::File::create(&path)
            .unwrap()
            .set_len(FILE_LIMIT + 1)
            .unwrap();
        let mut budget = CONTENT_LIMIT;
        let found = discover(root.path(), None, ENTRY_LIMIT, FILE_LIMIT, &mut budget);
        assert_eq!(
            found.reasons,
            BTreeSet::from(["fileLimit", "ignoreInputError"])
        );
        assert_eq!(budget, CONTENT_LIMIT);
    }

    #[cfg(target_os = "linux")]
    #[rstest::rstest]
    #[case(false)]
    #[case(true)]
    fn dependency_evidence_rejects_linked_or_special_ignore(#[case] fifo: bool) {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let path = root.path().join(".ignore");
        if fifo {
            nix::unistd::mkfifo(&path, nix::sys::stat::Mode::S_IRUSR).unwrap();
        } else {
            let target = outside.path().join("rules");
            std::fs::write(&target, b"*").unwrap();
            std::os::unix::fs::symlink(target, path).unwrap();
        }
        let mut budget = CONTENT_LIMIT;
        let found = discover(root.path(), None, ENTRY_LIMIT, FILE_LIMIT, &mut budget);
        assert_eq!(found.reasons, BTreeSet::from(["ignoreInputError"]));
        assert_eq!(budget, CONTENT_LIMIT);
    }

    #[test]
    fn dependency_evidence_preserves_layered_ignore_precedence() {
        let root = tempfile::tempdir().unwrap();
        let repo = gix::init(root.path()).unwrap();
        std::fs::create_dir(root.path().join("nested")).unwrap();
        std::fs::write(root.path().join(".gitignore"), "*.rs\n!kept.rs\n").unwrap();
        std::fs::write(root.path().join("nested/.ignore"), "!keep.rs\n").unwrap();
        std::fs::write(root.path().join("kept.rs"), "").unwrap();
        std::fs::write(root.path().join("ignored.rs"), "").unwrap();
        std::fs::write(root.path().join("nested/keep.rs"), "").unwrap();
        let mut budget = CONTENT_LIMIT;
        let found = discover(
            root.path(),
            Some(&repo),
            ENTRY_LIMIT,
            FILE_LIMIT,
            &mut budget,
        );
        assert!(found.reasons.is_empty(), "{:?}", found.reasons);
        assert_eq!(
            found.sources.keys().map(String::as_str).collect::<Vec<_>>(),
            ["kept.rs", "nested/keep.rs"]
        );
    }
    #[rstest::rstest]
    #[case(false)]
    #[case(true)]
    fn dependency_evidence_shared_ignore_cap_drops_provisional_subtrees(#[case] reverse: bool) {
        let root = tempfile::tempdir().unwrap();
        let order = if reverse { ["b", "a"] } else { ["a", "b"] };
        for name in order {
            let path = root.path().join(name);
            std::fs::create_dir(&path).unwrap();
            std::fs::write(path.join(".ignore"), "irrelevant\n").unwrap();
            std::fs::write(path.join("a.rs"), "fn a() {}").unwrap();
        }
        std::fs::write(root.path().join("a/other.unknown"), "").unwrap();
        let mut budget = "irrelevant\n".len() as u64;
        let found = discover(root.path(), None, ENTRY_LIMIT, FILE_LIMIT, &mut budget);
        assert_eq!(budget, 0);
        assert_eq!(
            found.reasons,
            BTreeSet::from(["contentLimit", "ignoreInputError"])
        );
        assert!(found.sources.is_empty());
        assert!(found.stamps.is_empty());
        assert!(!found.other);
    }
}

#[cfg(all(test, not(any(target_os = "linux", windows))))]
mod unsupported_discovery_tests {
    #[test]
    fn dependency_evidence_without_verified_enumeration_stays_unknown() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("a.rs"), "fn a() {}").unwrap();
        let (native, other) = super::assess(root.path(), None, None);
        assert_eq!(native.state, "unknown");
        assert!(native.reasons.contains("discoveryUnavailable"));
        assert!(!other);
    }
}
