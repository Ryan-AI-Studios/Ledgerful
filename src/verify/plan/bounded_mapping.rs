use rusqlite::{OptionalExtension, Transaction, params};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

const MAX_LOOKUPS: usize = 64;
const MAX_ROWS_PER_FILE: usize = 64;
const MAX_TEXT_BYTES: usize = 4096;

pub(super) const ROWS: &str = "
WITH bounded AS MATERIALIZED (
    SELECT id, test_file_id, test_symbol_id
    FROM test_mapping INDEXED BY idx_change_evidence_mapping_tested_file
    WHERE tested_file_id = ?1 ORDER BY id LIMIT ?2
)
SELECT bounded.id, bounded.test_file_id, pf.id,
    CASE WHEN typeof(pf.file_path) = 'text' AND octet_length(pf.file_path) <= ?3
        THEN pf.file_path END,
    bounded.test_symbol_id, ps.id, ps.file_id,
    CASE WHEN typeof(ps.symbol_name) = 'text' AND octet_length(ps.symbol_name) <= ?3
        THEN ps.symbol_name END
FROM bounded
LEFT JOIN project_files pf ON bounded.test_file_id = pf.id
LEFT JOIN project_symbols ps ON bounded.test_symbol_id = ps.id
ORDER BY bounded.id";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MappingUnproved;

pub(crate) struct BoundedMapping<'a, 'connection> {
    tx: &'a Transaction<'connection>,
    paths: BTreeMap<String, BTreeSet<String>>,
}

impl<'a, 'connection> BoundedMapping<'a, 'connection> {
    pub(super) fn new(
        tx: &'a Transaction<'connection>,
        expected_head: &str,
    ) -> Result<Self, MappingUnproved> {
        require_mapping_index(tx)?;

        let indexed_head: Option<String> = tx
            .query_row(
                "SELECT CASE WHEN typeof(value) = 'text' AND octet_length(value) IN (40, 64)
                 THEN value END FROM index_metadata WHERE key = 'head_hash'",
                [],
                |row| row.get(0),
            )
            .map_err(|_| MappingUnproved)?;
        if indexed_head.as_deref() != Some(expected_head) {
            return Err(MappingUnproved);
        }
        // An empty catalog is governed by the ordinary freshness/repair policy.
        tx.query_row("SELECT 1 FROM test_mapping LIMIT 1", [], |_| Ok(()))
            .map_err(|_| MappingUnproved)?;
        Ok(Self {
            tx,
            paths: BTreeMap::new(),
        })
    }

    pub(crate) fn stems<'p>(
        &mut self,
        paths: impl IntoIterator<Item = &'p Path>,
    ) -> Result<Option<Vec<String>>, MappingUnproved> {
        let mut stems = BTreeSet::new();
        for path in paths {
            let path = path.to_str().ok_or(MappingUnproved)?;
            if path.is_empty() || path.len() > MAX_TEXT_BYTES {
                return Err(MappingUnproved);
            }
            let path = path.replace('\\', "/");
            if !self.paths.contains_key(&path) {
                if self.paths.len() == MAX_LOOKUPS {
                    return Err(MappingUnproved);
                }
                let rows = self.read_stems(&path).map_err(|_| MappingUnproved)?;
                self.paths.insert(path.clone(), rows);
            }
            stems.extend(self.paths[&path].iter().cloned());
        }
        Ok((!stems.is_empty()).then(|| stems.into_iter().collect()))
    }

    fn read_stems(&self, path: &str) -> rusqlite::Result<BTreeSet<String>> {
        let file_id = self
            .tx
            .query_row(
                "SELECT id FROM project_files WHERE file_path = ?1",
                [path],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        let Some(file_id) = file_id else {
            return Ok(BTreeSet::new());
        };
        let mut stmt = self.tx.prepare_cached(ROWS)?;
        let mut rows = stmt.query(params![
            file_id,
            (MAX_ROWS_PER_FILE + 1) as i64,
            MAX_TEXT_BYTES as i64
        ])?;
        let mut stems = BTreeSet::new();
        let mut previous = None;
        let mut count = 0;
        while let Some(row) = rows.next()? {
            count += 1;
            let mapping_id = row.get::<_, i64>(0)?;
            let test_file_id = row.get::<_, i64>(1)?;
            let test_path = row.get::<_, String>(3)?;
            let test_symbol_id = row.get::<_, i64>(4)?;
            let symbol = row.get::<_, String>(7)?;
            if count > MAX_ROWS_PER_FILE
                || previous.is_some_and(|id| id >= mapping_id)
                || row.get::<_, i64>(2)? != test_file_id
                || row.get::<_, i64>(5)? != test_symbol_id
                || row.get::<_, i64>(6)? != test_file_id
            {
                return Err(rusqlite::Error::InvalidQuery);
            }
            previous = Some(mapping_id);
            if let Some(stem) = super::mapping_row_stem(&test_path, Some(symbol)) {
                stems.insert(stem);
            }
        }
        Ok(stems)
    }
}

fn require_mapping_index(tx: &Transaction<'_>) -> Result<(), MappingUnproved> {
    // A second declared key would put rowid behind that key, forcing SQLite
    // to sort all matching mappings before it can apply the physical row cap.
    let mut info = tx
        .prepare(
            "SELECT cid, name, coll, desc, key FROM pragma_index_xinfo(
                'idx_change_evidence_mapping_tested_file', 'main')
             ORDER BY seqno LIMIT 3",
        )
        .map_err(|_| MappingUnproved)?;
    let mut rows = info.query([]).map_err(|_| MappingUnproved)?;
    let key = rows
        .next()
        .map_err(|_| MappingUnproved)?
        .ok_or(MappingUnproved)?;
    let read_entry = |row: &rusqlite::Row<'_>| -> rusqlite::Result<_> {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, i64>(4)?,
        ))
    };
    let (cid, name, collation, descending, declared) =
        read_entry(key).map_err(|_| MappingUnproved)?;
    if cid < 0
        || name.as_deref() != Some("tested_file_id")
        || collation != "BINARY"
        || descending != 0
        || declared != 1
    {
        return Err(MappingUnproved);
    }
    let rowid = rows
        .next()
        .map_err(|_| MappingUnproved)?
        .ok_or(MappingUnproved)?;
    let (cid, name, collation, descending, declared) =
        read_entry(rowid).map_err(|_| MappingUnproved)?;
    if cid != -1
        || name.is_some()
        || collation != "BINARY"
        || descending != 0
        || declared != 0
        || rows.next().map_err(|_| MappingUnproved)?.is_some()
    {
        return Err(MappingUnproved);
    }

    tx.query_row(
        "SELECT 1 FROM pragma_index_list('test_mapping', 'main')
         WHERE name = 'idx_change_evidence_mapping_tested_file'
           AND origin = 'c' AND [unique] = 0 AND partial = 0",
        [],
        |_| Ok(()),
    )
    .map_err(|_| MappingUnproved)
}
