use crate::config::model::TemporalConfig;
use crate::git::GitError;
use crate::impact::budget::{AnalysisBudget, HistoryWalkStop};
use crate::impact::packet::TemporalCoupling;
use camino::Utf8PathBuf;
use gix::Repository;
use gix::object::tree::diff::ChangeDetached;
use miette::Result;
use std::collections::{BTreeSet, HashMap, HashSet};

#[derive(Clone, Default)]
pub struct CommitFileSet {
    pub files: HashSet<Utf8PathBuf>,
    /// Directory entries from the same commit. Not counted toward the leaf cap.
    pub tree_paths: HashSet<Utf8PathBuf>,
    pub is_merge: bool,
    /// Hex id when the walk recorded one (`None` in mocks).
    pub id: Option<String>,
}

/// File pairs, the wider audit pairs, and tree paths from commits the leaf cap kept.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TemporalCouplingReport {
    pub file_couplings: Vec<TemporalCoupling>,
    pub audit_couplings: Vec<TemporalCoupling>,
    pub discarded_tree_paths: Vec<Utf8PathBuf>,
}

/// Result of a budgeted first-parent (or all-parents) walk.
#[derive(Clone)]
pub struct HistoryWalkResult {
    pub history: Vec<CommitFileSet>,
    pub stop: HistoryWalkStop,
    pub commits_walked: usize,
    pub head: Option<String>,
}

pub trait HistoryProvider {
    fn get_history(
        &self,
        max_commits: usize,
        max_days: Option<u64>,
        since_commit: Option<String>,
        all_parents: bool,
    ) -> Result<Vec<CommitFileSet>, GitError>;

    /// Default wraps [`Self::get_history`] as a complete walk so existing
    /// mocks keep compiling. `GixHistoryProvider` overrides this.
    fn get_history_budgeted(
        &self,
        max_commits: usize,
        max_days: Option<u64>,
        since_commit: Option<String>,
        all_parents: bool,
        _budget: Option<&AnalysisBudget>,
    ) -> Result<HistoryWalkResult, GitError> {
        let history = self.get_history(max_commits, max_days, since_commit, all_parents)?;
        let commits_walked = history.len();
        Ok(HistoryWalkResult {
            history,
            stop: HistoryWalkStop::Complete,
            commits_walked,
            head: None,
        })
    }
}

#[derive(Clone)]
pub struct GixHistoryProvider<'repo> {
    repo: &'repo Repository,
    start_commit: Option<gix::ObjectId>,
}

impl<'repo> GixHistoryProvider<'repo> {
    pub fn new(repo: &'repo Repository) -> Self {
        Self {
            repo,
            start_commit: None,
        }
    }

    /// Create a provider that walks history starting from `commit_id` rather
    /// than the current HEAD. This lets callers compute "as of commit X"
    /// hotspot scores without checking out the commit in the working tree.
    pub fn from_commit(repo: &'repo Repository, commit_id: gix::ObjectId) -> Self {
        Self {
            repo,
            start_commit: Some(commit_id),
        }
    }
}

impl<'repo> HistoryProvider for GixHistoryProvider<'repo> {
    fn get_history(
        &self,
        max_commits: usize,
        max_days: Option<u64>,
        since_commit: Option<String>,
        all_parents: bool,
    ) -> Result<Vec<CommitFileSet>, GitError> {
        Ok(self
            .get_history_budgeted(max_commits, max_days, since_commit, all_parents, None)?
            .history)
    }

    fn get_history_budgeted(
        &self,
        max_commits: usize,
        max_days: Option<u64>,
        since_commit: Option<String>,
        all_parents: bool,
        budget: Option<&AnalysisBudget>,
    ) -> Result<HistoryWalkResult, GitError> {
        #[cfg(test)]
        crate::impact::budget::test_hooks::incr_walk_count();

        if self.repo.is_shallow() {
            return Err(GitError::ShallowClone);
        }

        let mut history = Vec::new();
        let mut stop = HistoryWalkStop::Complete;
        let head = if let Some(start) = self.start_commit {
            Some(start.to_string())
        } else {
            self.repo.head_commit().ok().map(|c| c.id().to_string())
        };

        let mut walk = if let Some(start) = self.start_commit {
            self.repo.rev_walk([start.as_ref()])
        } else {
            let head = self
                .repo
                .head_commit()
                .map_err(|e| GitError::HeadCommitFailed {
                    source: Box::new(e),
                })?;
            head.id().ancestors()
        };

        if !all_parents {
            walk = walk.first_parent_only();
        } else {
            walk = walk.sorting(gix::revision::walk::Sorting::BreadthFirst);
        }

        let walk = walk.all().map_err(|e| GitError::HistoryWalkFailed {
            source: Box::new(e),
        })?;

        let cutoff_time = max_days.map(|d| {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            now.saturating_sub(d * 86400)
        });

        for res in walk {
            if let Some(b) = budget
                && let Some(early) = b.should_stop()
            {
                stop = early;
                break;
            }
            if history.len() >= max_commits {
                break;
            }

            let info = match res {
                Ok(info) => info,
                Err(e) => {
                    tracing::warn!("Failed to retrieve commit info during history walk: {e}");
                    continue;
                }
            };

            // since_commit check
            if let Some(ref since) = since_commit
                && info.id().to_string().starts_with(since)
            {
                break;
            }

            let commit = match info.id().object().map(|obj| obj.into_commit()) {
                Ok(commit) => commit,
                Err(e) => {
                    tracing::warn!("Failed to retrieve commit object for {}: {e}", info.id());
                    continue;
                }
            };

            // Time-based filtering
            if let Some(cutoff) = cutoff_time {
                let commit_time = commit
                    .time()
                    .map_err(|e| GitError::CommitTimeFailed {
                        source: Box::new(e),
                    })?
                    .seconds;
                if (commit_time as u64) < cutoff {
                    if !all_parents {
                        break;
                    } else {
                        continue;
                    }
                }
            }

            let is_merge = commit.parent_ids().count() > 1;
            let mut files = HashSet::new();
            let mut tree_paths = HashSet::new();

            if !is_merge {
                let current_tree = match commit.tree() {
                    Ok(tree) => tree,
                    Err(e) => {
                        tracing::warn!("Failed to retrieve tree for commit {}: {e}", info.id());
                        continue;
                    }
                };

                let parent_id = commit.parent_ids().next();
                let parent_tree = if let Some(p_id) = parent_id {
                    match p_id.object().map(|obj| obj.into_commit().tree()) {
                        Ok(Ok(tree)) => tree,
                        _ => {
                            tracing::warn!(
                                "Failed to retrieve parent tree for commit {}: parent {}",
                                info.id(),
                                p_id
                            );
                            self.repo.empty_tree()
                        }
                    }
                } else {
                    self.repo.empty_tree()
                };

                let changes =
                    match self
                        .repo
                        .diff_tree_to_tree(Some(&parent_tree), Some(&current_tree), None)
                    {
                        Ok(changes) => changes,
                        Err(e) => {
                            tracing::warn!("Failed to diff tree for commit {}: {e}", info.id());
                            continue;
                        }
                    };

                for change in changes {
                    match change {
                        ChangeDetached::Addition {
                            location,
                            entry_mode,
                            ..
                        }
                        | ChangeDetached::Deletion {
                            location,
                            entry_mode,
                            ..
                        } => {
                            record_leaf_or_tree(
                                &mut files,
                                &mut tree_paths,
                                location.as_ref(),
                                entry_mode,
                            );
                        }
                        ChangeDetached::Modification {
                            location,
                            previous_entry_mode,
                            entry_mode,
                            ..
                        } => {
                            let path = location_path(&location);
                            if previous_entry_mode.is_tree() && entry_mode.is_tree() {
                                tree_paths.insert(path);
                            } else {
                                files.insert(path);
                            }
                        }
                        ChangeDetached::Rewrite {
                            location,
                            source_location,
                            source_entry_mode,
                            entry_mode,
                            ..
                        } => {
                            record_leaf_or_tree(
                                &mut files,
                                &mut tree_paths,
                                source_location.as_ref(),
                                source_entry_mode,
                            );
                            record_leaf_or_tree(
                                &mut files,
                                &mut tree_paths,
                                location.as_ref(),
                                entry_mode,
                            );
                        }
                    }
                }
                prefer_leaf(&files, &mut tree_paths);
            }

            history.push(CommitFileSet {
                files,
                tree_paths,
                is_merge,
                id: Some(info.id().to_string()),
            });
        }

        let commits_walked = history.len();
        Ok(HistoryWalkResult {
            history,
            stop,
            commits_walked,
            head,
        })
    }
}

fn location_path(location: &[u8]) -> Utf8PathBuf {
    Utf8PathBuf::from(String::from_utf8_lossy(location).into_owned())
}

fn record_leaf_or_tree(
    files: &mut HashSet<Utf8PathBuf>,
    trees: &mut HashSet<Utf8PathBuf>,
    location: &[u8],
    mode: gix::objs::tree::EntryMode,
) {
    let path = location_path(location);
    if mode.is_tree() {
        trees.insert(path);
    } else {
        files.insert(path);
    }
}

/// A path recorded as both a leaf and a tree in one commit stays a leaf.
fn prefer_leaf(files: &HashSet<Utf8PathBuf>, trees: &mut HashSet<Utf8PathBuf>) {
    for path in files {
        trees.remove(path);
    }
}

pub struct TemporalEngine<P: HistoryProvider> {
    provider: P,
    config: TemporalConfig,
}

impl<P: HistoryProvider> TemporalEngine<P> {
    pub fn new(provider: P, config: TemporalConfig) -> Self {
        Self { provider, config }
    }

    pub fn calculate_couplings(&self) -> Result<Vec<TemporalCoupling>, GitError> {
        self.calculate_couplings_budgeted(None)
    }

    pub fn calculate_couplings_budgeted(
        &self,
        budget: Option<&AnalysisBudget>,
    ) -> Result<Vec<TemporalCoupling>, GitError> {
        Ok(self
            .calculate_coupling_report_budgeted(budget)?
            .file_couplings)
    }

    /// File pairs, audit pairs, and tree paths from commits the leaf cap kept.
    /// Does not change-filter. An incomplete walk under the 10-commit floor
    /// returns an empty report. A completed walk under that floor is
    /// `InsufficientHistory` with no partial audit list.
    pub fn calculate_coupling_report_budgeted(
        &self,
        budget: Option<&AnalysisBudget>,
    ) -> Result<TemporalCouplingReport, GitError> {
        let walk = self.provider.get_history_budgeted(
            self.config.max_commits,
            None,
            None,
            self.config.all_parents,
            budget,
        )?;
        let walk_stop = walk.stop;
        let history = walk.history;

        let mut commit_count = 0;
        let mut file_weighted_commits: HashMap<Utf8PathBuf, Vec<(usize, f64)>> = HashMap::new();
        let mut audit_weighted_commits: HashMap<Utf8PathBuf, Vec<(usize, f64)>> = HashMap::new();
        let mut kept_trees: BTreeSet<Utf8PathBuf> = BTreeSet::new();
        let mut total_commit_index = 0;

        for commit_set in history {
            if commit_set.is_merge {
                continue;
            }

            if commit_set.files.len() > self.config.max_files_per_commit
                || commit_set.files.is_empty()
            {
                continue;
            }

            // Exponential decay: most recent kept commit gets weight 1.0
            // weight = 2^(-commit_index / half_life)
            let weight = if self.config.decay_half_life > 0 {
                (2.0_f64).powf(-(total_commit_index as f64) / (self.config.decay_half_life as f64))
            } else {
                1.0
            };

            for file in &commit_set.files {
                file_weighted_commits
                    .entry(file.clone())
                    .or_default()
                    .push((total_commit_index, weight));
                audit_weighted_commits
                    .entry(file.clone())
                    .or_default()
                    .push((total_commit_index, weight));
            }
            for tree in &commit_set.tree_paths {
                if commit_set.files.contains(tree) {
                    continue;
                }
                audit_weighted_commits
                    .entry(tree.clone())
                    .or_default()
                    .push((total_commit_index, weight));
                kept_trees.insert(tree.clone());
            }

            commit_count += 1;
            total_commit_index += 1;
        }

        if commit_count < 10 {
            if !matches!(walk_stop, HistoryWalkStop::Complete) {
                return Ok(TemporalCouplingReport::default());
            }
            return Err(GitError::InsufficientHistory {
                found: commit_count,
                required: 10,
            });
        }

        Ok(TemporalCouplingReport {
            file_couplings: couplings_from_weighted(&file_weighted_commits, &self.config),
            audit_couplings: couplings_from_weighted(&audit_weighted_commits, &self.config),
            discarded_tree_paths: kept_trees.into_iter().collect(),
        })
    }
}

fn couplings_from_weighted(
    file_weighted_commits: &HashMap<Utf8PathBuf, Vec<(usize, f64)>>,
    config: &TemporalConfig,
) -> Vec<TemporalCoupling> {
    let mut eligible_files: Vec<Utf8PathBuf> = file_weighted_commits
        .iter()
        .filter(|(_, commits)| commits.len() >= config.min_revisions)
        .map(|(path, _)| path.clone())
        .collect();
    eligible_files.sort_unstable();

    let mut file_total_weight: HashMap<&Utf8PathBuf, f64> = HashMap::new();
    for path in &eligible_files {
        let total: f64 = file_weighted_commits[path].iter().map(|(_, w)| w).sum();
        file_total_weight.insert(path, total);
    }

    let mut couplings = Vec::new();

    for i in 0..eligible_files.len() {
        for j in (i + 1)..eligible_files.len() {
            let file_a = &eligible_files[i];
            let file_b = &eligible_files[j];

            let commits_a = &file_weighted_commits[file_a];
            let commits_b = &file_weighted_commits[file_b];

            let mut shared_weight: f64 = 0.0;
            let mut shared_count: usize = 0;

            let mut ai = 0;
            let mut bi = 0;
            while ai < commits_a.len() && bi < commits_b.len() {
                match commits_a[ai].0.cmp(&commits_b[bi].0) {
                    std::cmp::Ordering::Less => ai += 1,
                    std::cmp::Ordering::Greater => bi += 1,
                    std::cmp::Ordering::Equal => {
                        shared_weight += commits_a[ai].1;
                        shared_count += 1;
                        ai += 1;
                        bi += 1;
                    }
                }
            }

            if shared_count < config.min_shared_commits {
                continue;
            }

            let total_a = file_total_weight[file_a];
            let total_b = file_total_weight[file_b];

            let score_a = if total_a > 0.0 {
                (shared_weight / total_a) as f32
            } else {
                0.0
            };
            let score_b = if total_b > 0.0 {
                (shared_weight / total_b) as f32
            } else {
                0.0
            };

            if score_a > config.coupling_threshold {
                couplings.push(TemporalCoupling {
                    file_a: file_a.clone().into(),
                    file_b: file_b.clone().into(),
                    score: score_a,
                });
            }

            if score_b > config.coupling_threshold {
                couplings.push(TemporalCoupling {
                    file_a: file_b.clone().into(),
                    file_b: file_a.clone().into(),
                    score: score_b,
                });
            }
        }
    }

    couplings.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.file_a.cmp(&b.file_a))
            .then_with(|| a.file_b.cmp(&b.file_b))
    });

    couplings
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn make_config(
        min_shared_commits: usize,
        min_revisions: usize,
        decay_half_life: usize,
        coupling_threshold: f32,
    ) -> TemporalConfig {
        TemporalConfig {
            max_commits: 1000,
            max_files_per_commit: 50,
            coupling_threshold,
            all_parents: false,
            min_shared_commits,
            min_revisions,
            decay_half_life,
        }
    }

    struct MockHistoryProvider {
        commits: Vec<CommitFileSet>,
    }

    impl MockHistoryProvider {
        fn new(commits: Vec<CommitFileSet>) -> Self {
            Self { commits }
        }
    }

    impl HistoryProvider for MockHistoryProvider {
        fn get_history(
            &self,
            _max: usize,
            _days: Option<u64>,
            _since: Option<String>,
            _all: bool,
        ) -> Result<Vec<CommitFileSet>, GitError> {
            Ok(self.commits.clone())
        }
    }

    struct CountingHistoryProvider {
        commits: Vec<CommitFileSet>,
        diffs: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl HistoryProvider for CountingHistoryProvider {
        fn get_history(
            &self,
            max: usize,
            days: Option<u64>,
            since: Option<String>,
            all: bool,
        ) -> Result<Vec<CommitFileSet>, GitError> {
            Ok(self
                .get_history_budgeted(max, days, since, all, None)?
                .history)
        }

        fn get_history_budgeted(
            &self,
            max_commits: usize,
            _days: Option<u64>,
            _since: Option<String>,
            _all: bool,
            budget: Option<&AnalysisBudget>,
        ) -> Result<HistoryWalkResult, GitError> {
            let mut history = Vec::new();
            let mut stop = HistoryWalkStop::Complete;
            for c in self.commits.iter().take(max_commits) {
                if let Some(b) = budget
                    && let Some(early) = b.should_stop()
                {
                    stop = early;
                    break;
                }
                self.diffs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                history.push(c.clone());
            }
            let commits_walked = history.len();
            Ok(HistoryWalkResult {
                history,
                stop,
                commits_walked,
                head: Some("deadbeef".to_string()),
            })
        }
    }

    fn dummy_commits(n: usize) -> Vec<CommitFileSet> {
        (0..n)
            .map(|i| {
                let mut files = HashSet::new();
                files.insert(Utf8PathBuf::from(format!("src/f{i}.rs")));
                CommitFileSet {
                    files,
                    tree_paths: std::collections::HashSet::new(),
                    is_merge: false,
                    id: None,
                }
            })
            .collect()
    }

    #[test]
    fn history_walk_stops_at_deadline_without_further_diffs() {
        let diffs = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let budget = AnalysisBudget::expired(cancel);
        let provider = CountingHistoryProvider {
            commits: dummy_commits(20),
            diffs: std::sync::Arc::clone(&diffs),
        };
        let walk = provider
            .get_history_budgeted(20, None, None, false, Some(&budget))
            .expect("walk");
        assert_eq!(walk.stop, HistoryWalkStop::Budget);
        assert!(walk.commits_walked < 20);
        let after = diffs.load(std::sync::atomic::Ordering::SeqCst);
        assert_eq!(after, walk.commits_walked);
        let again = diffs.load(std::sync::atomic::Ordering::SeqCst);
        assert_eq!(again, after, "no further diffs after return");
    }

    #[test]
    fn history_walk_stops_on_cancel_flag() {
        let diffs = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let budget = AnalysisBudget {
            deadline: None,
            cancel: std::sync::Arc::clone(&cancel),
            budget_secs: None,
        };
        let provider = CountingHistoryProvider {
            commits: dummy_commits(20),
            diffs: std::sync::Arc::clone(&diffs),
        };
        // Flip after the provider would see 2 successful diffs: first two
        // iterations pass, then we set the flag from a wrapper by pre-setting
        // after constructing a mid-walk cancel via a custom loop.
        struct CancelAfterTwo {
            inner: CountingHistoryProvider,
            cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
        }
        impl HistoryProvider for CancelAfterTwo {
            fn get_history(
                &self,
                max: usize,
                days: Option<u64>,
                since: Option<String>,
                all: bool,
            ) -> Result<Vec<CommitFileSet>, GitError> {
                Ok(self
                    .get_history_budgeted(max, days, since, all, None)?
                    .history)
            }
            fn get_history_budgeted(
                &self,
                max_commits: usize,
                _days: Option<u64>,
                _since: Option<String>,
                _all: bool,
                budget: Option<&AnalysisBudget>,
            ) -> Result<HistoryWalkResult, GitError> {
                let mut history = Vec::new();
                let mut stop = HistoryWalkStop::Complete;
                for c in self.inner.commits.iter().take(max_commits) {
                    if let Some(b) = budget
                        && let Some(early) = b.should_stop()
                    {
                        stop = early;
                        break;
                    }
                    self.inner
                        .diffs
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    history.push(c.clone());
                    if history.len() == 2 {
                        self.cancel.store(true, std::sync::atomic::Ordering::SeqCst);
                    }
                }
                let commits_walked = history.len();
                Ok(HistoryWalkResult {
                    history,
                    stop,
                    commits_walked,
                    head: Some("deadbeef".to_string()),
                })
            }
        }
        let walker = CancelAfterTwo {
            inner: provider,
            cancel,
        };
        let walk = walker
            .get_history_budgeted(20, None, None, false, Some(&budget))
            .expect("walk");
        assert_eq!(walk.stop, HistoryWalkStop::Cancelled);
        assert_eq!(walk.commits_walked, 2);
        assert_eq!(
            diffs.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "no further diffs after cancel"
        );
    }

    #[test]
    fn test_coupling_respects_min_shared_commits() {
        // Two files that only share 2 commits (below default threshold of 3)
        // should NOT produce a coupling, even if they appear together often enough
        // in percentage terms.
        let mut commits = Vec::new();

        // Create 10 commits where file_a and file_b share only 2 commits
        // but each file appears in enough commits to pass min_revisions
        for i in 0..10 {
            let mut files = HashSet::new();
            files.insert(Utf8PathBuf::from("src/main.rs"));
            if i < 5 {
                files.insert(Utf8PathBuf::from("src/lib.rs"));
            }
            // file_b only shares commits 0 and 1 with file_a
            if i < 2 {
                files.insert(Utf8PathBuf::from("src/helper.rs"));
            }
            if i >= 5 {
                files.insert(Utf8PathBuf::from("src/extra.rs"));
            }
            commits.push(CommitFileSet {
                files,
                tree_paths: std::collections::HashSet::new(),
                is_merge: false,
                id: None,
            });
        }

        let config = make_config(3, 5, 100, 0.5);
        let provider = MockHistoryProvider::new(commits);
        let engine = TemporalEngine::new(provider, config);

        let couplings = engine.calculate_couplings().unwrap();

        // src/main.rs and src/helper.rs only share 2 commits (< min_shared_commits=3)
        // so no coupling between them should be reported
        let main_helper_couplings: Vec<_> = couplings
            .iter()
            .filter(|c| {
                (c.file_a == path("src/main.rs") && c.file_b == path("src/helper.rs"))
                    || (c.file_a == path("src/helper.rs") && c.file_b == path("src/main.rs"))
            })
            .collect();
        assert!(
            main_helper_couplings.is_empty(),
            "Expected no coupling between main.rs and helper.rs (only 2 shared commits), but found: {:?}",
            main_helper_couplings
        );
    }

    #[test]
    fn test_coupling_respects_min_revisions() {
        // A file that only appears in 3 commits (below default min_revisions of 5)
        // should be excluded from coupling analysis entirely.
        let mut commits = Vec::new();

        for i in 0..10 {
            let mut files = HashSet::new();
            files.insert(Utf8PathBuf::from("src/main.rs"));
            files.insert(Utf8PathBuf::from("src/lib.rs"));
            // rare.rs only appears in 3 commits
            if i < 3 {
                files.insert(Utf8PathBuf::from("src/rare.rs"));
            }
            commits.push(CommitFileSet {
                files,
                tree_paths: std::collections::HashSet::new(),
                is_merge: false,
                id: None,
            });
        }

        let config = make_config(3, 5, 100, 0.5);
        let provider = MockHistoryProvider::new(commits);
        let engine = TemporalEngine::new(provider, config);

        let couplings = engine.calculate_couplings().unwrap();

        // rare.rs should not appear in any coupling because it only has 3 revisions (< min_revisions=5)
        let rare_couplings: Vec<_> = couplings
            .iter()
            .filter(|c| c.file_a == path("src/rare.rs") || c.file_b == path("src/rare.rs"))
            .collect();
        assert!(
            rare_couplings.is_empty(),
            "Expected no coupling involving rare.rs (only 3 revisions, below min_revisions=5), but found: {:?}",
            rare_couplings
        );
    }

    #[test]
    fn test_recent_shared_commits_score_higher_than_old() {
        // Two file pairs: (recent, old) share the same raw count of commits,
        // but recent shared commits should produce a higher coupling score
        // due to exponential decay weighting.
        let mut commits = Vec::new();

        // Commits 0-4: file_a + file_recent change together (recent)
        // Commits 5-9: file_a changes alone
        // Commits 10-19: file_a + file_old change together (old)
        // Commits 20-29: file_a changes alone
        // Both file_recent and file_old share 5 commits with file_a.
        // But file_recent's shared commits are more recent (indices 0-4),
        // so its weighted shared score should be higher.
        for _ in 0..5 {
            let mut files = HashSet::new();
            files.insert(Utf8PathBuf::from("src/a.rs"));
            files.insert(Utf8PathBuf::from("src/recent.rs"));
            commits.push(CommitFileSet {
                files,
                tree_paths: std::collections::HashSet::new(),
                is_merge: false,
                id: None,
            });
        }
        for _ in 0..5 {
            let mut files = HashSet::new();
            files.insert(Utf8PathBuf::from("src/a.rs"));
            commits.push(CommitFileSet {
                files,
                tree_paths: std::collections::HashSet::new(),
                is_merge: false,
                id: None,
            });
        }
        for _ in 0..5 {
            let mut files = HashSet::new();
            files.insert(Utf8PathBuf::from("src/a.rs"));
            files.insert(Utf8PathBuf::from("src/old.rs"));
            commits.push(CommitFileSet {
                files,
                tree_paths: std::collections::HashSet::new(),
                is_merge: false,
                id: None,
            });
        }
        for _ in 0..5 {
            let mut files = HashSet::new();
            files.insert(Utf8PathBuf::from("src/a.rs"));
            commits.push(CommitFileSet {
                files,
                tree_paths: std::collections::HashSet::new(),
                is_merge: false,
                id: None,
            });
        }

        // Use low thresholds so both pairs are included
        let config = make_config(1, 3, 50, 0.2);
        let provider = MockHistoryProvider::new(commits);
        let engine = TemporalEngine::new(provider, config);

        let couplings = engine.calculate_couplings().unwrap();

        // Find a→recent and a→old directional couplings
        let recent_score = couplings
            .iter()
            .find(|c| c.file_a == path("src/a.rs") && c.file_b == path("src/recent.rs"))
            .map(|c| c.score);
        let old_score = couplings
            .iter()
            .find(|c| c.file_a == path("src/a.rs") && c.file_b == path("src/old.rs"))
            .map(|c| c.score);

        // Both should exist
        assert!(recent_score.is_some(), "Expected a→recent coupling");
        assert!(old_score.is_some(), "Expected a→old coupling");

        // Recent shared commits should score higher than old shared commits
        assert!(
            recent_score.unwrap() > old_score.unwrap(),
            "Recent shared commits should produce higher coupling score: recent={:?}, old={:?}",
            recent_score,
            old_score
        );
    }

    fn path(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    fn has_pair(couplings: &[TemporalCoupling], a: &str, b: &str) -> bool {
        couplings.iter().any(|c| {
            (c.file_a == path(a) && c.file_b == path(b))
                || (c.file_a == path(b) && c.file_b == path(a))
        })
    }

    fn path_set(paths: &[&str]) -> HashSet<Utf8PathBuf> {
        paths.iter().copied().map(Utf8PathBuf::from).collect()
    }

    #[test]
    fn leaf_cap_keeps_file_pairs_and_audits_tree_partners() {
        let mut commits = Vec::new();
        for _ in 0..10 {
            commits.push(CommitFileSet {
                files: path_set(&["a/b/c.rs", "a/b/d.rs"]),
                tree_paths: path_set(&["a", "a/b"]),
                is_merge: false,
                id: None,
            });
        }
        commits.push(CommitFileSet {
            files: path_set(&["w.rs", "x.rs", "y.rs", "z.rs"]),
            tree_paths: path_set(&["only-over-cap"]),
            is_merge: false,
            id: None,
        });

        let mut config = make_config(1, 1, 0, 0.1);
        config.max_files_per_commit = 3;
        let engine = TemporalEngine::new(MockHistoryProvider::new(commits), config);
        let report = engine
            .calculate_coupling_report_budgeted(None)
            .expect("report");

        assert!(
            has_pair(&report.file_couplings, "a/b/c.rs", "a/b/d.rs"),
            "leaf pair missing from file list: {:?}",
            report.file_couplings
        );
        assert!(
            !has_pair(&report.file_couplings, "a/b/c.rs", "a"),
            "tree partner leaked into file pairs: {:?}",
            report.file_couplings
        );
        assert!(
            has_pair(&report.audit_couplings, "a", "a/b"),
            "tree pair missing from audit: {:?}",
            report.audit_couplings
        );
        assert!(
            has_pair(&report.audit_couplings, "a/b/c.rs", "a"),
            "leaf-tree pair missing from audit: {:?}",
            report.audit_couplings
        );
        for leaf in ["w.rs", "x.rs", "y.rs", "z.rs"] {
            assert!(
                report
                    .file_couplings
                    .iter()
                    .chain(report.audit_couplings.iter())
                    .all(|c| c.file_a != path(leaf) && c.file_b != path(leaf)),
                "over-cap commit contributed {leaf}"
            );
        }
        assert!(
            !report
                .discarded_tree_paths
                .iter()
                .any(|p| p.as_str() == "only-over-cap"),
            "over-cap tree path must not be recorded: {:?}",
            report.discarded_tree_paths
        );
        assert!(
            report
                .discarded_tree_paths
                .iter()
                .any(|p| p.as_str() == "a")
        );
        assert!(report.discarded_tree_paths.windows(2).all(|w| w[0] <= w[1]));
    }

    #[test]
    fn incomplete_walk_under_floor_returns_empty_report() {
        let diffs = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let budget = AnalysisBudget::expired(cancel);
        let provider = CountingHistoryProvider {
            commits: dummy_commits(20),
            diffs: std::sync::Arc::clone(&diffs),
        };
        let engine = TemporalEngine::new(provider, make_config(1, 1, 0, 0.1));
        let report = engine
            .calculate_coupling_report_budgeted(Some(&budget))
            .expect("incomplete walk is an empty report, not InsufficientHistory");
        assert!(report.file_couplings.is_empty());
        assert!(report.audit_couplings.is_empty());
        assert!(report.discarded_tree_paths.is_empty());
    }

    #[test]
    fn gix_history_classifies_trees_links_and_gitlinks() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = gix::init(dir.path()).expect("init");
        let sig = gix::actor::SignatureRef::from_bytes(b"Tester <t@example.com> 1700000000 +0000")
            .expect("signature");
        let mode_blob = gix::objs::tree::EntryMode::from(gix::objs::tree::EntryKind::Blob);
        let mode_tree = gix::objs::tree::EntryMode::from(gix::objs::tree::EntryKind::Tree);
        let mode_link = gix::objs::tree::EntryMode::from(gix::objs::tree::EntryKind::Link);
        let mode_commit = gix::objs::tree::EntryMode::from(gix::objs::tree::EntryKind::Commit);

        let blob_c = repo.write_blob(b"one").expect("blob c").detach();
        let tree_b = gix::objs::Tree {
            entries: vec![gix::objs::tree::Entry {
                mode: mode_blob,
                filename: "c.rs".into(),
                oid: blob_c,
            }],
        };
        let tree_b_id = repo.write_object(&tree_b).expect("tree b").detach();
        let tree_a = gix::objs::Tree {
            entries: vec![gix::objs::tree::Entry {
                mode: mode_tree,
                filename: "b".into(),
                oid: tree_b_id,
            }],
        };
        let tree_a_id = repo.write_object(&tree_a).expect("tree a").detach();
        let root1 = gix::objs::Tree {
            entries: vec![gix::objs::tree::Entry {
                mode: mode_tree,
                filename: "a".into(),
                oid: tree_a_id,
            }],
        };
        let root1_id = repo.write_object(&root1).expect("root1").detach();
        let first = repo
            .commit_as(
                sig,
                sig,
                "HEAD",
                "add nested blob\n",
                root1_id,
                std::iter::empty::<gix::ObjectId>(),
            )
            .expect("commit 1")
            .detach();

        let blob_d = repo.write_blob(b"two").expect("blob d").detach();
        let blob_link = repo.write_blob(b"target").expect("link blob").detach();
        let tree_b2 = gix::objs::Tree {
            entries: vec![gix::objs::tree::Entry {
                mode: mode_blob,
                filename: "d.rs".into(),
                oid: blob_d,
            }],
        };
        let tree_b2_id = repo.write_object(&tree_b2).expect("tree b2").detach();
        let tree_a2 = gix::objs::Tree {
            entries: vec![gix::objs::tree::Entry {
                mode: mode_tree,
                filename: "b".into(),
                oid: tree_b2_id,
            }],
        };
        let tree_a2_id = repo.write_object(&tree_a2).expect("tree a2").detach();
        let mut root_entries = vec![
            gix::objs::tree::Entry {
                mode: mode_tree,
                filename: "a".into(),
                oid: tree_a2_id,
            },
            gix::objs::tree::Entry {
                mode: mode_link,
                filename: "link".into(),
                oid: blob_link,
            },
            gix::objs::tree::Entry {
                mode: mode_commit,
                filename: "sub".into(),
                oid: blob_c,
            },
        ];
        root_entries.sort();
        let root2 = gix::objs::Tree {
            entries: root_entries,
        };
        let root2_id = repo.write_object(&root2).expect("root2").detach();
        repo.commit_as(
            sig,
            sig,
            "HEAD",
            "delete blob and add leaf modes\n",
            root2_id,
            [first],
        )
        .expect("commit 2");

        let history = GixHistoryProvider::new(&repo)
            .get_history(10, None, None, false)
            .expect("history");
        let deletion = history.first().expect("newest commit is the deletion");
        for leaf in ["a/b/c.rs", "a/b/d.rs", "link", "sub"] {
            assert!(
                deletion.files.contains(&Utf8PathBuf::from(leaf)),
                "deletion commit missing leaf {leaf} in {:?}",
                deletion.files
            );
        }
        for tree in ["a", "a/b"] {
            assert!(
                deletion.tree_paths.contains(&Utf8PathBuf::from(tree)),
                "deletion commit missing tree {tree} in {:?}",
                deletion.tree_paths
            );
            assert!(
                !deletion.files.contains(&Utf8PathBuf::from(tree)),
                "deletion commit leaked tree {tree} into {:?}",
                deletion.files
            );
        }
        let files: HashSet<Utf8PathBuf> = history
            .iter()
            .flat_map(|c| c.files.iter().cloned())
            .collect();
        let trees: HashSet<Utf8PathBuf> = history
            .iter()
            .flat_map(|c| c.tree_paths.iter().cloned())
            .collect();
        for leaf in ["a/b/c.rs", "a/b/d.rs", "link", "sub"] {
            assert!(
                files.contains(&Utf8PathBuf::from(leaf)),
                "missing leaf {leaf} in {files:?}"
            );
        }
        for tree in ["a", "a/b"] {
            assert!(
                trees.contains(&Utf8PathBuf::from(tree)),
                "missing tree {tree} in {trees:?}; files={files:?}"
            );
            assert!(
                !files.contains(&Utf8PathBuf::from(tree)),
                "tree {tree} leaked into files {files:?}"
            );
        }
    }
}
