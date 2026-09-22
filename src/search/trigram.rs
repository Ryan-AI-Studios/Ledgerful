use regex_syntax::hir::{Hir, HirKind};
use std::collections::HashSet;

/// Extracts all unique trigrams from a string.
pub fn extract_trigrams(text: &str) -> HashSet<String> {
    let mut trigrams = HashSet::new();
    let chars: Vec<char> = text.chars().collect();
    if chars.len() < 3 {
        return trigrams;
    }
    for i in 0..chars.len() - 2 {
        let trigram: String = chars[i..i + 3].iter().collect();
        trigrams.insert(trigram);
    }
    trigrams
}

/// A trigram the `code_trigram` analyzer can store as one token.
///
/// Tantivy `WhitespaceTokenizer` splits on [`char::is_ascii_whitespace`] only.
fn trigram_is_queryable(trigram: &str) -> bool {
    !trigram.chars().any(|c| c.is_ascii_whitespace())
}

/// Extracts literal trigrams from a regex pattern if possible.
/// Returns None if no queryable literal trigrams can be derived.
pub fn regex_to_trigrams(pattern: &str) -> Option<Vec<String>> {
    let hir = regex_syntax::Parser::new().parse(pattern).ok()?;
    let mut literals = Vec::new();
    extract_literals(&hir, &mut literals);

    let mut trigrams = HashSet::new();
    for lit in literals {
        let chars: Vec<char> = lit.chars().collect();
        if chars.len() >= 3 {
            for i in 0..chars.len() - 2 {
                let trigram: String = chars[i..i + 3].iter().collect();
                if trigram_is_queryable(&trigram) {
                    trigrams.insert(trigram);
                }
            }
        }
    }

    if trigrams.is_empty() {
        None
    } else {
        let mut out: Vec<String> = trigrams.into_iter().collect();
        out.sort();
        Some(out)
    }
}

fn extract_literals(hir: &Hir, literals: &mut Vec<String>) {
    match hir.kind() {
        HirKind::Literal(lit) => {
            if let Ok(s) = std::str::from_utf8(&lit.0) {
                literals.push(s.to_string());
            }
        }
        HirKind::Concat(subs) => {
            let mut current_lit = String::new();
            for sub in subs {
                if let HirKind::Literal(lit) = sub.kind() {
                    if let Ok(s) = std::str::from_utf8(&lit.0) {
                        current_lit.push_str(s);
                    }
                } else {
                    if !current_lit.is_empty() {
                        literals.push(current_lit.clone());
                        current_lit.clear();
                    }
                    extract_literals(sub, literals);
                }
            }
            if !current_lit.is_empty() {
                literals.push(current_lit);
            }
        }
        HirKind::Alternation(subs) => {
            // For alternation, we can only take trigrams that appear in ALL branches
            // to be sound for filtering. But simpler is just to ignore them or take literals from them.
            // For pre-filtering, we can't easily use trigrams from alternation unless we do more complex logic.
            for sub in subs {
                extract_literals(sub, literals);
            }
        }
        HirKind::Capture(capture) => {
            extract_literals(&capture.sub, literals);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_trigrams() {
        let text = "hello";
        let trigrams = extract_trigrams(text);
        assert!(trigrams.contains("hel"));
        assert!(trigrams.contains("ell"));
        assert!(trigrams.contains("llo"));
        assert_eq!(trigrams.len(), 3);
    }

    #[test]
    fn test_regex_to_trigrams() {
        let pattern = r"function\s+foo";
        let trigrams = regex_to_trigrams(pattern).unwrap();
        assert!(trigrams.iter().any(|s| s == "fun"));
        assert!(trigrams.iter().any(|s| s == "unc"));
        assert!(trigrams.iter().any(|s| s == "nct"));

        let pattern_no_lit = r".*";
        assert!(regex_to_trigrams(pattern_no_lit).is_none());
    }

    #[test]
    fn regex_to_trigrams_drops_ascii_space_keeps_literal_pieces() {
        let trigrams = regex_to_trigrams("trait ImpactProvider").expect("some trigrams");
        assert!(
            trigrams
                .iter()
                .all(|s| !s.chars().any(|c| c.is_ascii_whitespace())),
            "space-bearing windows must be dropped: {trigrams:?}"
        );
        assert!(trigrams.iter().any(|s| s == "tra"));
        assert!(!trigrams.iter().any(|s| s == "it "));
    }

    #[test]
    fn regex_to_trigrams_none_when_every_window_has_ascii_space() {
        assert!(regex_to_trigrams("a = b").is_none());
        assert!(regex_to_trigrams(".*").is_none());
    }

    #[test]
    fn regex_to_trigrams_keeps_nbsp_window() {
        let trigrams = regex_to_trigrams("a\u{00A0}bc").expect("nbsp trigrams stay");
        assert!(trigrams.iter().any(|s| s == "a\u{00A0}b"));
    }

    #[test]
    fn regex_to_trigrams_multibyte_literal_does_not_panic() {
        let trigrams = regex_to_trigrams("éab").expect("char window");
        assert!(trigrams.iter().any(|s| s == "éab"));
    }
}
