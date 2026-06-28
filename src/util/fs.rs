use std::fs;
use std::io;
use std::path::Path;

pub fn read_utf8_if_exists(path: &Path) -> io::Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(content) => Ok(Some(content)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

/// Read a file to a `String`, detecting common Windows encodings.
///
/// Detection order:
/// 1. UTF-16 LE BOM (`FF FE`) — decoded via UTF-16 LE.
/// 2. UTF-16 BE BOM (`FE FF`) — decoded via UTF-16 BE.
/// 3. UTF-8 BOM (`EF BB BF`) — stripped, then decoded as UTF-8.
/// 4. Bare UTF-8 — standard `String::from_utf8`.
/// 5. Lossy UTF-8 — replaces unmappable bytes with U+FFFD rather than
///    returning an error, so callers always get *some* text.
pub fn read_to_string_with_encoding(path: &Path) -> io::Result<String> {
    let bytes = fs::read(path)?;
    decode_bytes_with_encoding(&bytes)
}

/// Like `read_utf8_if_exists` but also handles UTF-16/BOM files.
pub fn read_with_encoding_if_exists(path: &Path) -> io::Result<Option<String>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(decode_bytes_with_encoding(&bytes)?)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

fn decode_bytes_with_encoding(bytes: &[u8]) -> io::Result<String> {
    // UTF-16 LE BOM: FF FE
    if bytes.starts_with(&[0xFF, 0xFE]) {
        return Ok(decode_utf16_le(&bytes[2..]));
    }

    // UTF-16 BE BOM: FE FF
    if bytes.starts_with(&[0xFE, 0xFF]) {
        return Ok(decode_utf16_be(&bytes[2..]));
    }

    // UTF-8 BOM: EF BB BF
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        let slice = &bytes[3..];
        return Ok(String::from_utf8_lossy(slice).into_owned());
    }

    // Plain UTF-8 or lossy fallback
    Ok(String::from_utf8_lossy(bytes).into_owned())
}

fn decode_utf16_le(bytes: &[u8]) -> String {
    let code_units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16_lossy(&code_units)
}

fn decode_utf16_be(bytes: &[u8]) -> String {
    let code_units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_be_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16_lossy(&code_units)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_plain_utf8() {
        let s = decode_bytes_with_encoding(b"hello world").unwrap();
        assert_eq!(s, "hello world");
    }

    #[test]
    fn decode_utf8_bom() {
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(b"hello");
        let s = decode_bytes_with_encoding(&bytes).unwrap();
        assert_eq!(s, "hello");
    }

    #[test]
    fn decode_utf16_le_bom() {
        // "hi" in UTF-16 LE: h=0x68, i=0x69
        let bytes: Vec<u8> = vec![0xFF, 0xFE, 0x68, 0x00, 0x69, 0x00];
        let s = decode_bytes_with_encoding(&bytes).unwrap();
        assert_eq!(s, "hi");
    }

    #[test]
    fn decode_utf16_be_bom() {
        // "hi" in UTF-16 BE
        let bytes: Vec<u8> = vec![0xFE, 0xFF, 0x00, 0x68, 0x00, 0x69];
        let s = decode_bytes_with_encoding(&bytes).unwrap();
        assert_eq!(s, "hi");
    }

    #[test]
    fn decode_invalid_utf8_falls_back_lossily() {
        let bytes = vec![0xFF, 0x00, 0x41]; // invalid UTF-8 sequence then 'A'
        // Should not panic, should return Some string
        let s = decode_bytes_with_encoding(&bytes).unwrap();
        assert!(!s.is_empty());
    }
}
