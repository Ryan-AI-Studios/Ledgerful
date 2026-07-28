//! SCIP symbol string → native `project_symbols.id` resolver (0095).
//!
//! Definition occurrences (`symbol_roles & 0x1`) map via document path + range
//! containment to the **innermost** containing native symbol. Ambiguous or
//! missing matches yield no mapping — never a guess.

use crate::scip::range::{ScipRange, line_in_span, parse_scip_range};
use miette::{IntoDiagnostic, Result};
use rusqlite::Connection;
use std::collections::HashMap;
use tracing::warn;

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

/// Identify the caller native symbol for a reference occurrence.
///
/// Prefer `enclosing_range` when present; fall back to occurrence range
/// containment. When both are present they must agree — warn + skip on
/// disagreement.
pub fn resolve_caller_for_reference(
    occurrence_range: &[i32],
    enclosing_range: &[i32],
    native_in_file: &[NativeSymbolSpan],
) -> Option<i64> {
    let occ_parsed = match parse_scip_range(occurrence_range) {
        Ok(r) => r,
        Err(e) => {
            warn!("Skipping SCIP occurrence with invalid range: {e}");
            return None;
        }
    };

    let from_occ = resolve_innermost(occ_parsed.start_line, native_in_file);

    if enclosing_range.is_empty() {
        return from_occ;
    }

    let enc_parsed = match parse_scip_range(enclosing_range) {
        Ok(r) => r,
        Err(e) => {
            warn!("SCIP enclosing_range invalid ({e}); falling back to occurrence range");
            return from_occ;
        }
    };

    // Prefer a representative line of the enclosing range (start) for caller id.
    let from_enc = resolve_innermost(enc_parsed.start_line, native_in_file);

    match (from_enc, from_occ) {
        (Some(a), Some(b)) if a != b => {
            warn!(
                "SCIP enclosing_range caller {a} disagrees with occurrence-range caller {b}; skipping"
            );
            None
        }
        (Some(a), _) => Some(a),
        (None, other) => other,
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
    fn caller_enclosing_agrees_with_occurrence() {
        let cands = vec![span(7, 1, 50)];
        // occurrence line 0-based 9 → native 10; enclosing same
        let id = resolve_caller_for_reference(&[9, 0, 5], &[0, 0, 49, 0], &cands);
        assert_eq!(id, Some(7));
    }

    #[test]
    fn caller_disagreement_skips() {
        let cands = vec![span(1, 1, 20), span(2, 30, 50)];
        // occurrence at line 5 (0-based) → native 6 → id 1
        // enclosing starts at line 35 (0-based 34) → native 35 → id 2
        let id = resolve_caller_for_reference(&[5, 0, 3], &[34, 0, 40, 0], &cands);
        assert_eq!(id, None);
    }
}
