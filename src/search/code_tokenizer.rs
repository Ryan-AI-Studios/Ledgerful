use std::collections::HashSet;
use tree_sitter::{Language, Parser};

pub struct CodeTokenizer {
    language: Language,
}

impl CodeTokenizer {
    pub fn new(language: Language) -> Self {
        Self { language }
    }

    pub fn tokenize(&self, code: &str) -> Vec<String> {
        let mut parser = Parser::new();
        if let Err(err) = parser.set_language(&self.language) {
            tracing::warn!("code_tokenizer: failed to load grammar: {err}");
            return Vec::new();
        }

        let Some(tree) = parser.parse(code, None) else {
            tracing::warn!("code_tokenizer: failed to parse code");
            return Vec::new();
        };
        let mut tokens = HashSet::new();

        self.traverse_nodes(tree.root_node(), code, &mut tokens);

        let mut result: Vec<String> = tokens.into_iter().collect();
        result.sort();
        result
    }

    #[allow(clippy::collapsible_if)]
    fn traverse_nodes(&self, node: tree_sitter::Node, source: &str, tokens: &mut HashSet<String>) {
        // We only care about identifiers and related tokens
        let kind = node.kind();
        if kind == "identifier"
            || kind == "type_identifier"
            || kind == "field_identifier"
            || kind == "function_item"
        {
            if let Ok(text) = node.utf8_text(source.as_bytes()) {
                if text.len() > 1 {
                    tokens.insert(text.to_string());
                }
            }
        }

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            self.traverse_nodes(child, source, tokens);
        }
    }
}

// NOTE (0084 DoD-8 / plan Phase 8): There is no live production dispatch that selects a
// per-language `CodeTokenizer` for Rust, TypeScript, *or* Go — `get_*_tokenizer` helpers are
// retained as thin constructors for potential future callers/tests but are unreached today.
// Wiring a new dispatch mechanism is out of scope for language-parity tracks; removal of the
// dead helpers is logged to deferred.md as cross-language hygiene rather than inventing a
// call site solely to satisfy wire-up for Go alone.
pub fn get_rust_tokenizer() -> CodeTokenizer {
    CodeTokenizer::new(tree_sitter_rust::LANGUAGE.into())
}

pub fn get_typescript_tokenizer() -> CodeTokenizer {
    CodeTokenizer::new(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())
}

pub fn get_go_tokenizer() -> CodeTokenizer {
    CodeTokenizer::new(tree_sitter_go::LANGUAGE.into())
}

#[cfg(test)]
mod tokenize_tests {
    use super::*;

    #[test]
    fn tokenize_invalid_syntax_returns_without_panic() {
        let tokenizer = get_rust_tokenizer();
        let tokens = tokenizer.tokenize("fn {{{ not valid rust");
        // tree-sitter may still recover partial identifiers; the contract is no panic.
        let _ = tokens;
    }

    #[test]
    fn tokenize_valid_rust_extracts_identifiers() {
        let tokenizer = get_rust_tokenizer();
        let tokens = tokenizer.tokenize("fn main() { let value = 1; }");
        assert!(tokens.contains(&"main".to_string()));
        assert!(tokens.contains(&"value".to_string()));
    }
}
