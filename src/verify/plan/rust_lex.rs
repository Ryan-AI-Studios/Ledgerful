//! Shared Rust lexical primitives (comments, string/char literals, delimiter
//! matching). Used by the embedded-material scanner and by the
//! `INCLUDE_STR_MATERIAL` drift test so there is exactly one lexer (0549 D5).

pub(crate) fn parse_rust_string_literal(s: &str) -> Option<String> {
    let s = s.trim();
    if s.len() < 2 || !s.starts_with('"') || !s.ends_with('"') {
        return None;
    }
    let inner = &s[1..s.len() - 1];
    let mut out = String::new();
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next()? {
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                '\\' => out.push('\\'),
                '"' => out.push('"'),
                '\'' => out.push('\''),
                '0' => out.push('\0'),
                'x' => {
                    let high = chars.next()?.to_digit(16)?;
                    let low = chars.next()?.to_digit(16)?;
                    let value = high * 16 + low;
                    // Rust string hex escapes are ASCII, unlike byte strings.
                    if value > 0x7f {
                        return None;
                    }
                    out.push(char::from_u32(value)?);
                }
                'u' => {
                    if chars.next()? != '{' {
                        return None;
                    }
                    let first = chars.next()?.to_digit(16)?;
                    let mut value = first;
                    let mut digits = 1;
                    loop {
                        match chars.next()? {
                            '}' => break,
                            '_' => {}
                            c => {
                                digits += 1;
                                if digits > 6 {
                                    return None;
                                }
                                value = value * 16 + c.to_digit(16)?;
                            }
                        }
                    }
                    out.push(char::from_u32(value)?);
                }
                '\n' => {
                    while chars
                        .peek()
                        .is_some_and(|c| matches!(c, ' ' | '\t' | '\n' | '\r'))
                    {
                        chars.next();
                    }
                }
                '\r' => {
                    if chars.next()? != '\n' {
                        return None;
                    }
                    while chars
                        .peek()
                        .is_some_and(|c| matches!(c, ' ' | '\t' | '\n' | '\r'))
                    {
                        chars.next();
                    }
                }
                _ => return None,
            }
        } else if c == '"' {
            return None;
        } else if c == '\r' {
            if chars.next()? != '\n' {
                return None;
            }
            out.push('\n');
        } else {
            out.push(c);
        }
    }
    Some(out)
}

pub(crate) fn is_ident_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

pub(crate) fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

pub(crate) fn is_ident_continue(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

pub(crate) fn is_hex(c: u8) -> bool {
    c.is_ascii_hexdigit()
}

/// Skip whitespace and comments. Strings and char literals stay in place.
pub(crate) fn skip_trivia(src: &str, mut i: usize, source_rel: &str) -> Result<usize, String> {
    let b = src.as_bytes();
    loop {
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if src[i..].starts_with("//") {
            i = end_of_line_comment(src, i);
            continue;
        }
        if src[i..].starts_with("/*") {
            i = end_of_block_comment(src, i, source_rel)?;
            continue;
        }
        return Ok(i);
    }
}

pub(crate) fn end_of_line_comment(src: &str, i: usize) -> usize {
    src[i..]
        .find('\n')
        .map(|rel| i + rel + 1)
        .unwrap_or(src.len())
}

pub(crate) fn end_of_block_comment(src: &str, i: usize, source_rel: &str) -> Result<usize, String> {
    let b = src.as_bytes();
    let mut k = i + 2;
    let mut depth = 1i32;
    while k + 1 < b.len() {
        if b[k] == b'/' && b[k + 1] == b'*' {
            depth += 1;
            k += 2;
            continue;
        }
        if b[k] == b'*' && b[k + 1] == b'/' {
            depth -= 1;
            k += 2;
            if depth == 0 {
                return Ok(k);
            }
            continue;
        }
        k += 1;
    }
    Err(format!("{source_rel}: unterminated block comment"))
}

pub(crate) enum RawString {
    NotRaw,
    End(usize),
    Unterminated,
    TooManyHashes,
}

pub(crate) fn classify_raw_string(src: &str, r_idx: usize) -> RawString {
    let b = src.as_bytes();
    if r_idx >= b.len() || b[r_idx] != b'r' {
        return RawString::NotRaw;
    }
    let mut k = r_idx + 1;
    let mut hashes = 0usize;
    while k < b.len() && b[k] == b'#' {
        hashes += 1;
        if hashes >= 256 {
            return RawString::TooManyHashes;
        }
        k += 1;
    }
    if k >= b.len() || b[k] != b'"' {
        return RawString::NotRaw;
    }
    k += 1;
    while k < b.len() {
        if b[k] == b'"' {
            let mut seen = 0usize;
            let mut t = k + 1;
            while seen < hashes && t < b.len() && b[t] == b'#' {
                seen += 1;
                t += 1;
            }
            if seen == hashes {
                return RawString::End(t);
            }
        }
        k += 1;
    }
    RawString::Unterminated
}

pub(crate) fn raw_string_body(src: &str, r_idx: usize, end: usize) -> String {
    let b = src.as_bytes();
    let mut k = r_idx + 1;
    let mut hashes = 0usize;
    while k < b.len() && b[k] == b'#' {
        hashes += 1;
        k += 1;
    }
    let body_start = k + 1;
    let body_end = end - hashes - 1;
    src[body_start..body_end].to_string()
}

pub(crate) fn end_of_ordinary_string(
    src: &str,
    i: usize,
    source_rel: &str,
) -> Result<usize, String> {
    let b = src.as_bytes();
    let mut k = i + 1;
    while k < b.len() {
        if b[k] == b'\\' {
            if k + 1 >= b.len() {
                return Err(format!("{source_rel}: unterminated string"));
            }
            k += 2;
            continue;
        }
        if b[k] == b'"' {
            return Ok(k + 1);
        }
        k += 1;
    }
    Err(format!("{source_rel}: unterminated string"))
}

pub(crate) fn end_of_escaped_char(src: &str, i: usize, source_rel: &str) -> Result<usize, String> {
    let b = src.as_bytes();
    let mut k = i + 2;
    if k >= b.len() {
        return Err(format!("{source_rel}: unterminated char literal"));
    }
    match b[k] {
        b'n' | b'r' | b't' | b'0' | b'\\' | b'\'' | b'"' => k += 1,
        b'x' => {
            if k + 3 > b.len() || !is_hex(b[k + 1]) || !is_hex(b[k + 2]) {
                return Err(format!("{source_rel}: unterminated char literal"));
            }
            k += 3;
        }
        b'u' => {
            k += 1;
            if b.get(k) != Some(&b'{') {
                return Err(format!("{source_rel}: unterminated char literal"));
            }
            k += 1;
            let start = k;
            while k < b.len() && b[k] != b'}' {
                if !is_hex(b[k]) {
                    return Err(format!("{source_rel}: unterminated char literal"));
                }
                k += 1;
            }
            if k >= b.len() || k == start {
                return Err(format!("{source_rel}: unterminated char literal"));
            }
            k += 1;
        }
        _ => return Err(format!("{source_rel}: unterminated char literal")),
    }
    if b.get(k) != Some(&b'\'') {
        return Err(format!("{source_rel}: unterminated char literal"));
    }
    Ok(k + 1)
}

pub(crate) fn end_of_char_or_lifetime(
    src: &str,
    i: usize,
    source_rel: &str,
) -> Result<usize, String> {
    let b = src.as_bytes();
    if i + 1 >= b.len() {
        return Err(format!("{source_rel}: unterminated char literal"));
    }
    if b[i + 1] == b'\\' {
        return end_of_escaped_char(src, i, source_rel);
    }
    let Some(first) = src[i + 1..].chars().next() else {
        return Err(format!("{source_rel}: unterminated char literal"));
    };
    let after = i + 1 + first.len_utf8();
    if b.get(after) == Some(&b'\'') {
        return Ok(after + 1);
    }
    if is_ident_start(first) {
        let mut k = after;
        while k < src.len() {
            let Some(c) = src[k..].chars().next() else {
                break;
            };
            if !is_ident_continue(c) {
                break;
            }
            k += c.len_utf8();
        }
        return Ok(k);
    }
    Err(format!("{source_rel}: unterminated char literal"))
}

/// When `i` starts a comment or literal, return the index after it.
pub(crate) fn consume_skipped(
    src: &str,
    i: usize,
    source_rel: &str,
) -> Result<Option<usize>, String> {
    let b = src.as_bytes();
    if i >= b.len() || !src.is_char_boundary(i) {
        return Ok(None);
    }
    if src[i..].starts_with("//") {
        return Ok(Some(end_of_line_comment(src, i)));
    }
    if src[i..].starts_with("/*") {
        return Ok(Some(end_of_block_comment(src, i, source_rel)?));
    }
    if b[i] == b'"' {
        return Ok(Some(end_of_ordinary_string(src, i, source_rel)?));
    }
    if b[i] == b'\'' {
        return Ok(Some(end_of_char_or_lifetime(src, i, source_rel)?));
    }
    if b[i] == b'b' {
        match b.get(i + 1).copied() {
            Some(b'\'') => return Ok(Some(end_of_char_or_lifetime(src, i + 1, source_rel)?)),
            Some(b'"') => return Ok(Some(end_of_ordinary_string(src, i + 1, source_rel)?)),
            Some(b'r') => match classify_raw_string(src, i + 1) {
                RawString::NotRaw => return Ok(None),
                RawString::End(end) => return Ok(Some(end)),
                RawString::Unterminated => {
                    return Err(format!("{source_rel}: unterminated raw byte string"));
                }
                RawString::TooManyHashes => {
                    return Err(format!("{source_rel}: raw string has 256 or more hashes"));
                }
            },
            _ => return Ok(None),
        }
    }
    if b[i] == b'r' {
        match classify_raw_string(src, i) {
            RawString::NotRaw => return Ok(None),
            RawString::End(end) => return Ok(Some(end)),
            RawString::Unterminated => {
                return Err(format!("{source_rel}: unterminated raw string"));
            }
            RawString::TooManyHashes => {
                return Err(format!("{source_rel}: raw string has 256 or more hashes"));
            }
        }
    }
    Ok(None)
}

pub(crate) fn take_string_literal(
    src: &str,
    i: usize,
    source_rel: &str,
) -> Result<Option<(String, usize)>, String> {
    let b = src.as_bytes();
    if i >= b.len() {
        return Ok(None);
    }
    if b[i] == b'r' {
        match classify_raw_string(src, i) {
            RawString::NotRaw => return Ok(None),
            RawString::End(end) => return Ok(Some((raw_string_body(src, i, end), end))),
            RawString::Unterminated => {
                return Err(format!("{source_rel}: unterminated raw string"));
            }
            RawString::TooManyHashes => {
                return Err(format!("{source_rel}: raw string has 256 or more hashes"));
            }
        }
    }
    if b[i] == b'"' {
        let end = end_of_ordinary_string(src, i, source_rel)?;
        let lit = parse_rust_string_literal(&src[i..end]).ok_or_else(|| {
            format!(
                "{source_rel}: unresolved include_str! form: {}",
                src[i..end].trim()
            )
        })?;
        return Ok(Some((lit, end)));
    }
    Ok(None)
}

/// Pair `(` with `)` and `{` with `}`. `[` is not a delimiter here.
pub(crate) fn find_matching_delimiter(
    src: &str,
    open_idx: usize,
    source_rel: &str,
) -> Result<Option<usize>, String> {
    let b = src.as_bytes();
    if open_idx >= b.len() {
        return Ok(None);
    }
    let (open, close) = match b[open_idx] {
        b'(' => (b'(', b')'),
        b'{' => (b'{', b'}'),
        _ => return Ok(None),
    };
    let mut depth = 0i32;
    let mut i = open_idx;
    while i < b.len() {
        if i != open_idx
            && let Some(next) = consume_skipped(src, i, source_rel)?
        {
            if next <= i {
                return Err(format!("{source_rel}: scanner did not advance"));
            }
            i = next;
            continue;
        }
        if b[i] == open {
            depth += 1;
        } else if b[i] == close {
            depth -= 1;
            if depth == 0 {
                return Ok(Some(i));
            }
        }
        i += 1;
    }
    Ok(None)
}

#[cfg(test)]
mod string_escape_tests {
    use super::parse_rust_string_literal;

    #[test]
    fn hex_escape_decodes_material_path() {
        assert_eq!(
            parse_rust_string_literal(r#""../templates/mess\x61ge.txt""#).as_deref(),
            Some("../templates/message.txt")
        );
    }

    #[test]
    fn unicode_escape_decodes_material_path() {
        assert_eq!(
            parse_rust_string_literal(r#""../templates/mess\u{0061}ge-\u{fc}.txt""#).as_deref(),
            Some("../templates/message-ü.txt")
        );
    }

    #[test]
    fn escaped_line_continuation_does_not_change_path() {
        assert_eq!(
            parse_rust_string_literal("\"../templates/mess\\\r\n    age.txt\"").as_deref(),
            Some("../templates/message.txt")
        );
    }

    #[test]
    fn invalid_escape_is_rejected_instead_of_changing_path() {
        assert_eq!(
            parse_rust_string_literal(r#""../templates/mess\qge.txt""#),
            None
        );
    }
}
