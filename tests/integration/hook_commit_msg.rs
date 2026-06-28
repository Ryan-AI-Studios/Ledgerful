use ledgerful::commands::hook_commit_msg::{
    canonical_entity, extract_trailers, is_trivial_commit, parse_category_from_message,
};
use ledgerful::ledger::Category;

#[test]
fn test_category_inference_covers_all_prefixes() {
    assert_eq!(
        parse_category_from_message("feat: add something"),
        Category::Feature
    );
    assert_eq!(
        parse_category_from_message("fix: fix bug"),
        Category::Bugfix
    );
    assert_eq!(
        parse_category_from_message("bug: fix another bug"),
        Category::Bugfix
    );
    assert_eq!(
        parse_category_from_message("docs: update readme"),
        Category::Docs
    );
    assert_eq!(
        parse_category_from_message("refactor: clean up"),
        Category::Refactor
    );
    assert_eq!(
        parse_category_from_message("perf: make it faster"),
        Category::Refactor
    );
    assert_eq!(
        parse_category_from_message("chore: cleanup"),
        Category::Chore
    );
    assert_eq!(
        parse_category_from_message("ci: update workflow"),
        Category::Infra
    );
    assert_eq!(
        parse_category_from_message("infra: update server"),
        Category::Infra
    );
    assert_eq!(
        parse_category_from_message("build: compile fix"),
        Category::Infra
    );
    assert_eq!(
        parse_category_from_message("style: format code"),
        Category::Tooling
    );
    assert_eq!(
        parse_category_from_message("revert: undo last"),
        Category::Bugfix
    );
    assert_eq!(
        parse_category_from_message("security: fix vulnerability"),
        Category::Security
    );
    assert_eq!(
        parse_category_from_message("breaking: major change"),
        Category::Architecture
    );
    assert_eq!(
        parse_category_from_message("random: no prefix"),
        Category::Chore
    );
}

#[test]
fn test_multi_file_entity_canonical_path() {
    let files = vec![
        "src/ledger/mod.rs".to_string(),
        "src/ledger/types.rs".to_string(),
    ];
    assert_eq!(canonical_entity(&files), "src/ledger");

    let files = vec![
        "src/ledger/mod.rs".to_string(),
        "src/commands/mod.rs".to_string(),
    ];
    assert_eq!(canonical_entity(&files), "src");

    let files = vec![
        "src/ledger/mod.rs".to_string(),
        "docs/README.md".to_string(),
    ];
    assert_eq!(canonical_entity(&files), "src/ledger/mod.rs (+1 more)");
}

#[test]
fn test_trivial_bypass_skips_tui() {
    assert!(is_trivial_commit("chore: cleanup"));
    assert!(is_trivial_commit("docs: update"));
    assert!(is_trivial_commit("style: format"));
    assert!(is_trivial_commit("test: add tests"));
    assert!(!is_trivial_commit("feat: new feature"));
}

#[test]
fn test_trailer_preservation() {
    let msg = "feat: add feature\n\nThis adds a new feature.\n\nSigned-off-by: Alice <alice@example.com>\nCo-authored-by: Bob <bob@example.com>";
    let trailers = extract_trailers(msg);
    assert!(trailers.contains("Signed-off-by: Alice <alice@example.com>"));
    assert!(trailers.contains("Co-authored-by: Bob <bob@example.com>"));

    let msg_no_trailers = "feat: add feature\n\nThis adds a new feature.";
    assert_eq!(extract_trailers(msg_no_trailers), "");
}

#[test]
fn test_non_interactive_bypasses_tui() {
    // This would test the environment variable check in execute_hook_commit_msg
    // but requires full command execution setup.
}
