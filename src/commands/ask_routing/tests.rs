use super::*;
use crate::search::tantivy_engine::{SearchResult, TantivySearchEngine};
use crate::state::layout::Layout;
use crate::state::storage::StorageManager;
use tempfile::tempdir;

#[test]
fn test_parse_intent_recognizes_callers_phrasings() {
    assert!(matches!(
        parse_intent("what calls remove_snippets_for_files"),
        Some(ExactIntent::CallersOf(t)) if t == "remove_snippets_for_files"
    ));
    assert!(matches!(
        parse_intent("who calls remove_snippets_for_files"),
        Some(ExactIntent::CallersOf(t)) if t == "remove_snippets_for_files"
    ));
    assert!(matches!(
        parse_intent("show callers of remove_snippets_for_files"),
        Some(ExactIntent::CallersOf(t)) if t == "remove_snippets_for_files"
    ));
}

#[test]
fn test_parse_intent_recognizes_callees_and_routes() {
    assert!(matches!(
        parse_intent("what does execute_ask call"),
        Some(ExactIntent::CalleesOf(t)) if t == "execute_ask"
    ));
    assert!(matches!(
        parse_intent("list route handlers"),
        Some(ExactIntent::ListRoutes)
    ));
    assert!(matches!(
        parse_intent("which handler owns route /api/users"),
        Some(ExactIntent::RouteOwner(t)) if t == "/api/users"
    ));
    assert!(matches!(
        parse_intent("where is symbol execute_ask defined"),
        Some(ExactIntent::SymbolDefinition(t)) if t == "execute_ask"
    ));
}

#[test]
fn test_parse_intent_returns_none_for_narrative_questions() {
    assert!(parse_intent("what should I refactor in this module?").is_none());
    assert!(parse_intent("give me an overview of this codebase").is_none());
}

// --- 0142 local grounding / locate parse ---

#[test]
fn test_parse_intent_local_grounding_find_locate_positives() {
    let positives = [
        ("find the function verify_step_key", "verify_step_key"),
        ("find verify_step_key", "verify_step_key"),
        ("locate verify_step_key", "verify_step_key"),
        ("find the fn foo", "foo"),
        ("find FooBar", "FooBar"),
        ("locate the function verify_step_key", "verify_step_key"),
        ("find symbol run_with", "run_with"),
        ("find verify_step_key in the codebase", "verify_step_key"),
        ("find the method process_batch", "process_batch"),
        ("locate struct Layout", "Layout"),
    ];
    for (q, expected) in positives {
        assert!(
            matches!(
                parse_intent(q),
                Some(ExactIntent::SymbolDefinition(ref t)) if t == expected
            ),
            "expected SymbolDefinition({expected}) for: {q}, got {:?}",
            parse_intent(q)
        );
    }
}

#[test]
fn test_parse_intent_local_grounding_negatives() {
    // No `where is X` without "defined"
    assert!(parse_intent("where is the config").is_none());
    assert!(parse_intent("where is the README").is_none());
    assert!(parse_intent("where is Cargo.toml").is_none());

    // Conceptual / narrative
    assert!(parse_intent("how does verify_step_key work").is_none());
    assert!(parse_intent("find the best way to handle errors").is_none());
    assert!(parse_intent("explain the architecture of indexing").is_none());
    assert!(parse_intent("how to implement verify_step_key").is_none());
    assert!(parse_intent("why does verify_step_key exist").is_none());
    assert!(parse_intent("find the architecture overview").is_none());
    assert!(parse_intent("find the best implementation").is_none());

    // ProductDocs / CG-F31 phrases must not parse as SymbolDefinition (wire owns them).
    assert!(parse_intent("What is the Daily 5?").is_none());
    assert!(parse_intent("what are the session start commands?").is_none());
    assert!(parse_intent("what commands show repo health").is_none());
}

#[test]
fn test_parse_intent_local_grounding_list_routes_collision() {
    // ListRoutes must win over bare find (parse order load-bearing).
    assert!(
        matches!(
            parse_intent("find all axum route handlers"),
            Some(ExactIntent::ListRoutes)
        ),
        "expected ListRoutes, got {:?}",
        parse_intent("find all axum route handlers")
    );
}

#[test]
fn test_local_grounding_banners_locked_strings() {
    assert_eq!(
        LOCAL_GROUNDING_SEARCH_BANNER,
        "Local grounding query resolved via index/search."
    );
    assert_eq!(
        LOCAL_GROUNDING_MISS_BANNER,
        "Local grounding found no index/search hits."
    );
}

#[test]
fn test_format_local_grounding_miss_body_remediation() {
    let body = format_local_grounding_miss("verify_step_key");
    assert!(
        body.contains("no hits for `verify_step_key`"),
        "must state local zero hits:\n{body}"
    );
    assert!(
        body.contains("ledgerful search \"verify_step_key\" --auto-index")
            || body.contains("ledgerful search") && body.contains("--auto-index"),
        "must recommend ledgerful search --auto-index:\n{body}"
    );
    assert!(
        body.contains("ledgerful search"),
        "must name ledgerful search:\n{body}"
    );
    let lower = body.to_lowercase();
    // Banned as primary advice: bare grep/rg without ledgerful search.
    // Presence of ledgerful search is required; bare-grep-primary is not allowed.
    assert!(
        !lower.contains("use grep")
            && !lower.contains("use rg")
            && !lower.contains("run grep")
            && !lower.contains("run rg"),
        "must not recommend bare grep/rg as primary:\n{body}"
    );
}

#[test]
fn test_format_search_evidence_body_shape() {
    let hits = vec![SearchResult {
        path: "src/verify/probability.rs".into(),
        line_count: 10,
        score: 1.0,
        snippet: Some("fn verify_step_key() { /* body */ }".into()),
        highlight_ranges: None,
        line_number: Some(45),
    }];
    let body = format_search_evidence("verify_step_key", &hits);
    assert!(
        body.starts_with("Search evidence for `verify_step_key`:"),
        "header:\n{body}"
    );
    assert!(
        body.contains("- src/verify/probability.rs (line 45):"),
        "path+line:\n{body}"
    );
    assert!(body.contains("verify_step_key"), "snippet:\n{body}");

    // Missing line → `?`
    let hits_no_line = vec![SearchResult {
        path: "src/foo.rs".into(),
        line_count: 1,
        score: 1.0,
        snippet: Some("x".into()),
        highlight_ranges: None,
        line_number: None,
    }];
    let body2 = format_search_evidence("x", &hits_no_line);
    assert!(
        body2.contains("(line ?):"),
        "missing line must use `?`:\n{body2}"
    );

    // Snippet truncated ≤ 120 chars
    let long = "a".repeat(200);
    let hits_long = vec![SearchResult {
        path: "p.rs".into(),
        line_count: 1,
        score: 1.0,
        snippet: Some(long),
        highlight_ranges: None,
        line_number: Some(1),
    }];
    let body3 = format_search_evidence("a", &hits_long);
    let snippet_part = body3
        .lines()
        .nth(1)
        .and_then(|l| l.split(": ").nth(1))
        .unwrap_or("");
    assert!(
        snippet_part.chars().count() <= 120,
        "snippet must be ≤120 chars, got {}",
        snippet_part.chars().count()
    );
}

#[test]
fn test_residual_wording_forbids_overclaim_phrases() {
    // Global empty-chunk note (execute.rs) and CodebaseFocus oracle (local_model).
    let execute_src = include_str!("../ask/execute.rs");
    let context_src = include_str!("../../local_model/context.rs");
    assert!(
        execute_src.contains("no retrieved snippets for this query"),
        "execute.rs must use residual snippets wording"
    );
    assert!(
        !execute_src.contains("no project context available"),
        "execute.rs must not overclaim no project context"
    );
    assert!(
        context_src.contains("answering without retrieved snippets"),
        "local_model context must use residual snippets wording"
    );
    assert!(
        !context_src.contains("answering without codebase context"),
        "local_model context must not overclaim without codebase context"
    );
}

#[test]
fn test_search_symbol_secondary_term_exact_format() {
    // tempdir + TantivySearchEngine + index_doc (not seeded_storage alone).
    use crate::search::trigram::extract_trigrams;
    use tantivy::TantivyDocument;

    let tmp = tempdir().expect("tempdir");
    let root = camino::Utf8Path::from_path(tmp.path()).expect("utf8");
    let layout = Layout::new(root);
    layout.ensure_state_dir().expect("state dir");

    let engine = TantivySearchEngine::open_or_create(layout.search_index_dir().as_std_path())
        .expect("engine");
    {
        let schema = engine.schema();
        let path_field = schema.get_field("path").expect("path");
        let content_field = schema.get_field("content").expect("content");
        let line_count_field = schema.get_field("line_count").expect("line_count");
        let trigrams_field = schema.get_field("trigrams").expect("trigrams");
        let content = "fn verify_step_key() { /* 0142 secondary */ }";
        let tgrams_str = extract_trigrams(content)
            .into_iter()
            .collect::<Vec<_>>()
            .join(" ");
        let mut writer = engine.get_writer(15_000_000).expect("writer");
        let mut doc = TantivyDocument::default();
        doc.add_text(path_field, "src/verify/probability.rs");
        doc.add_text(content_field, content);
        doc.add_u64(line_count_field, 1);
        doc.add_text(trigrams_field, &tgrams_str);
        writer.add_document(doc).expect("add");
        writer.commit().expect("commit");
        engine.reload_reader().expect("reload");
    }

    let hits = search_symbol_secondary(&layout, "verify_step_key");
    assert!(
        !hits.is_empty(),
        "secondary TermQuery must hit dual-emitted full id"
    );
    let body = format_search_evidence("verify_step_key", &hits);
    assert!(
        body.contains("Search evidence for `verify_step_key`:"),
        "header:\n{body}"
    );
    assert!(body.contains("src/verify/probability.rs"), "path:\n{body}");
    assert!(
        body.lines()
            .any(|l| l.starts_with("- ") && l.contains("(line ")),
        "evidence lines:\n{body}"
    );
}

// --- 0139 ProductDocs / Daily 5 tests ---

#[test]
fn test_parse_product_docs_intent_recognizes_daily5_phrasings() {
    let positives = [
        "What is the Daily 5?",
        "what is the daily 5",
        "daily five",
        "What is Daily Five?",
        "agent default path",
        "What is the agent default path?",
        "session start commands",
        "what are the session start commands?",
        "Daily 5!",
        "tell me about the daily 5.",
    ];
    for q in positives {
        assert_eq!(
            parse_product_docs_intent(q),
            Some(ProductDocsIntent::Daily5),
            "expected ProductDocs for: {q}"
        );
    }
}

#[test]
fn test_parse_product_docs_intent_ignores_non_product_queries() {
    // Implementation (no product phrase, or impl keywords)
    assert_eq!(
        parse_product_docs_intent("how does doctor compute readiness"),
        None
    );
    assert_eq!(
        parse_product_docs_intent("how does the Daily 5 compute scores"),
        None
    );
    // Pure CG-F20 structural shapes
    assert_eq!(parse_product_docs_intent("what calls execute_ask"), None);
    assert_eq!(
        parse_product_docs_intent("where is symbol execute_ask defined"),
        None
    );
    // Plain command-discovery without product phrases → leave for CG-F31
    assert_eq!(
        parse_product_docs_intent("what command shows hotspots"),
        None
    );
    assert_eq!(
        parse_product_docs_intent("what commands show repo health?"),
        None
    );
    // Random narrative / daily noise without product phrase
    assert_eq!(
        parse_product_docs_intent("what should I refactor in this module?"),
        None
    );
    assert_eq!(
        parse_product_docs_intent("daily standup notes from git"),
        None
    );
}

#[test]
fn test_product_docs_wins_session_start_overlap_with_generic_discovery() {
    // Wire-order regression (AI3): this phrase matches GenericDiscovery
    // in isolation, but ProductDocs must own it when both parse.
    let q = "what are the session start commands?";
    assert_eq!(
        parse_product_docs_intent(q),
        Some(ProductDocsIntent::Daily5),
        "ProductDocs must match session-start synonym"
    );
    assert_eq!(
        parse_command_discovery_intent(q),
        Some(CommandDiscoveryIntent::GenericDiscovery),
        "GenericDiscovery may still match in isolation — wire order wins in execute_ask"
    );
}

#[test]
fn test_build_daily5_answer_contains_required_argv_tokens() {
    let corpus = build_command_corpus();
    let answer = build_daily5_answer(&corpus);

    for required in [
        "doctor --json",
        "change-context --json",
        "ledger status",
        "search",
        "verify --scope fast",
    ] {
        assert!(
            answer.contains(required),
            "Daily 5 answer missing required token `{required}`:\n{answer}"
        );
    }
}

#[test]
fn test_build_daily5_answer_excludes_banned_flags_and_framing() {
    let corpus = build_command_corpus();
    let answer = build_daily5_answer(&corpus);
    let lower = answer.to_lowercase();

    for banned in [
        "--machine-output",
        "--json-lines",
        "--narrative",
        "--mode",
        "--semantic",
        "--auto-scan",
    ] {
        assert!(
            !answer.contains(banned),
            "Daily 5 answer must not contain banned flag `{banned}`:\n{answer}"
        );
    }
    assert!(
        !lower.contains("top 5 findings"),
        "must not redefine Daily 5 as topFindings / top 5 findings:\n{answer}"
    );
    // Bare --json is legitimate on doctor/change-context — do not ban it.
    assert!(
        answer.contains("--json"),
        "Daily 5 must retain skill --json on doctor/change-context:\n{answer}"
    );
}

#[test]
fn test_daily5_skill_sot_contains_five_argv_substrings() {
    // Tracked SoT only (docs/Ledgerful/skill.md). Path relative to this
    // source file: src/commands/ask_routing/tests.rs → ../../../docs/Ledgerful/skill.md
    let skill = include_str!("../../../docs/Ledgerful/skill.md");
    for token in [
        "doctor --json",
        "change-context --json",
        "ledger status",
        "search",
        "verify --scope fast",
    ] {
        assert!(
            skill.contains(token),
            "skill.md missing Daily 5 argv substring `{token}`"
        );
    }
}

#[test]
fn test_daily5_skill_sot_is_ai_only_procedure() {
    // 0221: tracked skill is an AI-only Daily 5 card, not a product brochure.
    let skill = include_str!("../../../docs/Ledgerful/skill.md");
    for banned in [
        "handleGetUser",
        "Purpose",
        "viz-server",
        "token budgeting",
        "compiler-grade",
        "This file is intentionally portable",
    ] {
        assert!(
            !skill.contains(banned),
            "skill.md must not contain brochure leftover `{banned}`"
        );
    }
    let lower = skill.to_lowercase();
    assert!(
        lower.contains("do not ledger start"),
        "collision skip must say do not ledger start on overlapping pending"
    );
    assert!(
        skill.contains("latest-impact.json"),
        "skill must name latest-impact.json"
    );
    assert!(
        lower.contains("does not rewrite") || lower.contains("does **not** rewrite"),
        "skill must state change-context does not rewrite latest-impact.json"
    );

    let mut fence = 0usize;
    let mut body_start = 0usize;
    for (i, line) in skill.lines().enumerate() {
        if line.trim() == "---" {
            fence += 1;
            if fence == 2 {
                body_start = i + 1;
                break;
            }
        }
    }
    let body_lines = skill
        .lines()
        .skip(body_start)
        .filter(|l| !l.trim().is_empty())
        .count();
    assert!(
        body_lines <= 120,
        "skill body must be ≤120 non-blank lines excluding YAML, got {body_lines}"
    );
}

#[test]
fn test_product_docs_daily5_banner_locked_string() {
    assert_eq!(
        PRODUCT_DOCS_DAILY5_BANNER,
        "Product-docs query resolved via skill Daily 5."
    );
}

// --- CG-F31 tests ---

#[test]
fn test_build_command_corpus_includes_known_health_commands() {
    let corpus = build_command_corpus();
    let names: Vec<&str> = corpus.iter().map(|c| c.qualified_name.as_str()).collect();
    assert!(names.contains(&"doctor"), "expected doctor in corpus");
    assert!(names.contains(&"status"), "expected status in corpus");
    assert!(names.contains(&"audit"), "expected audit in corpus");
    assert!(
        names.contains(&"ledger status"),
        "expected nested ledger status in corpus"
    );
    assert!(
        names.contains(&"hotspots trend"),
        "expected nested hotspots trend in corpus"
    );
    assert!(
        !names.contains(&"help"),
        "synthetic help subcommand must not leak into corpus"
    );
}

#[test]
fn test_parse_command_discovery_intent_recognizes_repo_health_phrasings() {
    assert_eq!(
        parse_command_discovery_intent("what commands show repo health?"),
        Some(CommandDiscoveryIntent::RepoHealth)
    );
    assert_eq!(
        parse_command_discovery_intent("how do i check repo health"),
        Some(CommandDiscoveryIntent::RepoHealth)
    );
    assert_eq!(
        parse_command_discovery_intent("what command shows project status"),
        Some(CommandDiscoveryIntent::RepoHealth)
    );
    assert_eq!(
        parse_command_discovery_intent("which command shows the current state of the repo"),
        Some(CommandDiscoveryIntent::RepoHealth)
    );
}

#[test]
fn test_parse_command_discovery_intent_recognizes_generic_discovery() {
    assert_eq!(
        parse_command_discovery_intent("what command shows hotspots"),
        Some(CommandDiscoveryIntent::GenericDiscovery)
    );
    assert_eq!(
        parse_command_discovery_intent("which command lists dependencies"),
        Some(CommandDiscoveryIntent::GenericDiscovery)
    );
}

#[test]
fn test_parse_command_discovery_intent_ignores_implementation_questions() {
    assert_eq!(
        parse_command_discovery_intent("how does calculate_hotspots compute scores"),
        None
    );
    assert_eq!(
        parse_command_discovery_intent("how does the embedding pipeline work"),
        None
    );
    assert_eq!(
        parse_command_discovery_intent("what calls execute_ask"),
        None
    );
    assert_eq!(
        parse_command_discovery_intent("what should I refactor in this module?"),
        None
    );
    assert_eq!(
        parse_command_discovery_intent("give me an overview of this codebase"),
        None
    );
}

#[test]
fn test_parse_command_discovery_intent_ignores_unqualified_status_questions() {
    // Regression for CG-F31 review finding: the bare words "status"/
    // "health" must not trigger `RepoHealth` without a repo/project/CLI
    // qualifier (or an explicit "command(s)" mention). These are
    // "how do I check ..." questions, which would have falsely matched
    // the old optional-qualifier topic regex.
    assert_eq!(
        parse_command_discovery_intent("how do I check the status of my database connection"),
        None
    );
    assert_eq!(
        parse_command_discovery_intent("how do I check if the status field is set"),
        None
    );
}

#[test]
fn test_build_command_discovery_answer_repo_health_mentions_canonical_commands() {
    let corpus = build_command_corpus();
    let answer = build_command_discovery_answer(
        &CommandDiscoveryIntent::RepoHealth,
        "what commands show repo health?",
        &corpus,
    )
    .expect("expected a grounded repo-health answer");

    assert!(answer.contains("doctor"), "got: {answer}");
    assert!(answer.contains("status"), "got: {answer}");
    assert!(answer.contains("audit"), "got: {answer}");
}

#[test]
fn test_build_command_discovery_answer_generic_returns_none_for_low_confidence() {
    let corpus = build_command_corpus();
    let answer = build_command_discovery_answer(
        &CommandDiscoveryIntent::GenericDiscovery,
        "zzzznonsensequery1234",
        &corpus,
    );
    assert!(answer.is_none());
}

fn seeded_storage() -> StorageManager {
    let tmp = tempdir().unwrap();
    let storage = StorageManager::init(&tmp.path().join("ledger.db")).unwrap();
    let conn = storage.get_connection();

    conn.execute(
        "INSERT INTO project_files (id, file_path, last_indexed_at) VALUES \
         (1, 'src/state/storage_cozo.rs', '2026-01-01T00:00:00Z'), \
         (2, 'src/index/incremental.rs', '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();

    conn.execute(
        "INSERT INTO project_symbols (id, file_id, qualified_name, symbol_name, symbol_kind, last_indexed_at) VALUES \
         (1, 1, 'remove_snippets_for_files', 'remove_snippets_for_files', 'Function', '2026-01-01T00:00:00Z'), \
         (2, 2, 'reindex_file', 'reindex_file', 'Function', '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();

    conn.execute(
        "INSERT INTO structural_edges (caller_symbol_id, caller_file_id, callee_symbol_id, callee_file_id) \
         VALUES (2, 2, 1, 1)",
        [],
    )
    .unwrap();

    conn.execute(
        "INSERT INTO api_routes (method, path_pattern, handler_symbol_name, handler_file_id, framework, last_indexed_at) \
         VALUES ('GET', '/api/users', 'reindex_file', 2, 'axum', '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();

    storage
}

#[test]
fn test_resolve_intent_finds_real_caller() {
    let storage = seeded_storage();
    let resolved = resolve_intent(
        &ExactIntent::CallersOf("remove_snippets_for_files".to_string()),
        storage.get_connection(),
    )
    .unwrap();

    let text = resolved.expect("expected a caller to be found");
    assert!(text.contains("reindex_file"), "got: {text}");
    assert!(
        text.contains("src/index/incremental.rs"),
        "expected a file citation: {text}"
    );
}

#[test]
fn test_resolve_intent_finds_route_owner() {
    let storage = seeded_storage();
    let resolved = resolve_intent(
        &ExactIntent::RouteOwner("/api/users".to_string()),
        storage.get_connection(),
    )
    .unwrap();

    let text = resolved.expect("expected a route owner to be found");
    assert!(text.contains("reindex_file"), "got: {text}");
    assert!(
        text.contains("src/index/incremental.rs"),
        "expected a file citation: {text}"
    );
}

#[test]
fn test_resolve_intent_returns_none_for_unknown_symbol() {
    let storage = seeded_storage();
    let resolved = resolve_intent(
        &ExactIntent::CallersOf("nonexistent_symbol_xyz".to_string()),
        storage.get_connection(),
    )
    .unwrap();

    assert!(resolved.is_none());
}
