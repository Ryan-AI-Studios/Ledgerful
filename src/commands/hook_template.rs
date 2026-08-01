//! Product hook template stamp + shared ensure (track 0121).
//!
//! One source of truth for marker-bounded Ledgerful gate blocks used by
//! `init`, `update --repair-hooks` product-refresh, and
//! `doctor --apply-hook-refresh`.
//!
//! Classifier: **current** | **stale** | **unknown**.
//! Replace only **stale**. Skip **unknown** / unparseable boundaries.
//! Never silent-rewrite on bare `verify` or bare `doctor`.

use camino::{Utf8Path, Utf8PathBuf};
use miette::{IntoDiagnostic, Result};
use std::fs;

use crate::commands::hook_repair::{
    HooksDirResolution, detect_third_party_hook_manager, resolve_hooks_dir,
};

/// Product template version stamped into marker comments (`:vN`).
pub const VERIFY_GATE_TEMPLATE_VERSION: u32 = 2;
pub const LEDGER_GATE_TEMPLATE_VERSION: u32 = 2;
pub const INTENT_GATE_TEMPLATE_VERSION: u32 = 2;
pub const POST_COMMIT_GATE_TEMPLATE_VERSION: u32 = 2;

pub const LEDGER_GATE_MARKER: &str = "# ledgerful-ledger-gate";
pub const VERIFY_GATE_MARKER: &str = "# ledgerful-verify-gate";
pub const INTENT_GATE_MARKER: &str = "# ledgerful-intent-gate";
pub const POST_COMMIT_GATE_MARKER: &str = "# ledgerful-post-commit-gate";

/// Classification of an installed marker-bounded block vs the binary template.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemplateClass {
    /// Matches the current stamped product template.
    Current,
    /// Known historical / unstamped product body — safe to replace.
    Stale,
    /// Unrecognised customisation or unparseable boundary — never rewrite.
    Unknown,
}

/// Kind of product gate block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GateKind {
    Ledger,
    Verify,
    Intent,
    PostCommit,
}

impl GateKind {
    pub fn marker(self) -> &'static str {
        match self {
            Self::Ledger => LEDGER_GATE_MARKER,
            Self::Verify => VERIFY_GATE_MARKER,
            Self::Intent => INTENT_GATE_MARKER,
            Self::PostCommit => POST_COMMIT_GATE_MARKER,
        }
    }

    pub fn stamp_prefix(self) -> String {
        let v = match self {
            Self::Ledger => LEDGER_GATE_TEMPLATE_VERSION,
            Self::Verify => VERIFY_GATE_TEMPLATE_VERSION,
            Self::Intent => INTENT_GATE_TEMPLATE_VERSION,
            Self::PostCommit => POST_COMMIT_GATE_TEMPLATE_VERSION,
        };
        format!("{}:v{v}", self.marker())
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ledger => "ledger-gate",
            Self::Verify => "verify-gate",
            Self::Intent => "intent-gate",
            Self::PostCommit => "post-commit-gate",
        }
    }
}

// ---------------------------------------------------------------------------
// Current product templates (stamped v2)
// ---------------------------------------------------------------------------

/// Ledger cleanliness gate (pre-commit / pre-push). `{bypass_command}` substituted.
pub fn ledger_gate_block(bypass_command: &str) -> String {
    format!(
        "\
# ledgerful-ledger-gate:v{LEDGER_GATE_TEMPLATE_VERSION} auto-installed by `ledgerful init`
if command -v ledgerful &>/dev/null; then
    if ! ledgerful ledger status --compact --exit-code --verify-signatures; then
        echo \"[Ledgerful] Blocked by ledger state.\"
        echo \"[Ledgerful] Resolve with:\"
        echo \"[Ledgerful]   Pending tx:  ledgerful ledger commit <tx-id> --summary '...' --reason '...'\"
        echo \"[Ledgerful]   Drift:       ledgerful ledger reconcile --all --reason '...'\"
        echo \"[Ledgerful] Fix the issues or bypass with: {bypass_command}\"
        exit 1
    fi
fi
"
    )
}

/// Pre-push only: fast scoped verify.
pub fn verify_gate_block(bypass_command: &str) -> String {
    format!(
        "\
# ledgerful-verify-gate:v{VERIFY_GATE_TEMPLATE_VERSION} fast scoped verification (pre-push only)
if command -v ledgerful &>/dev/null; then
    if ! ledgerful verify --scope fast; then
        echo \"[Ledgerful] Push blocked by verification failure.\"
        echo \"[Ledgerful] Fix the issues or bypass with: {bypass_command}\"
        exit 1
    fi
fi
"
    )
}

pub fn intent_gate_block() -> String {
    format!(
        "\
# ledgerful-intent-gate:v{INTENT_GATE_TEMPLATE_VERSION} auto-installed by `ledgerful init`
if command -v ledgerful &>/dev/null; then
    ledgerful internal hook-commit-msg \"$1\"
fi
"
    )
}

pub fn post_commit_gate_block() -> String {
    format!(
        "\
# ledgerful-post-commit-gate:v{POST_COMMIT_GATE_TEMPLATE_VERSION} auto-installed by `ledgerful init`
if command -v ledgerful &>/dev/null; then
    ledgerful internal hook-post-commit \"$@\"
fi
"
    )
}

// ---------------------------------------------------------------------------
// Historical (unstamped / pre-v2) product bodies — treat as stale
// ---------------------------------------------------------------------------

fn historical_ledger_gate_bodies(bypass_command: &str) -> Vec<String> {
    vec![format!(
        "\
# ledgerful-ledger-gate: auto-installed by `ledgerful init`
if command -v ledgerful &>/dev/null; then
    if ! ledgerful ledger status --compact --exit-code --verify-signatures; then
        echo \"[Ledgerful] Blocked by ledger state.\"
        echo \"[Ledgerful] Resolve with:\"
        echo \"[Ledgerful]   Pending tx:  ledgerful ledger commit <tx-id> --summary '...' --reason '...'\"
        echo \"[Ledgerful]   Drift:       ledgerful ledger reconcile --all --reason '...'\"
        echo \"[Ledgerful] Fix the issues or bypass with: {bypass_command}\"
        exit 1
    fi
fi
"
    )]
}

fn historical_verify_gate_bodies(bypass_command: &str) -> Vec<String> {
    vec![format!(
        "\
# ledgerful-verify-gate: fast scoped verification (pre-push only)
if command -v ledgerful &>/dev/null; then
    if ! ledgerful verify --scope fast; then
        echo \"[Ledgerful] Push blocked by verification failure.\"
        echo \"[Ledgerful] Fix the issues or bypass with: {bypass_command}\"
        exit 1
    fi
fi
"
    )]
}

fn historical_intent_gate_bodies() -> Vec<String> {
    vec![
        "\
# ledgerful-intent-gate: auto-installed by `ledgerful init`
if command -v ledgerful &>/dev/null; then
    ledgerful internal hook-commit-msg \"$1\"
fi
"
        .to_string(),
    ]
}

fn historical_post_commit_gate_bodies() -> Vec<String> {
    vec![
        "\
# ledgerful-post-commit-gate: auto-installed by `ledgerful init`
if command -v ledgerful &>/dev/null; then
    ledgerful internal hook-post-commit \"$@\"
fi
"
        .to_string(),
    ]
}

fn current_body(kind: GateKind, bypass_command: &str) -> String {
    match kind {
        GateKind::Ledger => ledger_gate_block(bypass_command),
        GateKind::Verify => verify_gate_block(bypass_command),
        GateKind::Intent => intent_gate_block(),
        GateKind::PostCommit => post_commit_gate_block(),
    }
}

fn historical_bodies(kind: GateKind, bypass_command: &str) -> Vec<String> {
    match kind {
        GateKind::Ledger => historical_ledger_gate_bodies(bypass_command),
        GateKind::Verify => historical_verify_gate_bodies(bypass_command),
        GateKind::Intent => historical_intent_gate_bodies(),
        GateKind::PostCommit => historical_post_commit_gate_bodies(),
    }
}

fn normalize_block(s: &str) -> String {
    s.replace("\r\n", "\n").trim().to_string()
}

/// Classify an extracted block body (including its marker line).
pub fn classify_block(kind: GateKind, block: &str, bypass_command: &str) -> TemplateClass {
    let norm = normalize_block(block);
    if norm.is_empty() {
        return TemplateClass::Unknown;
    }
    let current = normalize_block(&current_body(kind, bypass_command));
    if norm == current {
        return TemplateClass::Current;
    }
    for hist in historical_bodies(kind, bypass_command) {
        if norm == normalize_block(&hist) {
            return TemplateClass::Stale;
        }
    }
    // Marker present but body does not match any known product template.
    TemplateClass::Unknown
}

// ---------------------------------------------------------------------------
// Marker-bounded extraction (marker → matching closing fi boundary)
// ---------------------------------------------------------------------------

/// Outcome of attempting to extract a marker-bounded product block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarkerExtract {
    /// Marker string not present in content.
    Absent,
    /// Marker present but matching `fi` boundary could not be resolved.
    Unparseable { reason: String },
    /// Marker-bounded block from `start..end` (byte offsets into content).
    Found {
        start: usize,
        end: usize,
        block: String,
    },
}

/// Extract the first marker-bounded block for `kind` from hook content.
///
/// Boundary: from the marker line through the matching `fi` nest of the outer
/// `if command -v ledgerful` block (product templates use two closing `fi`).
/// End is exclusive and may include a single trailing newline after the last
/// `fi` for clean replace. Offsets are CRLF-safe.
pub fn extract_marker_block(content: &str, kind: GateKind) -> MarkerExtract {
    let marker = kind.marker();
    let Some(start) = content.find(marker) else {
        return MarkerExtract::Absent;
    };
    // Walk back to start of line if marker is mid-line (should not happen).
    let line_start = content[..start].rfind('\n').map(|i| i + 1).unwrap_or(0);

    // From the marker line, count `if`/`fi` shell keywords to find block end.
    let rest = &content[line_start..];
    let mut depth: i32 = 0;
    let mut saw_if = false;
    let mut end_rel: Option<usize> = None;
    let mut offset = 0usize;

    for line in rest.lines() {
        let line_len = line.len();
        let after_content = offset + line_len;
        // CRLF-safe: str::lines() strips endings; advance by the real ending length.
        let ending_len = line_ending_len(rest, after_content);
        let line_with_nl = after_content + ending_len;

        let t = line.trim();
        // Shell keywords: crude but matches product templates and known history.
        // Also accept `fi\r` residue if any caller passes un-normalized content
        // (lines() already strips `\r`).
        if t.starts_with("if ") || t == "if" {
            depth += 1;
            saw_if = true;
        } else if t == "fi" {
            depth -= 1;
            if saw_if && depth == 0 {
                end_rel = Some(line_with_nl.min(rest.len()));
                break;
            }
        }
        offset = line_with_nl;
    }

    let Some(end_rel) = end_rel else {
        // Marker present but no matching closing `fi` — refuse rewrite.
        return MarkerExtract::Unparseable {
            reason: "unrecognised block boundary".to_string(),
        };
    };
    let end = line_start + end_rel;
    // Swallow one trailing newline after the block for clean replace (LF or CRLF).
    let end = end + line_ending_len(content, end);
    let block = content[line_start..end].to_string();
    MarkerExtract::Found {
        start: line_start,
        end,
        block,
    }
}

/// Byte length of the line ending at `pos` in `s` (`\r\n` → 2, `\n`/`\r` → 1, else 0).
fn line_ending_len(s: &str, pos: usize) -> usize {
    if pos >= s.len() {
        return 0;
    }
    let bytes = s.as_bytes();
    if bytes[pos] == b'\r' && bytes.get(pos + 1) == Some(&b'\n') {
        2
    } else if bytes[pos] == b'\n' || bytes[pos] == b'\r' {
        1
    } else {
        0
    }
}

/// Result of ensuring one gate kind in one hook file's content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnsureAction {
    AlreadyCurrent,
    Refreshed,
    SkippedUnknown {
        snippet: String,
    },
    /// Marker present but boundary unparseable — never rewrite; surface reason.
    SkippedUnparseable {
        reason: String,
        snippet: String,
    },
    MarkerAbsent,
}

/// Replace a stale block in-place; leave current/unknown/absent alone.
pub fn ensure_block_in_content(
    content: &str,
    kind: GateKind,
    bypass_command: &str,
) -> (String, EnsureAction) {
    match extract_marker_block(content, kind) {
        MarkerExtract::Absent => (content.to_string(), EnsureAction::MarkerAbsent),
        MarkerExtract::Unparseable { reason } => {
            // Spec §2.5 option A #4: skip with clear message + recommended snippet.
            let mut snippet = current_body(kind, bypass_command);
            if !snippet.ends_with('\n') {
                snippet.push('\n');
            }
            (
                content.to_string(),
                EnsureAction::SkippedUnparseable { reason, snippet },
            )
        }
        MarkerExtract::Found { start, end, block } => {
            match classify_block(kind, &block, bypass_command) {
                TemplateClass::Current => (content.to_string(), EnsureAction::AlreadyCurrent),
                TemplateClass::Stale => {
                    let replacement = {
                        let mut b = current_body(kind, bypass_command);
                        if !b.ends_with('\n') {
                            b.push('\n');
                        }
                        b
                    };
                    let mut out = String::with_capacity(content.len() + replacement.len());
                    out.push_str(&content[..start]);
                    // Preserve a leading newline if we are mid-file and replacement
                    // does not already start after one.
                    if start > 0 && !content[..start].ends_with('\n') {
                        out.push('\n');
                    }
                    out.push_str(&replacement);
                    if end < content.len()
                        && !out.ends_with('\n')
                        && !content[end..].starts_with('\n')
                        && !content[end..].starts_with('\r')
                    {
                        out.push('\n');
                    }
                    out.push_str(&content[end..]);
                    (out, EnsureAction::Refreshed)
                }
                TemplateClass::Unknown => {
                    let snippet: String = block.lines().take(6).collect::<Vec<_>>().join("\n");
                    (
                        content.to_string(),
                        EnsureAction::SkippedUnknown { snippet },
                    )
                }
            }
        }
    }
}

/// Which gates apply to a given hook filename.
pub fn gates_for_hook(hook_name: &str) -> Vec<GateKind> {
    match hook_name {
        "pre-commit" => vec![GateKind::Ledger],
        "pre-push" => vec![GateKind::Ledger, GateKind::Verify],
        "commit-msg" => vec![GateKind::Intent],
        "post-commit" => vec![GateKind::PostCommit],
        _ => Vec::new(),
    }
}

fn bypass_for_hook(hook_name: &str) -> &'static str {
    match hook_name {
        "pre-commit" => "git commit --no-verify",
        "pre-push" => "git push --no-verify",
        _ => "git commit --no-verify",
    }
}

/// Report from a product-template refresh pass over a repo's hooks.
#[derive(Debug, Clone, Default)]
pub struct HookTemplateRefreshReport {
    /// Labels like `pre-push:verify-gate`.
    pub refreshed: Vec<String>,
    pub already_current: Vec<String>,
    /// `(label, reason_or_snippet)`.
    pub skipped_unknown: Vec<(String, String)>,
    /// Third-party / outside-repo / cannot-look refuse reason.
    pub refused: Option<String>,
    pub dry_run: bool,
    pub discovery_notes: Vec<String>,
}

impl HookTemplateRefreshReport {
    pub fn empty(dry_run: bool) -> Self {
        Self {
            dry_run,
            ..Default::default()
        }
    }

    pub fn is_noop(&self) -> bool {
        self.refreshed.is_empty()
            && self.skipped_unknown.is_empty()
            && self.refused.is_none()
            && self.discovery_notes.is_empty()
    }
}

/// Refresh stale product templates under the resolved hooks dir.
///
/// Refuses third-party managers and outside-repo hooksPath. Never appends
/// missing markers (that is `init`); only upgrades existing marker blocks.
pub fn refresh_product_templates_at(
    repo_root: &Utf8Path,
    dry_run: bool,
) -> Result<HookTemplateRefreshReport> {
    let mut report = HookTemplateRefreshReport::empty(dry_run);

    if let Some(manager) = detect_third_party_hook_manager(repo_root) {
        report.refused = Some(format!(
            "third-party hook manager '{}' owns hooks; paste the product snippet from docs or configure {} to call ledgerful (refusing rewrite)",
            manager.name(),
            manager.name()
        ));
        return Ok(report);
    }

    let hooks_dir = match resolve_hooks_dir(repo_root) {
        HooksDirResolution::Found { hooks_dir } => hooks_dir,
        HooksDirResolution::OutsideRepo { hooks_dir } => {
            report.refused = Some(format!(
                "hooks path '{hooks_dir}' is outside the repository; refusing rewrite"
            ));
            return Ok(report);
        }
        HooksDirResolution::CannotLook { reason } => {
            report
                .discovery_notes
                .push(format!("cannot resolve hooks directory: {reason}"));
            return Ok(report);
        }
    };

    if !hooks_dir.is_dir() {
        report.discovery_notes.push(format!(
            "hooks directory '{hooks_dir}' does not exist or is not a directory"
        ));
        return Ok(report);
    }

    refresh_hooks_in_dir(&hooks_dir, dry_run, &mut report)?;
    report.refreshed.sort();
    report.already_current.sort();
    report.skipped_unknown.sort_by(|a, b| a.0.cmp(&b.0));
    report.discovery_notes.sort();
    Ok(report)
}

fn refresh_hooks_in_dir(
    hooks_dir: &Utf8Path,
    dry_run: bool,
    report: &mut HookTemplateRefreshReport,
) -> Result<()> {
    let entries = fs::read_dir(hooks_dir.as_std_path()).into_diagnostic()?;
    let mut files: Vec<Utf8PathBuf> = Vec::new();
    for entry in entries {
        let entry = entry.into_diagnostic()?;
        let path = entry.path();
        if path.is_dir() {
            continue;
        }
        let Ok(utf8) = Utf8PathBuf::from_path_buf(path) else {
            continue;
        };
        let Some(name) = utf8.file_name() else {
            continue;
        };
        if name.ends_with(".sample") {
            continue;
        }
        if gates_for_hook(name).is_empty() {
            continue;
        }
        files.push(utf8);
    }
    files.sort();

    for hook_path in files {
        let name = hook_path.file_name().unwrap_or("").to_string();
        let content = fs::read_to_string(hook_path.as_std_path()).into_diagnostic()?;
        let bypass = bypass_for_hook(&name);
        let mut next = content.clone();
        let mut any_write = false;

        for kind in gates_for_hook(&name) {
            let label = format!("{name}:{}", kind.as_str());
            let (updated, action) = ensure_block_in_content(&next, kind, bypass);
            match action {
                EnsureAction::AlreadyCurrent => {
                    report.already_current.push(label);
                }
                EnsureAction::Refreshed => {
                    report.refreshed.push(label);
                    next = updated;
                    any_write = true;
                }
                EnsureAction::SkippedUnknown { snippet } => {
                    report.skipped_unknown.push((
                        label,
                        format!(
                            "unrecognised block boundary or customised body; not rewritten. Snippet:\n{snippet}"
                        ),
                    ));
                }
                EnsureAction::SkippedUnparseable { reason, snippet } => {
                    report.skipped_unknown.push((
                        label,
                        format!("{reason}; not rewritten. Recommended snippet:\n{snippet}"),
                    ));
                }
                EnsureAction::MarkerAbsent => {
                    // Product refresh does not install missing gates.
                }
            }
        }

        if any_write && !dry_run {
            fs::write(hook_path.as_std_path(), &next).into_diagnostic()?;
        }
    }

    Ok(())
}

/// Doctor findings for product template stamp drift (Info + Gate).
///
/// Does **not** extend legacy-only findings. Code `hook-template-stale`.
/// Severity Info so `readyForPublish` stays true.
pub fn hook_template_stale_findings(
    repo_root: &Utf8Path,
) -> Vec<crate::commands::doctor::DoctorFinding> {
    use crate::commands::doctor::{DoctorCategory, DoctorFinding};

    let mut findings = Vec::new();

    // Third-party / outside: still detect if we can read files, but prefer a
    // non-apply path message. Spec: refuse apply; detect can still surface.
    let hooks_dir = match resolve_hooks_dir(repo_root) {
        HooksDirResolution::Found { hooks_dir } => hooks_dir,
        HooksDirResolution::OutsideRepo { .. } | HooksDirResolution::CannotLook { .. } => {
            return findings;
        }
    };
    if !hooks_dir.is_dir() {
        return findings;
    }

    let entries = match fs::read_dir(hooks_dir.as_std_path()) {
        Ok(e) => e,
        Err(_) => return findings,
    };

    let mut stale_labels = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        if name.ends_with(".sample") {
            continue;
        }
        let kinds = gates_for_hook(&name);
        if kinds.is_empty() {
            continue;
        }
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        let bypass = bypass_for_hook(&name);
        for kind in kinds {
            if let MarkerExtract::Found { block, .. } = extract_marker_block(&content, kind)
                && classify_block(kind, &block, bypass) == TemplateClass::Stale
            {
                stale_labels.push(format!("{name}:{}", kind.as_str()));
            }
        }
    }

    if !stale_labels.is_empty() {
        stale_labels.sort();
        let list = stale_labels.join(", ");
        findings.push(DoctorFinding::info(
            "hook-template-stale",
            DoctorCategory::Gate,
            format!(
                "Ledgerful hook product template(s) are stale ({list}). Run `ledgerful doctor --apply-hook-refresh` (or `ledgerful update --repair-hooks`) to refresh known marker-bounded blocks. Third-party managers are refused — paste the snippet from docs."
            ),
        ));
    }

    findings.sort_by(|a, b| a.code.cmp(&b.code).then(a.message.cmp(&b.message)));
    findings
}

/// Human summary for apply / dry-run.
pub fn print_refresh_report(report: &HookTemplateRefreshReport) {
    use owo_colors::OwoColorize;

    if let Some(reason) = &report.refused {
        println!("{} {reason}", "REFUSED:".yellow().bold());
        return;
    }
    for note in &report.discovery_notes {
        println!("{} {note}", "WARN:".yellow().bold());
    }
    let prefix = if report.dry_run {
        "DRY-RUN".yellow().bold().to_string()
    } else {
        "DONE".green().bold().to_string()
    };
    if report.refreshed.is_empty()
        && report.already_current.is_empty()
        && report.skipped_unknown.is_empty()
    {
        println!("{prefix} No product hook templates to refresh.");
        return;
    }
    let verb = if report.dry_run {
        "Would refresh"
    } else {
        "Refreshed"
    };
    println!(
        "{prefix} {verb} {} block(s): {}",
        report.refreshed.len(),
        if report.refreshed.is_empty() {
            "(none)".to_string()
        } else {
            report.refreshed.join(", ")
        }
    );
    if !report.already_current.is_empty() {
        println!("  Already current: {}", report.already_current.join(", "));
    }
    for (label, reason) in &report.skipped_unknown {
        println!("{} skipped {label}: {reason}", "SKIP:".yellow().bold());
    }
    if report.refreshed.is_empty()
        && report.skipped_unknown.is_empty()
        && !report.already_current.is_empty()
    {
        println!(
            "{} Hook product templates already current.",
            "OK:".green().bold()
        );
    }
}

/// Ensure a single gate in a hook file path (used by init). Returns true when
/// a write occurred (append or refresh).
pub fn ensure_gate_on_hook_file(
    hook_path: &Utf8Path,
    kind: GateKind,
    bypass_command: &str,
    append_if_absent: bool,
) -> Result<bool> {
    if !hook_path.exists() {
        return Ok(false);
    }
    let existing = fs::read_to_string(hook_path.as_std_path()).into_diagnostic()?;
    if existing.contains(kind.marker()) {
        let (updated, action) = ensure_block_in_content(&existing, kind, bypass_command);
        match action {
            EnsureAction::Refreshed => {
                fs::write(hook_path.as_std_path(), updated).into_diagnostic()?;
                return Ok(true);
            }
            EnsureAction::SkippedUnknown { snippet } => {
                eprintln!(
                    "INFO: hook {} has customised {} block; not rewritten. Snippet:\n{}",
                    hook_path.file_name().unwrap_or("hook"),
                    kind.as_str(),
                    snippet
                );
                return Ok(false);
            }
            EnsureAction::SkippedUnparseable { reason, snippet } => {
                eprintln!(
                    "INFO: hook {} {} {}; not rewritten. Recommended snippet:\n{}",
                    hook_path.file_name().unwrap_or("hook"),
                    kind.as_str(),
                    reason,
                    snippet
                );
                return Ok(false);
            }
            EnsureAction::AlreadyCurrent | EnsureAction::MarkerAbsent => return Ok(false),
        }
    }
    if append_if_absent {
        use std::io::Write as _;
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(hook_path.as_std_path())
            .into_diagnostic()?;
        let block = current_body(kind, bypass_command);
        let block = format!("\n{block}");
        file.write_all(block.as_bytes()).into_diagnostic()?;
        return Ok(true);
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_current_stamped_verify() {
        let body = verify_gate_block("git push --no-verify");
        assert_eq!(
            classify_block(GateKind::Verify, &body, "git push --no-verify"),
            TemplateClass::Current
        );
        assert!(body.contains("# ledgerful-verify-gate:v2"));
        assert!(body.contains("# ledgerful-verify-gate")); // prefix for contains checks
    }

    #[test]
    fn classify_historical_unstamped_verify_as_stale() {
        let hist = &historical_verify_gate_bodies("git push --no-verify")[0];
        assert!(!hist.contains(":v2"));
        assert_eq!(
            classify_block(GateKind::Verify, hist, "git push --no-verify"),
            TemplateClass::Stale
        );
    }

    #[test]
    fn classify_custom_verify_as_unknown() {
        let custom = "\
# ledgerful-verify-gate: full quality gate before push
if command -v ledgerful &>/dev/null; then
    if ! ledgerful verify; then
        exit 1
    fi
fi
";
        assert_eq!(
            classify_block(GateKind::Verify, custom, "git push --no-verify"),
            TemplateClass::Unknown
        );
    }

    #[test]
    fn ensure_replaces_stale_preserves_surrounding() {
        let hist = historical_verify_gate_bodies("git push --no-verify")[0].clone();
        let content = format!("#!/usr/bin/env bash\n# user line\n{hist}\necho after\n");
        let (out, action) =
            ensure_block_in_content(&content, GateKind::Verify, "git push --no-verify");
        assert_eq!(action, EnsureAction::Refreshed);
        assert!(out.contains("# user line"));
        assert!(out.contains("echo after"));
        assert!(out.contains("# ledgerful-verify-gate:v2"));
        assert!(!out.contains("# ledgerful-verify-gate: fast scoped"));
    }

    #[test]
    fn ensure_skips_unknown() {
        let custom = "\
#!/usr/bin/env bash
# ledgerful-verify-gate: custom
if command -v ledgerful &>/dev/null; then
    if ! ledgerful verify; then
        exit 1
    fi
fi
";
        let (out, action) =
            ensure_block_in_content(custom, GateKind::Verify, "git push --no-verify");
        assert!(matches!(action, EnsureAction::SkippedUnknown { .. }));
        assert_eq!(out, custom);
    }

    #[test]
    fn ensure_already_current_noop() {
        let body = verify_gate_block("git push --no-verify");
        let content = format!("#!/usr/bin/env bash\n{body}");
        let (out, action) =
            ensure_block_in_content(&content, GateKind::Verify, "git push --no-verify");
        assert_eq!(action, EnsureAction::AlreadyCurrent);
        assert_eq!(out, content);
    }

    #[test]
    fn extract_marker_block_finds_nested_fi() {
        let body = verify_gate_block("git push --no-verify");
        let content = format!("#!/bin/bash\n{body}\necho x\n");
        let MarkerExtract::Found { start, end, block } =
            extract_marker_block(&content, GateKind::Verify)
        else {
            panic!("expected Found block");
        };
        assert!(block.contains("ledgerful verify --scope fast"));
        assert!(
            content[start..end].contains("fi\nfi") || content[start..end].contains("fi\r\nfi") || {
                // Count fi lines
                block.lines().filter(|l| l.trim() == "fi").count() == 2
            }
        );
        assert!(!block.contains("echo x"));
    }

    #[test]
    fn extract_and_ensure_crlf_body_classifies_current() {
        // Windows hook files often use CRLF; offsets must advance by 2.
        let body = verify_gate_block("git push --no-verify");
        let lf = format!("#!/usr/bin/env bash\n{body}");
        let crlf = lf.replace('\n', "\r\n");
        let MarkerExtract::Found { block, .. } = extract_marker_block(&crlf, GateKind::Verify)
        else {
            panic!("CRLF extract must find block");
        };
        assert_eq!(
            classify_block(GateKind::Verify, &block, "git push --no-verify"),
            TemplateClass::Current
        );
        let (out, action) =
            ensure_block_in_content(&crlf, GateKind::Verify, "git push --no-verify");
        assert_eq!(action, EnsureAction::AlreadyCurrent);
        assert_eq!(out, crlf);
    }

    #[test]
    fn extract_and_ensure_crlf_stale_refreshes() {
        let hist = historical_verify_gate_bodies("git push --no-verify")[0].clone();
        let lf = format!("#!/usr/bin/env bash\n{hist}\necho after\n");
        let crlf = lf.replace('\n', "\r\n");
        let (out, action) =
            ensure_block_in_content(&crlf, GateKind::Verify, "git push --no-verify");
        assert_eq!(action, EnsureAction::Refreshed);
        assert!(out.contains("# ledgerful-verify-gate:v2"));
        assert!(out.contains("echo after"));
        assert!(!out.contains("# ledgerful-verify-gate: fast scoped"));
    }

    #[test]
    fn unparseable_boundary_surfaces_skip_not_marker_absent() {
        // Marker present, no matching `fi` — must not look like MarkerAbsent.
        let broken = "\
#!/usr/bin/env bash
# ledgerful-verify-gate:v2 broken open block
if command -v ledgerful &>/dev/null; then
    if ! ledgerful verify --scope fast; then
        exit 1
";
        let (out, action) =
            ensure_block_in_content(broken, GateKind::Verify, "git push --no-verify");
        assert_eq!(out, broken);
        match action {
            EnsureAction::SkippedUnparseable { reason, snippet } => {
                assert!(
                    reason.contains("unrecognised block boundary"),
                    "reason={reason}"
                );
                assert!(
                    snippet.contains("# ledgerful-verify-gate:v2"),
                    "recommended snippet must include current stamp"
                );
                assert!(snippet.contains("ledgerful verify --scope fast"));
            }
            other => panic!("expected SkippedUnparseable, got {other:?}"),
        }
        // extract API also surfaces Unparseable (not Absent).
        assert!(matches!(
            extract_marker_block(broken, GateKind::Verify),
            MarkerExtract::Unparseable { .. }
        ));
    }

    #[test]
    fn refresh_reports_unparseable_boundary() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        fs::create_dir_all(root.join(".git").join("hooks")).unwrap();
        let hook = root.join(".git").join("hooks").join("pre-push");
        let broken = "\
#!/usr/bin/env bash
# ledgerful-verify-gate:v2 broken
if command -v ledgerful &>/dev/null; then
    ledgerful verify --scope fast
";
        fs::write(&hook, broken).unwrap();
        let report = refresh_product_templates_at(&root, false).unwrap();
        assert!(
            report.skipped_unknown.iter().any(|(label, reason)| {
                label.contains("verify-gate")
                    && reason.contains("unrecognised block boundary")
                    && reason.contains("Recommended snippet")
            }),
            "skipped_unknown={:?}",
            report.skipped_unknown
        );
        // File must remain unchanged (no partial rewrite).
        assert_eq!(fs::read_to_string(&hook).unwrap(), broken);
    }

    #[test]
    fn refresh_stale_then_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        fs::create_dir_all(root.join(".git").join("hooks")).unwrap();
        let hook = root.join(".git").join("hooks").join("pre-push");
        let hist = historical_verify_gate_bodies("git push --no-verify")[0].clone();
        let ledger = historical_ledger_gate_bodies("git push --no-verify")[0].clone();
        fs::write(&hook, format!("#!/usr/bin/env bash\n{ledger}\n{hist}\n")).unwrap();

        let r1 = refresh_product_templates_at(&root, false).unwrap();
        assert!(
            r1.refreshed.iter().any(|l| l.contains("verify-gate")),
            "refreshed={:?}",
            r1.refreshed
        );
        let after = fs::read_to_string(&hook).unwrap();
        assert!(after.contains("# ledgerful-verify-gate:v2"));
        assert!(after.contains("# ledgerful-ledger-gate:v2"));

        let r2 = refresh_product_templates_at(&root, false).unwrap();
        assert!(r2.refreshed.is_empty(), "second apply should no-op");
        assert!(!r2.already_current.is_empty());
    }

    #[test]
    fn refresh_refuses_husky() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        fs::create_dir_all(root.join(".husky")).unwrap();
        fs::create_dir_all(root.join(".git").join("hooks")).unwrap();
        let r = refresh_product_templates_at(&root, false).unwrap();
        assert!(r.refused.as_ref().unwrap().contains("husky"));
    }

    #[test]
    fn stale_findings_info_gate_not_block() {
        use crate::commands::doctor::{DoctorCategory, DoctorSeverity, ready_for_publish};

        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        fs::create_dir_all(root.join(".git").join("hooks")).unwrap();
        let hook = root.join(".git").join("hooks").join("pre-push");
        let hist = historical_verify_gate_bodies("git push --no-verify")[0].clone();
        fs::write(&hook, format!("#!/usr/bin/env bash\n{hist}\n")).unwrap();

        let findings = hook_template_stale_findings(&root);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].code, "hook-template-stale");
        assert_eq!(findings[0].severity, DoctorSeverity::Info);
        assert_eq!(findings[0].category, DoctorCategory::Gate);
        assert!(
            findings[0]
                .message
                .contains("ledgerful doctor --apply-hook-refresh")
        );
        assert!(ready_for_publish(&findings));
    }

    #[test]
    fn dry_run_does_not_write() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        fs::create_dir_all(root.join(".git").join("hooks")).unwrap();
        let hook = root.join(".git").join("hooks").join("pre-push");
        let hist = historical_verify_gate_bodies("git push --no-verify")[0].clone();
        let before = format!("#!/usr/bin/env bash\n{hist}\n");
        fs::write(&hook, &before).unwrap();

        let r = refresh_product_templates_at(&root, true).unwrap();
        assert!(r.dry_run);
        assert!(!r.refreshed.is_empty());
        let after = fs::read_to_string(&hook).unwrap();
        assert_eq!(after, before);
    }
}
