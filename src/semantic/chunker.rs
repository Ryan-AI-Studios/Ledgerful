use crate::index::symbols::SymbolKind;
use miette::{IntoDiagnostic, Result, miette};
use std::path::Path;
use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator};

fn is_standalone_chunk_kind(kind: &SymbolKind) -> bool {
    match kind {
        SymbolKind::Function
        | SymbolKind::Struct
        | SymbolKind::Enum
        | SymbolKind::Trait
        | SymbolKind::Module
        | SymbolKind::Type => true,
        SymbolKind::Method
        | SymbolKind::Class
        | SymbolKind::Interface
        | SymbolKind::Variable
        | SymbolKind::Constant => false,
    }
}

/// Grain id mixed into incremental `semantic_file_hash` so embed-text
/// formula changes refresh without a dim wipe or mandatory `--full`.
pub const EMBED_TEXT_GRAIN: &str = "fn-name-v1";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AstChunk {
    pub file_path: String,
    pub name: String,
    #[serde(default)]
    pub qualified_name: Option<String>,
    pub kind: SymbolKind,
    pub content: String,
    pub docstring: Option<String>,
    pub range: (usize, usize), // (byte_start, byte_end)
    pub lines: (usize, usize), // (line_start, line_end)
    pub offset: usize,         // byte index into the un-headered body
}

pub(crate) fn semantic_file_content_hash(content: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(EMBED_TEXT_GRAIN.as_bytes());
    hasher.update(b"\n");
    hasher.update(content.as_bytes());
    hasher.finalize().to_hex().to_string()
}

fn kind_token(kind: &SymbolKind) -> &'static str {
    match kind {
        SymbolKind::Function => "fn",
        SymbolKind::Method => "method",
        SymbolKind::Struct => "struct",
        SymbolKind::Enum => "enum",
        SymbolKind::Trait => "trait",
        SymbolKind::Type => "type",
        SymbolKind::Module => "mod",
        SymbolKind::Class => "class",
        SymbolKind::Interface => "interface",
        SymbolKind::Variable => "var",
        SymbolKind::Constant => "const",
    }
}

fn display_name(qualified_name: &Option<String>, name: &str) -> String {
    qualified_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| name.trim())
        .to_string()
}

fn grain_header(kind: &SymbolKind, display: &str) -> String {
    let token = kind_token(kind);
    if display.is_empty() {
        token.to_string()
    } else {
        format!("{token} {display}")
    }
}

impl AstChunk {
    fn embedding_body(&self) -> String {
        let mut text = String::new();
        if let Some(doc) = &self.docstring {
            text.push_str(doc);
            text.push_str("\n\n");
        }
        text.push_str(&self.content);
        text
    }

    pub fn to_embedding_text(&self) -> String {
        let header = grain_header(&self.kind, &display_name(&self.qualified_name, &self.name));
        let body = self.embedding_body();
        if body.is_empty() {
            format!("{header}\n\n")
        } else {
            format!("{header}\n\n{body}")
        }
    }

    pub fn split(&self, max_chars: usize, overlap: usize) -> Vec<AstChunk> {
        let body = self.embedding_body();
        let chars: Vec<(usize, char)> = body.char_indices().collect();
        if chars.len() <= max_chars {
            return vec![self.clone()];
        }

        let mut chunks = Vec::new();
        let mut start_idx = 0;
        while start_idx < chars.len() {
            let end_idx = std::cmp::min(start_idx + max_chars, chars.len());

            let byte_start = chars[start_idx].0;
            let byte_end = if end_idx < chars.len() {
                chars[end_idx].0
            } else {
                body.len()
            };

            let chunk_text = body[byte_start..byte_end].to_string();

            chunks.push(AstChunk {
                file_path: self.file_path.clone(),
                name: self.name.clone(),
                qualified_name: self.qualified_name.clone(),
                kind: self.kind.clone(),
                content: chunk_text,
                docstring: None,
                range: self.range,
                lines: self.lines,
                offset: byte_start,
            });

            if end_idx == chars.len() {
                break;
            }

            let step = if max_chars > overlap {
                max_chars - overlap
            } else {
                1
            };
            start_idx += step;
        }
        chunks
    }
}

pub struct AstChunker;

impl AstChunker {
    pub fn chunk_file(path: &Path, content: &str) -> Result<Vec<AstChunk>> {
        let extension = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let raw_chunks = match extension {
            "rs" => Self::chunk_rust(path, content)?,
            "ts" | "tsx" | "js" | "jsx" => Self::chunk_typescript(path, content)?,
            "py" => Self::chunk_python(path, content)?,
            _ => vec![],
        };

        let mut final_chunks = Vec::new();
        for chunk in raw_chunks {
            // max_chars roughly 2000 corresponds to ~512 tokens for nomic/bge
            final_chunks.extend(chunk.split(2000, 200));
        }
        Ok(final_chunks)
    }

    fn chunk_rust(path: &Path, content: &str) -> Result<Vec<AstChunk>> {
        let file_path = path.to_string_lossy().to_string();

        // Delegate symbol discovery to symbols.rs
        let extracted_symbols =
            match crate::index::languages::rust::symbols::extract_symbols(content)? {
                Some(symbols) if !symbols.is_empty() => symbols,
                _ => return Ok(Vec::new()),
            };

        // Parse once to get the tree for docstring and content extraction
        let mut parser = Parser::new();
        let language = tree_sitter_rust::LANGUAGE;
        parser.set_language(&language.into()).into_diagnostic()?;

        let tree = parser
            .parse(content, None)
            .ok_or_else(|| miette!("Failed to parse Rust content"))?;

        let mut chunks = Vec::new();

        for symbol in extracted_symbols {
            // Skip symbols that are not meaningful standalone chunks
            if !is_standalone_chunk_kind(&symbol.kind) {
                continue;
            }

            let Some(byte_start) = symbol.byte_start else {
                continue;
            };
            let Some(byte_end) = symbol.byte_end else {
                continue;
            };

            let start = byte_start as usize;
            let end = byte_end as usize;

            let node = tree
                .root_node()
                .descendant_for_byte_range(start, end)
                .filter(|n| n.start_byte() == start && n.end_byte() == end);

            let Some(node) = node else {
                continue;
            };

            if symbol.kind == SymbolKind::Module && node.child_by_field_name("body").is_none() {
                continue;
            }

            let chunk_content = node
                .utf8_text(content.as_bytes())
                .into_diagnostic()?
                .to_string();

            // Extract docstring from preceding siblings
            let mut docstring = Vec::new();
            let mut prev = node.prev_sibling();
            while let Some(p) = prev {
                if p.kind() == "line_comment" || p.kind() == "block_comment" {
                    docstring.push(
                        p.utf8_text(content.as_bytes())
                            .into_diagnostic()?
                            .trim()
                            .to_string(),
                    );
                    prev = p.prev_sibling();
                } else if p.kind() == "attribute_item" {
                    // Skip attributes but keep looking for comments
                    prev = p.prev_sibling();
                } else {
                    break;
                }
            }
            docstring.reverse();
            let docstring = if docstring.is_empty() {
                None
            } else {
                Some(docstring.join("\n"))
            };

            chunks.push(AstChunk {
                file_path: file_path.clone(),
                name: symbol.name,
                qualified_name: symbol.qualified_name,
                kind: symbol.kind,
                content: chunk_content,
                docstring,
                range: (start, end),
                lines: (
                    symbol.line_start.unwrap_or(0) as usize,
                    symbol.line_end.unwrap_or(0) as usize,
                ),
                offset: 0,
            });
        }

        Ok(chunks)
    }

    fn chunk_typescript(path: &Path, content: &str) -> Result<Vec<AstChunk>> {
        let mut parser = Parser::new();
        let language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT;
        parser.set_language(&language.into()).into_diagnostic()?;

        let tree = parser
            .parse(content, None)
            .ok_or_else(|| miette!("Failed to parse TypeScript content"))?;

        let query_str = r#"
            (function_declaration name: (identifier) @name) @symbol
            (class_declaration name: (type_identifier) @name) @symbol
            (interface_declaration name: (type_identifier) @name) @symbol
            (method_definition name: (property_identifier) @name) @symbol
            (export_statement declaration: (function_declaration name: (identifier) @name)) @symbol
        "#;

        let query = Query::new(&language.into(), query_str).into_diagnostic()?;
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&query, tree.root_node(), content.as_bytes());

        let mut chunks = Vec::new();
        let file_path = path.to_string_lossy().to_string();

        while let Some(m) = matches.next() {
            let mut name = String::new();
            let mut kind = SymbolKind::Function;
            let mut symbol_node = None;

            for capture in m.captures {
                let capture_name = query.capture_names()[capture.index as usize];
                match capture_name {
                    "name" => {
                        name = capture
                            .node
                            .utf8_text(content.as_bytes())
                            .into_diagnostic()?
                            .to_string();
                    }
                    "symbol" => {
                        symbol_node = Some(capture.node);
                        match capture.node.kind() {
                            "function_declaration" => kind = SymbolKind::Function,
                            "class_declaration" => kind = SymbolKind::Class,
                            "interface_declaration" => kind = SymbolKind::Interface,
                            "method_definition" => kind = SymbolKind::Method,
                            "export_statement" => kind = SymbolKind::Function, // usually
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }

            if let Some(node) = symbol_node {
                let chunk_content = node
                    .utf8_text(content.as_bytes())
                    .into_diagnostic()?
                    .to_string();

                let mut docstring = Vec::new();
                let mut prev = node.prev_sibling();
                while let Some(p) = prev {
                    if p.kind() == "comment" {
                        docstring.push(
                            p.utf8_text(content.as_bytes())
                                .into_diagnostic()?
                                .trim()
                                .to_string(),
                        );
                        prev = p.prev_sibling();
                    } else {
                        break;
                    }
                }
                docstring.reverse();
                let docstring = if docstring.is_empty() {
                    None
                } else {
                    Some(docstring.join("\n"))
                };

                chunks.push(AstChunk {
                    file_path: file_path.clone(),
                    name,
                    qualified_name: None,
                    kind,
                    content: chunk_content,
                    docstring,
                    range: (node.start_byte(), node.end_byte()),
                    lines: (node.start_position().row + 1, node.end_position().row + 1),
                    offset: 0,
                });
            }
        }

        Ok(chunks)
    }

    fn chunk_python(path: &Path, content: &str) -> Result<Vec<AstChunk>> {
        let mut parser = Parser::new();
        let language = tree_sitter_python::LANGUAGE;
        parser.set_language(&language.into()).into_diagnostic()?;

        let tree = parser
            .parse(content, None)
            .ok_or_else(|| miette!("Failed to parse Python content"))?;

        let query_str = r#"
            (function_definition name: (identifier) @name) @symbol
            (class_definition name: (identifier) @name) @symbol
        "#;

        let query = Query::new(&language.into(), query_str).into_diagnostic()?;
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&query, tree.root_node(), content.as_bytes());

        let mut chunks = Vec::new();
        let file_path = path.to_string_lossy().to_string();

        while let Some(m) = matches.next() {
            let mut name = String::new();
            let mut kind = SymbolKind::Function;
            let mut symbol_node = None;

            for capture in m.captures {
                let capture_name = query.capture_names()[capture.index as usize];
                match capture_name {
                    "name" => {
                        name = capture
                            .node
                            .utf8_text(content.as_bytes())
                            .into_diagnostic()?
                            .to_string();
                    }
                    "symbol" => {
                        symbol_node = Some(capture.node);
                        match capture.node.kind() {
                            "function_definition" => kind = SymbolKind::Function,
                            "class_definition" => kind = SymbolKind::Class,
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }

            if let Some(node) = symbol_node {
                let chunk_content = node
                    .utf8_text(content.as_bytes())
                    .into_diagnostic()?
                    .to_string();

                // In Python, docstring is the first child if it's a string expression
                let mut docstring = None;
                let first_child = node
                    .child_by_field_name("body")
                    .and_then(|b| b.children(&mut b.walk()).next());

                match first_child {
                    Some(child) if child.kind() == "expression_statement" => match child.child(0) {
                        Some(expr) if expr.kind() == "string" => {
                            docstring = Some(
                                expr.utf8_text(content.as_bytes())
                                    .into_diagnostic()?
                                    .to_string(),
                            );
                        }
                        _ => {}
                    },
                    _ => {}
                }

                chunks.push(AstChunk {
                    file_path: file_path.clone(),
                    name,
                    qualified_name: None,
                    kind,
                    content: chunk_content,
                    docstring,
                    range: (node.start_byte(), node.end_byte()),
                    lines: (node.start_position().row + 1, node.end_position().row + 1),
                    offset: 0,
                });
            }
        }

        Ok(chunks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::symbols::SymbolKind;
    use std::path::Path;

    #[test]
    fn is_standalone_chunk_kind_classifies_every_variant() {
        let variants = [
            SymbolKind::Function,
            SymbolKind::Method,
            SymbolKind::Class,
            SymbolKind::Struct,
            SymbolKind::Enum,
            SymbolKind::Trait,
            SymbolKind::Interface,
            SymbolKind::Type,
            SymbolKind::Variable,
            SymbolKind::Constant,
            SymbolKind::Module,
        ];
        for kind in &variants {
            let keep = match kind {
                SymbolKind::Function
                | SymbolKind::Struct
                | SymbolKind::Enum
                | SymbolKind::Trait
                | SymbolKind::Module
                | SymbolKind::Type => true,
                SymbolKind::Method
                | SymbolKind::Class
                | SymbolKind::Interface
                | SymbolKind::Variable
                | SymbolKind::Constant => false,
            };
            assert_eq!(
                is_standalone_chunk_kind(kind),
                keep,
                "{kind:?} keep/skip mismatch"
            );
        }
    }

    #[test]
    fn chunk_rust_keeps_four_standalone_and_skips_trait_method_signature() {
        let src = r#"
fn free_fn() {}

impl Foo {
    fn impl_method(&self) {}
}

trait Bar {
    fn trait_sig(&self);
}
"#;
        let chunks =
            AstChunker::chunk_file(Path::new("fixture.rs"), src).expect("chunk rust fixture");
        assert!(
            chunks.iter().all(|c| !matches!(c.kind, SymbolKind::Method)),
            "trait_sig Method must be skipped"
        );
        assert!(
            chunks.iter().all(|c| c.name != "trait_sig"),
            "trait_sig must not appear as a chunk"
        );

        let mut names_kinds: Vec<(String, SymbolKind)> =
            chunks.into_iter().map(|c| (c.name, c.kind)).collect();
        names_kinds.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.as_str().cmp(b.1.as_str())));

        assert_eq!(
            names_kinds,
            vec![
                ("Bar".to_string(), SymbolKind::Trait),
                ("Foo".to_string(), SymbolKind::Type),
                ("free_fn".to_string(), SymbolKind::Function),
                ("impl_method".to_string(), SymbolKind::Function),
            ],
            "4 kept (free_fn Function, impl_method Function, impl Foo Type, Bar Trait) + 1 skip"
        );
    }

    #[test]
    fn to_embedding_text_rust_fn_includes_name_header() {
        let chunk = AstChunk {
            file_path: "src/commands/config_verify.rs".to_string(),
            name: "apply_provenance".to_string(),
            qualified_name: None,
            kind: SymbolKind::Function,
            content: "fn apply_provenance() {}".to_string(),
            docstring: None,
            range: (0, 0),
            lines: (1, 1),
            offset: 0,
        };
        let text = chunk.to_embedding_text();
        assert!(text.starts_with("fn apply_provenance\n\n"), "{text}");
        assert!(text.contains("fn apply_provenance() {}"), "{text}");
    }

    #[test]
    fn to_embedding_text_impl_method_uses_qualified_name() {
        let chunk = AstChunk {
            file_path: "src/foo.rs".to_string(),
            name: "impl_method".to_string(),
            qualified_name: Some("Foo.impl_method".to_string()),
            kind: SymbolKind::Function,
            content: "fn impl_method(&self) {}".to_string(),
            docstring: None,
            range: (0, 0),
            lines: (1, 1),
            offset: 0,
        };
        let text = chunk.to_embedding_text();
        assert!(text.starts_with("fn Foo.impl_method\n\n"), "{text}");
        assert_eq!(text.matches("fn Foo.impl_method\n\n").count(), 1);
    }

    #[test]
    fn split_children_prefix_header_once_and_offset_is_body_index() {
        let body = "abcdefghij".repeat(8);
        let chunk = AstChunk {
            file_path: "src/long.rs".to_string(),
            name: "f".to_string(),
            qualified_name: None,
            kind: SymbolKind::Function,
            content: body.clone(),
            docstring: None,
            range: (0, body.len()),
            lines: (1, 1),
            offset: 0,
        };
        let parts = chunk.split(20, 5);
        assert!(parts.len() > 1, "expected split, got {}", parts.len());
        for part in &parts {
            let text = part.to_embedding_text();
            assert!(text.starts_with("fn f\n\n"), "{text}");
            assert_eq!(text.matches("fn f\n\n").count(), 1, "{text}");
        }
        assert_eq!(parts[0].offset, 0);
        assert!(parts[1].offset > 0, "second offset {}", parts[1].offset);
        assert!(
            !parts[0].content.starts_with("fn f"),
            "split content must be un-headered body, got {}",
            parts[0].content
        );
        let end = parts[1].offset + parts[1].content.len();
        assert_eq!(&body[parts[1].offset..end], parts[1].content.as_str());
        assert!(parts[1].offset < body.len());
    }

    #[test]
    fn to_embedding_text_empty_display_name_has_no_trailing_space() {
        let chunk = AstChunk {
            file_path: "src/empty.rs".to_string(),
            name: "   ".to_string(),
            qualified_name: None,
            kind: SymbolKind::Function,
            content: "fn x() {}".to_string(),
            docstring: None,
            range: (0, 0),
            lines: (1, 1),
            offset: 0,
        };
        let text = chunk.to_embedding_text();
        assert!(text.starts_with("fn\n\n"), "{text}");
        assert!(!text.starts_with("fn \n"), "{text}");
    }

    #[test]
    fn chunk_rust_skips_declaration_only_mod() {
        let chunks = AstChunker::chunk_file(Path::new("db.rs"), "mod provenance;\n")
            .expect("chunk declaration-only mod");
        assert!(
            chunks.iter().all(|c| !matches!(c.kind, SymbolKind::Module)),
            "{chunks:?}"
        );
        assert!(chunks.iter().all(|c| c.name != "provenance"), "{chunks:?}");
    }

    #[test]
    fn chunk_rust_keeps_mod_with_body_and_inner_fn() {
        let src = "mod inner {\n    fn x() {}\n}\n";
        let chunks = AstChunker::chunk_file(Path::new("lib.rs"), src).expect("chunk mod with body");
        assert!(
            chunks
                .iter()
                .any(|c| c.name == "x" && matches!(c.kind, SymbolKind::Function)),
            "{chunks:?}"
        );
        assert!(
            chunks
                .iter()
                .any(|c| c.name == "inner" && matches!(c.kind, SymbolKind::Module)),
            "{chunks:?}"
        );
    }

    #[test]
    fn semantic_file_content_hash_differs_from_raw_blake3() {
        let content = "fn apply_provenance() {}";
        let raw = blake3::hash(content.as_bytes()).to_hex().to_string();
        let grain = semantic_file_content_hash(content);
        assert_ne!(grain, raw);
        assert_eq!(grain, semantic_file_content_hash(content));
    }
}
