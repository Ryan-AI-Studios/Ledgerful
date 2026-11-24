//! Validate a conservative attribute-rule subset from already charged bytes.
//! This does not match paths, open files, inspect info/attributes or compare blobs.
use super::pack::Work;
use gix::attrs::{self, StateRef, parse::Kind};

type PatternAndAssignments<'a> = (&'a [u8], &'a [u8]);

fn trim_h(mut s: &[u8]) -> &[u8] {
    while matches!(s.first(), Some(b' ' | b'\t')) {
        s = &s[1..]
    }
    s
}
fn tokens(s: &[u8]) -> impl Iterator<Item = &[u8]> {
    s.split(|c| matches!(c, b' ' | b'\t'))
        .filter(|s| !s.is_empty())
}
fn frame(line: &[u8]) -> Result<Option<PatternAndAssignments<'_>>, &'static str> {
    if line.len() > 512
        || !line.is_ascii()
        || line.iter().any(|b| *b < 32 && !matches!(b, b'\t'))
        || line.contains(&127)
    {
        return Err("framing");
    }
    let line = trim_h(line);
    if line.is_empty() || line[0] == b'#' {
        return Ok(None);
    }
    let pos = line
        .iter()
        .position(|c| matches!(c, b' ' | b'\t'))
        .ok_or("framing")?;
    let pat = &line[..pos];
    if pat.is_empty()
        || !pat
            .iter()
            .all(|c| c.is_ascii_alphanumeric() || b"*._-".contains(c))
    {
        return Err("framing");
    }
    let a = &line[pos..];
    let n = tokens(a).count();
    if n == 0 || n > 32 {
        return Err("framing");
    }
    for t in tokens(a) {
        if !matches!(
            t,
            b"text" | b"-text" | b"!text" | b"text=auto" | b"eol=lf" | b"eol=crlf" | b"binary"
        ) {
            return Err("unsupported");
        }
    }
    Ok(Some((pat, a)))
}
fn body(raw: &[u8]) -> Result<&[u8], &'static str> {
    if raw.len() > 4096 {
        return Err("input-cap");
    }
    let b = raw.strip_prefix(b"\xef\xbb\xbf").unwrap_or(raw);
    if b.windows(3).any(|x| x == b"\xef\xbb\xbf") {
        return Err("framing");
    }
    Ok(b)
}
pub(super) fn validate_admitted_attribute_rules(
    raw: &[u8],
    work: &mut Work<'_>,
) -> Result<(), &'static str> {
    // Whole-file preflight before invoking parser; accept LF, CRLF and unterminated last line only.
    if raw.len() > 4096 {
        return Err("input-cap");
    }
    work.charge(raw.len().checked_mul(12).ok_or("work-cap")?)?;
    let bytes = body(raw)?;
    if bytes.last() == Some(&b'\r') {
        return Err("framing");
    }
    for segment in bytes.split(|c| *c == b'\n') {
        work.charge(0)?;
        let line = segment.strip_suffix(b"\r").unwrap_or(segment);
        if line.contains(&b'\r') {
            return Err("framing");
        }
        if let Some((_, a)) = frame(line)? {
            work.charge(1 + tokens(a).count())?;
        }
    }
    let mut parsed = attrs::parse(raw);
    for (idx, segment) in bytes.split(|c| *c == b'\n').enumerate() {
        work.charge(0)?;
        let line = segment.strip_suffix(b"\r").unwrap_or(segment);
        if let Some((pat, a)) = frame(line)? {
            work.charge(1 + tokens(a).count())?;
            if work.read_limit(pat.len(), 0)? != pat.len() {
                return Err("memory-cap");
            }
            let _held = work
                .memory_budget()
                .reserve(pat.len())
                .map_err(|_| "memory-cap")?;
            let (kind, mut assignments, number) =
                parsed.next().ok_or("missing")?.map_err(|_| "parser")?;
            if number != idx + 1 {
                return Err("line-map");
            }
            match kind {
                Kind::Pattern(p) if &p.text[..] == pat && p.text.capacity() <= pat.len() => (),
                _ => return Err("pattern-map"),
            }
            for tok in tokens(a) {
                let v = assignments
                    .next()
                    .ok_or("assignment-map")?
                    .map_err(|_| "parser")?;
                let matches = match tok {
                    b"text" => v.name.as_str() == "text" && matches!(v.state, StateRef::Set),
                    b"-text" => v.name.as_str() == "text" && matches!(v.state, StateRef::Unset),
                    b"!text" => {
                        v.name.as_str() == "text" && matches!(v.state, StateRef::Unspecified)
                    }
                    b"text=auto" => {
                        v.name.as_str() == "text"
                            && matches!(v.state,StateRef::Value(x) if x.as_bstr()=="auto")
                    }
                    b"eol=lf" => {
                        v.name.as_str() == "eol"
                            && matches!(v.state,StateRef::Value(x) if x.as_bstr()=="lf")
                    }
                    b"eol=crlf" => {
                        v.name.as_str() == "eol"
                            && matches!(v.state,StateRef::Value(x) if x.as_bstr()=="crlf")
                    }
                    b"binary" => v.name.as_str() == "binary" && matches!(v.state, StateRef::Set),
                    _ => false,
                };
                if !matches {
                    return Err("assignment-map");
                }
            }
            if assignments.next().is_some() {
                return Err("extra-assignment");
            }
        }
    }
    work.charge(0)?;
    if parsed.next().is_some() {
        return Err("extra-line");
    }
    work.charge(0)?;
    Ok(())
}
