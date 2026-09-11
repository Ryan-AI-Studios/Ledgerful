//! Reconstruct a source preview from a semantic hit (`path`, `name`, `offset`).
//!
//! Stored `line_offset` is `AstChunk.offset` (embedding-text split byte), not a
//! file byte or source line. This helper locates a definition-first identifier
//! and derives `line` from the reconstructed window.

use crate::search::tantivy_engine::trim_mid_identifier;
use crate::util::fs::read_to_string_with_encoding;
use crate::util::path::resolve_under_work_root;
use std::path::Path;

/// Search `--json` / human preview width (0298 keep 240).
pub const SEARCH_PREVIEW_CHARS: usize = 240;
/// Ask gather window (replaces whole-file `.take(1000)`).
pub const ASK_PREVIEW_CHARS: usize = 1000;

const DEF_KEYWORDS: &[&str] = &[
    "fn",
    "struct",
    "enum",
    "trait",
    "type",
    "impl",
    "mod",
    "const",
    "static",
    "def",
    "class",
    "interface",
    "function",
];

/// Disk reconstruction of one semantic ranking hit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticPreview {
    pub path: String,
    pub name: String,
    pub line: Option<usize>,
    pub content: String,
    pub read_failed: bool,
}

/// Locate `name` in `file_path` under `work_root` and take a source window.
///
/// `offset` is an embedding-text skip after a definition is found (`offset == 0`
/// → start of the definition line). It is never treated as a file byte locator.
pub fn preview_semantic_hit(
    work_root: &Path,
    file_path: &str,
    name: &str,
    offset: usize,
    max_chars: usize,
) -> SemanticPreview {
    let path = file_path.replace('\\', "/");
    let open_path = resolve_under_work_root(work_root, file_path);
    let Ok(source) = read_to_string_with_encoding(&open_path) else {
        return SemanticPreview {
            path,
            name: name.to_string(),
            line: None,
            content: String::new(),
            read_failed: true,
        };
    };

    let Some(def_byte) = locate_definition(&source, name) else {
        return SemanticPreview {
            path,
            name: name.to_string(),
            line: None,
            content: String::new(),
            read_failed: false,
        };
    };

    let def_line_start = line_start_at(&source, def_byte);
    let window_start = if offset == 0 {
        def_line_start
    } else {
        clamp_char_boundary(&source, def_line_start.saturating_add(offset))
    };
    let content = take_window(&source, window_start, max_chars);
    let line = Some(line_number_at(&source, window_start));

    SemanticPreview {
        path,
        name: name.to_string(),
        line,
        content,
        read_failed: false,
    }
}

/// Pinned human template (0312 fold-in).
pub fn format_semantic_human(name: &str, path: &str, line: Option<usize>, dist: f32) -> String {
    match line {
        Some(n) => format!("- {name} ({path}:{n}) [dist: {dist:.4}]"),
        None => format!("- {name} ({path}) [dist: {dist:.4}]"),
    }
}

fn locate_definition(source: &str, name: &str) -> Option<usize> {
    if name.is_empty() {
        return None;
    }
    let matches = ident_matches(source, name);
    if matches.is_empty() {
        return None;
    }
    let mut first_non_import = None;
    for &idx in &matches {
        let line = line_containing(source, idx);
        if is_import_line(line) {
            continue;
        }
        if first_non_import.is_none() {
            first_non_import = Some(idx);
        }
        if has_definition_keyword(line) {
            return Some(idx);
        }
    }
    first_non_import
}

fn ident_matches(haystack: &str, name: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let mut start = 0;
    while let Some(rel) = haystack[start..].find(name) {
        let idx = start + rel;
        let before_ok = idx == 0
            || !haystack[..idx]
                .chars()
                .next_back()
                .is_some_and(is_ident_continue);
        let after = idx + name.len();
        let after_ok = after >= haystack.len()
            || !haystack[after..]
                .chars()
                .next()
                .is_some_and(is_ident_continue);
        if before_ok && after_ok {
            out.push(idx);
        }
        start = after.max(start + 1);
    }
    out
}

fn is_ident_continue(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn is_import_line(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("use ")
        || t.starts_with("pub use ")
        || t.starts_with("pub(crate) use ")
        || t.starts_with("pub(super) use ")
        || t.starts_with("import ")
        || t.starts_with("from ")
}

fn has_definition_keyword(line: &str) -> bool {
    DEF_KEYWORDS
        .iter()
        .any(|kw| !ident_matches(line, kw).is_empty())
}

fn line_start_at(s: &str, byte: usize) -> usize {
    let byte = byte.min(s.len());
    s[..byte].rfind('\n').map(|i| i + 1).unwrap_or(0)
}

fn line_containing(s: &str, byte: usize) -> &str {
    let start = line_start_at(s, byte);
    let end = s[start..].find('\n').map(|i| start + i).unwrap_or(s.len());
    &s[start..end]
}

fn line_number_at(s: &str, byte: usize) -> usize {
    1 + s[..byte.min(s.len())]
        .bytes()
        .filter(|&b| b == b'\n')
        .count()
}

fn clamp_char_boundary(s: &str, mut byte: usize) -> usize {
    if byte > s.len() {
        return s.len();
    }
    while byte > 0 && !s.is_char_boundary(byte) {
        byte -= 1;
    }
    byte
}

fn take_window(source: &str, start: usize, max_chars: usize) -> String {
    let start = clamp_char_boundary(source, start);
    let sliced: String = source[start..].chars().take(max_chars).collect();
    trim_mid_identifier(&sliced, source, start)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write_src(root: &Path, rel: &str, body: &str) {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("mkdir");
        }
        fs::write(&path, body).expect("write");
    }

    #[test]
    fn semantic_preview_uses_source_text() {
        let tmp = tempdir().expect("tmp");
        write_src(
            tmp.path(),
            "src/lib.rs",
            "fn execute_search() {\n    let x = 1;\n}\n",
        );
        let preview = preview_semantic_hit(
            tmp.path(),
            "src/lib.rs",
            "execute_search",
            0,
            SEARCH_PREVIEW_CHARS,
        );
        assert!(
            preview.content.contains("fn execute_search"),
            "content={:?}",
            preview.content
        );
        assert!(
            !preview.content.contains("(offset") && !preview.content.contains("dist "),
            "must not emit offset/dist labels: {:?}",
            preview.content
        );
    }

    #[test]
    fn semantic_preview_sets_line_when_name_found() {
        let tmp = tempdir().expect("tmp");
        write_src(
            tmp.path(),
            "src/lib.rs",
            "use crate::commands::search::execute_search;\n\npub fn execute_search() {}\n",
        );
        let preview = preview_semantic_hit(
            tmp.path(),
            "src/lib.rs",
            "execute_search",
            0,
            SEARCH_PREVIEW_CHARS,
        );
        assert_eq!(preview.line, Some(3));
        assert!(preview.content.starts_with("pub fn execute_search"));
    }

    #[test]
    fn semantic_preview_prefers_definition_over_import() {
        let tmp = tempdir().expect("tmp");
        write_src(
            tmp.path(),
            "src/lib.rs",
            "use crate::foo::execute_search;\n\nfn other() {}\n\nfn execute_search() { let body = 1; }\n",
        );
        let preview = preview_semantic_hit(
            tmp.path(),
            "src/lib.rs",
            "execute_search",
            0,
            SEARCH_PREVIEW_CHARS,
        );
        assert!(
            preview.content.contains("fn execute_search()"),
            "content={:?}",
            preview.content
        );
        assert!(
            !preview.content.contains("use crate"),
            "must not preview the import: {:?}",
            preview.content
        );
        assert_eq!(preview.line, Some(5));
    }

    #[test]
    fn semantic_preview_omits_line_when_unread() {
        let tmp = tempdir().expect("tmp");
        let preview = preview_semantic_hit(
            tmp.path(),
            "src/missing.rs",
            "ghost",
            0,
            SEARCH_PREVIEW_CHARS,
        );
        assert!(preview.read_failed);
        assert_eq!(preview.line, None);
        assert!(preview.content.is_empty());
        assert_eq!(preview.path, "src/missing.rs");
        assert_eq!(preview.name, "ghost");
    }

    #[test]
    fn semantic_preview_omits_line_and_content_when_symbol_missing_from_file() {
        let tmp = tempdir().expect("tmp");
        write_src(tmp.path(), "src/lib.rs", "fn other() {}\n");
        let preview = preview_semantic_hit(
            tmp.path(),
            "src/lib.rs",
            "renamed_away",
            0,
            SEARCH_PREVIEW_CHARS,
        );
        assert!(!preview.read_failed);
        assert_eq!(preview.line, None);
        assert!(preview.content.is_empty());
    }

    #[test]
    fn semantic_preview_does_not_treat_offset_as_file_byte() {
        let tmp = tempdir().expect("tmp");
        // 42 bytes into this file is mid-line noise, not a line number.
        write_src(
            tmp.path(),
            "src/lib.rs",
            "fn execute_search() {\n    let padding = \"xxxxxxxxxxxxxxxxxxxxxxxx\";\n}\n",
        );
        let preview = preview_semantic_hit(
            tmp.path(),
            "src/lib.rs",
            "execute_search",
            42,
            SEARCH_PREVIEW_CHARS,
        );
        assert_ne!(preview.line, Some(42));
        // offset is applied after the definition line start (line 1), not as file[42].
        assert_eq!(preview.line, Some(2));
        assert!(
            !preview.content.contains("fn execute_search"),
            "offset 42 must skip past the definition line: {:?}",
            preview.content
        );
    }

    #[test]
    fn semantic_preview_offset_zero_starts_at_definition_line() {
        let tmp = tempdir().expect("tmp");
        write_src(tmp.path(), "src/lib.rs", "    pub fn execute_search() {}\n");
        let preview = preview_semantic_hit(
            tmp.path(),
            "src/lib.rs",
            "execute_search",
            0,
            SEARCH_PREVIEW_CHARS,
        );
        assert!(
            preview.content.contains("pub fn execute_search"),
            "window must keep the declaration keyword: {:?}",
            preview.content
        );
        assert_eq!(preview.line, Some(1));
    }

    #[test]
    fn format_semantic_human_pins_template() {
        assert_eq!(
            format_semantic_human("foo", "src/a.rs", Some(12), 0.125),
            "- foo (src/a.rs:12) [dist: 0.1250]"
        );
        assert_eq!(
            format_semantic_human("foo", "src/a.rs", None, 0.5),
            "- foo (src/a.rs) [dist: 0.5000]"
        );
    }

    #[test]
    fn search_semantic_human_shows_preview() {
        let tmp = tempdir().expect("tmp");
        write_src(
            tmp.path(),
            "src/lib.rs",
            "fn execute_search() { let x = 1; }\n",
        );
        let preview = preview_semantic_hit(
            tmp.path(),
            "src/lib.rs",
            "execute_search",
            0,
            SEARCH_PREVIEW_CHARS,
        );
        let header = format_semantic_human(&preview.name, &preview.path, preview.line, 0.125);
        assert_eq!(header, "- execute_search (src/lib.rs:1) [dist: 0.1250]");
        assert!(
            !header.contains("at offset"),
            "human header must not be an offset label: {header}"
        );
        let body = format!("  {}", preview.content.replace('\n', "\n  "));
        assert!(body.contains("fn execute_search"));
    }

    #[test]
    fn ask_gather_uses_hit_window_not_file_start() {
        let tmp = tempdir().expect("tmp");
        let padding = "x".repeat(1200);
        let body = format!("{padding}\nfn buried_symbol() {{}}\n");
        write_src(tmp.path(), "src/lib.rs", &body);
        let preview = preview_semantic_hit(
            tmp.path(),
            "src/lib.rs",
            "buried_symbol",
            0,
            ASK_PREVIEW_CHARS,
        );
        assert!(
            preview.content.contains("fn buried_symbol"),
            "window must include the definition past the first 1000 chars: {:?}",
            preview.content
        );
        assert!(
            !preview.content.starts_with('x'),
            "must not start at file start: {:?}",
            preview.content.chars().take(20).collect::<String>()
        );
        assert_eq!(preview.line, Some(2));
    }

    #[test]
    fn semantic_preview_skips_pub_crate_use() {
        let tmp = tempdir().expect("tmp");
        write_src(
            tmp.path(),
            "src/lib.rs",
            "pub(crate) use crate::foo::execute_search;\n\nfn execute_search() {}\n",
        );
        let preview = preview_semantic_hit(
            tmp.path(),
            "src/lib.rs",
            "execute_search",
            0,
            SEARCH_PREVIEW_CHARS,
        );
        assert!(preview.content.contains("fn execute_search"));
        assert!(!preview.content.contains("pub(crate) use"));
        assert_eq!(preview.line, Some(3));
    }
}
