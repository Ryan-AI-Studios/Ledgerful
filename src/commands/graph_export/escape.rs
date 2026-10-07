//! GraphML text and Cypher literal escaping. Not MCP sanitize.

/// XML 1.0 Char excludes these code points. Surrogates cannot be a Rust `char`.
pub(crate) fn is_illegal_xml(c: char) -> bool {
    matches!(
        c,
        '\u{0000}'..='\u{0008}'
            | '\u{000B}'
            | '\u{000C}'
            | '\u{000E}'..='\u{001F}'
            | '\u{FFFE}'
            | '\u{FFFF}'
    )
}

/// Escape XML text and replace illegal characters with U+FFFD.
pub(crate) fn xml_text(input: &str, replaced: &mut u64) -> String {
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        let c = if is_illegal_xml(c) {
            *replaced += 1;
            '\u{FFFD}'
        } else {
            c
        };
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            other => out.push(other),
        }
    }
    out
}

pub(crate) fn is_cypher_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

pub(crate) fn cypher_label(category: &str) -> String {
    if is_cypher_ident(category) {
        category.to_string()
    } else {
        "Node".to_string()
    }
}

pub(crate) fn cypher_rel_type(relation: &str) -> String {
    if is_cypher_ident(relation) {
        relation.to_string()
    } else {
        format!("`{}`", relation.replace('`', "``"))
    }
}

/// Single-quoted Cypher string. Illegal XML characters stay as `\uXXXX`.
pub(crate) fn cypher_string(input: &str) -> String {
    let mut out = String::from("'");
    for c in input.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if is_illegal_xml(c) => {
                let code = u32::from(c);
                out.push_str(&format!("\\u{code:04X}"));
            }
            other => out.push(other),
        }
    }
    out.push('\'');
    out
}
