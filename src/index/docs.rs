use serde::{Deserialize, Serialize};

const MAX_SUMMARY_CHARS: usize = 5000;
const MAX_SUMMARY_LINES: usize = 500;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedDoc {
    pub file_path: String,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub sections: Vec<DocSection>,
    pub code_blocks: Vec<CodeBlock>,
    pub internal_links: Vec<InternalLink>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocSection {
    pub title: String,
    pub level: u8,
    pub line_start: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeBlock {
    pub language: Option<String>,
    pub content_preview: String,
    pub line_start: usize,
    pub line_end: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InternalLink {
    pub target: String,
    pub line_start: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocIndexStats {
    pub docs_indexed: usize,
    pub parse_failures: usize,
    pub missing_readme: bool,
}

/// Parse a Markdown document and extract structured content.
pub fn parse_markdown(content: &str, file_path: &str) -> ParsedDoc {
    let mut title = None;
    let mut sections = Vec::new();
    let mut code_blocks = Vec::new();
    let mut internal_links = Vec::new();

    let parser = pulldown_cmark::Parser::new_ext(content, pulldown_cmark::Options::ENABLE_TABLES);

    let mut current_heading_text = String::new();
    let mut current_heading_level: Option<u8> = None;
    let mut in_code_block = false;
    let mut code_block_language: Option<String> = None;
    let mut code_block_content = String::new();
    let mut code_block_start: usize = 0;
    let mut line_number = 0;

    // Pre-compute line offsets for accurate line_start tracking
    let line_offsets: Vec<usize> = std::iter::once(0)
        .chain(content.match_indices('\n').map(|(i, _)| i + 1))
        .collect();

    for event in parser {
        match event {
            pulldown_cmark::Event::Start(pulldown_cmark::Tag::Heading { level, .. }) => {
                current_heading_text.clear();
                current_heading_level = Some(level as u8);
            }
            pulldown_cmark::Event::End(pulldown_cmark::TagEnd::Heading(_)) => {
                if let Some(lvl) = current_heading_level.take() {
                    let heading_text = current_heading_text.trim().to_string();
                    if !heading_text.is_empty() {
                        // Estimate line number from byte offset (approximate)
                        let est_line = estimate_line_number(&line_offsets, content.len());
                        sections.push(DocSection {
                            title: heading_text.clone(),
                            level: lvl,
                            line_start: if lvl == 1 {
                                est_line.saturating_sub(1)
                            } else {
                                est_line
                            },
                        });
                        if lvl == 1 && title.is_none() {
                            title = Some(heading_text);
                        }
                    }
                }
            }
            pulldown_cmark::Event::Text(text) => {
                if in_code_block {
                    code_block_content.push_str(&text);
                } else if current_heading_level.is_some() {
                    current_heading_text.push_str(&text);
                }
            }
            pulldown_cmark::Event::Code(code)
                if current_heading_level.is_some() && !in_code_block =>
            {
                current_heading_text.push_str(&code);
            }
            pulldown_cmark::Event::Start(pulldown_cmark::Tag::CodeBlock(lang_info)) => {
                in_code_block = true;
                code_block_content.clear();
                code_block_language = match &lang_info {
                    pulldown_cmark::CodeBlockKind::Fenced(lang) => {
                        if lang.is_empty() {
                            None
                        } else {
                            Some(lang.to_string())
                        }
                    }
                    pulldown_cmark::CodeBlockKind::Indented => None,
                };
                code_block_start = line_number;
            }
            pulldown_cmark::Event::End(pulldown_cmark::TagEnd::CodeBlock) => {
                in_code_block = false;
                let preview: String = code_block_content.chars().take(200).collect();
                code_blocks.push(CodeBlock {
                    language: code_block_language.take(),
                    content_preview: preview,
                    line_start: code_block_start,
                    line_end: line_number,
                });
                code_block_content.clear();
            }
            pulldown_cmark::Event::Start(pulldown_cmark::Tag::Link { dest_url, .. }) => {
                let url = dest_url.to_string();
                if url.ends_with(".md")
                    && !url.starts_with("http://")
                    && !url.starts_with("https://")
                {
                    internal_links.push(InternalLink {
                        target: url,
                        line_start: line_number,
                    });
                }
            }
            pulldown_cmark::Event::SoftBreak | pulldown_cmark::Event::HardBreak => {
                line_number += 1;
            }
            _ => {}
        }
    }

    // Extract summary from the raw content (first MAX_SUMMARY_LINES lines, truncated to MAX_SUMMARY_CHARS)
    let summary = extract_summary(content, MAX_SUMMARY_LINES, MAX_SUMMARY_CHARS);

    // Fallback title from filename
    if title.is_none() {
        let stem = std::path::Path::new(file_path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Document");
        title = Some(stem.to_string());
    }

    ParsedDoc {
        file_path: file_path.to_string(),
        title,
        summary,
        sections,
        code_blocks,
        internal_links,
    }
}

/// Extract a summary from the content by stripping Markdown formatting
/// from the first `max_lines` lines, truncated to `max_chars` characters.
pub fn extract_summary(content: &str, max_lines: usize, max_chars: usize) -> Option<String> {
    let lines: Vec<&str> = content.lines().take(max_lines).collect();
    let raw_text = lines.join("\n");

    // Strip Markdown formatting using pulldown-cmark
    let parser = pulldown_cmark::Parser::new_ext(&raw_text, pulldown_cmark::Options::ENABLE_TABLES);
    let mut plain_text = String::new();
    for event in parser {
        match event {
            pulldown_cmark::Event::Text(text) => {
                plain_text.push_str(&text);
                plain_text.push(' ');
            }
            pulldown_cmark::Event::Code(code) => {
                plain_text.push_str(&code);
                plain_text.push(' ');
            }
            pulldown_cmark::Event::SoftBreak | pulldown_cmark::Event::HardBreak => {
                plain_text.push('\n');
            }
            _ => {}
        }
    }

    let result = plain_text.trim().to_string();
    if result.is_empty() {
        None
    } else if result.len() > max_chars {
        // Truncate at a character boundary near max_chars
        let mut end = max_chars;
        while !result.is_char_boundary(end) {
            end -= 1;
        }
        let truncated = &result[..end];
        if let Some(last_space) = truncated.rfind(' ') {
            Some(format!("{}...", &result[..last_space]))
        } else {
            Some(format!("{}...", truncated))
        }
    } else {
        Some(result)
    }
}

fn estimate_line_number(line_offsets: &[usize], byte_pos: usize) -> usize {
    match line_offsets.binary_search(&byte_pos) {
        Ok(line) => line,
        Err(idx) => idx,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_markdown_with_headings_and_code() {
        let content = r#"# My Project

This is a sample project.

## Installation

```rust
fn main() {
    println!("Hello");
}
```

## Usage

See [contributing](CONTRIBUTING.md) for details.
"#;

        let doc = parse_markdown(content, "README.md");

        assert_eq!(doc.title, Some("My Project".to_string()));
        assert!(doc.summary.is_some());
        assert!(doc.sections.len() >= 2); // "My Project" and "Installation" and "Usage"
        assert!(!doc.code_blocks.is_empty());
        assert_eq!(doc.code_blocks[0].language, Some("rust".to_string()));
        assert!(
            doc.internal_links
                .iter()
                .any(|l| l.target == "CONTRIBUTING.md")
        );
    }

    #[test]
    fn test_parse_markdown_no_heading() {
        let content = "Just some text without a heading.";
        let doc = parse_markdown(content, "docs/guide.md");

        // Title falls back to filename
        assert_eq!(doc.title, Some("guide".to_string()));
    }

    #[test]
    fn test_parse_markdown_empty() {
        let doc = parse_markdown("", "empty.md");
        assert_eq!(doc.title, Some("empty".to_string()));
        assert!(doc.summary.is_none());
        assert!(doc.sections.is_empty());
    }

    #[test]
    fn test_parse_markdown_internal_links() {
        let content = "[API Docs](docs/api.md) and [External](https://example.com)";
        let doc = parse_markdown(content, "README.md");

        assert_eq!(doc.internal_links.len(), 1);
        assert_eq!(doc.internal_links[0].target, "docs/api.md");
    }

    #[test]
    fn test_extract_summary_truncation() {
        let long_text = "word ".repeat(2000); // ~10,000 chars
        let summary = extract_summary(&long_text, 500, 5000);
        assert!(summary.is_some());
        let s = summary.unwrap();
        assert!(
            s.len() <= 5004,
            "Summary should be truncated: got {} chars",
            s.len()
        ); // +4 for "..."
    }

    #[test]
    fn test_extract_summary_line_limit() {
        let many_lines: Vec<String> = (0..1000).map(|i| format!("Line {}", i)).collect();
        let content = many_lines.join("\n");
        let summary = extract_summary(&content, 500, 5000);
        assert!(summary.is_some());
    }

    #[test]
    fn test_code_block_extraction() {
        let content = r#"# Test

```typescript
const x: number = 42;
console.log(x);
```

```python
def hello():
    print("world")
```
"#;

        let doc = parse_markdown(content, "test.md");
        assert_eq!(doc.code_blocks.len(), 2);
        assert_eq!(doc.code_blocks[0].language, Some("typescript".to_string()));
        assert_eq!(doc.code_blocks[1].language, Some("python".to_string()));
    }
}
