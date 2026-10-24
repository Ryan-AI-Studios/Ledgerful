//! Pure definition-line witness selection for identifier-likely hybrid search.
//!
//! The helper does not read the filesystem and does not rank keyword forms
//! ahead of method shorthand. Pool order is body/test-local, then whole-token
//! count, then path, then line.

use std::cmp::Ordering;

use camino::Utf8Path;

use crate::index::orchestrator::SUPPORTED_EXTENSIONS;

/// One declaration that can compete for the witness seat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WitnessCandidate {
    pub path: String,
    pub line: usize,
    pub content: String,
    pub has_body: bool,
    pub test_local: bool,
    pub occurrence_count: usize,
}

/// Final extension is in [`SUPPORTED_EXTENSIONS`], compared the same way as
/// `src/index/incremental.rs` (`extension()`, case-sensitive).
pub(crate) fn extension_eligible(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    let Some(ext) = Utf8Path::new(&normalized).extension() else {
        return false;
    };
    SUPPORTED_EXTENSIONS.contains(&ext)
}

/// Declarations of `token` in one already-normalized file.
///
/// Lines longer than 1000 bytes are skipped for both the occurrence count and
/// declaration matching. Ineligible paths contribute nothing.
pub(crate) fn scan_file(path: &str, text: &str, token: &str) -> Vec<WitnessCandidate> {
    if token.is_empty() || !extension_eligible(path) {
        return Vec::new();
    }
    let lines: Vec<&str> = text.lines().collect();
    let mut count = 0usize;
    let mut found: Vec<(usize, bool, String)> = Vec::new();
    for (idx, line) in lines.iter().enumerate() {
        if line.len() > 1000 {
            continue;
        }
        let spans = whole_token_spans(line, token);
        count += spans.len();
        for (start, end) in spans {
            if let Some(has_body) = classify_span(line, start, end, &lines, idx) {
                found.push((idx, has_body, line.trim().to_string()));
                break;
            }
        }
    }
    let path = path.replace('\\', "/");
    found
        .into_iter()
        .map(|(idx, has_body, content)| WitnessCandidate {
            path: path.clone(),
            line: idx + 1,
            content,
            has_body,
            test_local: is_test_local(&lines, idx),
            occurrence_count: count,
        })
        .collect()
}

/// First non-empty pool wins: body, then signature, then test-local body,
/// then test-local signature. Inside a pool: lowest count, path, line.
pub(crate) fn choose_witness(candidates: &[WitnessCandidate]) -> Option<WitnessCandidate> {
    let mut best: Option<&WitnessCandidate> = None;
    for candidate in candidates {
        best = Some(match best {
            None => candidate,
            Some(current) if witness_ord(candidate, current) == Ordering::Less => candidate,
            Some(current) => current,
        });
    }
    best.cloned()
}

/// Scan caller-supplied file texts. Ineligible paths are ignored.
#[cfg(test)]
pub(crate) fn choose_witness_in_files(
    files: &[(&str, &str)],
    token: &str,
) -> Option<WitnessCandidate> {
    let mut all = Vec::new();
    for (path, text) in files {
        all.extend(scan_file(path, text, token));
    }
    choose_witness(&all)
}

fn witness_ord(left: &WitnessCandidate, right: &WitnessCandidate) -> Ordering {
    pool_rank(left)
        .cmp(&pool_rank(right))
        .then(left.occurrence_count.cmp(&right.occurrence_count))
        .then_with(|| left.path.cmp(&right.path))
        .then(left.line.cmp(&right.line))
}

fn pool_rank(candidate: &WitnessCandidate) -> u8 {
    match (candidate.has_body, candidate.test_local) {
        (true, false) => 0,
        (false, false) => 1,
        (true, true) => 2,
        (false, true) => 3,
    }
}

fn whole_token_spans(line: &str, token: &str) -> Vec<(usize, usize)> {
    if token.is_empty() {
        return Vec::new();
    }
    let mut spans = Vec::new();
    let mut from = 0usize;
    while from < line.len() {
        let Some(rel) = line[from..].find(token) else {
            break;
        };
        let start = from + rel;
        let end = start + token.len();
        if prev_allows(line, start) && next_allows(line, end) {
            spans.push((start, end));
        }
        from = end;
    }
    spans
}

fn prev_allows(line: &str, start: usize) -> bool {
    match line[..start].chars().next_back() {
        None => true,
        Some(ch) => !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '.'),
    }
}

fn next_allows(line: &str, end: usize) -> bool {
    match line[end..].chars().next() {
        None => true,
        Some(ch) => !(ch.is_ascii_alphanumeric() || ch == '_'),
    }
}

/// `Some(has_body)` when this occurrence is a declaration.
fn classify_span(line: &str, start: usize, end: usize, lines: &[&str], idx: usize) -> Option<bool> {
    let prefix = &line[..start];
    let after = &line[end..];
    if is_keyword_declaration(prefix) {
        return Some(keyword_has_body(line, after, lines, idx));
    }
    if prefix.trim().is_empty() && after.trim_start().starts_with('(') {
        return shorthand_kind(line, end, lines, idx);
    }
    None
}

fn is_keyword_declaration(prefix: &str) -> bool {
    let words = prefix_words(prefix);
    let Some(keyword) = words.last().copied() else {
        return false;
    };
    if !is_decl_keyword(keyword) {
        return false;
    }
    let trimmed_end = prefix.trim_end_matches(|ch: char| ch.is_ascii_whitespace());
    if !trimmed_end.ends_with(keyword) {
        return false;
    }
    let head = &trimmed_end[..trimmed_end.len() - keyword.len()];
    prefix_before_keyword_allowed(head)
}

fn prefix_before_keyword_allowed(head: &str) -> bool {
    let qualifiers: &[&str] = &[
        "pub(crate)",
        "pub(super)",
        "protected",
        "private",
        "public",
        "export",
        "default",
        "unsafe",
        "static",
        "async",
        "const",
        "pub",
    ];
    let mut rest = head;
    loop {
        rest = rest.trim_start_matches(|ch: char| ch.is_ascii_whitespace());
        if rest.is_empty() {
            return true;
        }
        if rest.starts_with("#![") || rest.starts_with("#[") {
            let Some(end) = rest.find(']') else {
                return false;
            };
            rest = &rest[end + 1..];
            continue;
        }
        let mut matched = false;
        for qualifier in qualifiers {
            if rest.starts_with(qualifier) {
                let after = &rest[qualifier.len()..];
                let boundary_ok = match after.chars().next() {
                    None => true,
                    Some(ch) => !(ch.is_ascii_alphanumeric() || ch == '_'),
                };
                if boundary_ok {
                    rest = after;
                    matched = true;
                    break;
                }
            }
        }
        if !matched {
            return false;
        }
    }
}

#[allow(dead_code)]
fn is_qualifier(word: &str) -> bool {
    matches!(
        word,
        "pub"
            | "pub(crate)"
            | "pub(super)"
            | "export"
            | "public"
            | "private"
            | "protected"
            | "async"
            | "const"
            | "static"
            | "unsafe"
            | "default"
    )
}

fn is_decl_keyword(word: &str) -> bool {
    matches!(
        word,
        "fn" | "def"
            | "func"
            | "function"
            | "class"
            | "struct"
            | "enum"
            | "trait"
            | "type"
            | "interface"
    )
}

fn prefix_words(prefix: &str) -> Vec<&str> {
    let bytes = prefix.as_bytes();
    let mut words = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let start = i;
        while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
            i += 1;
        }
        if start == i {
            i += 1;
            continue;
        }
        let mut end = i;
        if &prefix[start..end] == "pub" {
            if prefix[end..].starts_with("(crate)") {
                end += "(crate)".len();
                i = end;
            } else if prefix[end..].starts_with("(super)") {
                end += "(super)".len();
                i = end;
            }
        }
        words.push(&prefix[start..end]);
    }
    words
}

fn keyword_has_body(line: &str, after: &str, lines: &[&str], idx: usize) -> bool {
    if brace_marks_body(after) || line.trim().ends_with(':') {
        return true;
    }
    let decl_indent = indent_width(line);
    let mut seen = 0usize;
    let mut i = idx + 1;
    while i < lines.len() && seen < 2 {
        if lines[i].trim().is_empty() {
            i += 1;
            continue;
        }
        seen += 1;
        if indent_width(lines[i]) > decl_indent {
            return true;
        }
        i += 1;
    }
    false
}

/// `{` after the token is a body when a non-type brace appears, or when any
/// `{` opened after the token is still unclosed at end of text. A balanced
/// type brace (`: { value: number }`) is not a body. A same-line block
/// (`{ return 1; }`) is.
fn brace_marks_body(after: &str) -> bool {
    let chars: Vec<char> = after.chars().collect();
    let mut depth = 0i32;
    let mut non_type = false;
    for (i, ch) in chars.iter().enumerate() {
        match ch {
            '{' => {
                if !preceded_by_colon(&chars, i) {
                    non_type = true;
                }
                depth += 1;
            }
            '}' if depth > 0 => {
                depth -= 1;
            }
            _ => {}
        }
    }
    non_type || depth > 0
}

fn preceded_by_colon(chars: &[char], brace_at: usize) -> bool {
    let mut i = brace_at;
    while i > 0 {
        i -= 1;
        if chars[i].is_whitespace() {
            continue;
        }
        return chars[i] == ':';
    }
    false
}

/// `Some(true)` body, `Some(false)` signature, `None` call (not a candidate).
fn shorthand_kind(line: &str, token_end: usize, lines: &[&str], idx: usize) -> Option<bool> {
    let after = &line[token_end..];
    let rel = after.find('(')?;
    let open_at = token_end + rel;
    if let Some(close_at) = find_close(line, open_at) {
        return same_line_remainder(&line[close_at + 1..]);
    }
    let mut depth = depth_at_end_from_open(line, open_at);
    let mut seen = 0usize;
    let mut i = idx + 1;
    while i < lines.len() && seen < 2 {
        if lines[i].trim().is_empty() {
            i += 1;
            continue;
        }
        seen += 1;
        let (next_depth, close_at) = close_on_line(lines[i], depth);
        if let Some(at) = close_at {
            let after_close = &lines[i][at + 1..];
            let trimmed_line = lines[i].trim();
            if brace_marks_body(after_close)
                || trimmed_line.ends_with('{')
                || trimmed_line.ends_with(':')
            {
                return Some(true);
            }
            return None;
        }
        depth = next_depth;
        i += 1;
    }
    None
}

fn same_line_remainder(remainder: &str) -> Option<bool> {
    let trimmed = remainder.trim();
    if trimmed.is_empty() || trimmed == ";" {
        return None;
    }
    if brace_marks_body(remainder) || trimmed.ends_with('{') {
        return Some(true);
    }
    if trimmed.starts_with(':') {
        return Some(false);
    }
    None
}

fn find_close(line: &str, open_at: usize) -> Option<usize> {
    let mut depth = 0i32;
    for (idx, ch) in line.char_indices() {
        if idx < open_at {
            continue;
        }
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(idx);
                }
            }
            _ => {}
        }
    }
    None
}

fn depth_at_end_from_open(line: &str, open_at: usize) -> i32 {
    let mut depth = 0i32;
    for (idx, ch) in line.char_indices() {
        if idx < open_at {
            continue;
        }
        match ch {
            '(' => depth += 1,
            ')' => depth -= 1,
            _ => {}
        }
    }
    depth
}

fn close_on_line(line: &str, mut depth: i32) -> (i32, Option<usize>) {
    let mut close_at = None;
    for (idx, ch) in line.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    close_at = Some(idx);
                    break;
                }
            }
            _ => {}
        }
    }
    (depth, close_at)
}

fn is_test_local(lines: &[&str], idx: usize) -> bool {
    let indent = indent_width(lines[idx]);
    equal_indent_test_attribute(lines, idx, indent) || strict_less_test_local(lines, idx, indent)
}

fn strict_less_test_local(lines: &[&str], idx: usize, decl_indent: usize) -> bool {
    if decl_indent == 0 {
        return false;
    }
    let start = idx.saturating_sub(40);
    for opener_idx in start..idx {
        let line = lines[opener_idx];
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let opener_indent = indent_width(line);
        if opener_indent >= decl_indent || !is_test_opener(trimmed) {
            continue;
        }
        if test_block_still_open(lines, opener_idx, idx, opener_indent) {
            return true;
        }
    }
    false
}

fn test_block_still_open(
    lines: &[&str],
    opener_idx: usize,
    decl_idx: usize,
    opener_indent: usize,
) -> bool {
    if lines[opener_idx].contains('}') {
        return false;
    }
    let opener_trimmed = lines[opener_idx].trim();
    let attribute_opener = opener_trimmed.starts_with("#[") || opener_trimmed.starts_with('@');
    let mut skipped_header = false;
    for line in &lines[opener_idx + 1..decl_idx] {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if attribute_opener && indent_width(line) == opener_indent {
            if trimmed.starts_with("#[") || trimmed.starts_with("#![") || trimmed.starts_with('@') {
                continue;
            }
            if !skipped_header && !trimmed.starts_with('}') {
                skipped_header = true;
                continue;
            }
        }
        if indent_width(line) <= opener_indent {
            return false;
        }
    }
    true
}

fn equal_indent_test_attribute(lines: &[&str], idx: usize, decl_indent: usize) -> bool {
    let mut saw = false;
    let mut i = idx;
    while i > 0 {
        i -= 1;
        if idx - i > 40 {
            break;
        }
        let trimmed = lines[i].trim();
        if trimmed.is_empty() {
            continue;
        }
        if indent_width(lines[i]) != decl_indent {
            break;
        }
        if !(trimmed.starts_with('#') || trimmed.starts_with('@')) {
            break;
        }
        if is_test_attribute(trimmed) {
            saw = true;
        }
    }
    saw
}

fn is_test_attribute(trimmed: &str) -> bool {
    trimmed == "#[test]"
        || trimmed.contains("#[tokio::test]")
        || trimmed.contains("#[async_std::test]")
        || trimmed.starts_with("@pytest")
        || trimmed.starts_with("@Test")
}

fn is_test_opener(trimmed: &str) -> bool {
    trimmed.starts_with("#[test]")
        || trimmed.contains("#[tokio::test]")
        || trimmed.contains("#[async_std::test]")
        || trimmed.starts_with("def test_")
        || trimmed.starts_with("async def test_")
        || trimmed.starts_with("fn test_")
        || trimmed.starts_with("func Test")
        || trimmed.starts_with("function test")
        || trimmed.starts_with("@pytest")
        || trimmed.starts_with("@Test")
        || has_call_opener(trimmed)
}

fn has_call_opener(text: &str) -> bool {
    ["it", "test", "describe"]
        .into_iter()
        .any(|name| call_name_opens(text, name))
}

fn call_name_opens(text: &str, name: &str) -> bool {
    let mut search_from = 0usize;
    while search_from < text.len() {
        let Some(rel) = text[search_from..].find(name) else {
            return false;
        };
        let start = search_from + rel;
        let end = start + name.len();
        let prev_ok = start == 0
            || text[..start]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_whitespace());
        if prev_ok && text[end..].trim_start().starts_with('(') {
            return true;
        }
        search_from = end;
    }
    false
}

fn indent_width(line: &str) -> usize {
    let mut width = 0usize;
    for ch in line.chars() {
        match ch {
            ' ' => width += 1,
            '\t' => width += 4,
            _ => break,
        }
    }
    width
}

#[cfg(test)]
#[allow(non_snake_case)]
mod tests {
    use super::*;

    fn witness(files: &[(&str, &str)], token: &str) -> Option<WitnessCandidate> {
        choose_witness_in_files(files, token)
    }

    #[test]
    fn definition_line__dot_call__is_not_a_declaration() {
        let text = "obj.widget_open(source);\nobj.widget_open(source);\n";
        assert!(
            witness(&[("pkg/widget.js", text)], "widget_open").is_none(),
            "a dotted call is not a declaration"
        );
    }

    #[test]
    fn definition_line__longer_ident__does_not_match() {
        let text = "function widget_open_works() {\n    return 1;\n}\n";
        assert!(
            witness(&[("pkg/widget.js", text)], "widget_open").is_none(),
            "widget_open must not match widget_open_works"
        );
    }

    #[test]
    fn definition_line__body_beats_signature() {
        let files = [
            ("types/widget.d.ts", "widget_open(source: Widget): void;\n"),
            (
                "pkg/widget.js",
                "widget_open(source) {\n    return source;\n}\n",
            ),
        ];
        let chosen = witness(&files, "widget_open").expect("witness");
        assert!(chosen.has_body);
        assert!(!chosen.test_local);
        assert_eq!(chosen.path, "pkg/widget.js");
        assert!(chosen.content.contains("widget_open(source) {"));
    }

    #[test]
    fn definition_line__test_indent__is_test_local() {
        let text = "\
class Widget {
    constructor(name) {
        return name;
    }
}

test(\"opens\", () => {
    constructor(name) {
        return name;
    }
});
";
        let scanned = scan_file("pkg/widget.js", text, "constructor");
        let nested = scanned
            .iter()
            .find(|candidate| candidate.test_local)
            .expect("nested constructor");
        assert!(nested.has_body);
        let chosen = choose_witness(&scanned).expect("witness");
        assert!(!chosen.test_local);
        assert!(chosen.has_body);
        assert!(chosen.line < nested.line);
    }

    #[test]
    fn definition_line__lower_count_body__wins() {
        let files = [
            (
                "pkg/widget.js",
                "class Widget {\n    widget_open(source) {\n        return source;\n    }\n}\n",
            ),
            (
                "tests/helper.js",
                "function widget_open() {\n    widget_open();\n    widget_open();\n}\n",
            ),
        ];
        let chosen = witness(&files, "widget_open").expect("witness");
        assert_eq!(chosen.path, "pkg/widget.js");
        assert!(chosen.occurrence_count < 3);
        assert!(chosen.has_body);
    }

    #[test]
    fn definition_line__equal_count__lower_path_wins() {
        let files = [
            (
                "b/widget.js",
                "function widget_open() {\n    return 1;\n}\n",
            ),
            (
                "a/widget.js",
                "widget_open(source) {\n    return source;\n}\n",
            ),
        ];
        let chosen = witness(&files, "widget_open").expect("witness");
        assert_eq!(chosen.path, "a/widget.js");
        assert!(chosen.has_body);
        assert_eq!(chosen.occurrence_count, 1);
    }

    #[test]
    fn definition_line__extension_gate__skips_readme_keeps_code() {
        assert!(!extension_eligible("Readme.md"));
        assert!(!extension_eligible("docs/guide.rst"));
        assert!(!extension_eligible("notes.txt"));
        assert!(!extension_eligible("Makefile"));
        assert!(extension_eligible("pkg/widget.js"));
        assert!(extension_eligible("types/widget.d.ts"));
        assert!(extension_eligible("src/keeper.rs"));
        assert!(
            extension_eligible("src/widget.cpp"),
            "C++ spellings in SUPPORTED_EXTENSIONS stay eligible"
        );
        assert!(extension_eligible("include/widget.h++"));
        let files = [
            ("Readme.md", "function widget_open() {\n    return 1;\n}\n"),
            (
                "pkg/widget.js",
                "widget_open(source) {\n    return source;\n}\n",
            ),
        ];
        let chosen = witness(&files, "widget_open").expect("code file wins");
        assert_eq!(chosen.path, "pkg/widget.js");
        assert!(scan_file("Readme.md", files[0].1, "widget_open").is_empty());
    }

    #[test]
    fn definition_line__shorthand_call__is_not_a_declaration() {
        let calls = [
            "widget_open();\n",
            "widget_open(source);\n",
            "widget_open({ source: 1 });\n",
            "widget_open(\n    source,\n);\n",
        ];
        for text in calls {
            assert!(
                witness(&[("pkg/widget.js", text)], "widget_open").is_none(),
                "call must not be a declaration: {text:?}"
            );
        }
        let body = witness(
            &[(
                "pkg/widget.js",
                "widget_open(source) {\n    return source;\n}\n",
            )],
            "widget_open",
        )
        .expect("body");
        assert!(body.has_body);
        let split_body = witness(
            &[(
                "pkg/widget.js",
                "widget_open(\n    source,\n) {\n    return source;\n}\n",
            )],
            "widget_open",
        )
        .expect("split body");
        assert!(split_body.has_body);
        let signature = witness(
            &[("types/widget.d.ts", "widget_open(source: Widget): void;\n")],
            "widget_open",
        )
        .expect("signature");
        assert!(!signature.has_body);
        let both = [
            ("types/widget.d.ts", "widget_open(source: Widget): void;\n"),
            ("pkg/widget.js", "widget_open(source) {\n}\n"),
        ];
        let chosen = witness(&both, "widget_open").expect("body wins");
        assert_eq!(chosen.path, "pkg/widget.js");
        assert!(chosen.has_body);
    }

    #[test]
    fn definition_line__type_brace__is_not_a_body() {
        let signature = "function widget_open(): { value: number };\n";
        let open_body = "function widget_open() {\n    return 1;\n}\n";
        let same_line = "function widget_open() { return 1; }\n";
        let signature_hit = scan_file("types/widget.d.ts", signature, "widget_open");
        assert_eq!(signature_hit.len(), 1);
        assert!(
            !signature_hit[0].has_body,
            "balanced type brace is not a body"
        );
        let open_hit = scan_file("pkg/open.js", open_body, "widget_open");
        assert!(open_hit[0].has_body);
        let same_hit = scan_file("pkg/same.js", same_line, "widget_open");
        assert!(
            same_hit[0].has_body,
            "same-line block with a closing brace is a body"
        );
        let chosen = witness(
            &[("types/widget.d.ts", signature), ("pkg/open.js", open_body)],
            "widget_open",
        )
        .expect("body wins");
        assert_eq!(chosen.path, "pkg/open.js");
        assert!(chosen.has_body);
        assert_eq!(chosen.content, "function widget_open() {");
    }

    #[test]
    fn definition_line__equal_indent_test_attr__is_test_local() {
        let text = "\
#[test]
fn widget_open() {
    let _ = 1;
}

fn widget_open() {
    let _ = 2;
}
";
        let scanned = scan_file("tests/keeper.rs", text, "widget_open");
        assert_eq!(scanned.len(), 2);
        assert!(scanned[0].test_local);
        assert!(scanned[0].has_body);
        assert!(!scanned[1].test_local, "later sibling is not test-local");
        let chosen = choose_witness(&scanned).expect("non-test body");
        assert_eq!(chosen.line, scanned[1].line);
        assert!(!chosen.test_local);
    }

    #[test]
    fn definition_line__keeper_slot__src_beats_equal_indent_test() {
        let files = [
            (
                "tests/keeper.rs",
                "#[test]\nfn keeper_slot() {\n    keeper_slot();\n    keeper_slot();\n    keeper_slot();\n}\n",
            ),
            (
                "src/keeper.rs",
                "pub fn keeper_slot() {\n    let _ = 1;\n}\n",
            ),
        ];
        let chosen = witness(&files, "keeper_slot").expect("witness");
        assert_eq!(chosen.path, "src/keeper.rs");
        assert!(!chosen.test_local);
        assert!(chosen.has_body);
        let tests = scan_file("tests/keeper.rs", files[0].1, "keeper_slot");
        assert_eq!(tests.len(), 1, "bare calls are not declarations");
        assert!(tests[0].test_local);
    }

    #[test]
    fn definition_line__const_arrow__is_not_a_declaration() {
        let text = "const widget_open = (source) => {\n    return source;\n};\n";
        assert!(
            witness(&[("pkg/widget.js", text)], "widget_open").is_none(),
            "const arrow forms stay unmatched"
        );
    }

    #[test]
    fn definition_line__tab_indent__counts_as_four() {
        let text = "  test(\"x\", () => {\n\twidget_open(source) {\n\t}\n});\n";
        let chosen = witness(&[("pkg/widget.js", text)], "widget_open").expect("decl");
        assert!(
            chosen.test_local,
            "one tab is wider than the two-space test opener"
        );
    }

    #[test]
    fn definition_line__keyword_call_shape__stays_a_declaration() {
        let text = "function widget_open();\n";
        let chosen = witness(&[("pkg/widget.js", text)], "widget_open").expect("keyword");
        assert!(!chosen.has_body);
    }

    #[test]
    fn definition_line__type_field__is_not_a_declaration() {
        assert!(
            witness(&[("pkg/action.js", "({ type: ADD_TODO })\n")], "ADD_TODO").is_none(),
            "a type field is not a declaration"
        );
        assert!(
            witness(
                &[("pkg/action.js", "dispatch({ type: ADD_TODO })\n")],
                "ADD_TODO",
            )
            .is_none(),
            "a dispatch type field is not a declaration"
        );
        let alias = witness(
            &[("types/add.d.ts", "type ADD_TODO = string;\n")],
            "ADD_TODO",
        )
        .expect("type alias");
        assert_eq!(alias.path, "types/add.d.ts");
        assert!(!alias.has_body);
        assert!(!alias.test_local);
        assert!(
            witness(
                &[(
                    "pkg/check.py",
                    "isinstance(x, type) and widget_open(source);\n"
                )],
                "widget_open",
            )
            .is_none(),
            "isinstance stays unmatched"
        );
        let qualified = witness(
            &[(
                "src/widget.rs",
                "pub(crate) async fn widget_open() {\n    let _ = 1;\n}\n",
            )],
            "widget_open",
        )
        .expect("qualified fn");
        assert!(qualified.has_body);
        assert!(!qualified.test_local);
        let attributed = witness(
            &[(
                "src/widget.rs",
                "#[cfg(test)] fn widget_open() {\n    let _ = 1;\n}\n",
            )],
            "widget_open",
        )
        .expect("attributed fn");
        assert!(attributed.has_body);
        assert!(!attributed.test_local);
        let inner = witness(
            &[(
                "src/widget.rs",
                "#![allow(dead_code)] fn widget_open() {\n    let _ = 1;\n}\n",
            )],
            "widget_open",
        )
        .expect("inner attribute");
        assert!(inner.has_body);
        assert!(!inner.test_local);
        let inner_pub = witness(
            &[(
                "src/widget.rs",
                "#![allow(dead_code)] pub fn widget_open() {\n    let _ = 1;\n}\n",
            )],
            "widget_open",
        )
        .expect("inner attribute with pub");
        assert!(inner_pub.has_body);
        assert!(!inner_pub.test_local);
    }

    #[test]
    fn definition_line__comment_line__is_not_a_declaration() {
        assert!(
            witness(
                &[("pkg/widget.js", "// calls function widget_open\n")],
                "widget_open",
            )
            .is_none(),
            "a calls-function comment is not a declaration"
        );
        assert!(
            witness(
                &[("pkg/widget.js", "// the function widget_open does this\n")],
                "widget_open",
            )
            .is_none(),
            "a function comment is not a declaration"
        );
        assert!(
            witness(&[("pkg/widget.rs", "/// see struct Foo\n")], "Foo").is_none(),
            "a doc comment is not a declaration"
        );
        assert!(
            witness(&[("pkg/widget.rs", " * struct Foo\n")], "Foo").is_none(),
            "a block-comment star line is not a declaration"
        );
        assert!(
            witness(
                &[("pkg/widget.js", "/* see function widget_open */\n")],
                "widget_open",
            )
            .is_none(),
            "a block comment is not a declaration"
        );
        let real = witness(
            &[(
                "pkg/widget.js",
                "function widget_open() {\n    return 1;\n}\n",
            )],
            "widget_open",
        )
        .expect("function");
        assert_eq!(real.path, "pkg/widget.js");
        assert!(real.has_body);
        assert!(!real.test_local);
    }

    #[test]
    fn definition_line__closed_test_then_method__seats_the_method() {
        let method_file = "\
test(\"x\", () => {
    return 1;
});

class Widget {
    widget_open(source) {
        return source;
    }
}
";
        let chosen = witness(
            &[
                ("pkg/widget.js", method_file),
                ("types/widget.d.ts", "widget_open(source: Widget): void;\n"),
            ],
            "widget_open",
        )
        .expect("class method");
        assert_eq!(chosen.path, "pkg/widget.js");
        assert!(chosen.has_body);
        assert!(!chosen.test_local);
        assert_eq!(chosen.line, 6, "class method after the closed test");
    }

    #[test]
    fn definition_line__closed_test_then_impl_method__seats_the_method() {
        let method_file = "\
#[test]
fn test_widget() {
    let _ = 1;
}

impl Widget {
    fn widget_open() {
        let _ = 2;
    }
}
";
        let chosen = witness(
            &[
                ("src/widget.rs", method_file),
                ("types/widget.d.ts", "widget_open(): void;\n"),
            ],
            "widget_open",
        )
        .expect("impl method");
        assert_eq!(chosen.path, "src/widget.rs");
        assert!(chosen.has_body);
        assert!(!chosen.test_local);
        assert_eq!(chosen.line, 7, "impl method after the closed test");
    }

    #[test]
    fn definition_line__open_rust_test_any_name__nested_fn_is_test_local() {
        let text = "\
#[test]
fn verifies_widget_open() {
    fn widget_open() {}
}
";
        let chosen = witness(&[("src/widget.rs", text)], "widget_open").expect("nested fn");
        assert!(chosen.test_local);
        let stacked = "\
#[test]
#[ignore]
fn verifies_widget_open() {
    fn widget_open() {}
}
";
        let stacked_hit =
            witness(&[("src/widget.rs", stacked)], "widget_open").expect("ignore stack");
        assert!(
            stacked_hit.test_local,
            "an ignore attribute between the test attribute and the function is not a closure"
        );
    }

    #[test]
    fn definition_line__single_line_test__seats_the_following_method() {
        let method_file = "\
test(\"empty\", () => {});
    widget_open(source) {
        return source;
    }
";
        let chosen = witness(
            &[
                ("pkg/widget.js", method_file),
                ("types/widget.d.ts", "widget_open(source: Widget): void;\n"),
            ],
            "widget_open",
        )
        .expect("method after single-line test");
        assert_eq!(chosen.path, "pkg/widget.js");
        assert!(chosen.has_body);
        assert!(!chosen.test_local);
    }
}
