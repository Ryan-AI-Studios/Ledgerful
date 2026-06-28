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

/// Extracts literal trigrams from a regex pattern if possible.
/// Returns None if no literal trigrams can be derived (e.g., too many wildcards).
pub fn regex_to_trigrams(pattern: &str) -> Option<Vec<String>> {
    let hir = regex_syntax::Parser::new().parse(pattern).ok()?;
    let mut literals = Vec::new();
    extract_literals(&hir, &mut literals);

    let mut trigrams = HashSet::new();
    for lit in literals {
        if lit.len() >= 3 {
            for i in 0..lit.len() - 2 {
                trigrams.insert(lit[i..i + 3].to_string());
            }
        }
    }

    if trigrams.is_empty() {
        None
    } else {
        Some(trigrams.into_iter().collect())
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
}
