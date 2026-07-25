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
        parser
            .set_language(&self.language)
            .expect("Error loading grammar");

        let tree = parser.parse(code, None).expect("Error parsing code");
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
