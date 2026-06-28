use crate::config::model::LocalModelConfig;
use crate::embed::client::embed_long_text;
use crate::embed::similarity::pairwise_cosine;
use crate::embed::storage::load_candidates;
use crate::impact::packet::StalenessTier;
use chrono::NaiveDate;
use rusqlite::Connection;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::SystemTime;

/// Broad intent classification for `ledgerful ask` queries.
///
/// This is a *conceptual vs. task* classifier, deliberately separate from the
/// structural `ExactIntent` router in `src/commands/ask_routing.rs` (which
/// handles "what calls X" style queries and early-returns before this runs).
/// `QueryIntent` decides whether the active `ImpactPacket` should be injected
/// into the LLM context: broad architectural questions (`GlobalConceptual`)
/// get the packet pruned, while diff/task questions (`DiffTask`) keep it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryIntent {
    /// Broad architecture/overview/conceptual questions. The active
    /// `ImpactPacket` is deliberately excluded from the RAG context payload.
    GlobalConceptual,
    /// Questions about the current diff/changes/verification. The
    /// `ImpactPacket` is aggressively included.
    DiffTask,
    /// Cannot classify — leave existing behavior unchanged.
    Unknown,
}

/// Classify a free-text `ask` query into a broad intent bucket.
///
/// Deterministic, case-insensitive, and operates on the trimmed query. Pure
/// function with no I/O. The query is tokenized once (see `tokenize`) into a
/// lowercase alphanumeric token sequence. Phrase triggers (conceptual and
/// task) are matched as contiguous token subsequences via `contains_phrase`,
/// so `"the changes"` no longer matches inside `"describe the changeset
/// format"`. Single-token checks (task verbs, framing tokens, work nouns) use
/// direct token equality.
///
/// # Design principle (conservative pruning)
/// A false `GlobalConceptual` is DANGEROUS — it prunes the `ImpactPacket` for a
/// query that actually needs the current diff. A false `DiffTask` or `Unknown`
/// is SAFE — it just keeps the packet (the pre-classification default). So the classifier
/// only returns `GlobalConceptual` for clearly conceptual queries; when in
/// doubt it prefers `Unknown`. Bare single-word triggers that match conceptual
/// queries (`pipeline`, `verification`) are NOT used. Words with strong
/// conceptual readings (`regression`, `test`, `build`, `failing`) are only
/// used in gated combos (with a framing token or a co-occurring work noun),
/// never bare, so `walk me through the test suite` stays `GlobalConceptual`.
///
/// # Trigger vocabulary
/// - `GlobalConceptual` triggers (conceptual nouns/phrasing, no bare verbs):
///   "architecture", "overview", "how does", "how do", "walk me through",
///   "high level", "design of", "structure of", "explain the architecture",
///   "describe the architecture", "explain the design", "describe the design",
///   "explain the data flow", "data flow".
/// - `DiffTask` scoped phrases (see `TASK_TRIGGERS`): "what did i change",
///   "what changed", "current diff", "my changes", "the changes",
///   "test failures", "failing tests", "verify the build", "fix this",
///   "fix the", "refactor this", "refactor the", "current impact",
///   "impact of my", "impact of this", "impact of these", "analyze the
///   current impact", "analyze the impact", …
/// - Task verbs (whole tokens, combo-only): `fix`, `verify`, `refactor`.
/// - Bare change-nouns (whole tokens, any presence overrides): `change`,
///   `changes`, `diff`, `patch`, `patches`, `hunk`, `hunks`, `edit`, `edits`,
///   `modification`, `modifications`, `update`, `updates`.
/// - Bare fix/problem nouns (whole tokens): `fix`, `refactor`, `bugfix`,
///   `bugfixes`, `hotfix`, `hotfixes`, `failure`, `failures`.
/// - Combo work nouns (whole tokens, need framing or a task verb): `build`,
///   `test`, `tests`, `regression`, `migration`, `migrations`, `refactoring`,
///   `restructuring`, `reorganization`, `conversion`, `error`, `errors`, `bug`,
///   `bugs` (kept combo-only — strong conceptual readings: build system, test
///   suite, regression testing, error handling, the migration architecture).
/// - Edit verbs (whole tokens, framing-gated): `add`/`added`, `remove`/
///   `removed`, `delete`/`deleted`, `modify`/`modified`, `change`/`changed`,
///   `update`/`updated`, `create`/`created`, `write`/`wrote`, `introduce`/
///   `introduced`, `fixed`, `refactored`.
/// - Problem verbs (whole tokens, framing-gated): `break`, `broke`, `broken`,
///   `crash`, `crashed`.
/// - Framing tokens (whole tokens): `i`, `my`, `this`, `these`, `just`,
///   `current`, `now`, `we`, `you`, `our`.
///
/// # Precedence
/// 1. **Task override**: return `DiffTask` if ANY of the following hold:
///    a. a TASK trigger phrase is present;
///    b. a task verb (`fix`/`verify`/`refactor`) is present as a whole token
///    AND a work noun is present as a distinct whole token (the distinctness
///    check avoids double-counting when the verb and noun are the same word);
///    c. a task verb is present AND a framing token is present;
///    d. a change noun (`change`/`changes`/`diff`/`patch`/`patches`/`hunk`/
///    `hunks`/`edit`/`edits`/`modification`/`modifications`/`update`/`updates`)
///    is present as a whole token;
///    e. a bare whole token `fix`/`refactor`/`bugfix`/`bugfixes`/`hotfix`/
///    `hotfixes` is present (inherently current-work);
///    f. a bare whole token `failure` or `failures` is present (a failure is
///    inherently a current problem being investigated);
///    g. a framing token (`current`/`my`/`this`/`these`/`we`/…) co-occurs with
///    a work noun (`build`/`test`/`change`/`changes`/`diff`/`regression`/…);
///    h. the adjective `failing` co-occurs with a work noun
///    (`failing build`/`failing test`);
///    i. a framing token co-occurs with an edit verb (base or past tense:
///    `what I just added`/`what I changed`/`how do we modify this function`);
///    j. a framing token co-occurs with a breakage verb
///    (`what I broke`/`what I crashed`).
///    This catches `how do we verify the build`, `how do you fix the
///    regression`, `how do we refactor this module`, `summarize the fix`,
///    `summarize the refactor`, `summarize the architecture changes`,
///    `walk me through the test changes`, `walk me through the current
///    build failure`, and `walk me through the build failure` even when a
///    conceptual trigger like `how do` or `walk me through` also matches.
/// 2. **Conceptual**: else if a CONCEPTUAL trigger is present →
///    `GlobalConceptual`.
/// 3. **Task-bare fallback**: else if a TASK trigger was present (already
///    covered by step 1 in practice) → `DiffTask`.
/// 4. **Unknown**: else → `Unknown`.
///
/// Conceptual triggers are nouns/phrases (`architecture`, `overview`, `data
/// flow`, `how does`, `walk me through`, …), NOT bare verbs — so `summarize`
/// alone does not force `GlobalConceptual`; `summarize the architecture` is
/// conceptual via the `architecture` noun, while `summarize the fix` is
/// current-work via the bare-`fix` override. Bare
/// `analyze`/`impact`/`explain the`/`describe the`/`pipeline` no longer force a
/// classification. Framing tokens and work nouns are matched as whole tokens so
/// that substrings like the letter `i` inside `architecture` or `now` inside
/// `snow` do not produce false matches.
pub fn classify_query(query: &str) -> QueryIntent {
    const CONCEPTUAL_TRIGGERS: &[&str] = &[
        "architecture",
        "overview",
        "how does",
        "how do",
        "walk me through",
        "high level",
        "design of",
        "structure of",
        "explain the architecture",
        "describe the architecture",
        "explain the design",
        "describe the design",
        "explain the data flow",
        "data flow",
    ];
    const TASK_TRIGGERS: &[&str] = &[
        "what did i change",
        "what did i just change",
        "what changed",
        "current diff",
        "my changes",
        "the changes",
        "these changes",
        "test failures",
        "these test failures",
        "the test failures",
        "failing tests",
        "the failing tests",
        "verify the build",
        "fix this",
        "fix the",
        "refactor this",
        "refactor the",
        "current impact",
        "impact of my",
        "impact of this",
        "impact of these",
        "risk of this change",
        "analyze the current impact",
        "analyze the impact",
    ];
    const FRAMING_TOKENS: &[&str] = &[
        "i", "my", "this", "these", "just", "current", "now", "we", "you", "our",
    ];
    const TASK_VERBS: &[&str] = &["fix", "verify", "refactor"];
    const WORK_NOUNS: &[&str] = &[
        "build",
        "test",
        "tests",
        "change",
        "changes",
        "diff",
        "failure",
        "failures",
        "regression",
        "edit",
        "edits",
        "update",
        "updates",
        "modification",
        "modifications",
        "migration",
        "migrations",
        "refactoring",
        "restructuring",
        "reorganization",
        "conversion",
        "error",
        "errors",
        "bug",
        "bugs",
    ];

    let tokens = tokenize(query.trim());
    let has_conceptual = CONCEPTUAL_TRIGGERS
        .iter()
        .any(|t| contains_phrase(&tokens, t));
    let has_task = TASK_TRIGGERS.iter().any(|t| contains_phrase(&tokens, t));
    let has_framing = FRAMING_TOKENS
        .iter()
        .any(|t| tokens.iter().any(|tok| tok == t));
    let has_task_verb = TASK_VERBS.iter().any(|v| tokens.iter().any(|tok| tok == v));
    let has_work_noun = WORK_NOUNS.iter().any(|n| tokens.iter().any(|tok| tok == n));
    // Task-verb + work-noun combo only counts when the work noun is a distinct
    // token from the task verb, so "summarize the fix" (verb=noun=fix) does NOT
    // trigger the task override via this clause.
    let task_verb_work_noun_combo = TASK_VERBS.iter().any(|verb| {
        tokens.iter().any(|tok| tok == verb)
            && WORK_NOUNS
                .iter()
                .any(|noun| *noun != *verb && tokens.iter().any(|tok| tok == noun))
    });
    // Framing + work-noun override: a first-person/current framing token
    // (`current`, `my`, `this`, `these`, …) co-occurring with a work noun
    // (`build`, `test`, `failure`, `changes`, …) denotes the user's current
    // work, e.g. `walk me through the current build failure`. The framing token
    // is required so a bare conceptual `walk me through the test suite`
    // (no framing) stays `GlobalConceptual`.
    let framing_work_noun_combo = has_framing && has_work_noun;
    // Framing + edit-verb combo: a first-person/recency framing token
    // (`i`/`my`/`just`/`we`/`this`/…) co-occurring with an edit verb (base or
    // past tense: `add`/`added`/`modify`/`modified`/`remove`/`removed`/`update`/
    // `updated`/…) denotes the user's recent/current work, e.g.
    // `walk me through what I just added`, `walk me through what I changed`, or
    // `how do we modify this function`. The framing token is required so a
    // conceptual `how does the updated cache work` or `how does the add method
    // work` (no first-person) still prunes. `fixed`/`refactored` are included
    // because the bare `fix`/`refactor` overrides match the infinitive/noun,
    // not the past tense; `change`/`changes` are already bare change-nouns.
    const EDIT_VERBS: &[&str] = &[
        "add",
        "added",
        "remove",
        "removed",
        "delete",
        "deleted",
        "modify",
        "modified",
        "change",
        "changed",
        "update",
        "updated",
        "create",
        "created",
        "write",
        "wrote",
        "introduce",
        "introduced",
        "fixed",
        "refactored",
        "rename",
        "renamed",
        "reorganize",
        "reorganized",
        "restructure",
        "restructured",
        "move",
        "moved",
        "extract",
        "extracted",
        "inline",
        "inlined",
        "migrate",
        "migrated",
        "port",
        "ported",
        "convert",
        "converted",
    ];
    let has_edit_verb = EDIT_VERBS.iter().any(|v| tokens.iter().any(|t| t == v));
    let framing_edit_verb_combo = has_framing && has_edit_verb;
    // Framing + problem-verb combo: a first-person/recency framing token
    // co-occurring with a breakage verb (`break`/`broke`/`broken`/`crash`/
    // `crashed`) denotes the user's current problem, e.g.
    // `walk me through what I broke`. Framing-gated so a conceptual
    // `explain the crash handling design` (no first-person) still prunes.
    const PROBLEM_VERBS: &[&str] = &["break", "broke", "broken", "crash", "crashed"];
    let has_problem_verb = PROBLEM_VERBS.iter().any(|v| tokens.iter().any(|t| t == v));
    let framing_problem_verb_combo = has_framing && has_problem_verb;
    // `failing` + work-noun combo: the adjective `failing` modifying a work
    // noun (`failing build`, `failing test`, `failing tests`) denotes a current
    // problem being investigated and overrides conceptual triggers even without
    // a framing token. Bare `failing` alone is NOT an override (e.g.
    // `describe the failing verification` stays `Unknown` because
    // `verification` is not a work noun).
    let has_failing = tokens.iter().any(|t| t == "failing");
    let failing_work_noun_combo = has_failing && has_work_noun;
    // Change-noun override: the whole tokens `change`, `changes`, `diff`,
    // `patch`, `patches`, `hunk`, `hunks`, `edit`, `edits`, `modification`,
    // `modifications`, `update`, `updates` are inherently current-work (a
    // patch/hunk/change/edit/update IS a diff). Any query containing them
    // overrides conceptual triggers (`summarize the architecture changes`,
    // `walk me through the test changes`, `walk me through the patch`,
    // `walk me through the change`, `walk me through the update`, `give me an
    // overview of the edits`) so the ImpactPacket is preserved. Whole-token
    // matching via `tokenize` means `changeset` (one token) does NOT match
    // `change`/`changes`. This is strictly safe: it only converts would-be
    // prunes into keeps, never the reverse — rare conceptual uses like
    // `change management` or `the update mechanism` keep the packet (labelled
    // `DiffTask`) instead of pruning, which is the safe direction per the
    // false-keep > false-prune principle. `build`/`test`/`tests`/`regression`
    // are intentionally NOT bare (they have strong conceptual readings — build
    // system, test suite, regression testing) and stay combo-only.
    let has_change_noun = tokens.iter().any(|t| {
        t == "change"
            || t == "changes"
            || t == "diff"
            || t == "patch"
            || t == "patches"
            || t == "hunk"
            || t == "hunks"
            || t == "edit"
            || t == "edits"
            || t == "modification"
            || t == "modifications"
            || t == "update"
            || t == "updates"
    });
    // Bare `fix`/`refactor`/`bugfix`/`hotfix` override: these whole tokens
    // inherently denote current-work activities, so a query like `summarize the
    // fix`, `summarize the refactor`, or `walk me through the bugfix` is
    // current-work and must keep the ImpactPacket. `verify` is intentionally
    // NOT a bare override because it can frame conceptual checks
    // (`verify the architecture is sound`).
    let has_fix_or_refactor = tokens.iter().any(|t| {
        t == "fix"
            || t == "refactor"
            || t == "bugfix"
            || t == "bugfixes"
            || t == "hotfix"
            || t == "hotfixes"
    });
    // Bare `failure`/`failures` override: a failure is inherently a current
    // problem the user is investigating, so `walk me through the build failure`
    // or `walk me through the test failure` must keep the ImpactPacket even
    // without a framing token. `regression` is intentionally NOT a bare
    // override (it can frame conceptual design, e.g.
    // `explain the regression-handling design`) and stays combo-only.
    let has_failure = tokens.iter().any(|t| t == "failure" || t == "failures");
    let task_override = has_task
        || task_verb_work_noun_combo
        || (has_task_verb && has_framing)
        || has_change_noun
        || has_fix_or_refactor
        || has_failure
        || framing_work_noun_combo
        || failing_work_noun_combo
        || framing_edit_verb_combo
        || framing_problem_verb_combo;

    if task_override {
        QueryIntent::DiffTask
    } else if has_conceptual {
        QueryIntent::GlobalConceptual
    } else if has_task {
        // Step 3 fallback — in practice step 1 (has_task) already covers this.
        QueryIntent::DiffTask
    } else {
        QueryIntent::Unknown
    }
}

/// Tokenize a string into a lowercase sequence of alphanumeric tokens, dropping
/// empty tokens. Splits on every non-alphanumeric boundary, so
/// `"describe the changeset format"` → `["describe", "the", "changeset",
/// "format"]` and `"regression-handling"` → `["regression", "handling"]`.
/// Deterministic and panic-free on empty/whitespace/unicode input (unicode
/// alphanumeric chars are preserved by `char::is_alphanumeric`).
fn tokenize(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_string())
        .collect()
}

/// Check whether the token sequence of `phrase` appears as a contiguous
/// subsequence of `tokens`. Both sides are tokenized identically, so
/// `"the changes"` (tokens `["the", "changes"]`) does NOT match
/// `["describe", "the", "changeset", "format"]` because `"changes" !=
/// "changeset"`. Returns `false` for an empty phrase.
fn contains_phrase(tokens: &[String], phrase: &str) -> bool {
    let phrase_tokens = tokenize(phrase);
    if phrase_tokens.is_empty() || phrase_tokens.len() > tokens.len() {
        return false;
    }
    tokens
        .windows(phrase_tokens.len())
        .any(|window| window == phrase_tokens.as_slice())
}

#[derive(Debug, Clone, PartialEq)]
pub struct RetrievedChunk {
    pub entity_id: String,
    pub similarity: f32,
    pub content: String,
    pub heading: Option<String>,
    pub file_path: String,
}

impl Eq for RetrievedChunk {}

impl PartialOrd for RetrievedChunk {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for RetrievedChunk {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.similarity
            .partial_cmp(&other.similarity)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| self.entity_id.cmp(&other.entity_id))
    }
}

pub fn retrieve_top_k(
    conn: &Connection,
    query_vec: &[f32],
    entity_type: &str,
    model_name: &str,
    k: usize,
) -> Result<Vec<RetrievedChunk>, String> {
    if k == 0 {
        return Ok(Vec::new());
    }

    let candidates = load_candidates(conn, entity_type, model_name)?;
    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    let scores = pairwise_cosine(query_vec, &candidates);

    // Over-fetch: top k*3 for reranker
    let overfetch = (k * 3).min(scores.len());
    let top_scores: Vec<_> = scores.into_iter().take(overfetch).collect();

    let mut results = Vec::with_capacity(top_scores.len());
    for (entity_id, similarity) in top_scores {
        if entity_type == "doc_chunk" {
            if let Ok(Some((content, heading, file_path))) = resolve_doc_chunk(conn, &entity_id) {
                results.push(RetrievedChunk {
                    entity_id,
                    similarity,
                    content,
                    heading,
                    file_path,
                });
            }
        } else {
            results.push(RetrievedChunk {
                entity_id,
                similarity,
                content: String::new(),
                heading: None,
                file_path: String::new(),
            });
        }
    }

    results.sort_unstable_by(|a, b| {
        b.similarity
            .partial_cmp(&a.similarity)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.entity_id.cmp(&b.entity_id))
    });

    Ok(results)
}

/// Compute the staleness (age in days) of an ADR file.
///
/// Uses multi-source age detection in priority order, taking the **most recent** date found.
/// Sources: file mtime → YAML frontmatter `date:` → `created:` metadata line → git log.
///
/// Recently-updated exemption: if file mtime is within 30 days, returns `None`.
pub fn compute_staleness(file_path: &Path, _threshold_days: u32) -> Option<u32> {
    const EXEMPTION_DAYS: u64 = 30;
    const SECS_PER_DAY: u64 = 86400;

    let now = SystemTime::now();

    // 1. Check mtime first for exemption
    let mtime_opt = fs::metadata(file_path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|m| now.duration_since(m).ok());

    if let Some(elapsed) = mtime_opt
        && elapsed.as_secs() / SECS_PER_DAY < EXEMPTION_DAYS
    {
        return None;
    }

    let mut most_recent: Option<SystemTime> = mtime_opt.map(|_| {
        fs::metadata(file_path)
            .ok()
            .and_then(|m| m.modified().ok())
            .unwrap_or(SystemTime::UNIX_EPOCH)
    });

    // 2. Parse YAML frontmatter `date:` field
    if let Ok(content) = fs::read_to_string(file_path) {
        if let Some(stripped) = content.strip_prefix("---")
            && let Some(end) = stripped.find("---")
        {
            let frontmatter = &stripped[..end];
            for line in frontmatter.lines() {
                let line = line.trim();
                if let Some(val) = line.strip_prefix("date:") {
                    let val = val.trim();
                    if let Ok(naive) = NaiveDate::parse_from_str(val, "%Y-%m-%d") {
                        let date_time = naive.and_hms_opt(0, 0, 0).unwrap_or_default();
                        let ts = SystemTime::UNIX_EPOCH
                            + std::time::Duration::from_secs(date_time.and_utc().timestamp() as u64);
                        most_recent = most_recent.map(|m| m.max(ts)).or(Some(ts));
                    }
                    break;
                }
            }
        }

        // 3. Parse `created:` metadata line in body
        for line in content.lines() {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("created:") {
                let val = rest.trim();
                if let Ok(naive) = NaiveDate::parse_from_str(val, "%Y-%m-%d") {
                    let date_time = naive.and_hms_opt(0, 0, 0).unwrap_or_default();
                    let ts = SystemTime::UNIX_EPOCH
                        + std::time::Duration::from_secs(date_time.and_utc().timestamp() as u64);
                    most_recent = most_recent.map(|m| m.max(ts)).or(Some(ts));
                }
                break;
            }
        }
    }

    // 4. Git-based fallback
    let git_ts = git_last_commit_timestamp(file_path);
    if let Some(ts) = git_ts {
        most_recent = most_recent.map(|m| m.max(ts)).or(Some(ts));
    }

    let most_recent = most_recent?;
    let age = now.duration_since(most_recent).ok()?;
    let days = age.as_secs() / SECS_PER_DAY;
    Some(days as u32)
}

fn git_last_commit_timestamp(file_path: &Path) -> Option<SystemTime> {
    let output = Command::new("git")
        .args(["log", "-1", "--format=%ct", "--", file_path.to_str()?])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8(output.stdout).ok()?;
    let timestamp_secs: u64 = stdout.trim().parse().ok()?;

    Some(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(timestamp_secs))
}

pub fn compute_staleness_tier(days: u32, threshold_days: u32) -> Option<StalenessTier> {
    if days < threshold_days {
        None
    } else if days <= threshold_days.saturating_mul(2) {
        Some(StalenessTier::Warning)
    } else {
        Some(StalenessTier::Critical)
    }
}

fn resolve_doc_chunk(
    conn: &Connection,
    entity_id: &str,
) -> Result<Option<(String, Option<String>, String)>, String> {
    let (file_path, chunk_index) = entity_id
        .rsplit_once("::")
        .ok_or_else(|| format!("Invalid entity_id format: {entity_id}"))?;

    let chunk_index: i64 = chunk_index
        .parse::<i64>()
        .map_err(|_| format!("Invalid chunk_index in entity_id: {entity_id}"))?;

    let row: Option<(String, Option<String>, String)> = conn
        .query_row(
            "SELECT content, heading, file_path FROM doc_chunks WHERE file_path = ?1 AND chunk_index = ?2",
            rusqlite::params![file_path, chunk_index],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                ))
            },
        )
        .ok();

    Ok(row)
}

pub fn query_docs(
    config: &LocalModelConfig,
    conn: &Connection,
    diff_text: &str,
    top_n: usize,
) -> Result<Vec<RetrievedChunk>, String> {
    if config.base_url.is_empty() || diff_text.is_empty() {
        return Ok(Vec::new());
    }

    let query_vec = embed_long_text(config, diff_text)?;
    retrieve_top_k(
        conn,
        &query_vec,
        "doc_chunk",
        &config.embedding_model,
        top_n,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::migrations::get_migrations;
    use rstest::rstest;

    fn setup_db() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        let migrations = get_migrations();
        migrations.to_latest(&mut conn).unwrap();
        conn
    }

    #[test]
    fn retrieve_top_k_empty_db_returns_empty() {
        let conn = setup_db();
        let query = vec![1.0_f32, 0.0, 0.0];
        let results = retrieve_top_k(&conn, &query, "doc_chunk", "test-model", 3).unwrap();
        assert_eq!(results.len(), 0, "empty DB should return no results");
    }

    #[test]
    fn retrieve_top_k_zero_k_returns_empty() {
        let conn = setup_db();
        let query = vec![1.0_f32, 0.0, 0.0];
        let results = retrieve_top_k(&conn, &query, "doc_chunk", "test-model", 0).unwrap();
        assert_eq!(results.len(), 0, "top_k=0 should return no results");
    }

    #[test]
    fn retrieve_top_k_returns_sorted_by_similarity() {
        let conn = setup_db();

        // Insert doc_chunks
        conn.execute(
            "INSERT INTO doc_chunks (file_path, chunk_index, heading, content, token_count) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params!["docs/a.md", 0_i64, "Intro", "some content about getting started", 10_i64],
        ).unwrap();
        conn.execute(
            "INSERT INTO doc_chunks (file_path, chunk_index, heading, content, token_count) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params!["docs/b.md", 0_i64, "API Reference", "api endpoint definition here", 10_i64],
        ).unwrap();
        conn.execute(
            "INSERT INTO doc_chunks (file_path, chunk_index, heading, content, token_count) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params!["docs/c.md", 0_i64, "Testing", "test framework and runners", 10_i64],
        ).unwrap();

        // Insert embeddings with known similarity profile
        // Query: [1.0, 0.0, 0.0] — closer to vectors with high first component
        let store_embedding = |entity_id: &str, vec: Vec<f32>| {
            let blob: Vec<u8> = vec.iter().flat_map(|f| f.to_le_bytes()).collect();
            conn.execute(
                "INSERT INTO embeddings (entity_type, entity_id, content_hash, model_name, dimensions, vector) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    "doc_chunk",
                    entity_id,
                    format!("hash-{entity_id}"),
                    "test-model",
                    3_i64,
                    blob,
                ],
            ).unwrap();
        };

        store_embedding("docs/a.md::0", vec![0.9_f32, 0.1, 0.1]); // High similarity
        store_embedding("docs/b.md::0", vec![0.5, 0.5, 0.5]); // Medium
        store_embedding("docs/c.md::0", vec![0.1, 0.9, 0.0]); // Low

        let query = vec![1.0_f32, 0.0, 0.0];
        let results = retrieve_top_k(&conn, &query, "doc_chunk", "test-model", 3).unwrap();

        assert_eq!(results.len(), 3);
        assert!(
            results[0].similarity > results[1].similarity,
            "expected similarity[0] > similarity[1]: {:?}",
            results.iter().map(|r| r.similarity).collect::<Vec<_>>()
        );
        assert!(
            results[1].similarity > results[2].similarity,
            "expected similarity[1] > similarity[2]: {:?}",
            results.iter().map(|r| r.similarity).collect::<Vec<_>>()
        );

        // Highest similarity should be "docs/a.md::0" (most aligned with [1,0,0])
        assert_eq!(results[0].entity_id, "docs/a.md::0");
        assert_eq!(results[0].file_path, "docs/a.md");
        assert_eq!(results[0].heading, Some("Intro".to_string()));
        assert!(!results[0].content.is_empty());
    }

    #[test]
    fn retrieve_top_k_overfetches_for_reranker() {
        let conn = setup_db();

        // Insert 5 chunks + embeddings
        for i in 0..5 {
            conn.execute(
                "INSERT INTO doc_chunks (file_path, chunk_index, heading, content, token_count) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![format!("docs/{i}.md"), 0_i64, format!("H{i}"), format!("content {i}"), 10_i64],
            ).unwrap();

            let entity_id = format!("docs/{i}.md::0");
            let val = 1.0 - (i as f32 * 0.15);
            let blob: Vec<u8> = [val, 0.1_f32, 0.1]
                .iter()
                .flat_map(|f| f.to_le_bytes())
                .collect();
            conn.execute(
                "INSERT INTO embeddings (entity_type, entity_id, content_hash, model_name, dimensions, vector) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params!["doc_chunk", &entity_id, format!("hash-{i}"), "test-model", 3_i64, blob],
            ).unwrap();
        }

        let query = vec![1.0_f32, 0.0, 0.0];
        let results = retrieve_top_k(&conn, &query, "doc_chunk", "test-model", 2).unwrap();

        // k=2, overfetch = 6 or min(6,5)=5. Returns all 5 sorted.
        assert_eq!(results.len(), 5);
        assert!(results[0].similarity >= results[1].similarity);
    }

    #[test]
    fn retrieved_chunk_ord_sorts_by_similarity_desc() {
        let a = RetrievedChunk {
            entity_id: "a".to_string(),
            similarity: 0.9,
            content: String::new(),
            heading: None,
            file_path: String::new(),
        };
        let b = RetrievedChunk {
            entity_id: "b".to_string(),
            similarity: 0.5,
            content: String::new(),
            heading: None,
            file_path: String::new(),
        };
        assert!(a > b, "higher similarity should sort first");
        assert_eq!(a.cmp(&b), std::cmp::Ordering::Greater);
    }

    #[test]
    fn retrieved_chunk_ord_tiebreaks_on_entity_id() {
        let a = RetrievedChunk {
            entity_id: "a".to_string(),
            similarity: 0.5,
            content: String::new(),
            heading: None,
            file_path: String::new(),
        };
        let b = RetrievedChunk {
            entity_id: "b".to_string(),
            similarity: 0.5,
            content: String::new(),
            heading: None,
            file_path: String::new(),
        };
        assert!(a < b, "entity_id a should tiebreak before b"); // entity_id a < b when similarity tied
        assert_eq!(a.cmp(&b), std::cmp::Ordering::Less);
    }

    #[test]
    fn compute_staleness_exempt_when_mtime_within_30_days() {
        use std::fs::OpenOptions;
        use std::io::Write;
        use std::time::{Duration, SystemTime};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("adr.md");
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        file.write_all(b"content").unwrap();
        let recent = SystemTime::now() - Duration::from_secs(5 * 86400);
        file.set_modified(recent).unwrap();
        let result = compute_staleness(&path, 365);
        assert_eq!(result, None, "mtime within 30 days should be exempt");
    }

    #[test]
    fn compute_staleness_populated_when_mtime_older_than_threshold() {
        use std::fs::OpenOptions;
        use std::io::Write;
        use std::time::{Duration, SystemTime};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("adr.md");
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        file.write_all(b"content").unwrap();
        let old = SystemTime::now() - Duration::from_secs(400 * 86400);
        file.set_modified(old).unwrap();
        let result = compute_staleness(&path, 365);
        assert!(result.is_some());
        assert!(result.unwrap() >= 399);
    }

    #[test]
    fn compute_staleness_uses_frontmatter_date() {
        use std::fs::OpenOptions;
        use std::io::Write;
        use std::time::{Duration, SystemTime};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("adr.md");
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        let frontmatter = b"---\ndate: 2024-01-01\n---\nBody content\n";
        file.write_all(frontmatter).unwrap();
        let frontmatter_ts = SystemTime::UNIX_EPOCH + Duration::from_secs(1704067200);
        file.set_modified(frontmatter_ts).unwrap();
        let result = compute_staleness(&path, 365);
        assert!(result.is_some());
        let days = result.unwrap();
        assert!(days >= 850);
    }

    #[test]
    fn compute_staleness_uses_created_metadata_line() {
        use std::fs::OpenOptions;
        use std::io::Write;
        use std::time::{Duration, SystemTime};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("adr.md");
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        let content = b"created: 2024-01-01\n\nBody content\n";
        file.write_all(content).unwrap();
        let created_ts = SystemTime::UNIX_EPOCH + Duration::from_secs(1704067200);
        file.set_modified(created_ts).unwrap();
        let result = compute_staleness(&path, 365);
        assert!(result.is_some());
        assert!(result.unwrap() >= 850);
    }

    #[test]
    fn compute_staleness_tier_none_when_below_threshold() {
        let tier = compute_staleness_tier(100, 365);
        assert_eq!(tier, None);
    }

    #[test]
    fn compute_staleness_tier_warning_when_within_double_threshold() {
        let tier = compute_staleness_tier(500, 365);
        assert_eq!(tier, Some(StalenessTier::Warning));
    }

    #[test]
    fn compute_staleness_tier_critical_when_exceeds_double_threshold() {
        let tier = compute_staleness_tier(800, 365);
        assert_eq!(tier, Some(StalenessTier::Critical));
    }

    // --- QueryIntent classifier ---

    #[rstest]
    #[case::global_conceptual_summarize_architecture(
        "summarize the architecture",
        QueryIntent::GlobalConceptual
    )]
    #[case::global_conceptual_how_does(
        "how does the storage layer work",
        QueryIntent::GlobalConceptual
    )]
    #[case::global_conceptual_overview("give me an overview", QueryIntent::GlobalConceptual)]
    #[case::global_conceptual_data_flow("describe the data flow", QueryIntent::GlobalConceptual)]
    #[case::global_conceptual_walk_me_through_data_flow(
        "walk me through the data flow",
        QueryIntent::GlobalConceptual
    )]
    #[case::global_conceptual_walk_me_through(
        "walk me through the indexing pipeline",
        QueryIntent::GlobalConceptual
    )]
    #[case::diff_task_what_did_i_change("what did I just change", QueryIntent::DiffTask)]
    #[case::diff_task_explain_test_failures("explain these test failures", QueryIntent::DiffTask)]
    #[case::diff_task_refactor_this("refactor this function", QueryIntent::DiffTask)]
    #[case::diff_task_verify("verify the build", QueryIntent::DiffTask)]
    #[case::unknown_structural_list_routes("list all HTTP routes", QueryIntent::Unknown)]
    #[case::unknown_greeting("hello", QueryIntent::Unknown)]
    #[case::unknown_empty("", QueryIntent::Unknown)]
    #[case::unknown_whitespace_only("   ", QueryIntent::Unknown)]
    #[case::case_insensitive_summarize("SUMMARIZE THE ARCHITECTURE", QueryIntent::GlobalConceptual)]
    #[case::case_insensitive_what_did_i_change("What Did I Just Change", QueryIntent::DiffTask)]
    #[case::bare_fix_overrides_conceptual("summarize the fix", QueryIntent::DiffTask)]
    #[case::bare_refactor_overrides_conceptual("summarize the refactor", QueryIntent::DiffTask)]
    #[case::failing_tests_phrase_overrides_conceptual(
        "summarize the failing tests",
        QueryIntent::DiffTask
    )]
    #[case::conceptual_verify_without_framing_stays_global(
        "verify the architecture is sound",
        QueryIntent::GlobalConceptual
    )]
    #[case::task_framed_override_how_do_i_fix_this_regression(
        "how do I fix this regression",
        QueryIntent::DiffTask
    )]
    #[case::task_framed_override_how_do_i_verify_the_build(
        "how do I verify the build",
        QueryIntent::DiffTask
    )]
    #[case::task_framed_override_how_do_i_refactor_this_function(
        "how do I refactor this function",
        QueryIntent::DiffTask
    )]
    #[case::conceptual_how_does_no_framing_stays_global(
        "how does the storage layer work",
        QueryIntent::GlobalConceptual
    )]
    #[case::structural_list_routes_stays_unknown("list all HTTP routes", QueryIntent::Unknown)]
    #[case::framing_token_word_boundary_not_substring_architecture(
        "summarize the architecture",
        QueryIntent::GlobalConceptual
    )]
    #[case::framing_token_word_boundary_not_substring_snow("snow", QueryIntent::Unknown)]
    #[case::global_conceptual_explain_the_data_flow(
        "explain the data flow",
        QueryIntent::GlobalConceptual
    )]
    // --- Phrase-scoped trigger cases ---
    #[case::diff_task_explain_the_test_failures("explain the test failures", QueryIntent::DiffTask)]
    #[case::diff_task_describe_the_changes("describe the changes", QueryIntent::DiffTask)]
    #[case::unknown_describe_the_failing_verification(
        "describe the failing verification",
        QueryIntent::Unknown
    )]
    #[case::unknown_describe_the_verification_pipeline(
        "describe the verification pipeline",
        QueryIntent::Unknown
    )]
    #[case::unknown_explain_the_regression_handling_design(
        "explain the regression-handling design",
        QueryIntent::Unknown
    )]
    #[case::unknown_describe_the_changeset_format(
        "describe the changeset format",
        QueryIntent::Unknown
    )]
    #[case::diff_task_how_do_we_verify_the_build(
        "how do we verify the build",
        QueryIntent::DiffTask
    )]
    #[case::diff_task_how_do_you_fix_the_regression(
        "how do you fix the regression",
        QueryIntent::DiffTask
    )]
    #[case::diff_task_how_do_we_refactor_this_module(
        "how do we refactor this module",
        QueryIntent::DiffTask
    )]
    #[case::unknown_analyze_the_storage_layer("analyze the storage layer", QueryIntent::Unknown)]
    #[case::unknown_what_is_the_impact_packet_schema(
        "what is the impact packet schema",
        QueryIntent::Unknown
    )]
    #[case::default_global_overview_string(
        "Give me an overview of this codebase and its key components.",
        QueryIntent::GlobalConceptual
    )]
    #[case::default_diff_analyze_string(
        "Analyze the current impact and risk.",
        QueryIntent::DiffTask
    )]
    // --- Change-noun override cases ---
    #[case::diff_task_summarize_architecture_changes(
        "summarize the architecture changes",
        QueryIntent::DiffTask
    )]
    #[case::diff_task_walk_me_through_test_changes(
        "walk me through the test changes",
        QueryIntent::DiffTask
    )]
    #[case::diff_task_explain_recent_changes_to_storage_layer(
        "explain the recent changes to the storage layer",
        QueryIntent::DiffTask
    )]
    #[case::global_conceptual_walk_me_through_test_suite(
        "walk me through the test suite",
        QueryIntent::GlobalConceptual
    )]
    #[case::framing_work_noun_overrides_conceptual(
        "walk me through the current build failure",
        QueryIntent::DiffTask
    )]
    #[case::bare_singular_change_overrides_walk_me_through_the_change(
        "walk me through the change",
        QueryIntent::DiffTask
    )]
    #[case::bare_singular_change_overrides_give_me_an_overview_of_the_change(
        "give me an overview of the change",
        QueryIntent::DiffTask
    )]
    #[case::bare_singular_change_overrides_give_me_an_overview_of_this_change(
        "give me an overview of this change",
        QueryIntent::DiffTask
    )]
    #[case::bare_singular_change_overrides_change_management(
        "explain the change management process",
        QueryIntent::DiffTask
    )]
    #[case::bare_failure_overrides_build_failure(
        "walk me through the build failure",
        QueryIntent::DiffTask
    )]
    #[case::bare_failure_overrides_test_failure(
        "walk me through the test failure",
        QueryIntent::DiffTask
    )]
    #[case::bare_failure_guard_regression_design(
        "explain the regression-handling design",
        QueryIntent::Unknown
    )]
    #[case::bare_diff_synonym_patch("walk me through the patch", QueryIntent::DiffTask)]
    #[case::bare_diff_synonym_overview_of_this_patch(
        "give me an overview of this patch",
        QueryIntent::DiffTask
    )]
    #[case::bare_diff_synonym_hunk("walk me through the hunk", QueryIntent::DiffTask)]
    #[case::bare_edit_synonym_update("walk me through the update", QueryIntent::DiffTask)]
    #[case::bare_edit_synonym_edits("give me an overview of the edits", QueryIntent::DiffTask)]
    #[case::bare_edit_synonym_my_edits("walk me through my edits", QueryIntent::DiffTask)]
    #[case::bare_edit_synonym_edit_mode("explain the edit mode", QueryIntent::DiffTask)]
    #[case::failing_work_noun_build("walk me through the failing build", QueryIntent::DiffTask)]
    #[case::failing_work_noun_test("walk me through the failing test", QueryIntent::DiffTask)]
    #[case::failing_work_noun_guard_verification(
        "describe the failing verification",
        QueryIntent::Unknown
    )]
    #[case::bare_fix_synonym_bugfix("walk me through the bugfix", QueryIntent::DiffTask)]
    #[case::bare_fix_synonym_hotfix("walk me through the hotfix", QueryIntent::DiffTask)]
    #[case::framing_edit_verb_just_added(
        "walk me through what I just added",
        QueryIntent::DiffTask
    )]
    #[case::framing_edit_verb_changed("walk me through what I changed", QueryIntent::DiffTask)]
    #[case::framing_edit_verb_removed("walk me through what I removed", QueryIntent::DiffTask)]
    #[case::framing_edit_verb_guard_updated_cache(
        "how does the updated cache work",
        QueryIntent::GlobalConceptual
    )]
    #[case::framing_base_edit_verb_modify("how do we modify this function", QueryIntent::DiffTask)]
    #[case::framing_base_edit_verb_update_config(
        "how do we update the config",
        QueryIntent::DiffTask
    )]
    #[case::framing_base_edit_verb_guard_add_method(
        "how does the add method work",
        QueryIntent::GlobalConceptual
    )]
    #[case::framing_problem_verb_broke("walk me through what I broke", QueryIntent::DiffTask)]
    #[case::framing_problem_verb_crashed("walk me through what I crashed", QueryIntent::DiffTask)]
    #[case::framing_problem_verb_guard_crash_architecture(
        "explain the crash handling architecture",
        QueryIntent::GlobalConceptual
    )]
    #[case::framing_refactor_vocab_migration(
        "walk me through this migration",
        QueryIntent::DiffTask
    )]
    #[case::framing_refactor_vocab_refactoring(
        "walk me through this refactoring",
        QueryIntent::DiffTask
    )]
    #[case::framing_refactor_vocab_rename("how do we rename this method", QueryIntent::DiffTask)]
    #[case::framing_refactor_vocab_move("how do we move this function", QueryIntent::DiffTask)]
    #[case::framing_refactor_vocab_guard_migration_architecture(
        "explain the migration architecture",
        QueryIntent::GlobalConceptual
    )]
    #[case::framing_problem_noun_error("walk me through this error", QueryIntent::DiffTask)]
    #[case::framing_problem_noun_bug("walk me through this bug", QueryIntent::DiffTask)]
    #[case::framing_problem_noun_guard_error_architecture(
        "explain the error handling architecture",
        QueryIntent::GlobalConceptual
    )]
    #[case::global_conceptual_summarize_architecture_no_change_noun(
        "summarize the architecture",
        QueryIntent::GlobalConceptual
    )]
    #[case::unknown_describe_changeset_format_change_noun_boundary(
        "describe the changeset format",
        QueryIntent::Unknown
    )]
    fn classify_query_cases(#[case] query: &str, #[case] expected: QueryIntent) {
        let result = classify_query(query);
        assert_eq!(result, expected, "query: {query}");
    }
}
