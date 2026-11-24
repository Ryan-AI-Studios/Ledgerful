//! Typed Git decoders over caller-owned, bounded bytes.
use gix::hash::Kind::Sha1;
use gix::index::entry::{Flags, Stage};
use gix::objs::{Kind, ObjectRef, tree::EntryKind};
use std::borrow::Cow;

#[derive(Debug, thiserror::Error)]
pub(crate) enum DecodeError {
    #[error("repository input exceeded its byte limit")]
    Bytes,
    #[error("repository index exceeded its entry limit")]
    Entries,
    #[error("unsupported repository object format")]
    Format,
    #[error("unsupported repository object kind")]
    Kind,
    #[error("repository object hash did not match")]
    Hash,
    #[error("repository object could not be decoded")]
    Object,
    #[error("repository index could not be decoded")]
    Index,
    #[error("repository index checksum did not match")]
    IndexChecksum,
    #[error("repository index used an unsupported mode or extension")]
    IndexUnsupported,
    #[error("could not allocate a bounded repository index buffer")]
    Allocation,
}

pub(crate) fn decode_object(
    id: gix::ObjectId,
    kind: Kind,
    bytes: &[u8],
    byte_limit: usize,
) -> Result<ObjectRef<'_>, DecodeError> {
    if bytes.len() > byte_limit {
        return Err(DecodeError::Bytes);
    }
    if id.kind() != Sha1 {
        return Err(DecodeError::Format);
    }
    if kind == Kind::Tag {
        return Err(DecodeError::Kind);
    }
    if gix::objs::compute_hash(Sha1, kind, bytes).map_err(|_| DecodeError::Hash)? != id {
        return Err(DecodeError::Hash);
    }
    ObjectRef::from_bytes(bytes, kind, Sha1).map_err(|_| DecodeError::Object)
}

pub(crate) fn decode_index(
    bytes: &[u8],
    byte_limit: usize,
    entry_limit: usize,
) -> Result<gix::index::State, DecodeError> {
    let (admitted, count) = admit_index(bytes, byte_limit, entry_limit)?;
    let stamp = gix::index::State::new(Sha1).timestamp();
    let (index, _) = gix::index::State::from_bytes(
        admitted.as_ref(),
        stamp,
        Sha1,
        gix::index::decode::Options {
            thread_limit: Some(1),
            alloc_limit_bytes: Some(byte_limit),
            ..Default::default()
        },
    )
    .map_err(|_| DecodeError::Index)?;
    if index.link().is_some() || index.is_sparse() || index.entries().len() != count {
        return Err(DecodeError::IndexUnsupported);
    }
    index
        .verify_entries()
        .map_err(|_| DecodeError::IndexUnsupported)?;

    let unsupported = Flags::SKIP_WORKTREE | Flags::ASSUME_VALID | Flags::INTENT_TO_ADD;
    if index.entries().iter().any(|entry| {
        entry.flags.intersects(unsupported)
            || entry.stage() != Stage::Unconflicted
            || !entry.mode.to_tree_entry_mode().is_some_and(|mode| {
                matches!(mode.kind(), EntryKind::Blob | EntryKind::BlobExecutable)
            })
    }) {
        return Err(DecodeError::IndexUnsupported);
    }
    Ok(index)
}

fn index_checksum(body: &[u8]) -> Result<gix::ObjectId, DecodeError> {
    let mut hasher = gix::hash::hasher(Sha1);
    hasher.update(body);
    hasher
        .try_finalize()
        .map_err(|_| DecodeError::IndexChecksum)
}

fn be32(bytes: &[u8], at: usize) -> Result<u32, DecodeError> {
    bytes
        .get(at..at + 4)
        .ok_or(DecodeError::Index)?
        .try_into()
        .map(u32::from_be_bytes)
        .map_err(|_| DecodeError::Index)
}

fn index_entry_end(bytes: &[u8], start: usize, version: u32) -> Result<usize, DecodeError> {
    const FIXED: usize = 62;
    const MAX_PATH: usize = 4_096;
    let fixed = bytes.get(start..start + FIXED).ok_or(DecodeError::Index)?;
    if !matches!(be32(fixed, 24)?, 0o100644 | 0o100755) {
        return Err(DecodeError::IndexUnsupported);
    }
    let flags = u16::from_be_bytes([fixed[60], fixed[61]]);
    // The pinned saturated-path branch does not consume V2/V3 entry padding.
    if flags & 0xb000 != 0 || flags & 0x0fff == 0x0fff {
        return Err(DecodeError::IndexUnsupported);
    }
    let name = if flags & 0x4000 == 0 {
        start + FIXED
    } else {
        if version != 3 || bytes.get(start + FIXED..start + FIXED + 2) != Some(&[0, 0]) {
            return Err(DecodeError::IndexUnsupported);
        }
        start + FIXED + 2
    };
    let window = bytes.get(name..).ok_or(DecodeError::Index)?;
    let len = window
        .iter()
        .take(MAX_PATH + 1)
        .position(|byte| *byte == 0)
        .ok_or(DecodeError::IndexUnsupported)?;
    if len == 0 || flags & 0x0fff != (len.min(0x0fff) as u16) {
        return Err(DecodeError::Index);
    }
    let terminated = name + len + 1;
    let padding = (8 - (terminated - start) % 8) % 8;
    if !bytes
        .get(terminated..terminated + padding)
        .ok_or(DecodeError::Index)?
        .iter()
        .all(|byte| *byte == 0)
    {
        return Err(DecodeError::Index);
    }
    Ok(terminated + padding)
}

fn admit_index(
    bytes: &[u8],
    byte_limit: usize,
    entry_limit: usize,
) -> Result<(Cow<'_, [u8]>, usize), DecodeError> {
    if bytes.len() > byte_limit {
        return Err(DecodeError::Bytes);
    }
    let body_len = bytes
        .len()
        .checked_sub(Sha1.len_in_bytes())
        .ok_or(DecodeError::Index)?;
    let body = bytes.get(..body_len).ok_or(DecodeError::Index)?;
    if index_checksum(body)?.as_bytes() != &bytes[body_len..] {
        return Err(DecodeError::IndexChecksum);
    }
    if !body.starts_with(b"DIRC") {
        return Err(DecodeError::Index);
    }
    let version = be32(body, 4)?;
    if !matches!(version, 2 | 3) {
        return Err(DecodeError::IndexUnsupported);
    }
    let count = be32(body, 8)? as usize;
    if count > entry_limit {
        return Err(DecodeError::Entries);
    }
    let mut entries_end = 12;
    for _ in 0..count {
        entries_end = index_entry_end(body, entries_end, version)?;
    }
    if entries_end == body.len() {
        return Ok((Cow::Borrowed(bytes), count));
    }
    let mut cursor = entries_end;
    let mut admitted = std::collections::BTreeSet::new();
    while cursor < body.len() {
        let id = body.get(cursor..cursor + 4).ok_or(DecodeError::Index)?;
        if !matches!(
            id,
            b"TREE" | b"REUC" | b"UNTR" | b"FSMN" | b"EOIE" | b"IEOT"
        ) || !admitted.insert(id)
        {
            return Err(DecodeError::IndexUnsupported);
        }
        let size = be32(body, cursor + 4)? as usize;
        cursor = (cursor + 8).checked_add(size).ok_or(DecodeError::Index)?;
        if cursor > body.len() {
            return Err(DecodeError::Index);
        }
    }
    // Pass the exact entries on to gix with a freshly checksummed envelope.
    // The original index was checksummed first. No extension payload reaches
    // gix, including the recursive TREE cache.
    let entries = &body[..entries_end];
    let mut sanitized = Vec::new();
    sanitized
        .try_reserve_exact(entries.len() + Sha1.len_in_bytes())
        .map_err(|_| DecodeError::Allocation)?;
    sanitized.extend_from_slice(entries);
    sanitized.extend_from_slice(index_checksum(entries)?.as_bytes());
    Ok((Cow::Owned(sanitized), count))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seal(mut index: Vec<u8>) -> Vec<u8> {
        let checksum = index_checksum(&index).unwrap();
        index.extend_from_slice(checksum.as_bytes());
        index
    }

    fn header(version: u32, count: u32) -> Vec<u8> {
        let mut bytes = b"DIRC".to_vec();
        bytes.extend_from_slice(&version.to_be_bytes());
        bytes.extend_from_slice(&count.to_be_bytes());
        bytes
    }

    fn empty_index(count: u32) -> Vec<u8> {
        seal(header(2, count))
    }

    fn one_entry(version: u32, mode: u32) -> Vec<u8> {
        let mut bytes = header(version, 1);
        let mut fixed = [0; 62];
        fixed[24..28].copy_from_slice(&mode.to_be_bytes());
        fixed[60..62].copy_from_slice(&3u16.to_be_bytes());
        bytes.extend_from_slice(&fixed);
        bytes.extend_from_slice(b"src\0");
        bytes.extend_from_slice(&[0; 6]);
        bytes
    }

    fn append_extension(bytes: &mut Vec<u8>, name: &[u8; 4], payload: &[u8]) {
        bytes.extend_from_slice(name);
        bytes.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        bytes.extend_from_slice(payload);
    }

    #[test]
    fn decodes_verified_object_bytes_with_a_bound() {
        let raw = b"exact \r\n";
        let id = gix::objs::compute_hash(Sha1, Kind::Blob, raw).unwrap();
        assert!(matches!(
            decode_object(id, Kind::Blob, raw, raw.len()),
            Ok(ObjectRef::Blob(blob)) if blob.data == raw
        ));
        assert!(matches!(
            decode_object(id, Kind::Blob, raw, raw.len() - 1),
            Err(DecodeError::Bytes)
        ));
        assert!(matches!(
            decode_object(id, Kind::Tree, raw, raw.len()),
            Err(DecodeError::Hash)
        ));
        assert!(matches!(
            decode_object(id, Kind::Tag, raw, raw.len()),
            Err(DecodeError::Kind)
        ));
    }

    #[test]
    fn index_bound_and_checksum_are_enforced() {
        let mut data = empty_index(0);
        assert!(decode_index(&data, 1_024, 10).unwrap().entries().is_empty());
        assert!(matches!(
            decode_index(&data, 4, 10),
            Err(DecodeError::Bytes)
        ));
        assert!(matches!(
            decode_index(&empty_index(4097), 1_024, 4096),
            Err(DecodeError::Entries)
        ));
        *data.last_mut().unwrap() ^= 1;
        assert!(matches!(
            decode_index(&data, 1_024, 10),
            Err(DecodeError::IndexChecksum)
        ));
    }

    #[test]
    fn admits_nonempty_ordinary_v2_and_v3_entries() {
        for (version, mode) in [(2, 0o100644), (3, 0o100755)] {
            let index = seal(one_entry(version, mode));
            let state = decode_index(&index, 1024, 10).unwrap();
            assert_eq!(state.entries().len(), 1);
            assert_eq!(state.entries()[0].path(&state), "src");
        }
    }

    #[test]
    fn original_checksum_checked_and_optional_payload_discarded() {
        let mut body = one_entry(2, 0o100644);
        append_extension(&mut body, b"TREE", &b"n\0-1 1\n".repeat(512));
        let valid = seal(body);
        let (admitted, count) = admit_index(&valid, 8192, 10).unwrap();
        assert_eq!(count, 1);
        assert_eq!(admitted.as_ref(), seal(one_entry(2, 0o100644)));
        assert_eq!(decode_index(&valid, 8192, 10).unwrap().entries().len(), 1);
        let mut damaged = valid;
        damaged[90] ^= 1;
        assert!(matches!(
            decode_index(&damaged, 8192, 10),
            Err(DecodeError::IndexChecksum)
        ));
    }

    #[test]
    fn refuses_v4_unknown_and_required_extensions_or_truncated_framing() {
        assert!(matches!(
            decode_index(&seal(header(4, 0)), 1024, 10),
            Err(DecodeError::IndexUnsupported)
        ));
        for id in [b"link", b"sdir", b"NEWX"] {
            let mut body = header(2, 0);
            append_extension(&mut body, id, b"test");
            assert!(matches!(
                decode_index(&seal(body), 1024, 10),
                Err(DecodeError::IndexUnsupported)
            ));
        }
        let mut body = header(2, 0);
        body.extend_from_slice(b"TREE\0\0\0\x05abc");
        assert!(matches!(
            decode_index(&seal(body), 1024, 10),
            Err(DecodeError::Index)
        ));
    }

    #[test]
    fn refuses_noncanonical_mode_and_terminator_padding() {
        assert!(matches!(
            decode_index(&seal(one_entry(2, 0x0100_81a4)), 1024, 10),
            Err(DecodeError::IndexUnsupported)
        ));
        let mut body = one_entry(2, 0o100644);
        *body.last_mut().unwrap() = 1;
        assert!(matches!(
            decode_index(&seal(body), 1024, 10),
            Err(DecodeError::Index)
        ));
    }
}
