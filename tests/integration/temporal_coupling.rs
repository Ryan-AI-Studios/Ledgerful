use camino::Utf8PathBuf;
use ledgerful::config::model::TemporalConfig;
use ledgerful::git::GitError;
use ledgerful::impact::temporal::{
    CommitFileSet, GixHistoryProvider, HistoryProvider, TemporalEngine,
};
use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::tempdir;

struct MockHistoryProvider {
    history: Vec<CommitFileSet>,
}

impl HistoryProvider for MockHistoryProvider {
    fn get_history(
        &self,
        _max_commits: usize,
        _max_days: Option<u64>,
        _since: Option<String>,
        _all_parents: bool,
    ) -> Result<Vec<CommitFileSet>, GitError> {
        Ok(self.history.clone())
    }
}

#[test]
fn test_temporal_coupling_logic() {
    let mut history = Vec::new();

    // File A and B changed together in 10 commits
    for _ in 0..10 {
        let mut files = HashSet::new();
        files.insert(Utf8PathBuf::from("src/a.rs"));
        files.insert(Utf8PathBuf::from("src/b.rs"));
        history.push(CommitFileSet {
            files,
            is_merge: false,
        });
    }

    let config = TemporalConfig {
        max_commits: 100,
        max_files_per_commit: 50,
        coupling_threshold: 0.8,
        all_parents: false,
        min_shared_commits: 3,
        min_revisions: 5,
        decay_half_life: 100,
    };

    let provider = MockHistoryProvider { history };
    let engine = TemporalEngine::new(provider, config);
    let couplings = engine.calculate_couplings().unwrap();

    // With decay_half_life=100 and 10 commits, the weighted scores will be
    // slightly less than 1.0 but still well above 0.8
    assert_eq!(couplings.len(), 2);
    // Both directions should have scores very close to 1.0
    assert!(couplings[0].score > 0.95);
    assert!(couplings[1].score > 0.95);
}

#[test]
fn test_temporal_coupling_threshold() {
    let mut history = Vec::new();

    // File A changes in 10 commits
    // File B changes in 5 of those commits
    for i in 0..10 {
        let mut files = HashSet::new();
        files.insert(Utf8PathBuf::from("src/a.rs"));
        if i < 5 {
            files.insert(Utf8PathBuf::from("src/b.rs"));
        }
        history.push(CommitFileSet {
            files,
            is_merge: false,
        });
    }

    let config = TemporalConfig {
        max_commits: 100,
        max_files_per_commit: 50,
        coupling_threshold: 0.6,
        all_parents: false,
        min_shared_commits: 3,
        min_revisions: 3,
        decay_half_life: 100,
    };

    let provider = MockHistoryProvider { history };
    let engine = TemporalEngine::new(provider, config);
    let couplings = engine.calculate_couplings().unwrap();

    // With decay, A->B score is ~0.506 (below 0.6), B->A score is ~1.0 (above 0.6)
    assert_eq!(couplings.len(), 1);
    assert_eq!(couplings[0].file_a.to_str().unwrap(), "src/b.rs");
    assert_eq!(couplings[0].file_b.to_str().unwrap(), "src/a.rs");
    assert!(couplings[0].score > 0.6);
}

#[test]
fn test_insufficient_history_error() {
    let mut history = Vec::new();
    for _ in 0..5 {
        history.push(CommitFileSet {
            files: HashSet::new(),
            is_merge: false,
        });
    }

    let config = TemporalConfig::default();
    let provider = MockHistoryProvider { history };
    let engine = TemporalEngine::new(provider, config);
    let result = engine.calculate_couplings();

    assert!(matches!(result, Err(GitError::InsufficientHistory { .. })));
}

#[test]
fn test_gix_history_provider_uses_first_parent_by_default() {
    let tmp = tempdir().unwrap();
    run_git(tmp.path(), &["init"]);
    run_git(tmp.path(), &["config", "user.email", "test@example.com"]);
    run_git(tmp.path(), &["config", "user.name", "Ledgerful Test"]);

    fs::write(tmp.path().join("main.txt"), "0\n").unwrap();
    run_git(tmp.path(), &["add", "."]);
    run_git(tmp.path(), &["commit", "-m", "initial"]);

    for i in 1..12 {
        fs::write(tmp.path().join("main.txt"), format!("{i}\n")).unwrap();
        run_git(tmp.path(), &["add", "."]);
        run_git(tmp.path(), &["commit", "-m", &format!("main {i}")]);
    }

    run_git(tmp.path(), &["checkout", "-b", "side"]);
    fs::write(tmp.path().join("side.txt"), "side\n").unwrap();
    run_git(tmp.path(), &["add", "."]);
    run_git(tmp.path(), &["commit", "-m", "side"]);

    run_git(tmp.path(), &["checkout", "master"]);
    run_git(
        tmp.path(),
        &["merge", "--no-ff", "side", "-m", "merge side"],
    );

    let repo = gix::discover(tmp.path()).unwrap();
    let provider = GixHistoryProvider::new(&repo);

    let first_parent = provider.get_history(50, None, None, false).unwrap();
    let all_parents = provider.get_history(50, None, None, true).unwrap();

    assert!(
        first_parent
            .iter()
            .all(|commit| !commit.files.contains(&Utf8PathBuf::from("side.txt"))),
        "first-parent traversal should not include side-branch-only commits"
    );
    assert!(
        all_parents
            .iter()
            .any(|commit| commit.files.contains(&Utf8PathBuf::from("side.txt"))),
        "all-parent traversal should include side-branch commits"
    );
}

fn run_git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}
