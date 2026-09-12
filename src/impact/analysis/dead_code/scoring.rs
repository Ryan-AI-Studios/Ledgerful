use super::*;

impl<'a> ConfidenceScorer<'a> {
    /// Score a single symbol. Returns `None` if the symbol is an entrypoint,
    /// not Function/Method, or if the final confidence falls below the
    /// threshold after name-based penalties are applied.
    pub fn score_symbol(
        &self,
        symbol: &Symbol,
        file_path: &Path,
    ) -> Result<Option<DeadCodeFinding>> {
        self.score_symbol_inner(symbol, file_path, true)
    }

    fn score_symbol_inner(
        &self,
        symbol: &Symbol,
        file_path: &Path,
        omit_paths: bool,
    ) -> Result<Option<DeadCodeFinding>> {
        if filters::is_entrypoint(symbol) {
            return Ok(None);
        }

        if filters::is_bin_or_crate_main(symbol, file_path) {
            return Ok(None);
        }

        if !crate::index::resolve::is_callable_kind(symbol.kind.as_str()) {
            return Ok(None);
        }

        if omit_paths {
            let path_key = file_path.to_string_lossy().replace('\\', "/");
            if !self.include_tests && crate::index::test_mapping::is_test_path(&path_key) {
                self.omitted_test_paths.borrow_mut().insert(path_key);
                return Ok(None);
            }
            if !self.include_vendor && crate::impact::hotspots::is_vendor_hotspot_path(&path_key) {
                self.omitted_vendor_paths.borrow_mut().insert(path_key);
                return Ok(None);
            }
        }

        let reachability = self.reachability_score(symbol, file_path)?;
        let git_activity = self.git_activity_score(file_path)?;
        let test_coverage = self.test_coverage_score(symbol, file_path)?;

        let raw_confidence = self.blend(reachability, git_activity, test_coverage);
        let penalty = filters::name_penalty(&symbol.name);
        let confidence = (raw_confidence - penalty).max(0.0);

        if confidence < self.config.confidence_threshold {
            return Ok(None);
        }

        let mut factors = Vec::new();
        if reachability.is_some_and(|v| v >= 1.0) {
            factors.push(ConfidenceFactor::UnreachableFromEntrypoints);
        }
        if git_activity > 0.0 {
            let days = self
                .days_since_last_commit(file_path)?
                .unwrap_or(self.config.git_inactivity_days);
            factors.push(ConfidenceFactor::GitInactive {
                days_since_last_commit: days,
            });
        }
        if test_coverage.is_some_and(|v| v >= 1.0) {
            factors.push(ConfidenceFactor::NoTestCoverage);
        }

        let mut recommendation = format!(
            "Symbol '{}' in {} has {:.0}% confidence of being dead code. Consider reviewing for removal or adding tests.",
            symbol.name,
            file_path.display(),
            confidence * 100.0
        );

        if let Some(cfg) = symbol.metadata.get("cfg") {
            recommendation.push_str(&format!(" Note: symbol is feature-gated via {}.", cfg));
        }

        Ok(Some(DeadCodeFinding {
            symbol_name: symbol.name.clone(),
            file_path: file_path.to_path_buf(),
            confidence,
            factors,
            recommendation,
            line_start: symbol.line_start.map(|v| v.max(0) as usize),
            line_end: symbol.line_end.map(|v| v.max(0) as usize),
        }))
    }

    /// Score all symbols in a file.
    pub fn score_file(&self, file_path: &Path) -> Result<Vec<DeadCodeFinding>> {
        let resolved = self.get_symbols_for_file(file_path)?;
        self.score_resolved_symbols(&resolved)
    }

    /// Score an already-resolved set of file symbols using the stored path for
    /// cache keys and SQL lookups.
    pub(super) fn score_resolved_symbols(
        &self,
        resolved: &FileSymbols,
    ) -> Result<Vec<DeadCodeFinding>> {
        self.score_resolved_symbols_inner(resolved, true)
    }

    fn score_resolved_symbols_inner(
        &self,
        resolved: &FileSymbols,
        omit_paths: bool,
    ) -> Result<Vec<DeadCodeFinding>> {
        let stored_path = Path::new(&resolved.stored_path);
        let mut findings = Vec::new();
        for symbol in &resolved.symbols {
            if let Some(finding) = self.score_symbol_inner(symbol, stored_path, omit_paths)? {
                findings.push(finding);
            }
        }
        findings.sort_unstable();
        Ok(findings)
    }

    /// Optimized per-file explanation used by `dead-code --explain <file>`.
    ///
    /// This path skips the full-repo `precompute()` and `scan_repo()` caches:
    /// it only queries the knowledge graph / SQLite for symbols in the
    /// requested file, scores reachability/test-coverage/git-activity for those
    /// symbols alone, and returns a structured `DeadCodeExplanation`.
    pub fn explain_file(&mut self, file_path: &Path) -> Result<DeadCodeExplanation> {
        let resolved = self.get_symbols_for_file(file_path)?;
        self.precompute_for_file_with_symbols(&resolved)?;
        let findings = self.score_resolved_symbols_inner(&resolved, false)?;
        let file_str = file_path.display().to_string();
        Ok(crate::impact::analysis::dead_code::compute_dead_code_explanation(&file_str, &findings))
    }

    /// Full-repo scan (used by the standalone `dead-code` command).
    pub fn scan_repo(&self, limit: usize) -> Result<Vec<DeadCodeFinding>> {
        let start = std::time::Instant::now();
        let symbols = self.get_all_symbols()?;
        let total_symbols = symbols.len();
        let mut scanned = 0usize;
        let mut findings = Vec::new();
        for (symbol, file_path) in symbols {
            scanned += 1;
            if let Some(finding) = self.score_symbol(&symbol, &file_path)? {
                findings.push(finding);
                if findings.len() >= limit {
                    break;
                }
            }
        }
        findings.sort_unstable();
        debug!(
            total_symbols,
            scanned,
            findings = findings.len(),
            elapsed_ms = start.elapsed().as_millis() as u64,
            "dead-code scan_repo complete"
        );
        Ok(findings)
    }

    pub(super) fn blend(
        &self,
        reachability: Option<f64>,
        git_activity: f64,
        test_coverage: Option<f64>,
    ) -> f64 {
        let sum = self.config.reachability_weight
            + self.config.git_activity_weight
            + self.config.test_coverage_weight;
        if sum <= 0.0 {
            return 0.0;
        }
        (self.config.reachability_weight * reachability.unwrap_or(0.0)
            + self.config.git_activity_weight * git_activity
            + self.config.test_coverage_weight * test_coverage.unwrap_or(0.0))
            / sum
    }
}
