use crate::config::model::TemporalConfig;
use crate::git::GitError;
use crate::impact::packet::TemporalCoupling;
use camino::Utf8PathBuf;
use gix::Repository;
use gix::object::tree::diff::ChangeDetached;
use miette::Result;
use std::collections::{HashMap, HashSet};

#[derive(Clone)]
pub struct CommitFileSet {
    pub files: HashSet<Utf8PathBuf>,
    pub is_merge: bool,
}

pub trait HistoryProvider {
    fn get_history(
        &self,
        max_commits: usize,
        max_days: Option<u64>,
        since_commit: Option<String>,
        all_parents: bool,
    ) -> Result<Vec<CommitFileSet>, GitError>;
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
        if self.repo.is_shallow() {
            return Err(GitError::ShallowClone);
        }

        let mut history = Vec::new();
        let mut walk = if let Some(start) = self.start_commit {
            self.repo.rev_walk([start.as_ref()])
        } else {
            let head = self
                .repo
                .head_commit()
                .map_err(|e| GitError::MetadataError { source: e.into() })?;
            head.id().ancestors()
        };

        if !all_parents {
            walk = walk.first_parent_only();
        } else {
            walk = walk.sorting(gix::revision::walk::Sorting::BreadthFirst);
        }

        let walk = walk
            .all()
            .map_err(|e| GitError::MetadataError { source: e.into() })?;

        let cutoff_time = max_days.map(|d| {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            now.saturating_sub(d * 86400)
        });

        for res in walk {
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
                    .map_err(|e| GitError::MetadataError { source: e.into() })?
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
                        ChangeDetached::Addition { location, .. }
                        | ChangeDetached::Deletion { location, .. }
                        | ChangeDetached::Modification { location, .. } => {
                            files.insert(Utf8PathBuf::from(
                                String::from_utf8_lossy(&location).into_owned(),
                            ));
                        }
                        ChangeDetached::Rewrite {
                            location,
                            source_location,
                            ..
                        } => {
                            files.insert(Utf8PathBuf::from(
                                String::from_utf8_lossy(&location).into_owned(),
                            ));
                            files.insert(Utf8PathBuf::from(
                                String::from_utf8_lossy(&source_location).into_owned(),
                            ));
                        }
                    }
                }
            }

            history.push(CommitFileSet { files, is_merge });
        }

        Ok(history)
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
        let history = self.provider.get_history(
            self.config.max_commits,
            None,
            None,
            self.config.all_parents,
        )?;

        let mut commit_count = 0;
        // Store (commit_index, weight) for each file
        let mut file_weighted_commits: HashMap<Utf8PathBuf, Vec<(usize, f64)>> = HashMap::new();
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

            // Exponential decay: most recent commit gets weight 1.0
            // weight = 2^(-commit_index / half_life)
            let weight = if self.config.decay_half_life > 0 {
                (2.0_f64).powf(-(total_commit_index as f64) / (self.config.decay_half_life as f64))
            } else {
                1.0 // no decay if half_life is 0 (edge case)
            };

            for file in commit_set.files {
                file_weighted_commits
                    .entry(file)
                    .or_default()
                    .push((total_commit_index, weight));
            }

            commit_count += 1;
            total_commit_index += 1;
        }

        if commit_count < 10 {
            return Err(GitError::InsufficientHistory {
                found: commit_count,
                required: 10,
            });
        }

        // Filter files below min_revisions threshold
        let mut eligible_files: Vec<Utf8PathBuf> = file_weighted_commits
            .iter()
            .filter(|(_, commits)| commits.len() >= self.config.min_revisions)
            .map(|(path, _)| path.clone())
            .collect();
        eligible_files.sort_unstable();

        // Build per-file weighted totals for normalization
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

                // Calculate weighted shared commits using merge-join on sorted commit indices
                let mut shared_weight: f64 = 0.0;
                let mut shared_count: usize = 0;

                let mut ai = 0;
                let mut bi = 0;
                while ai < commits_a.len() && bi < commits_b.len() {
                    match commits_a[ai].0.cmp(&commits_b[bi].0) {
                        std::cmp::Ordering::Less => ai += 1,
                        std::cmp::Ordering::Greater => bi += 1,
                        std::cmp::Ordering::Equal => {
                            // Same commit — use the weight from file_a (same commit, same weight)
                            shared_weight += commits_a[ai].1;
                            shared_count += 1;
                            ai += 1;
                            bi += 1;
                        }
                    }
                }

                // Apply minimum shared commits threshold
                if shared_count < self.config.min_shared_commits {
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

                if score_a > self.config.coupling_threshold {
                    couplings.push(TemporalCoupling {
                        file_a: file_a.clone().into(),
                        file_b: file_b.clone().into(),
                        score: score_a,
                    });
                }

                if score_b > self.config.coupling_threshold {
                    couplings.push(TemporalCoupling {
                        file_a: file_b.clone().into(),
                        file_b: file_a.clone().into(),
                        score: score_b,
                    });
                }
            }
        }

        // Deterministic sorting by score (desc) then paths
        couplings.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.file_a.cmp(&b.file_a))
                .then_with(|| a.file_b.cmp(&b.file_b))
        });

        Ok(couplings)
    }
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
                is_merge: false,
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
                is_merge: false,
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
                is_merge: false,
            });
        }
        for _ in 0..5 {
            let mut files = HashSet::new();
            files.insert(Utf8PathBuf::from("src/a.rs"));
            commits.push(CommitFileSet {
                files,
                is_merge: false,
            });
        }
        for _ in 0..5 {
            let mut files = HashSet::new();
            files.insert(Utf8PathBuf::from("src/a.rs"));
            files.insert(Utf8PathBuf::from("src/old.rs"));
            commits.push(CommitFileSet {
                files,
                is_merge: false,
            });
        }
        for _ in 0..5 {
            let mut files = HashSet::new();
            files.insert(Utf8PathBuf::from("src/a.rs"));
            commits.push(CommitFileSet {
                files,
                is_merge: false,
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
}
