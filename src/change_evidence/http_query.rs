//! Pure, bounded parser for the protected change-evidence query.
//!
//! The HTTP adapter should pass the raw query bytes without the leading `?`.
//! This module has no request, response, logging, or storage access.

use crate::change_evidence::git::exact_oid;
use crate::state::storage::baselines::validate_baseline_name;
use gix::ObjectId;
use uuid::Uuid;

const MAX_RAW_QUERY_BYTES: usize = 512;
const MAX_PAIRS: usize = 5;
const MAX_DIGEST_BYTES: usize = 71; // `blake3:` + 64 lowercase hex digits.

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum QueryError {
    InvalidQuery,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct ChangeEvidenceQuery {
    pub(crate) repo_scope: Uuid,
    pub(crate) head: ObjectId,
    pub(crate) base: ObjectId,
    pub(crate) baseline_name: Option<String>,
    pub(crate) baseline_digest: Option<String>,
}

/// Parses an RFC 3986-style raw query with form-compatible `+` handling.
///
/// All decoded values have small field-specific limits; malformed escapes,
/// invalid UTF-8, duplicate/unknown keys, and incomplete selectors collapse to
/// one static error without retaining or formatting request data.
pub(crate) fn parse_change_evidence_query(raw: &[u8]) -> Result<ChangeEvidenceQuery, QueryError> {
    if raw.is_empty() || raw.len() > MAX_RAW_QUERY_BYTES {
        return Err(QueryError::InvalidQuery);
    }

    let mut repo_scope = None;
    let mut head = None;
    let mut base = None;
    let mut baseline_name = None;
    let mut baseline_digest = None;
    let mut pair_count = 0usize;

    for pair in raw.split(|b| *b == b'&') {
        pair_count += 1;
        if pair_count > MAX_PAIRS || pair.is_empty() {
            return Err(QueryError::InvalidQuery);
        }
        let Some(eq) = pair.iter().position(|b| *b == b'=') else {
            return Err(QueryError::InvalidQuery);
        };
        let key = decode_component(&pair[..eq], 32)?;
        let value = &pair[eq + 1..];
        match key.as_slice() {
            b"repoScope" => {
                if repo_scope.is_some() {
                    return Err(QueryError::InvalidQuery);
                }
                let value = decode_utf8(value, 36)?;
                let parsed = Uuid::parse_str(&value).map_err(|_| QueryError::InvalidQuery)?;
                if parsed.to_string() != value {
                    return Err(QueryError::InvalidQuery);
                }
                repo_scope = Some(parsed);
            }
            b"head" => {
                if head.is_some() {
                    return Err(QueryError::InvalidQuery);
                }
                head = Some(
                    exact_oid(&decode_component(value, 40)?)
                        .map_err(|_| QueryError::InvalidQuery)?,
                );
            }
            b"base" => {
                if base.is_some() {
                    return Err(QueryError::InvalidQuery);
                }
                base = Some(
                    exact_oid(&decode_component(value, 40)?)
                        .map_err(|_| QueryError::InvalidQuery)?,
                );
            }
            b"baselineName" => {
                if baseline_name.is_some() {
                    return Err(QueryError::InvalidQuery);
                }
                let value = decode_utf8(value, 64)?;
                validate_baseline_name(&value).map_err(|_| QueryError::InvalidQuery)?;
                baseline_name = Some(value);
            }
            b"baselineDigest" => {
                if baseline_digest.is_some() {
                    return Err(QueryError::InvalidQuery);
                }
                let value = decode_utf8(value, MAX_DIGEST_BYTES)?;
                if !valid_digest(value.as_bytes()) {
                    return Err(QueryError::InvalidQuery);
                }
                baseline_digest = Some(value);
            }
            _ => return Err(QueryError::InvalidQuery),
        }
    }

    if baseline_name.is_some() != baseline_digest.is_some() {
        return Err(QueryError::InvalidQuery);
    }

    Ok(ChangeEvidenceQuery {
        repo_scope: repo_scope.ok_or(QueryError::InvalidQuery)?,
        head: head.ok_or(QueryError::InvalidQuery)?,
        base: base.ok_or(QueryError::InvalidQuery)?,
        baseline_name,
        baseline_digest,
    })
}

fn valid_digest(raw: &[u8]) -> bool {
    raw.len() == MAX_DIGEST_BYTES
        && raw.starts_with(b"blake3:")
        && raw[7..]
            .iter()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
}

fn decode_utf8(raw: &[u8], max_bytes: usize) -> Result<String, QueryError> {
    String::from_utf8(decode_component(raw, max_bytes)?).map_err(|_| QueryError::InvalidQuery)
}

fn decode_component(raw: &[u8], max_bytes: usize) -> Result<Vec<u8>, QueryError> {
    let mut decoded = Vec::with_capacity(raw.len().min(max_bytes));
    let mut i = 0;
    while i < raw.len() {
        let byte = match raw[i] {
            b'+' => b' ',
            b'%' => {
                if i + 2 >= raw.len() {
                    return Err(QueryError::InvalidQuery);
                }
                let hi = hex(raw[i + 1]).ok_or(QueryError::InvalidQuery)?;
                let lo = hex(raw[i + 2]).ok_or(QueryError::InvalidQuery)?;
                i += 2;
                (hi << 4) | lo
            }
            b => b,
        };
        if decoded.len() == max_bytes {
            return Err(QueryError::InvalidQuery);
        }
        decoded.push(byte);
        i += 1;
    }
    Ok(decoded)
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CORE: &str = "repoScope=4826be00-5419-4a5a-9b51-27c0383dad03&head=0123456789abcdef0123456789abcdef01234567&base=abcdef0123456789abcdef0123456789abcdef01";
    const DIGEST: &str = "blake3:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn parse(q: &str) -> Result<ChangeEvidenceQuery, QueryError> {
        parse_change_evidence_query(q.as_bytes())
    }

    #[test]
    fn parses_exact_required_key_and_optional_complete_selector() {
        let q = parse(CORE).unwrap();
        assert_eq!(
            q.repo_scope.to_string(),
            "4826be00-5419-4a5a-9b51-27c0383dad03"
        );
        assert!(q.baseline_name.is_none());
        let q = parse(&format!(
            "{CORE}&baselineName=base-1&baselineDigest={DIGEST}"
        ))
        .unwrap();
        assert_eq!(q.baseline_name.as_deref(), Some("base-1"));
        assert_eq!(q.baseline_digest.as_deref(), Some(DIGEST));
    }

    #[test]
    fn rejects_duplicate_unknown_missing_and_partial_fields() {
        for q in [
            "repoScope=x",
            "",
            "repoScope=x&repoScope=y",
            "repoScope=x&tool=git",
            "repoScope=x&head=1",
            "repoScope=x&baselineName=a",
            "repoScope=x&baselineDigest=x",
            &format!("{CORE}&head=0123456789abcdef0123456789abcdef01234567"),
        ] {
            assert_eq!(parse(q), Err(QueryError::InvalidQuery), "{q}");
        }
    }

    #[test]
    fn rejects_bad_oid_name_digest_utf8_and_escape_syntax() {
        for q in [
            "repoScope=4826be00-5419-4a5a-9b51-27c0383dad03&head=0123456789ABCDEF0123456789abcdef01234567&base=abcdef0123456789abcdef0123456789abcdef01",
            &format!("{CORE}&baselineName=Upper&baselineDigest={DIGEST}"),
            &format!("{CORE}&baselineName=base&baselineDigest=blake3:BAD"),
            &format!("{CORE}%"),
            &format!("{CORE}&baselineName=%FF&baselineDigest={DIGEST}"),
        ] {
            assert_eq!(parse(q), Err(QueryError::InvalidQuery), "{q}");
        }
    }

    #[test]
    fn enforces_raw_and_decoded_field_bounds() {
        assert_eq!(
            parse_change_evidence_query(&vec![b'a'; MAX_RAW_QUERY_BYTES + 1]),
            Err(QueryError::InvalidQuery)
        );
        assert_eq!(
            parse(&format!("{CORE}&baselineName={}", "a".repeat(65))),
            Err(QueryError::InvalidQuery)
        );
        let selector = format!(
            "{CORE}&baselineName={}&baselineDigest={DIGEST}",
            "a".repeat(64)
        );
        assert!(parse(&selector).is_ok());

        // Each percent escape adds two raw bytes without changing the decoded
        // value. Choose an even starting length to reach the exact ceiling.
        let selector = selector.replacen(&"a".repeat(64), &"a".repeat(63), 1);
        let encode = |mut escapes| {
            use std::fmt::Write;
            let mut encoded = String::new();
            for byte in selector.bytes() {
                if escapes > 0 && byte.is_ascii_alphanumeric() {
                    write!(&mut encoded, "%{byte:02x}").unwrap();
                    escapes -= 1;
                } else {
                    encoded.push(char::from(byte));
                }
            }
            encoded
        };
        let escape_count = (MAX_RAW_QUERY_BYTES - selector.len()) / 2;
        let at_limit = encode(escape_count);
        assert_eq!(at_limit.len(), MAX_RAW_QUERY_BYTES);
        assert!(parse(&at_limit).is_ok());
        let beyond_limit = encode(escape_count + 1);
        assert_eq!(beyond_limit.len(), MAX_RAW_QUERY_BYTES + 2);
        assert_eq!(parse(&beyond_limit), Err(QueryError::InvalidQuery));
    }
}
