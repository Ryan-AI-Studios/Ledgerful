//! SCIP symbol string → native `project_symbols.id` resolver (0095).
//!
//! Definition occurrences (`symbol_roles & 0x1`) map via document path + range
//! containment to the **innermost** containing native symbol. Ambiguous or
//! missing matches yield no mapping — never a guess.

use crate::scip::range::{ScipRange, line_in_span, parse_scip_range};
use miette::{IntoDiagnostic, Result};
use rusqlite::Connection;
use std::collections::HashMap;

/// SCIP Definition bit (`SymbolRole::Definition = 1`).
pub const SCIP_ROLE_DEFINITION: i32 = 0x1;

/// Evidence marker distinguishing SCIP-augmented edges from native `call_expr:*`.
pub const SCIP_EDGE_EVIDENCE: &str = "scip:ref";

/// One native symbol row used for containment matching.
#[derive(Debug, Clone)]
pub struct NativeSymbolSpan {
    pub id: i64,
    pub file_id: i64,
    pub line_start: i32,
    pub line_end: i32,
}

/// Outcome of caller resolution for a SCIP reference occurrence (0157).
///
/// Callers must treat skip reasons as exclusive: one occurrence maps to at most
/// one skip counter in `ScipEdgeStats`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveCallerOutcome {
    /// Mapped to a native caller symbol id.
    Resolved(i64),
    /// Both enclosing and occurrence ranges mapped, but to different symbols.
    EnclosingDisagreement {
        enclosing_id: i64,
        occurrence_id: i64,
    },
    /// No native container for the chosen range(s).
    Unmapped,
    /// Occurrence classic range could not be parsed (empty / wrong length).
    InvalidOccurrenceRange,
}

/// Full result of `resolve_caller_for_reference`, including fallback honesty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolveCallerResult {
    pub outcome: ResolveCallerOutcome,
    /// True when `enclosing_range` was non-empty but invalid, so resolution
    /// fell back to the occurrence range (today's behavior; not an edge skip).
    pub used_invalid_enclosing_fallback: bool,
    /// True when enclosing and occurrence mapped to different ids but nest
    /// geometry recovered the innermost (0166). Exclusive with disagreement.
    pub recovered_nest_prefer: bool,
}

/// Resolve SCIP definition occurrences to native symbol ids.
///
/// Keyed by the raw SCIP symbol string. Built only from Definition occurrences
/// (`symbol_roles & SCIP_ROLE_DEFINITION != 0`).
#[derive(Debug, Default)]
pub struct ScipNativeResolver {
    /// SCIP symbol string → native `project_symbols.id`
    map: HashMap<String, i64>,
    /// Stats for JSON / logging
    pub definitions_seen: usize,
    pub definitions_mapped: usize,
    pub definitions_unmapped: usize,
}

impl ScipNativeResolver {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, scip_symbol: &str) -> Option<i64> {
        self.map.get(scip_symbol).copied()
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Record a Definition occurrence mapping when resolvable.
    pub fn try_map_definition(
        &mut self,
        scip_symbol: &str,
        occurrence_range: &ScipRange,
        native_in_file: &[NativeSymbolSpan],
    ) {
        self.definitions_seen += 1;
        match resolve_innermost(occurrence_range.start_line, native_in_file) {
            Some(id) => {
                // First mapping wins; subsequent defs of the same symbol keep
                // the first successful resolution (deterministic by document order).
                if let std::collections::hash_map::Entry::Vacant(e) =
                    self.map.entry(scip_symbol.to_string())
                {
                    e.insert(id);
                    self.definitions_mapped += 1;
                }
            }
            None => {
                self.definitions_unmapped += 1;
            }
        }
    }
}

/// Resolve the innermost native symbol containing `line` (1-based).
///
/// Among symbols with `line_start <= line <= line_end`, pick the smallest span
/// (`line_end - line_start`). A tie on span size → `None` (refuse ambiguity).
/// No containing symbol → `None`.
pub fn resolve_innermost(line: i32, candidates: &[NativeSymbolSpan]) -> Option<i64> {
    let mut best: Option<(i64, i32)> = None; // (id, span)
    let mut tie = false;

    for c in candidates {
        if !line_in_span(line, c.line_start, c.line_end) {
            continue;
        }
        let span = c.line_end.saturating_sub(c.line_start);
        match best {
            None => {
                best = Some((c.id, span));
                tie = false;
            }
            Some((_, best_span)) if span < best_span => {
                best = Some((c.id, span));
                tie = false;
            }
            Some((_, best_span)) if span == best_span => {
                // Equal span size: ambiguous unless same id (duplicate row)
                if best.map(|(id, _)| id) != Some(c.id) {
                    tie = true;
                }
            }
            Some(_) => {} // larger span — skip
        }
    }

    if tie {
        return None;
    }
    best.map(|(id, _)| id)
}

/// Load native symbols with line ranges for one file.
pub fn load_native_spans_for_file(
    conn: &Connection,
    file_id: i64,
) -> Result<Vec<NativeSymbolSpan>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, file_id, line_start, line_end FROM project_symbols \
             WHERE file_id = ?1 AND line_start IS NOT NULL AND line_end IS NOT NULL \
             ORDER BY id",
        )
        .into_diagnostic()?;

    let rows = stmt
        .query_map([file_id], |row| {
            Ok(NativeSymbolSpan {
                id: row.get(0)?,
                file_id: row.get(1)?,
                line_start: row.get(2)?,
                line_end: row.get(3)?,
            })
        })
        .into_diagnostic()?;

    let mut out = Vec::new();
    for row in rows {
        out.push(row.into_diagnostic()?);
    }
    // Deterministic order for equal-span tie-breaking walks
    out.sort_by_key(|s| (s.line_start, s.line_end, s.id));
    Ok(out)
}

/// True when `outer` strictly contains `inner` on the line axis.
///
/// Equal full spans return `false` (ambiguity refuse — same posture as
/// equal-span innermost ties).
pub fn strictly_contains(outer: &NativeSymbolSpan, inner: &NativeSymbolSpan) -> bool {
    outer.line_start <= inner.line_start
        && outer.line_end >= inner.line_end
        && (outer.line_start < inner.line_start || outer.line_end > inner.line_end)
}

/// Prefer the innermost of two mapped native ids when one span strictly contains
/// the other. Missing span rows, equal full spans, or non-nested geometry →
/// `None` (keep enclosing disagreement; never invent).
pub fn prefer_nested(a: i64, b: i64, spans: &[NativeSymbolSpan]) -> Option<i64> {
    let sa = spans.iter().find(|s| s.id == a)?;
    let sb = spans.iter().find(|s| s.id == b)?;
    if strictly_contains(sa, sb) {
        return Some(b);
    }
    if strictly_contains(sb, sa) {
        return Some(a);
    }
    None
}

/// Identify the caller native symbol for a reference occurrence.
///
/// Prefer `enclosing_range` when present; fall back to occurrence range
/// containment. When both are present and both map to **different** ids:
/// prefer the **innermost** if one native span strictly contains the other
/// (0166 nest-prefer recovery); otherwise `EnclosingDisagreement` (no edge;
/// never invent a caller for disjoint / equal-span / non-nested geometry).
///
/// Hot path does **not** emit `warn!` (0157 log budget); callers count and
/// summarize.
pub fn resolve_caller_for_reference(
    occurrence_range: &[i32],
    enclosing_range: &[i32],
    native_in_file: &[NativeSymbolSpan],
) -> ResolveCallerResult {
    let occ_parsed = match parse_scip_range(occurrence_range) {
        Ok(r) => r,
        Err(_) => {
            return ResolveCallerResult {
                outcome: ResolveCallerOutcome::InvalidOccurrenceRange,
                used_invalid_enclosing_fallback: false,
                recovered_nest_prefer: false,
            };
        }
    };

    let from_occ = resolve_innermost(occ_parsed.start_line, native_in_file);

    if enclosing_range.is_empty() {
        return ResolveCallerResult {
            outcome: option_to_outcome(from_occ),
            used_invalid_enclosing_fallback: false,
            recovered_nest_prefer: false,
        };
    }

    let enc_parsed = match parse_scip_range(enclosing_range) {
        Ok(r) => r,
        Err(_) => {
            // Invalid enclosing → fall back to occurrence range (policy unchanged).
            return ResolveCallerResult {
                outcome: option_to_outcome(from_occ),
                used_invalid_enclosing_fallback: true,
                recovered_nest_prefer: false,
            };
        }
    };

    // Prefer a representative line of the enclosing range (start) for caller id.
    let from_enc = resolve_innermost(enc_parsed.start_line, native_in_file);

    let (outcome, recovered_nest_prefer) = match (from_enc, from_occ) {
        (Some(a), Some(b)) if a != b => match prefer_nested(a, b, native_in_file) {
            Some(id) => (ResolveCallerOutcome::Resolved(id), true),
            None => (
                ResolveCallerOutcome::EnclosingDisagreement {
                    enclosing_id: a,
                    occurrence_id: b,
                },
                false,
            ),
        },
        (Some(a), _) => (ResolveCallerOutcome::Resolved(a), false),
        (None, other) => (option_to_outcome(other), false),
    };

    ResolveCallerResult {
        outcome,
        used_invalid_enclosing_fallback: false,
        recovered_nest_prefer,
    }
}

#[inline]
fn option_to_outcome(id: Option<i64>) -> ResolveCallerOutcome {
    match id {
        Some(id) => ResolveCallerOutcome::Resolved(id),
        None => ResolveCallerOutcome::Unmapped,
    }
}

/// Is this occurrence a Definition? (`symbol_roles & 0x1 != 0`)
#[inline]
pub fn is_definition_role(symbol_roles: i32) -> bool {
    symbol_roles & SCIP_ROLE_DEFINITION != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(id: i64, start: i32, end: i32) -> NativeSymbolSpan {
        NativeSymbolSpan {
            id,
            file_id: 1,
            line_start: start,
            line_end: end,
        }
    }

    #[test]
    fn innermost_picks_nested_method_over_impl() {
        // module 1-100, impl 10-80, method 20-30 — line 25 → method
        let cands = vec![span(1, 1, 100), span(2, 10, 80), span(3, 20, 30)];
        assert_eq!(resolve_innermost(25, &cands), Some(3));
    }

    #[test]
    fn innermost_no_container_is_none() {
        let cands = vec![span(1, 1, 10)];
        assert_eq!(resolve_innermost(50, &cands), None);
    }

    #[test]
    fn innermost_equal_span_tie_refuses() {
        // Two different symbols, same span, both contain line 5
        let cands = vec![span(10, 1, 10), span(20, 1, 10)];
        assert_eq!(resolve_innermost(5, &cands), None);
    }

    #[test]
    fn definition_mapping_first_wins() {
        let mut r = ScipNativeResolver::new();
        let cands = vec![span(42, 1, 5)];
        let range = ScipRange {
            start_line: 2,
            end_line: 2,
            start_char: 0,
            end_char: 1,
        };
        r.try_map_definition("scip:foo", &range, &cands);
        r.try_map_definition(
            "scip:foo",
            &ScipRange {
                start_line: 3,
                end_line: 3,
                start_char: 0,
                end_char: 1,
            },
            &cands,
        );
        assert_eq!(r.get("scip:foo"), Some(42));
        assert_eq!(r.definitions_mapped, 1);
        assert_eq!(r.definitions_seen, 2);
    }

    #[test]
    fn is_definition_role_bit() {
        assert!(is_definition_role(1));
        assert!(is_definition_role(1 | 8)); // Definition | ReadAccess
        assert!(!is_definition_role(0));
        assert!(!is_definition_role(2)); // Import only
        assert!(!is_definition_role(8)); // ReadAccess only
    }

    #[test]
    fn strictly_contains_nested_and_equal() {
        let outer = span(1, 1, 100);
        let inner = span(3, 20, 30);
        assert!(strictly_contains(&outer, &inner));
        assert!(!strictly_contains(&inner, &outer));
        let a = span(1, 1, 50);
        let b = span(2, 1, 50);
        assert!(!strictly_contains(&a, &b));
        assert!(!strictly_contains(&b, &a));
    }

    #[test]
    fn prefer_nested_picks_innermost() {
        let cands = vec![span(1, 1, 100), span(3, 20, 30)];
        assert_eq!(prefer_nested(1, 3, &cands), Some(3));
        assert_eq!(prefer_nested(3, 1, &cands), Some(3));
    }

    #[test]
    fn prefer_nested_disjoint_is_none() {
        let cands = vec![span(1, 1, 20), span(2, 30, 50)];
        assert_eq!(prefer_nested(1, 2, &cands), None);
    }

    #[test]
    fn prefer_nested_equal_span_is_none() {
        let cands = vec![span(1, 1, 50), span(2, 1, 50)];
        assert_eq!(prefer_nested(1, 2, &cands), None);
    }

    #[test]
    fn prefer_nested_missing_span_is_none() {
        // Only id 1 present — id 3 missing → refuse (no invent).
        let cands = vec![span(1, 1, 100)];
        assert_eq!(prefer_nested(1, 3, &cands), None);
    }

    #[test]
    fn caller_enclosing_agrees_with_occurrence() {
        let cands = vec![span(7, 1, 50)];
        // occurrence line 0-based 9 → native 10; enclosing same
        let result = resolve_caller_for_reference(&[9, 0, 5], &[0, 0, 49, 0], &cands);
        assert_eq!(result.outcome, ResolveCallerOutcome::Resolved(7));
        assert!(!result.used_invalid_enclosing_fallback);
        assert!(!result.recovered_nest_prefer);
    }

    #[test]
    fn caller_nest_prefer_recovers_innermost() {
        // outer 1–100 id=1, inner 20–30 id=3; enc start → outer, occ → inner
        let cands = vec![span(1, 1, 100), span(3, 20, 30)];
        // occ 0-based 24 → native 25 → id 3; enc start 0-based 0 → native 1 → id 1
        let result = resolve_caller_for_reference(&[24, 0, 5], &[0, 0, 99, 0], &cands);
        assert_eq!(result.outcome, ResolveCallerOutcome::Resolved(3));
        assert!(result.recovered_nest_prefer);
        assert!(!result.used_invalid_enclosing_fallback);
    }

    #[test]
    fn caller_disagreement_skips() {
        let cands = vec![span(1, 1, 20), span(2, 30, 50)];
        // occurrence at line 5 (0-based) → native 6 → id 1
        // enclosing starts at line 35 (0-based 34) → native 35 → id 2
        let result = resolve_caller_for_reference(&[5, 0, 3], &[34, 0, 40, 0], &cands);
        assert_eq!(
            result.outcome,
            ResolveCallerOutcome::EnclosingDisagreement {
                enclosing_id: 2,
                occurrence_id: 1,
            }
        );
        assert!(!result.used_invalid_enclosing_fallback);
        assert!(!result.recovered_nest_prefer);
    }

    #[test]
    fn caller_empty_enclosing_uses_occurrence() {
        let cands = vec![span(7, 1, 50)];
        let result = resolve_caller_for_reference(&[9, 0, 5], &[], &cands);
        assert_eq!(result.outcome, ResolveCallerOutcome::Resolved(7));
        assert!(!result.used_invalid_enclosing_fallback);
        assert!(!result.recovered_nest_prefer);
    }

    #[test]
    fn caller_unmapped_when_no_container() {
        let cands = vec![span(1, 1, 5)];
        // occurrence at native line 50 — outside any span
        let result = resolve_caller_for_reference(&[49, 0, 5], &[], &cands);
        assert_eq!(result.outcome, ResolveCallerOutcome::Unmapped);
        assert!(!result.used_invalid_enclosing_fallback);
        assert!(!result.recovered_nest_prefer);
    }

    #[test]
    fn caller_invalid_occurrence_range() {
        let cands = vec![span(1, 1, 50)];
        let result = resolve_caller_for_reference(&[], &[0, 0, 49, 0], &cands);
        assert_eq!(result.outcome, ResolveCallerOutcome::InvalidOccurrenceRange);
        assert!(!result.used_invalid_enclosing_fallback);
        assert!(!result.recovered_nest_prefer);
    }

    #[test]
    fn caller_invalid_enclosing_fallback_to_occ() {
        let cands = vec![span(7, 1, 50)];
        // invalid enclosing length; valid occurrence
        let result = resolve_caller_for_reference(&[9, 0, 5], &[1, 2], &cands);
        assert_eq!(result.outcome, ResolveCallerOutcome::Resolved(7));
        assert!(result.used_invalid_enclosing_fallback);
        assert!(!result.recovered_nest_prefer);
    }

    #[test]
    fn caller_invalid_enclosing_fallback_unmapped_when_occ_unmapped() {
        let cands = vec![span(1, 1, 5)];
        let result = resolve_caller_for_reference(&[49, 0, 5], &[1, 2], &cands);
        assert_eq!(result.outcome, ResolveCallerOutcome::Unmapped);
        assert!(result.used_invalid_enclosing_fallback);
        assert!(!result.recovered_nest_prefer);
    }
}
