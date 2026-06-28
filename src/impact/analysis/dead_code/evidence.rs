use super::*;

impl<'a> ConfidenceScorer<'a> {
    // ------------------------------------------------------------------
    // Reachability
    // ------------------------------------------------------------------

    pub(super) fn reachability_score(&self, symbol: &Symbol, file_path: &Path) -> Result<f64> {
        let symbol_id = self.find_symbol_id(symbol, file_path)?;

        if let Some(ref cache) = self.precomputed_reachable_symbols {
            if let Some(id) = symbol_id {
                if cache.contains(&id) {
                    return Ok(0.0);
                } else {
                    return Ok(1.0);
                }
            } else {
                return Ok(1.0);
            }
        }

        let reachable = match self.cozo {
            Some(cozo) => self.reachability_via_cozo(symbol, cozo),
            None => self.reachability_via_sqlite(symbol, file_path),
        };

        match reachable {
            Ok(true) => Ok(0.0),
            Ok(false) => Ok(1.0),
            Err(e) => {
                warn!("Reachability query failed for {}: {}", symbol.name, e);
                Ok(0.0)
            }
        }
    }

    fn reachability_via_cozo(&self, symbol: &Symbol, cozo: &CozoStorage) -> Result<bool> {
        use crate::platform::urn::build_urn;
        use crate::state::graph_kinds::NodeKind;

        let entrypoints = self.get_entrypoint_qualified_names()?;
        if entrypoints.is_empty() {
            return Ok(false);
        }

        let qualified = match &symbol.qualified_name {
            Some(q) => q.clone(),
            None => symbol.name.clone(),
        };

        let target_urn = build_urn(NodeKind::Symbol, &qualified);
        let entry_values: Vec<serde_json::Value> = entrypoints
            .iter()
            .map(|e| serde_json::json!([build_urn(NodeKind::Symbol, e)]))
            .collect();
        let entry_list_json = serde_json::Value::Array(entry_values);

        let mut params = std::collections::BTreeMap::new();
        params.insert(
            "entry_list".to_string(),
            cozo::DataValue::from(entry_list_json),
        );
        params.insert(
            "target_node".to_string(),
            cozo::DataValue::Str(target_urn.into()),
        );

        let script = "
            entry[id] <- $entry_list
            reachable[node] := entry[e], *edge{source: e, target: node}
            reachable[node] := reachable[mid], *edge{source: mid, target: node}
            ?[count(node)] := reachable[node], node = $target_node
        ";

        let res = cozo.run_script_with_params(script, params, cozo::ScriptMutability::Immutable)?;
        let count = res
            .rows
            .first()
            .and_then(|r| r.first())
            .and_then(|v| match v {
                cozo::DataValue::Num(cozo::Num::Int(n)) => Some(*n),
                _ => None,
            })
            .unwrap_or(0);

        Ok(count > 0)
    }

    fn reachability_via_sqlite(&self, symbol: &Symbol, _file_path: &Path) -> Result<bool> {
        let conn = self.storage.get_connection();

        let mut stmt = conn
            .prepare(
                "SELECT id FROM project_symbols WHERE entrypoint_kind IN ('ENTRYPOINT', 'HANDLER', 'PUBLIC_API')"
            )
            .into_diagnostic()?;
        let entrypoint_ids: HashSet<i64> = stmt
            .query_map([], |row| row.get::<_, i64>(0))
            .into_diagnostic()?
            .collect::<Result<Vec<_>, _>>()
            .into_diagnostic()?
            .into_iter()
            .collect();
        drop(stmt);

        if entrypoint_ids.is_empty() {
            return Ok(false);
        }

        let mut stmt = conn
            .prepare("SELECT caller_symbol_id, callee_symbol_id FROM structural_edges WHERE callee_symbol_id IS NOT NULL")
            .into_diagnostic()?;
        let mut adj: HashMap<i64, Vec<i64>> = HashMap::new();
        let rows = stmt
            .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))
            .into_diagnostic()?;
        for row in rows {
            let (caller, callee) = row.into_diagnostic()?;
            adj.entry(caller).or_default().push(callee);
        }
        drop(stmt);

        let mut visited: HashSet<i64> = HashSet::new();
        let mut queue: Vec<i64> = entrypoint_ids.iter().copied().collect();
        for &id in &queue {
            visited.insert(id);
        }

        let mut idx = 0;
        while idx < queue.len() {
            let current = queue[idx];
            idx += 1;
            if let Some(neighbors) = adj.get(&current) {
                for &neighbor in neighbors {
                    if visited.insert(neighbor) {
                        queue.push(neighbor);
                    }
                }
            }
        }

        let symbol_id = self.find_symbol_id(symbol, _file_path)?;
        match symbol_id {
            Some(id) => Ok(visited.contains(&id)),
            None => Ok(false),
        }
    }

    pub(super) fn precompute_reachability(&self) -> Result<HashSet<i64>> {
        let conn = self.storage.get_connection();

        let mut stmt = conn
            .prepare(
                "SELECT id FROM project_symbols WHERE entrypoint_kind IN ('ENTRYPOINT', 'HANDLER', 'PUBLIC_API')"
            )
            .into_diagnostic()?;
        let entrypoint_ids: HashSet<i64> = stmt
            .query_map([], |row| row.get::<_, i64>(0))
            .into_diagnostic()?
            .collect::<Result<Vec<_>, _>>()
            .into_diagnostic()?
            .into_iter()
            .collect();
        drop(stmt);

        let mut stmt = conn
            .prepare("SELECT caller_symbol_id, callee_symbol_id FROM structural_edges WHERE callee_symbol_id IS NOT NULL")
            .into_diagnostic()?;
        let mut adj: HashMap<i64, Vec<i64>> = HashMap::new();
        let rows = stmt
            .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))
            .into_diagnostic()?;
        for row in rows {
            let (caller, callee) = row.into_diagnostic()?;
            adj.entry(caller).or_default().push(callee);
        }
        drop(stmt);

        let mut visited: HashSet<i64> = HashSet::new();
        let mut queue: Vec<i64> = entrypoint_ids.iter().copied().collect();
        for &id in &queue {
            visited.insert(id);
        }

        let mut idx = 0;
        while idx < queue.len() {
            let current = queue[idx];
            idx += 1;
            if let Some(neighbors) = adj.get(&current) {
                for &neighbor in neighbors {
                    if visited.insert(neighbor) {
                        queue.push(neighbor);
                    }
                }
            }
        }

        Ok(visited)
    }

    pub(super) fn get_entrypoint_qualified_names(&self) -> Result<Vec<String>> {
        let conn = self.storage.get_connection();
        let mut stmt = conn
            .prepare(
                "SELECT qualified_name FROM project_symbols WHERE entrypoint_kind IN ('ENTRYPOINT', 'HANDLER', 'PUBLIC_API')"
            )
            .into_diagnostic()?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .into_diagnostic()?;
        let mut names = Vec::new();
        for row in rows {
            names.push(row.into_diagnostic()?);
        }
        Ok(names)
    }

    pub(super) fn find_symbol_id(&self, symbol: &Symbol, file_path: &Path) -> Result<Option<i64>> {
        if let Some(ref cache) = self.precomputed_symbol_ids {
            let key = (
                file_path.to_string_lossy().to_string(),
                symbol.name.clone(),
                symbol.kind.as_str().to_string(),
            );
            return Ok(cache.get(&key).copied());
        }

        let conn = self.storage.get_connection();
        let mut stmt = conn
            .prepare(
                "SELECT ps.id FROM project_symbols ps\n                 JOIN project_files pf ON ps.file_id = pf.id\n                 WHERE pf.file_path = ?1 AND ps.symbol_name = ?2 AND ps.symbol_kind = ?3",
            )
            .into_diagnostic()?;
        let mut rows = stmt
            .query([
                file_path.to_string_lossy().as_ref(),
                symbol.name.as_str(),
                symbol.kind.as_str(),
            ])
            .into_diagnostic()?;
        if let Some(row) = rows.next().into_diagnostic()? {
            let id: i64 = row.get(0).into_diagnostic()?;
            Ok(Some(id))
        } else {
            Ok(None)
        }
    }

    pub(super) fn precompute_symbol_ids(&self) -> Result<HashMap<(String, String, String), i64>> {
        let conn = self.storage.get_connection();
        let mut map = HashMap::new();
        let mut stmt = conn
            .prepare(
                "SELECT pf.file_path, ps.symbol_name, ps.symbol_kind, ps.id \
             FROM project_symbols ps JOIN project_files pf ON ps.file_id = pf.id",
            )
            .into_diagnostic()?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })
            .into_diagnostic()?;
        for row in rows {
            let (fp, name, kind, id) = row.into_diagnostic()?;
            map.insert((fp, name, kind), id);
        }
        Ok(map)
    }

    // ------------------------------------------------------------------
    // Git Activity
    // ------------------------------------------------------------------

    /// Walks commit history once and records the most recent commit that
    /// touched each file, instead of the old per-file approach that
    /// re-walked up to 1000 commits independently for every distinct file
    /// (CG-F15: this was the dominant cost on repos with many files and a
    /// deep history -- the per-file `RefCell` cache only avoided *repeated*
    /// lookups of the same file within a run, not the cross-file cost).
    pub(super) fn precompute_git_activity(&self) -> Result<GitActivityIndex> {
        let unavailable = || GitActivityIndex {
            last_touched_days: HashMap::new(),
            repo_available: false,
        };

        let repo = match gix::discover(self.repo_path) {
            Ok(discovered) => gix::open(discovered.path()),
            Err(_) => return Ok(unavailable()),
        };
        let repo = match repo {
            Ok(r) => r,
            Err(_) => return Ok(unavailable()),
        };
        let head = match repo.head_commit() {
            Ok(h) => h,
            Err(_) => return Ok(unavailable()),
        };
        let walk = match head.id().ancestors().all() {
            Ok(w) => w,
            Err(_) => return Ok(unavailable()),
        };

        let max_commits = 1000usize;
        let mut commit_count = 0;
        let mut last_touched_days: HashMap<PathBuf, u32> = HashMap::new();
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        for res in walk {
            if commit_count >= max_commits {
                break;
            }
            let info = match res {
                Ok(info) => info,
                Err(e) => {
                    debug!("Skip commit during git activity walk: {}", e);
                    continue;
                }
            };
            let commit = match info.id().object().map(|obj| obj.into_commit()) {
                Ok(commit) => commit,
                Err(e) => {
                    debug!("Skip commit object during git activity walk: {}", e);
                    continue;
                }
            };
            let current_tree = match commit.tree() {
                Ok(tree) => tree,
                Err(e) => {
                    debug!("Skip tree during git activity walk: {}", e);
                    continue;
                }
            };
            let parent_id = commit.parent_ids().next();
            let parent_tree = if let Some(p_id) = parent_id {
                match p_id.object().map(|obj| obj.into_commit().tree()) {
                    Ok(Ok(tree)) => tree,
                    _ => repo.empty_tree(),
                }
            } else {
                repo.empty_tree()
            };

            let changes =
                match repo.diff_tree_to_tree(Some(&parent_tree), Some(&current_tree), None) {
                    Ok(changes) => changes,
                    Err(e) => {
                        debug!("Skip diff during git activity walk: {}", e);
                        continue;
                    }
                };

            let days = commit
                .time()
                .ok()
                .map(|t| ((now_secs - t.seconds).max(0) as f64 / 86400.0).ceil() as u32);

            if let Some(days) = days {
                for change in changes {
                    let locations: Vec<Vec<u8>> = match change {
                        gix::object::tree::diff::ChangeDetached::Addition { location, .. }
                        | gix::object::tree::diff::ChangeDetached::Deletion { location, .. }
                        | gix::object::tree::diff::ChangeDetached::Modification {
                            location, ..
                        } => vec![location.into()],
                        gix::object::tree::diff::ChangeDetached::Rewrite {
                            location,
                            source_location,
                            ..
                        } => vec![location.into(), source_location.into()],
                    };
                    for loc in locations {
                        let path_str = String::from_utf8_lossy(&loc).replace('\\', "/");
                        // First time we see a path wins: commits are walked newest-first.
                        last_touched_days
                            .entry(PathBuf::from(path_str))
                            .or_insert(days);
                    }
                }
            }

            commit_count += 1;
        }

        Ok(GitActivityIndex {
            last_touched_days,
            repo_available: true,
        })
    }

    pub(super) fn git_activity_score(&self, file_path: &Path) -> Result<f64> {
        let days = match self.days_since_last_commit(file_path)? {
            Some(d) => d,
            None => {
                return Ok(1.0);
            }
        };

        let threshold = self.config.git_inactivity_days as f64;
        if threshold <= 0.0 {
            return Ok(0.0);
        }

        let score = (days as f64 / threshold).min(1.0);
        Ok(score)
    }

    pub(super) fn days_since_last_commit(&self, file_path: &Path) -> Result<Option<u32>> {
        if let Some(ref index) = self.precomputed_git_activity {
            if !index.repo_available {
                return Ok(None);
            }
            let normalized = PathBuf::from(file_path.to_string_lossy().replace('\\', "/"));
            return Ok(Some(
                index
                    .last_touched_days
                    .get(&normalized)
                    .copied()
                    .unwrap_or(self.config.git_inactivity_days),
            ));
        }

        if let Some(cached) = self.git_activity_cache.borrow().get(file_path) {
            return Ok(*cached);
        }

        let calculate = || -> Result<Option<u32>> {
            let repo = match gix::discover(self.repo_path) {
                Ok(discovered) => gix::open(discovered.path()),
                Err(_) => return Ok(None),
            };
            let repo = match repo {
                Ok(r) => r,
                Err(_) => return Ok(None),
            };

            let head = match repo.head_commit() {
                Ok(h) => h,
                Err(_) => return Ok(None),
            };

            let file_str = file_path.to_string_lossy();
            let target_path = file_str.replace('\\', "/");

            let walk = match head.id().ancestors().all() {
                Ok(w) => w,
                Err(_) => return Ok(None),
            };

            let max_commits = 1000usize;
            let mut commit_count = 0;

            for res in walk {
                if commit_count >= max_commits {
                    break;
                }
                let info = match res {
                    Ok(info) => info,
                    Err(e) => {
                        debug!("Skip commit during git walk: {}", e);
                        continue;
                    }
                };

                let commit = match info.id().object().map(|obj| obj.into_commit()) {
                    Ok(commit) => commit,
                    Err(e) => {
                        debug!("Skip commit object: {}", e);
                        continue;
                    }
                };

                let current_tree = match commit.tree() {
                    Ok(tree) => tree,
                    Err(e) => {
                        debug!("Skip tree: {}", e);
                        continue;
                    }
                };

                let parent_id = commit.parent_ids().next();
                let parent_tree = if let Some(p_id) = parent_id {
                    match p_id.object().map(|obj| obj.into_commit().tree()) {
                        Ok(Ok(tree)) => tree,
                        _ => repo.empty_tree(),
                    }
                } else {
                    repo.empty_tree()
                };

                let changes =
                    match repo.diff_tree_to_tree(Some(&parent_tree), Some(&current_tree), None) {
                        Ok(changes) => changes,
                        Err(e) => {
                            debug!("Skip diff: {}", e);
                            continue;
                        }
                    };

                let mut touches_file = false;
                for change in changes {
                    let location = match change {
                        gix::object::tree::diff::ChangeDetached::Addition { location, .. }
                        | gix::object::tree::diff::ChangeDetached::Deletion { location, .. }
                        | gix::object::tree::diff::ChangeDetached::Modification {
                            location, ..
                        } => String::from_utf8_lossy(&location).into_owned(),
                        gix::object::tree::diff::ChangeDetached::Rewrite {
                            location,
                            source_location,
                            ..
                        } => {
                            let loc = String::from_utf8_lossy(&location).into_owned();
                            let src = String::from_utf8_lossy(&source_location).into_owned();
                            if loc.replace('\\', "/") == target_path
                                || src.replace('\\', "/") == target_path
                            {
                                touches_file = true;
                            }
                            continue;
                        }
                    };
                    if location.replace('\\', "/") == target_path {
                        touches_file = true;
                    }
                }

                if touches_file {
                    let commit_time = match commit.time() {
                        Ok(t) => t,
                        Err(e) => {
                            debug!("Skip commit with unreadable time: {}", e);
                            continue;
                        }
                    };
                    let commit_secs = commit_time.seconds;
                    let now_secs = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(commit_secs);
                    let days = ((now_secs - commit_secs).max(0) as f64 / 86400.0).ceil() as u32;
                    return Ok(Some(days));
                }

                commit_count += 1;
            }

            Ok(Some(self.config.git_inactivity_days))
        };

        let result = calculate()?;
        self.git_activity_cache
            .borrow_mut()
            .insert(file_path.to_path_buf(), result);
        Ok(result)
    }

    // ------------------------------------------------------------------
    // Test Coverage
    // ------------------------------------------------------------------

    pub(super) fn test_coverage_score(&self, symbol: &Symbol, file_path: &Path) -> Result<f64> {
        let symbol_id = match self.find_symbol_id(symbol, file_path)? {
            Some(id) => id,
            None => return Ok(1.0),
        };

        if let Some(ref cache) = self.precomputed_tested_symbols {
            if cache.contains(&symbol_id) {
                return Ok(0.0);
            } else {
                return Ok(1.0);
            }
        }

        let conn = self.storage.get_connection();

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM test_mapping WHERE tested_symbol_id = ?1",
                [symbol_id],
                |row| row.get(0),
            )
            .into_diagnostic()?;

        if count > 0 {
            return Ok(0.0);
        }

        let fallback_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM test_outcome_history toh\n                 JOIN embeddings e ON toh.diff_embedding_id = e.id\n                 WHERE e.entity_id = ?1",
                [symbol_id.to_string()],
                |row| row.get(0),
            )
            .unwrap_or(0);

        if fallback_count > 0 {
            return Ok(0.0);
        }

        Ok(1.0)
    }

    pub(super) fn precompute_test_coverage(&self) -> Result<HashSet<i64>> {
        let conn = self.storage.get_connection();
        let mut tested = HashSet::new();

        let mut stmt = conn
            .prepare("SELECT tested_symbol_id FROM test_mapping")
            .into_diagnostic()?;
        let rows = stmt
            .query_map([], |row| row.get::<_, i64>(0))
            .into_diagnostic()?;
        for row in rows {
            tested.insert(row.into_diagnostic()?);
        }

        let mut stmt = conn
            .prepare(
                "SELECT e.entity_id FROM test_outcome_history toh \
             JOIN embeddings e ON toh.diff_embedding_id = e.id",
            )
            .into_diagnostic()?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .into_diagnostic()?;
        for row in rows {
            if let Ok(id) = row.into_diagnostic()?.parse::<i64>() {
                tested.insert(id);
            }
        }

        Ok(tested)
    }

    /// TA25: batch test-coverage lookup for a specific set of symbol ids.
    ///
    /// Issues two chunked queries (`test_mapping` and `test_outcome_history`
    /// via `embeddings`) and returns the ids that have any coverage. The
    /// result is bounded to the requested symbol set and does not load the
    /// full test history table.
    pub(super) fn precompute_test_coverage_for_symbols(
        &self,
        symbol_ids: &[i64],
    ) -> Result<HashSet<i64>> {
        if symbol_ids.is_empty() {
            return Ok(HashSet::new());
        }

        let conn = self.storage.get_connection();
        let mut tested = HashSet::new();
        const CHUNK_SIZE: usize = 500;

        for chunk in symbol_ids.chunks(CHUNK_SIZE) {
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(",");
            let query = format!(
                "SELECT DISTINCT tested_symbol_id FROM test_mapping WHERE tested_symbol_id IN ({})",
                placeholders
            );
            let mut stmt = conn.prepare(&query).into_diagnostic()?;
            let rows = stmt
                .query_map(rusqlite::params_from_iter(chunk), |row| {
                    row.get::<_, i64>(0)
                })
                .into_diagnostic()?;
            for row in rows {
                tested.insert(row.into_diagnostic()?);
            }
        }

        for chunk in symbol_ids.chunks(CHUNK_SIZE) {
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(",");
            let query = format!(
                "SELECT DISTINCT e.entity_id FROM test_outcome_history toh \
                 JOIN embeddings e ON toh.diff_embedding_id = e.id \
                 WHERE e.entity_id IN ({})",
                placeholders
            );
            let mut stmt = conn.prepare(&query).into_diagnostic()?;
            let str_chunk: Vec<String> = chunk.iter().map(|id| id.to_string()).collect();
            let rows = stmt
                .query_map(rusqlite::params_from_iter(&str_chunk), |row| {
                    row.get::<_, String>(0)
                })
                .into_diagnostic()?;
            for row in rows {
                if let Ok(id) = row.into_diagnostic()?.parse::<i64>() {
                    tested.insert(id);
                }
            }
        }

        Ok(tested)
    }

    /// TA25: batch single-hop reachability lookup for a specific set of
    /// symbol ids.
    ///
    /// A symbol is considered reachable if it is an entrypoint or if any
    /// entrypoint has a direct structural edge to it. The query is chunked
    /// at 500 ids per round-trip and never loads the full edge table.
    pub(super) fn precompute_reachability_for_symbols(
        &self,
        symbol_ids: &[i64],
    ) -> Result<HashSet<i64>> {
        if symbol_ids.is_empty() {
            return Ok(HashSet::new());
        }

        let conn = self.storage.get_connection();
        const CHUNK_SIZE: usize = 500;
        const ENTRYPOINT_KINDS: &str = "('ENTRYPOINT', 'HANDLER', 'PUBLIC_API')";

        let mut reachable = HashSet::new();

        for chunk in symbol_ids.chunks(CHUNK_SIZE) {
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(",");
            let query = format!(
                "SELECT DISTINCT ps.id \
                 FROM project_symbols ps \
                 WHERE ps.id IN ({}) \
                   AND ( \
                     ps.entrypoint_kind IN {ENTRYPOINT_KINDS} \
                     OR EXISTS ( \
                       SELECT 1 FROM structural_edges se \
                       JOIN project_symbols ep ON se.caller_symbol_id = ep.id \
                       WHERE se.callee_symbol_id = ps.id \
                         AND ep.entrypoint_kind IN {ENTRYPOINT_KINDS} \
                     ) \
                   )",
                placeholders
            );
            let mut stmt = conn.prepare(&query).into_diagnostic()?;
            let rows = stmt
                .query_map(rusqlite::params_from_iter(chunk), |row| {
                    row.get::<_, i64>(0)
                })
                .into_diagnostic()?;
            for row in rows {
                reachable.insert(row.into_diagnostic()?);
            }
        }

        Ok(reachable)
    }

    // ------------------------------------------------------------------
    // Symbol resolution helpers
    // ------------------------------------------------------------------

    pub(super) fn get_symbols_for_file(&self, file_path: &Path) -> Result<FileSymbols> {
        let original_input = file_path.to_string_lossy().to_string();
        let normalized = normalize_file_path(self.repo_path, &original_input);

        debug!(
            original_input = original_input,
            normalized_path = normalized,
            "dead-code explain resolving file path"
        );

        // Strategy 1: exact normalized match (case-insensitive on Windows).
        let conn = self.storage.get_connection();
        let mut rows = self.query_symbols_with_ids_by_path(conn, &normalized)?;
        debug!(
            exact_match_count = rows.len(),
            normalized_path = normalized,
            "dead-code explain exact path query"
        );

        let mut stored_path = normalized.clone();
        if !rows.is_empty() {
            debug!(
                final_result_count = rows.len(),
                normalized_path = normalized,
                "dead-code explain resolved via exact match"
            );
            let symbols: Vec<Symbol> = rows.iter().map(|(s, _)| s.clone()).collect();
            let mut symbol_ids = HashMap::new();
            for (symbol, id) in rows {
                symbol_ids.insert(
                    (
                        normalized.clone(),
                        symbol.name,
                        symbol.kind.as_str().to_string(),
                    ),
                    id,
                );
            }
            return Ok(FileSymbols {
                stored_path,
                symbols,
                symbol_ids,
            });
        }

        // Strategy 2: basename fallback for prefix mismatches (e.g. user typed
        // `src/hotspots.rs` but KG stores `crates/foo/src/hotspots.rs`).
        let basename = match std::path::Path::new(&normalized)
            .file_name()
            .and_then(|n| n.to_str())
        {
            Some(b) => b.to_string(),
            None => {
                debug!(
                    normalized_path = normalized,
                    "dead-code explain empty basename, no fallback possible"
                );
                return Ok(FileSymbols {
                    stored_path,
                    symbols: Vec::new(),
                    symbol_ids: HashMap::new(),
                });
            }
        };

        let candidates = self.query_file_paths_by_basename(conn, &basename)?;
        debug!(
            basename = basename,
            candidate_count = candidates.len(),
            "dead-code explain basename fallback candidates"
        );

        if candidates.is_empty() {
            debug!(
                normalized_path = normalized,
                basename = basename,
                "dead-code explain no candidates found"
            );
            return Ok(FileSymbols {
                stored_path,
                symbols: Vec::new(),
                symbol_ids: HashMap::new(),
            });
        }

        let selected = select_best_file_path_candidate(&normalized, &candidates)?;
        debug!(
            selected_path = selected,
            discarded = ?candidates.iter().filter(|p| *p != selected).collect::<Vec<_>>(),
            "dead-code explain selected basename fallback candidate"
        );

        stored_path = selected.to_string();
        rows = self.query_symbols_with_ids_by_path(conn, selected)?;
        debug!(
            final_result_count = rows.len(),
            selected_path = selected,
            "dead-code explain resolved via basename fallback"
        );

        let symbols: Vec<Symbol> = rows.iter().map(|(s, _)| s.clone()).collect();
        let mut symbol_ids = HashMap::new();
        for (symbol, id) in rows {
            symbol_ids.insert(
                (
                    stored_path.clone(),
                    symbol.name,
                    symbol.kind.as_str().to_string(),
                ),
                id,
            );
        }
        Ok(FileSymbols {
            stored_path,
            symbols,
            symbol_ids,
        })
    }

    /// Query symbols for a stored `file_path` value, returning each symbol
    /// together with its database id. Uses a case-insensitive comparison on
    /// Windows and exact match elsewhere.
    fn query_symbols_with_ids_by_path(
        &self,
        conn: &rusqlite::Connection,
        path: &str,
    ) -> Result<Vec<(Symbol, i64)>> {
        let sql = if cfg!(target_os = "windows") {
            "SELECT ps.id, ps.symbol_name, ps.symbol_kind, ps.is_public, ps.cognitive_complexity,\n\
                    ps.cyclomatic_complexity, ps.line_start, ps.line_end, ps.qualified_name,\n\
                    ps.byte_start, ps.byte_end, ps.entrypoint_kind, ps.metadata\n\
             FROM project_symbols ps\n\
             JOIN project_files pf ON ps.file_id = pf.id\n\
             WHERE LOWER(pf.file_path) = LOWER(?1)"
        } else {
            "SELECT ps.id, ps.symbol_name, ps.symbol_kind, ps.is_public, ps.cognitive_complexity,\n\
                    ps.cyclomatic_complexity, ps.line_start, ps.line_end, ps.qualified_name,\n\
                    ps.byte_start, ps.byte_end, ps.entrypoint_kind, ps.metadata\n\
             FROM project_symbols ps\n\
             JOIN project_files pf ON ps.file_id = pf.id\n\
             WHERE pf.file_path = ?1"
        };
        let mut stmt = conn.prepare(sql).into_diagnostic()?;
        let rows = stmt
            .query_map([path], Self::map_project_symbol_row_with_id)
            .into_diagnostic()?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row.into_diagnostic()?);
        }
        Ok(result)
    }

    /// Return stored file paths whose basename matches `basename`.
    fn query_file_paths_by_basename(
        &self,
        conn: &rusqlite::Connection,
        basename: &str,
    ) -> Result<Vec<String>> {
        // Match both nested files (`%/<basename>`) and root-level files
        // (exact basename match with no directory prefix).
        let like_pattern = format!("%/{basename}");
        #[cfg(target_os = "windows")]
        let lower_basename = basename.to_lowercase();
        #[cfg(target_os = "windows")]
        let exact_param: [&str; 2] = [&like_pattern, &lower_basename];
        #[cfg(not(target_os = "windows"))]
        let exact_param: [&str; 2] = [&like_pattern, basename];
        let mut stmt = conn
            .prepare(
                "SELECT DISTINCT pf.file_path FROM project_files pf \
                 WHERE pf.file_path LIKE ?1 OR pf.file_path = ?2 \
                 OR LOWER(pf.file_path) = LOWER(?2)",
            )
            .into_diagnostic()?;
        let rows = stmt
            .query_map(exact_param, |row| row.get::<_, String>(0))
            .into_diagnostic()?;
        let mut paths = Vec::new();
        for row in rows {
            paths.push(row.into_diagnostic()?);
        }
        Ok(paths)
    }

    /// Map a `project_symbols` row into a `Symbol`. Extracted so both exact and
    /// fallback queries share the same row mapping.
    pub(super) fn map_project_symbol_row(
        row: &rusqlite::Row,
    ) -> std::result::Result<Symbol, rusqlite::Error> {
        let kind_str: String = row.get(1)?;
        let kind = crate::index::symbols::SymbolKind::parse(&kind_str)
            .unwrap_or(crate::index::symbols::SymbolKind::Function);
        let is_public: i32 = row.get(2)?;
        let entrypoint: Option<String> = row.get(10)?;
        let metadata_str: Option<String> = row.get(11)?;
        let metadata = metadata_str
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();

        Ok(Symbol {
            name: row.get(0)?,
            kind,
            is_public: is_public != 0,
            cognitive_complexity: row.get(3)?,
            cyclomatic_complexity: row.get(4)?,
            line_start: row.get(5)?,
            line_end: row.get(6)?,
            qualified_name: row.get(7)?,
            byte_start: row.get(8)?,
            byte_end: row.get(9)?,
            entrypoint_kind: entrypoint,
            metadata,
        })
    }

    /// Map a `project_symbols` row into a `(Symbol, id)` pair. The row is
    /// expected to have `ps.id` as the first column, followed by the same
    /// columns as `map_project_symbol_row`.
    pub(super) fn map_project_symbol_row_with_id(
        row: &rusqlite::Row,
    ) -> std::result::Result<(Symbol, i64), rusqlite::Error> {
        let id: i64 = row.get(0)?;
        let kind_str: String = row.get(2)?;
        let kind = crate::index::symbols::SymbolKind::parse(&kind_str)
            .unwrap_or(crate::index::symbols::SymbolKind::Function);
        let is_public: i32 = row.get(3)?;
        let entrypoint: Option<String> = row.get(11)?;
        let metadata_str: Option<String> = row.get(12)?;
        let metadata = metadata_str
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();

        Ok((
            Symbol {
                name: row.get(1)?,
                kind,
                is_public: is_public != 0,
                cognitive_complexity: row.get(4)?,
                cyclomatic_complexity: row.get(5)?,
                line_start: row.get(6)?,
                line_end: row.get(7)?,
                qualified_name: row.get(8)?,
                byte_start: row.get(9)?,
                byte_end: row.get(10)?,
                entrypoint_kind: entrypoint,
                metadata,
            },
            id,
        ))
    }

    pub(super) fn get_all_symbols(&self) -> Result<Vec<(Symbol, PathBuf)>> {
        let conn = self.storage.get_connection();
        let mut stmt = conn
            .prepare(
                "SELECT ps.symbol_name, ps.symbol_kind, ps.is_public, ps.cognitive_complexity,\n\
                         ps.cyclomatic_complexity, ps.line_start, ps.line_end, ps.qualified_name,\n\
                         ps.byte_start, ps.byte_end, ps.entrypoint_kind, ps.metadata, pf.file_path\n\
                  FROM project_symbols ps\n\
                  JOIN project_files pf ON ps.file_id = pf.id\n\
                  WHERE pf.parse_status != 'DELETED'",
            )
            .into_diagnostic()?;
        let rows = stmt
            .query_map([], |row| {
                let symbol = Self::map_project_symbol_row(row)?;
                let file_path: String = row.get(12)?;
                Ok((symbol, PathBuf::from(file_path)))
            })
            .into_diagnostic()?;
        let mut symbols = Vec::new();
        for row in rows {
            symbols.push(row.into_diagnostic()?);
        }
        Ok(symbols)
    }
}

/// Normalize a user-supplied file path for index lookup.
///
/// - Converts backslashes to forward slashes.
/// - Strips a leading `./`.
/// - Strips trailing slashes.
/// - Relativizes absolute paths against `repo_root` when they are inside it.
/// - On Windows, lowercases the result for case-insensitive comparison.
/// - Does **not** canonicalize symlinks.
pub(super) fn normalize_file_path(repo_root: &Path, input: &str) -> String {
    let mut path = input.replace('\\', "/");

    if path.starts_with("./") {
        path = path[2..].to_string();
    }
    while path.ends_with('/') {
        path.pop();
    }

    // Relativize absolute paths that sit under the repo root. We only do
    // lexical relativization; symlinks are intentionally not canonicalized.
    if let Ok(cwd) = std::env::current_dir() {
        let absolute = normalize_to_absolute(&cwd, &path);
        let root_forward = repo_root.to_string_lossy().replace('\\', "/");
        let root_clean = root_forward.trim_end_matches('/');
        let absolute_forward = absolute.replace('\\', "/");
        // On Windows, paths are case-insensitive — compare lowercased.
        #[cfg(target_os = "windows")]
        let (cmp_root, cmp_abs) = (root_clean.to_lowercase(), absolute_forward.to_lowercase());
        #[cfg(not(target_os = "windows"))]
        let (cmp_root, cmp_abs) = (root_clean.to_string(), absolute_forward.clone());
        if let Some(_stripped) = cmp_abs.strip_prefix(&cmp_root) {
            // Compute the relative suffix from the original (non-lowercased)
            // absolute path so we preserve the stored casing on Unix.
            let suffix_len = absolute_forward.len().saturating_sub(cmp_root.len());
            let relative = absolute_forward[absolute_forward.len() - suffix_len..]
                .trim_start_matches('/')
                .to_string();
            if !relative.is_empty() {
                path = relative;
            }
        }
    }

    #[cfg(target_os = "windows")]
    let path = path.to_lowercase();

    path
}

/// If `path` is already absolute, return it; otherwise join with `cwd`.
fn normalize_to_absolute(cwd: &Path, path: &str) -> String {
    let candidate = Path::new(path);
    if candidate.is_absolute() {
        path.to_string()
    } else {
        cwd.join(path).to_string_lossy().to_string()
    }
}

/// Pick the best candidate from a basename fallback, or error if the match is
/// ambiguous.
fn select_best_file_path_candidate<'a>(
    normalized_input: &'a str,
    candidates: &'a [String],
) -> Result<&'a str> {
    let best = candidates
        .iter()
        .max_by(|a, b| {
            longest_common_path_suffix(normalized_input, a)
                .cmp(&longest_common_path_suffix(normalized_input, b))
                .then_with(|| b.len().cmp(&a.len())) // tie-break: shorter stored path wins
        })
        .ok_or_else(|| miette::miette!("No candidate file paths available"))?;

    let lcs = longest_common_path_suffix(normalized_input, best);
    let basename = std::path::Path::new(normalized_input)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(normalized_input);

    // Weak match: only the basename is common. Return an error listing the
    // ambiguous candidates instead of guessing.
    if lcs == basename {
        let list = candidates.join(", ");
        return Err(miette::miette!(
            "Multiple files match '{basename}': {list}. Please provide a more specific path."
        ));
    }

    Ok(best.as_str())
}

/// Length of the longest common *suffix* (tail) between two slash-normalized
/// paths. For path matching, what matters is how many trailing components
/// overlap between the user input and the stored path (e.g. `src/main.rs`
/// matches `crates/foo/src/main.rs` on the final two components).
fn longest_common_path_suffix(a: &str, b: &str) -> String {
    let a_parts: Vec<&str> = a.split('/').collect();
    let b_parts: Vec<&str> = b.split('/').collect();
    let min_len = a_parts.len().min(b_parts.len());
    let mut common = Vec::new();
    for i in 0..min_len {
        let a_idx = a_parts.len() - 1 - i;
        let b_idx = b_parts.len() - 1 - i;
        let a_part = if cfg!(target_os = "windows") {
            a_parts[a_idx].to_lowercase()
        } else {
            a_parts[a_idx].to_string()
        };
        let b_part = if cfg!(target_os = "windows") {
            b_parts[b_idx].to_lowercase()
        } else {
            b_parts[b_idx].to_string()
        };
        if a_part == b_part {
            common.push(a_parts[a_idx]);
        } else {
            break;
        }
    }
    common.reverse();
    common.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_file_path_backslash_and_dot_slash() {
        let root = Path::new("/repo");
        assert_eq!(normalize_file_path(root, ".\\src\\main.rs"), "src/main.rs");
        assert_eq!(normalize_file_path(root, "./src/main.rs"), "src/main.rs");
        assert_eq!(normalize_file_path(root, "src\\util.rs"), "src/util.rs");
    }

    #[test]
    fn normalize_file_path_trailing_slash() {
        let root = Path::new("/repo");
        assert_eq!(normalize_file_path(root, "src/main.rs/"), "src/main.rs");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn normalize_file_path_lowercase_on_windows() {
        let root = Path::new("C:\\repo");
        assert_eq!(normalize_file_path(root, "SRC\\Main.Rs"), "src/main.rs");
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn normalize_file_path_preserve_case_on_unix() {
        let root = Path::new("/repo");
        assert_eq!(normalize_file_path(root, "SRC/Main.Rs"), "SRC/Main.Rs");
    }

    #[test]
    fn longest_common_path_suffix_basic() {
        assert_eq!(
            longest_common_path_suffix("src/main.rs", "crates/foo/src/main.rs"),
            "src/main.rs"
        );
        assert_eq!(longest_common_path_suffix("a/b/c", "x/y/b/c"), "b/c");
        assert_eq!(longest_common_path_suffix("x/y", "a/b"), "");
    }

    #[test]
    fn select_best_candidate_prefers_longest_common_prefix() {
        let candidates = vec![
            "crates/foo/src/main.rs".to_string(),
            "src/main.rs".to_string(),
        ];
        let best = select_best_file_path_candidate("src/main.rs", &candidates).unwrap();
        assert_eq!(best, "src/main.rs");
    }

    #[test]
    fn select_best_candidate_tie_break_shorter_path() {
        let candidates = vec!["a/src/main.rs".to_string(), "b/src/main.rs".to_string()];
        let best = select_best_file_path_candidate("src/main.rs", &candidates).unwrap();
        // Both share `src/main.rs` only and have equal length. `Iterator::max_by`
        // returns the last element when the comparison is equal, so the second
        // candidate is deterministic here.
        assert_eq!(best, "b/src/main.rs");
    }

    #[test]
    fn select_best_candidate_errors_on_weak_basename_match() {
        let candidates = vec!["src/a/mod.rs".to_string(), "src/b/mod.rs".to_string()];
        let err = select_best_file_path_candidate("mod.rs", &candidates).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Multiple files match 'mod.rs'"), "{msg}");
        assert!(msg.contains("src/a/mod.rs"), "{msg}");
        assert!(msg.contains("src/b/mod.rs"), "{msg}");
    }
}
