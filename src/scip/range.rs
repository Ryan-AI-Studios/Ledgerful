//! SCIP occurrence range unpacking (0-based protocol → 1-based native lines).
//!
//! Lifted from the former `ScipSymbolMapper` so the 3-vs-4-element shape is
//! preserved after SCIP stops writing `project_symbols` rows (0095).

use miette::{Result, miette};

/// Parsed SCIP range, already converted to **1-based** native line numbers.
///
/// SCIP proto: line numbers are 0-based. Native extractors store
/// `node.start_position().row + 1` (1-based). Conversion happens once here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScipRange {
    /// Inclusive start line (1-based, matches `project_symbols.line_start`).
    pub start_line: i32,
    /// Inclusive end line (1-based, matches `project_symbols.line_end`).
    pub end_line: i32,
    pub start_char: i32,
    pub end_char: i32,
}

/// Unpack a SCIP `Occurrence.range` / `enclosing_range` vector.
///
/// Per scip.proto the range is exactly three or four elements:
/// - 4 elems: `[startLine, startCharacter, endLine, endCharacter]`
/// - 3 elems: `[startLine, startCharacter, endCharacter]` (single-line;
///   `end_line == start_line`)
///
/// Any other length is an **error** — never silent `None` (typed_range from
/// scip 0.9.0 is out of scope; empty ranges must fail loudly).
pub fn parse_scip_range(range: &[i32]) -> Result<ScipRange> {
    let (start_line_0, start_char, end_line_0, end_char) = match range.len() {
        4 => (range[0], range[1], range[2], range[3]),
        3 => (range[0], range[1], range[0], range[2]),
        other => {
            return Err(miette!(
                "SCIP range must have 3 or 4 elements (got {other}); \
                 empty ranges are not treated as 'no position'"
            ));
        }
    };

    // SCIP is 0-based; native project_symbols lines are 1-based.
    Ok(ScipRange {
        start_line: start_line_0 + 1,
        end_line: end_line_0 + 1,
        start_char,
        end_char,
    })
}

/// True when a 1-based native line is inside an inclusive `[start, end]` span.
#[inline]
pub fn line_in_span(line: i32, start: i32, end: i32) -> bool {
    line >= start && line <= end
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_four_element_range_converts_to_one_based() {
        // SCIP 0-based: lines 10-12 → native 11-13
        let r = parse_scip_range(&[10, 0, 12, 5]).unwrap();
        assert_eq!(
            r,
            ScipRange {
                start_line: 11,
                end_line: 13,
                start_char: 0,
                end_char: 5,
            }
        );
    }

    #[test]
    fn parse_three_element_range_single_line() {
        // SCIP single-line: [startLine, startChar, endChar]
        let r = parse_scip_range(&[4, 2, 18]).unwrap();
        assert_eq!(
            r,
            ScipRange {
                start_line: 5,
                end_line: 5,
                start_char: 2,
                end_char: 18,
            }
        );
    }

    #[test]
    fn parse_invalid_lengths_are_errors() {
        assert!(parse_scip_range(&[]).is_err());
        assert!(parse_scip_range(&[1]).is_err());
        assert!(parse_scip_range(&[1, 2]).is_err());
        assert!(parse_scip_range(&[1, 2, 3, 4, 5]).is_err());
    }

    #[test]
    fn line_in_span_inclusive() {
        assert!(line_in_span(5, 5, 5));
        assert!(line_in_span(3, 1, 5));
        assert!(!line_in_span(0, 1, 5));
        assert!(!line_in_span(6, 1, 5));
    }
}
