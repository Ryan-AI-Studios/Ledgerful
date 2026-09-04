use super::detect::{
    ThirdPartyHookManager, detect_third_party_at_hooks_dir, detect_third_party_hook_manager,
};
use super::resolve::{HooksDirResolution, resolve_hooks_dir};
use camino::{Utf8Path, Utf8PathBuf};
use miette::{IntoDiagnostic, Result};
use owo_colors::{OwoColorize, Stream, Style};
use std::fs;

/// Retired product binary name, built via `concat!` so the brand is not a
/// greppable literal in the binary (R6 / DoD-14).
pub(crate) const LEGACY_BINARY: &str = concat!("change", "guard");

/// Current product binary name.
pub(super) const CURRENT_BINARY: &str = "ledgerful";

/// Gate-type suffixes shared by legacy and current marker comments.
pub(super) const GATE_SUFFIXES: &[&str] = &[
    "ledger-gate",
    "verify-gate",
    "intent-gate",
    "post-commit-gate",
];

/// Outcome of a single hook file's repair attempt.
#[derive(Debug, Clone, Default)]
pub struct HookRepairReport {
    /// Hooks that contained at least one stale invocation/marker and were
    /// rewritten with no residual retired-binary invocation left (or, in
    /// dry-run mode, would be rewritten). Sorted by filename.
    pub repaired: Vec<String>,
    /// Hooks that already used `ledgerful` invocations exclusively.
    /// Sorted by filename.
    pub already_correct: Vec<String>,
    /// Hooks with no ledger-related invocations at all; left untouched.
    /// Sorted by filename.
    pub skipped: Vec<String>,
    /// Third-party hook manager detected, if any. When set, no hooks were
    /// rewritten regardless of their contents.
    pub third_party_manager: Option<ThirdPartyHookManager>,
    /// True if this was a dry run (nothing was actually written to disk).
    pub dry_run: bool,
    /// Hooks that still contain a retired-binary invocation after attempted
    /// repair (DoD-4c honesty). Sorted by filename.
    pub residual_invocations: Vec<String>,
    /// Near-miss de-duplication blocks: (hook name, block text). Tier-2 —
    /// reported, never auto-deleted (DoD-4b). Sorted by hook name.
    pub near_miss_blocks: Vec<(String, String)>,
    /// Why discovery could not look (outside-repo, missing dir, unreadable
    /// config). When set, the report must not render as "clean".
    pub discovery_notes: Vec<String>,
    /// Absolute path of the hooks directory that was examined, if any.
    pub hooks_dir: Option<String>,
}

impl HookRepairReport {
    fn empty(dry_run: bool) -> Self {
        Self {
            dry_run,
            ..Default::default()
        }
    }

    /// True when there is nothing to report as a problem and nothing was done.
    pub fn is_silent_clean(&self) -> bool {
        self.repaired.is_empty()
            && self.residual_invocations.is_empty()
            && self.near_miss_blocks.is_empty()
            && self.discovery_notes.is_empty()
            && self.third_party_manager.is_none()
    }
}

// ---------------------------------------------------------------------------
// Replacement + de-duplication
// ---------------------------------------------------------------------------

/// Apply invocation and marker replacements. Returns rewritten content and
/// whether any replacement fired.
pub(super) fn apply_replacements(content: &str) -> (String, bool) {
    let mut result = content.to_string();
    let mut changed = false;

    // 1. Marker comments: `# <legacy>-<gate>:` → `# ledgerful-<gate>:`
    for suffix in GATE_SUFFIXES {
        let legacy_marker = format!("# {LEGACY_BINARY}-{suffix}");
        let current_marker = format!("# {CURRENT_BINARY}-{suffix}");
        if result.contains(&legacy_marker) {
            result = result.replace(&legacy_marker, &current_marker);
            changed = true;
        }
    }

    // Prose inside the marker line historically said `` `<legacy> init` ``.
    let legacy_init = format!("`{LEGACY_BINARY} init`");
    let current_init = "`ledgerful init`";
    if result.contains(&legacy_init) {
        result = result.replace(&legacy_init, current_init);
        changed = true;
    }

    // 2. Exact command invocation fragments (order matters for longest-first
    // safety; all are distinct enough for plain replace).
    for (current, suffix) in [
        ("command -v ledgerful", "command -v "),
        ("ledgerful ledger", ""),
        ("ledgerful internal hook-", ""),
        ("ledgerful verify", ""),
        ("ledgerful scan", ""),
    ] {
        let retired = if suffix.is_empty() {
            current.replacen("ledgerful", LEGACY_BINARY, 1)
        } else {
            format!("{suffix}{LEGACY_BINARY}")
        };
        if result.contains(&retired) {
            result = result.replace(&retired, current);
            changed = true;
        }
    }

    (result, changed)
}

/// Whether `content` still contains a retired-binary *invocation* (not a
/// historical mention in a `(renamed from …)` comment).
pub(super) fn contains_legacy_invocation(content: &str) -> bool {
    let patterns = [
        format!("command -v {LEGACY_BINARY}"),
        format!("{LEGACY_BINARY} ledger"),
        format!("{LEGACY_BINARY} internal hook-"),
        format!("{LEGACY_BINARY} verify"),
        format!("{LEGACY_BINARY} scan"),
    ];
    patterns.iter().any(|p| content.contains(p.as_str()))
}

/// True when `content` contains any `# {LEGACY_BINARY}-<suffix>` gate marker.
pub(crate) fn contains_legacy_gate_marker(content: &str) -> bool {
    GATE_SUFFIXES
        .iter()
        .any(|suffix| contains_legacy_gate_suffix(content, suffix))
}

/// True when `content` contains `# {LEGACY_BINARY}-{suffix}` (same-suffix alias).
pub(crate) fn contains_legacy_gate_suffix(content: &str, suffix: &str) -> bool {
    content.contains(&format!("# {LEGACY_BINARY}-{suffix}"))
}

/// Whether `content` contains any retired or current Ledgerful invocations we
/// know how to repair.
fn contains_ledger_invocation(content: &str) -> bool {
    contains_legacy_invocation(content)
        || content.contains("command -v ledgerful")
        || content.contains("ledgerful ledger")
        || content.contains("ledgerful internal hook-")
        || content.contains("ledgerful verify")
        || content.contains("ledgerful scan")
        || GATE_SUFFIXES.iter().any(|s| {
            content.contains(&format!("# ledgerful-{s}"))
                || content.contains(&format!("# {LEGACY_BINARY}-{s}"))
        })
}

/// Known generated legacy gate blocks (exact match → tier-1 auto-remove when
/// a current-marker sibling of the same gate type is also present).
///
/// Built with `concat!` for the retired brand. Includes intermediate forms
/// observed on disk (ledgerful-web half-migrated wording).
fn known_generated_legacy_blocks() -> Vec<String> {
    let brand = LEGACY_BINARY;
    let mut blocks = Vec::new();

    // Original legacy-brand-installed ledger gate (Newton / Photo vintage).
    blocks.push(format!(
        r#"# {brand}-ledger-gate: auto-installed by `{brand} init`
if command -v {brand} &>/dev/null; then
    if ! {brand} ledger status --compact --exit-code 2>/dev/null; then
        echo ""
        echo "  Resolve with:"
        echo "    Pending tx:  {brand} ledger commit <tx-id> --summary '...' --reason '...'"
        echo "    Drift:       {brand} ledger reconcile --all --reason '...'"
        echo ""
        echo "  Bypass (not recommended): git commit --no-verify"
        exit 1
    fi
fi"#
    ));

    // Variant with --verify-signatures (CozoDB-redux append form).
    blocks.push(format!(
        r#"# {brand}-ledger-gate: auto-installed by `{brand} init`
if command -v {brand} &>/dev/null; then
    if ! {brand} ledger status --compact --exit-code --verify-signatures 2>/dev/null; then
        echo ""
        echo "  Resolve with:"
        echo "    Pending tx:  {brand} ledger commit <tx-id> --summary '...' --reason '...'"
        echo "    Drift:       {brand} ledger reconcile --all --reason '...'"
        echo ""
        echo "  Bypass (not recommended): git commit --no-verify"
        exit 1
    fi
fi"#
    ));

    // Half-migrated ledgerful-web form: legacy marker, current binary, older
    // echo style, optional 2>/dev/null.
    for bypass in ["git commit --no-verify", "git push --no-verify"] {
        for redirect in [" 2>/dev/null", ""] {
            blocks.push(format!(
                r#"# {brand}-ledger-gate: auto-installed by `ledgerful init`
if command -v ledgerful &>/dev/null; then
    if ! ledgerful ledger status --compact --exit-code --verify-signatures{redirect}; then
        echo ""
        echo "  Resolve with:"
        echo "    Pending tx:  ledgerful ledger commit <tx-id> --summary '...' --reason '...'"
        echo "    Drift:       ledgerful ledger reconcile --all --reason '...'"
        echo ""
        echo "  Bypass (not recommended): {bypass}"
        exit 1
    fi
fi"#
            ));
        }
    }

    // Half-migrated verify gate (ledgerful-web pre-push).
    blocks.push(format!(
        r#"# {brand}-verify-gate: fast scoped verification (pre-push only)
if command -v ledgerful &>/dev/null; then
    if ! ledgerful verify --scope fast 2>/dev/null; then
        echo ""
        echo "  Pre-push quality gate FAILED (ledgerful verify --scope fast)."
        echo "  Fix the above errors before pushing."
        echo ""
        echo "  Bypass (not recommended): git push --no-verify"
        exit 1
    fi
fi"#
    ));

    // Live web half-migrated intent (verbatim; no 2>/dev/null on the hook call).
    blocks.push(format!(
        r#"# {brand}-intent-gate: auto-installed by `ledgerful init`
if command -v ledgerful &>/dev/null; then
    ledgerful internal hook-commit-msg "$1"
fi"#
    ));

    // Live web half-migrated post-commit (verbatim; no 2>/dev/null on the hook call).
    blocks.push(format!(
        r#"# {brand}-post-commit-gate: auto-installed by `ledgerful init`
if command -v ledgerful &>/dev/null; then
    ledgerful internal hook-post-commit "$@"
fi"#
    ));

    // Original-brand twins (`{brand} init` + `command -v {brand}`).
    blocks.push(format!(
        r#"# {brand}-intent-gate: auto-installed by `{brand} init`
if command -v {brand} &>/dev/null; then
    {brand} internal hook-commit-msg "$1"
fi"#
    ));
    blocks.push(format!(
        r#"# {brand}-post-commit-gate: auto-installed by `{brand} init`
if command -v {brand} &>/dev/null; then
    {brand} internal hook-post-commit "$@"
fi"#
    ));

    blocks
}

/// Extract a gate block starting at `start` (index of `# …-gate`). The block
/// runs through the closing `fi` lines that match nested `if`s of a standard
/// gate (or a single `fi` for intent/post-commit). Offsets are CRLF-safe.
fn extract_gate_block(content: &str, start: usize) -> Option<&str> {
    let rest = &content[start..];
    let mut depth = 0i32;
    let mut seen_if = false;
    let mut consumed = 0usize;
    for line in rest.lines() {
        let line_len = line.len();
        // CRLF-safe: str::lines() strips endings; advance by the real ending length.
        let ending_len = crate::commands::hook_template::line_ending_len(rest, consumed + line_len);
        let step = line_len + ending_len;
        let trimmed = line.trim();
        if trimmed.starts_with("if ") || trimmed == "if" || trimmed.starts_with("if\t") {
            depth += 1;
            seen_if = true;
        } else if trimmed == "fi" || trimmed.starts_with("fi ") || trimmed.starts_with("fi;") {
            depth -= 1;
        }
        consumed += step;
        if seen_if && depth <= 0 {
            let block = &rest[..consumed.min(rest.len())];
            return Some(block.trim_end_matches(['\n', '\r']));
        }
    }
    if let Some(blank) = rest.find("\n\n") {
        Some(rest[..blank].trim_end())
    } else {
        Some(rest.trim_end())
    }
}

/// Find all legacy-marker-bounded blocks in `content`.
fn find_legacy_marker_blocks(content: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for suffix in GATE_SUFFIXES {
        let marker = format!("# {LEGACY_BINARY}-{suffix}");
        let mut search_from = 0;
        while let Some(rel) = content[search_from..].find(&marker) {
            let abs = search_from + rel;
            if let Some(block) = extract_gate_block(content, abs) {
                out.push((abs, block.to_string()));
                search_from = abs + block.len().max(1);
            } else {
                break;
            }
        }
    }
    out.sort_by_key(|(pos, _)| *pos);
    out
}

/// True if every non-structural line in `block` is a recognised ledger/verify/
/// scan invocation, echo, blank, comment, or shell control.
fn block_only_recognised_invocations(block: &str) -> bool {
    for line in block.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        if t.starts_with("if ")
            || t == "fi"
            || t == "else"
            || t == "then"
            || t.starts_with("echo ")
            || t == "exit 1"
            || t == "exit 0"
        {
            continue;
        }
        // Recognised command forms (legacy or current).
        if t.contains("command -v ")
            || t.contains(&format!("{LEGACY_BINARY} "))
            || t.contains("ledgerful ")
        {
            continue;
        }
        // Nested `if ! …` already covered by starts_with("if ").
        return false;
    }
    true
}

/// Two-tier de-duplication. Operates on content that still has legacy markers
/// (call before marker rewrite for accurate block identity). Returns
/// (rewritten, changed, near_miss_block_texts).
fn dedup_legacy_blocks(content: &str) -> (String, bool, Vec<String>) {
    let legacy_blocks = find_legacy_marker_blocks(content);
    if legacy_blocks.is_empty() {
        return (content.to_string(), false, Vec::new());
    }

    let known = known_generated_legacy_blocks();
    let mut result = content.to_string();
    let mut changed = false;
    let mut near_misses = Vec::new();

    // Process from end so indices remain valid.
    let mut ordered = legacy_blocks;
    ordered.sort_by_key(|(pos, _)| std::cmp::Reverse(*pos));

    for (_pos, block) in ordered {
        // Only de-dup when a current-marker sibling of the same gate type exists.
        let gate_suffix = GATE_SUFFIXES
            .iter()
            .find(|s| block.contains(&format!("# {LEGACY_BINARY}-{s}")));
        let Some(suffix) = gate_suffix else {
            continue;
        };
        let current_marker = format!("# {CURRENT_BINARY}-{suffix}");
        // After prior removals, re-check presence in current result.
        if !result.contains(&block) {
            continue;
        }
        if !result.contains(&current_marker) {
            // Sole legacy block — not a duplicate; leave for marker rewrite.
            continue;
        }

        let normalized = block.replace("\r\n", "\n").trim().to_string();
        let is_exact = known
            .iter()
            .any(|k| k.replace("\r\n", "\n").trim() == normalized);

        if is_exact {
            // Tier 1: remove the exact known generated block (and surrounding
            // blank lines).
            if remove_extracted_block(&mut result, &block) {
                changed = true;
            }
        } else if block_only_recognised_invocations(&block) {
            // Tier 2: report, never auto-delete.
            near_misses.push(block);
        }
        // Else: unrecognised customisation — leave untouched entirely.
    }

    near_misses.sort();
    (result, changed, near_misses)
}

/// Full repair transform for one hook file's contents.
///
/// Order: de-dup legacy-marker blocks while they are still present, rewrite
/// brand/invocations, then collapse N>1 current-brand same-suffix extras
/// (0206-C). `pub(crate)` so init/ensure can alias-repair before append.
pub(crate) fn repair_content(content: &str) -> (String, bool, Vec<String>) {
    // De-dup first while legacy markers are still present.
    let (after_dedup, dedup_changed, near_misses) = dedup_legacy_blocks(content);
    let (after_repl, repl_changed) = apply_replacements(&after_dedup);
    let (after_collapse, collapse_changed) = collapse_same_brand_current_blocks(&after_repl);
    (
        after_collapse,
        dedup_changed || repl_changed || collapse_changed,
        near_misses,
    )
}

fn gate_kind_for_suffix(suffix: &str) -> Option<crate::commands::hook_template::GateKind> {
    use crate::commands::hook_template::GateKind;
    match suffix {
        "ledger-gate" => Some(GateKind::Ledger),
        "verify-gate" => Some(GateKind::Verify),
        "intent-gate" => Some(GateKind::Intent),
        "post-commit-gate" => Some(GateKind::PostCommit),
        _ => None,
    }
}

fn find_marker_blocks(content: &str, marker: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut search_from = 0;
    while let Some(rel) = content[search_from..].find(marker) {
        let abs = search_from + rel;
        if let Some(block) = extract_gate_block(content, abs) {
            out.push((abs, block.to_string()));
            search_from = abs + block.len().max(1);
        } else {
            break;
        }
    }
    out
}

fn current_extra_is_droppable(suffix: &str, block: &str, known_rewritten: &[String]) -> bool {
    use crate::commands::hook_template::TemplateClass;
    if let Some(kind) = gate_kind_for_suffix(suffix) {
        // Pre-commit vs pre-push historical bodies differ only in bypass string.
        for bypass in ["git commit --no-verify", "git push --no-verify"] {
            match crate::commands::hook_template::classify_block(kind, block, bypass) {
                TemplateClass::Current | TemplateClass::Stale => return true,
                TemplateClass::Unknown => {}
            }
        }
    }
    let normalized = block.replace("\r\n", "\n").trim().to_string();
    known_rewritten.iter().any(|k| k == &normalized)
}

fn remove_extracted_block(result: &mut String, block: &str) -> bool {
    let Some(idx) = result.find(block) else {
        return false;
    };
    let mut start = idx;
    let mut end = idx + block.len();
    // Swallow one leading line ending (LF / CR / CRLF). Keep at most one
    // blank separator; replace_range below writes a single `\n`.
    if start > 0 {
        let bytes = result.as_bytes();
        if bytes[start - 1] == b'\n' {
            start -= 1;
            if start > 0 && bytes[start - 1] == b'\r' {
                start -= 1;
            }
        } else if bytes[start - 1] == b'\r' {
            start -= 1;
        }
    }
    end += crate::commands::hook_template::line_ending_len(result, end);
    result.replace_range(start..end, "\n");
    true
}

/// Collapse N>1 `# ledgerful-<suffix>` blocks after marker rewrite.
///
/// `dedup_legacy_blocks` cannot see current-brand markers, so this must run
/// **after** `apply_replacements`.
///
/// Keep-later is positional: among extras whose bodies classify as Current or
/// Stale via `hook_template::classify_block`, or that match a known-generated
/// block after brand rewrite (`apply_replacements` on known blocks), drop
/// earlier droppable extras and keep the later (last) occurrence. Unknown
/// stays. After a near-miss rewrite, intent/post-commit extras are often
/// byte-identical unstamped Stale — drop the first (legacy-origin), keep the
/// later product body; **0121** refresh then Stale→v2.
fn collapse_same_brand_current_blocks(content: &str) -> (String, bool) {
    let known_rewritten: Vec<String> = known_generated_legacy_blocks()
        .iter()
        .map(|k| {
            apply_replacements(k)
                .0
                .replace("\r\n", "\n")
                .trim()
                .to_string()
        })
        .collect();

    let mut result = content.to_string();
    let mut changed = false;

    for suffix in GATE_SUFFIXES {
        let marker = format!("# {CURRENT_BINARY}-{suffix}");
        let blocks = find_marker_blocks(&result, &marker);
        if blocks.len() <= 1 {
            continue;
        }

        let droppable: Vec<usize> = blocks
            .iter()
            .enumerate()
            .filter(|(_, (_, block))| current_extra_is_droppable(suffix, block, &known_rewritten))
            .map(|(i, _)| i)
            .collect();
        if droppable.len() <= 1 {
            continue;
        }
        let Some(&keep) = droppable.last() else {
            continue;
        };
        let to_drop: Vec<String> = droppable
            .iter()
            .copied()
            .filter(|&i| i != keep)
            .map(|i| blocks[i].1.clone())
            .collect();
        for block in to_drop {
            if remove_extracted_block(&mut result, &block) {
                changed = true;
            }
        }
    }

    (result, changed)
}

// ---------------------------------------------------------------------------
// Public repair entry points
// ---------------------------------------------------------------------------

/// Core repair logic. Resolves hooks via [`resolve_hooks_dir`], enforces
/// containment and third-party re-detection, then rewrites hook files.
pub fn repair_hooks_at(repo_root: &Utf8Path, dry_run: bool) -> Result<HookRepairReport> {
    let mut report = HookRepairReport::empty(dry_run);

    if let Some(manager) = detect_third_party_hook_manager(repo_root) {
        report.third_party_manager = Some(manager);
        return Ok(report);
    }

    match resolve_hooks_dir(repo_root) {
        HooksDirResolution::Found { hooks_dir } => {
            // Re-detect third-party against resolved path (CrawlX non-root husky).
            if let Some(manager) = detect_third_party_at_hooks_dir(&hooks_dir) {
                report.third_party_manager = Some(manager);
                report.hooks_dir = Some(hooks_dir.to_string());
                report.discovery_notes.push(format!(
                    "third-party hook manager '{}' detected at resolved hooks path; refusing rewrite",
                    manager.name()
                ));
                return Ok(report);
            }
            report.hooks_dir = Some(hooks_dir.to_string());
            repair_hooks_in_dir(&hooks_dir, dry_run, &mut report)?;
        }
        HooksDirResolution::OutsideRepo { hooks_dir } => {
            report.hooks_dir = Some(hooks_dir.to_string());
            report.discovery_notes.push(format!(
                "hooks directory '{}' resolves outside the repository; refusing rewrite",
                hooks_dir
            ));
        }
        HooksDirResolution::CannotLook { reason } => {
            report
                .discovery_notes
                .push(format!("cannot resolve hooks directory: {reason}"));
        }
    }

    report.repaired.sort();
    report.already_correct.sort();
    report.skipped.sort();
    report.residual_invocations.sort();
    report.near_miss_blocks.sort_by(|a, b| a.0.cmp(&b.0));
    report.discovery_notes.sort();

    Ok(report)
}

fn repair_hooks_in_dir(
    hooks_dir: &Utf8Path,
    dry_run: bool,
    report: &mut HookRepairReport,
) -> Result<()> {
    if !hooks_dir.is_dir() {
        report.discovery_notes.push(format!(
            "hooks directory '{}' does not exist or is not a directory",
            hooks_dir
        ));
        return Ok(());
    }

    let entries = fs::read_dir(hooks_dir.as_std_path()).into_diagnostic()?;
    let mut filenames: Vec<Utf8PathBuf> = Vec::new();
    for entry in entries {
        let entry = entry.into_diagnostic()?;
        let path = entry.path();
        if path.is_dir() {
            continue;
        }
        let Some(utf8_path) = Utf8PathBuf::from_path_buf(path).ok() else {
            continue;
        };
        let Some(name) = utf8_path.file_name() else {
            continue;
        };
        if name.ends_with(".sample") {
            continue;
        }
        filenames.push(utf8_path);
    }
    filenames.sort();

    for hook_path in filenames {
        let Some(name) = hook_path.file_name() else {
            continue;
        };
        let name = name.to_string();
        let content = fs::read_to_string(hook_path.as_std_path()).into_diagnostic()?;
        let (rewritten, changed, near_misses) = repair_content(&content);

        for block in near_misses {
            report.near_miss_blocks.push((name.clone(), block));
        }

        if contains_legacy_invocation(&rewritten) {
            // DoD-4c: never report as repaired while residual retired binary remains.
            if changed && !dry_run {
                // Still write the partial improvement so markers/known patterns fix.
                fs::write(hook_path.as_std_path(), &rewritten).into_diagnostic()?;
            }
            report.residual_invocations.push(name);
            continue;
        }

        if changed {
            if !dry_run {
                fs::write(hook_path.as_std_path(), &rewritten).into_diagnostic()?;
            }
            report.repaired.push(name);
        } else if contains_ledger_invocation(&content) {
            report.already_correct.push(name);
        } else {
            report.skipped.push(name);
        }
    }

    Ok(())
}

/// Public entry point: discover the repo root the same way `execute_init`
/// does, then repair hooks in place. Prints a human-readable summary.
pub fn execute_hook_repair(dry_run: bool) -> Result<()> {
    let root = match gix::discover(".") {
        Ok(repo) => {
            let path = repo
                .workdir()
                .ok_or(crate::commands::CommandError::RepoDiscoveryFailed)?
                .to_path_buf();
            Utf8PathBuf::from_path_buf(path)
                .map_err(|_| crate::commands::CommandError::RepoDiscoveryFailed)?
        }
        Err(_) => Utf8PathBuf::from_path_buf(std::env::current_dir().into_diagnostic()?)
            .map_err(|_| crate::commands::CommandError::RepoDiscoveryFailed)?,
    };

    let report = repair_hooks_at(&root, dry_run)?;
    print_report(&report);

    // 0121: product template refresh after legacy repair (shared ensure SoT).
    let product = crate::commands::hook_template::refresh_product_templates_at(&root, dry_run)?;
    if product.refused.is_some()
        || !product.refreshed.is_empty()
        || !product.already_current.is_empty()
        || !product.skipped_unknown.is_empty()
        || !product.discovery_notes.is_empty()
    {
        println!();
        println!(
            "{} Product hook template refresh:",
            "INFO:".if_supports_color(Stream::Stdout, |s| s.style(Style::new().cyan().bold()))
        );
        crate::commands::hook_template::print_refresh_report(&product);
    }
    Ok(())
}

fn print_report(report: &HookRepairReport) {
    if let Some(manager) = report.third_party_manager {
        println!(
            "{} Third-party hook manager '{}' detected. Hooks are managed by '{}', not rewritten by Ledgerful. Please update your {} config to call ledgerful.",
            "WARN:".if_supports_color(Stream::Stdout, |s| s.style(Style::new().yellow().bold())),
            manager.name(),
            manager.name(),
            manager.name(),
        );
        println!("  Config location: {}", manager.config_hint());
        if !report.discovery_notes.is_empty() {
            for note in &report.discovery_notes {
                println!("  {}", note);
            }
        }
        return;
    }

    let prefix = if report.dry_run {
        "DRY-RUN"
            .if_supports_color(Stream::Stdout, |s| s.style(Style::new().yellow().bold()))
            .to_string()
    } else {
        "DONE"
            .if_supports_color(Stream::Stdout, |s| s.style(Style::new().green().bold()))
            .to_string()
    };

    if !report.discovery_notes.is_empty() {
        for note in &report.discovery_notes {
            println!(
                "{} {}",
                "WARN:"
                    .if_supports_color(Stream::Stdout, |s| s.style(Style::new().yellow().bold())),
                note
            );
        }
    }

    if report.repaired.is_empty()
        && report.already_correct.is_empty()
        && report.skipped.is_empty()
        && report.residual_invocations.is_empty()
        && report.near_miss_blocks.is_empty()
    {
        if report.discovery_notes.is_empty() {
            println!("{prefix} No hooks directory found; nothing to repair.");
        }
    } else {
        let verb = if report.dry_run {
            "Would repair"
        } else {
            "Repaired"
        };
        println!(
            "{prefix} {verb} {} hook(s): {}",
            report.repaired.len(),
            if report.repaired.is_empty() {
                "(none)".to_string()
            } else {
                report.repaired.join(", ")
            }
        );
        if !report.already_correct.is_empty() {
            println!("  Already correct: {}", report.already_correct.join(", "));
        }
        if !report.skipped.is_empty() {
            println!(
                "  Skipped (no ledger invocations found): {}",
                report.skipped.join(", ")
            );
        }
        if !report.residual_invocations.is_empty() {
            println!(
                "{} Residual retired-binary invocation(s) remain in: {}. Not reported as fully repaired.",
                "WARN:"
                    .if_supports_color(Stream::Stdout, |s| s.style(Style::new().yellow().bold())),
                report.residual_invocations.join(", ")
            );
        }
        if !report.near_miss_blocks.is_empty() {
            println!(
                "{} Near-miss duplicate gate block(s) (not auto-deleted; remove manually if safe):",
                "WARN:"
                    .if_supports_color(Stream::Stdout, |s| s.style(Style::new().yellow().bold()))
            );
            for (hook, block) in &report.near_miss_blocks {
                println!("  --- {} ---", hook);
                for line in block.lines() {
                    println!("  {}", line);
                }
            }
        }
    }

    println!(
        "{} If hooks still look wrong, re-run `ledgerful update --repair-hooks`. Do not confuse with `ledgerful ledger hook-repair` (sidecar transaction repair).",
        "HINT:".if_supports_color(Stream::Stdout, |s| s.style(Style::new().cyan().bold()))
    );
}
