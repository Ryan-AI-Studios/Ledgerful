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

// NOTE (0084 DoD-8): `get_rust_tokenizer`, `get_typescript_tokenizer`, and `get_go_tokenizer`
// are all currently unreached outside this module — there is no live dispatch that selects a
// per-language CodeTokenizer. Do not invent a new dispatch mechanism here; wire-up is a
// separate finding if/when a call site is introduced for any language.
pub fn get_rust_tokenizer() -> CodeTokenizer {
    CodeTokenizer::new(tree_sitter_rust::LANGUAGE.into())
}

pub fn get_typescript_tokenizer() -> CodeTokenizer {
    CodeTokenizer::new(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())
}

pub fn get_go_tokenizer() -> CodeTokenizer {
    CodeTokenizer::new(tree_sitter_go::LANGUAGE.into())
}
