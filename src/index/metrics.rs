use crate::index::languages::Language;
use camino::Utf8Path;
use miette::Result;
use serde::{Deserialize, Serialize};
use tree_sitter::Node;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct FileComplexity {
    pub total_sloc: usize,
    pub functions: Vec<SymbolComplexity>,
    pub ast_incomplete: bool,
    pub complexity_capped: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct SymbolComplexity {
    pub name: String,
    pub cognitive: usize,
    pub cyclomatic: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ComplexityResult {
    Scored(FileComplexity),
    NotApplicable { reason: String },
}

pub trait ComplexityScorer {
    fn score_file(
        &self,
        path: &Utf8Path,
        source: &str,
        language: Language,
    ) -> Result<FileComplexity>;
}

pub struct NativeComplexityScorer;

impl Default for NativeComplexityScorer {
    fn default() -> Self {
        Self::new()
    }
}

impl NativeComplexityScorer {
    pub fn new() -> Self {
        Self
    }

    pub fn score_supported_path(&self, path: &Utf8Path, source: &str) -> Result<ComplexityResult> {
        let Some(extension) = path.extension() else {
            return Ok(ComplexityResult::NotApplicable {
                reason: "file has no extension".to_string(),
            });
        };

        let Some(language) = Language::from_extension(extension) else {
            return Ok(ComplexityResult::NotApplicable {
                reason: format!("unsupported extension .{extension}"),
            });
        };

        if matches!(language, Language::Markdown) {
            return Ok(ComplexityResult::NotApplicable {
                reason: "complexity analysis not applicable to markdown".to_string(),
            });
        }

        self.score_file(path, source, language)
            .map(ComplexityResult::Scored)
    }

    fn calculate_cyclomatic(&self, node: Node, language: Language) -> usize {
        let mut complexity = 1; // Base complexity
        let mut cursor = node.walk();
        let mut stack = vec![node];

        while let Some(current) = stack.pop() {
            let kind = current.kind();

            let is_branch = match language {
                Language::Rust => matches!(
                    kind,
                    "if_expression"
                        | "for_expression"
                        | "while_expression"
                        | "loop_expression"
                        | "match_arm"
                        | "&&"
                        | "||"
                ),
                Language::TypeScript => matches!(
                    kind,
                    "if_statement"
                        | "for_statement"
                        | "for_in_statement"
                        | "for_of_statement"
                        | "while_statement"
                        | "do_statement"
                        | "switch_case"
                        | "switch_default"
                        | "&&"
                        | "||"
                        | "??"
                        | "ternary_expression"
                ),
                Language::Python => matches!(
                    kind,
                    "if_statement"
                        | "elif_clause"
                        | "for_statement"
                        | "while_statement"
                        | "case_clause"
                        | "except_clause"
                        | "except_group_clause"
                        | "conditional_expression"
                        | "and"
                        | "or"
                ),
                Language::Markdown => false,
            };

            if is_branch {
                complexity += 1;
            }

            for child in current.children(&mut cursor) {
                stack.push(child);
            }
        }

        complexity
    }

    fn calculate_cognitive(&self, node: Node, language: Language) -> usize {
        self.calculate_cognitive_recursive(node, 0, language).0
    }

    fn calculate_cognitive_recursive(
        &self,
        node: Node,
        nesting: usize,
        language: Language,
    ) -> (usize, usize) {
        let mut score = 0;
        let kind = node.kind();
        let mut current_nesting = nesting;

        let is_nesting_increment = match language {
            Language::Rust => matches!(
                kind,
                "if_expression"
                    | "for_expression"
                    | "while_expression"
                    | "loop_expression"
                    | "match_expression"
            ),
            Language::TypeScript => matches!(
                kind,
                "if_statement"
                    | "for_statement"
                    | "for_in_statement"
                    | "for_of_statement"
                    | "while_statement"
                    | "do_statement"
                    | "switch_statement"
                    | "catch_clause"
            ),
            Language::Python => matches!(
                kind,
                "if_statement" | "for_statement" | "while_statement" | "try_statement"
            ),
            Language::Markdown => false,
        };

        if is_nesting_increment {
            score += 1 + nesting;
            current_nesting += 1;
        } else {
            let is_other_increment = match language {
                Language::Rust => matches!(kind, "match_arm" | "&&" | "||"),
                Language::TypeScript => matches!(
                    kind,
                    "switch_case" | "&&" | "||" | "??" | "ternary_expression"
                ),
                Language::Python => matches!(
                    kind,
                    "elif_clause"
                        | "except_clause"
                        | "except_group_clause"
                        | "and"
                        | "or"
                        | "conditional_expression"
                ),
                Language::Markdown => false,
            };
            if is_other_increment {
                score += 1;
            }
        }

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            let (child_score, _) =
                self.calculate_cognitive_recursive(child, current_nesting, language);
            score += child_score;
        }

        (score, current_nesting)
    }
}

impl ComplexityScorer for NativeComplexityScorer {
    fn score_file(
        &self,
        _path: &Utf8Path,
        source: &str,
        language: Language,
    ) -> Result<FileComplexity> {
        let total_sloc = source.lines().count();

        // Fast-path for non-code languages
        if matches!(language, Language::Markdown) {
            return Ok(FileComplexity {
                total_sloc,
                functions: Vec::new(),
                ast_incomplete: false,
                complexity_capped: false,
            });
        }

        let complexity_capped = total_sloc > 10_000;

        if complexity_capped {
            return Ok(FileComplexity {
                total_sloc,
                functions: Vec::new(),
                ast_incomplete: false,
                complexity_capped: true,
            });
        }

        let mut parser = tree_sitter::Parser::new();
        let ts_language = match language {
            Language::Rust => tree_sitter_rust::LANGUAGE.into(),
            Language::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Language::Python => tree_sitter_python::LANGUAGE.into(),
            Language::Markdown => unreachable!(), // Handled above
        };
        parser
            .set_language(&ts_language)
            .map_err(|e| miette::miette!("TS language error: {e}"))?;

        let tree = parser
            .parse(source, None)
            .ok_or_else(|| miette::miette!("Failed to parse source"))?;
        let root = tree.root_node();
        let ast_incomplete = root.has_error();

        let mut functions = Vec::new();
        let mut cursor = root.walk();
        let mut stack = vec![root];

        while let Some(node) = stack.pop() {
            let kind = node.kind();
            if matches!(
                kind,
                "function_item"
                    | "function_definition"
                    | "method_declaration"
                    | "method_definition"
                    | "arrow_function"
                    | "function_declaration"
                    | "generator_function_declaration"
            ) {
                let name = node
                    .child_by_field_name("name")
                    .map(|n| {
                        n.utf8_text(source.as_bytes())
                            .unwrap_or("anonymous")
                            .to_string()
                    })
                    .unwrap_or_else(|| "anonymous".to_string());

                functions.push(SymbolComplexity {
                    name,
                    cognitive: self.calculate_cognitive(node, language),
                    cyclomatic: self.calculate_cyclomatic(node, language),
                });
            }

            for child in node.children(&mut cursor) {
                stack.push(child);
            }
        }

        Ok(FileComplexity {
            total_sloc,
            functions,
            ast_incomplete,
            complexity_capped,
        })
    }
}
