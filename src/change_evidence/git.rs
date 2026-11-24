//! Read exact refs and loose objects from the retained checkout.
use super::buffer::{Reservation, ReservedBytes};
use super::contained::{ContainedRoot, ReadBudget, ReadError};
use super::pack::{Work, verify_hash};
use gix::ObjectId;
use gix::bstr::ByteSlice;
use gix::features::zlib::{Decompress, FlushDecompress, Status};
use gix::hash::Kind::Sha1;
use gix::objs::{CommitRefIter, Kind, TreeRefIter, commit::ref_iter::Token, tree::EntryRef};

type GitResult<T> = Result<T, &'static str>;
pub(super) const MAX_COMMIT_BYTES: usize = 16 * 1024;
pub(super) const MAX_TREE_BYTES: usize = 1024 * 1024;
const MAX_TREE_ENTRIES: usize = 4_096;
const MAX_TREE_NAME_BYTES: usize = 1_024;
const MAX_REF: usize = 512;
const MAX_PACKED_REFS: usize = 512 * 1024;
const MAX_REF_LINES: usize = 4096;
const MAX_HOPS: usize = 8;
const MAX_REF_INPUTS: usize = MAX_HOPS + 9;
const MAX_COMPRESSED: usize = 8 * 1024 * 1024;
const CHUNK: usize = 4096;

pub(super) struct CommitFacts {
    pub(super) tree: ObjectId,
    pub(super) only_parent: Option<ObjectId>,
}

/// Project complete, already object-ID-verified commit bytes to bounded facts.
pub(super) fn decode_commit_facts(raw: &[u8], work: &mut Work<'_>) -> GitResult<CommitFacts> {
    if raw.len() > MAX_COMMIT_BYTES {
        return Err("commit-byte-cap");
    }
    // The streaming decoder holds at most one owned multiline header at a time.
    work.reserve(raw.len(), 0)?;
    let _scratch = work
        .memory_budget()
        .reserve(raw.len())
        .map_err(|_| "memory-cap")?;
    work.charge(raw.len())?;

    let mut tokens = CommitRefIter::from_bytes(raw, Sha1);
    let Some(Token::Tree { id: tree }) = tokens.next().transpose().map_err(|_| "invalid-commit")?
    else {
        return Err("invalid-commit");
    };
    if tree.is_null() {
        return Err("invalid-commit");
    }
    let mut parent = None;
    let mut parent_count = 0;
    let mut saw_message = false;
    for token in tokens {
        match token.map_err(|_| "invalid-commit")? {
            Token::Parent { id } => {
                if id.is_null() {
                    return Err("invalid-commit");
                }
                parent_count += 1;
                parent = Some(id);
            }
            Token::ExtraHeader((name, _))
                if matches!(
                    name.as_bytes(),
                    b"tree" | b"parent" | b"author" | b"committer" | b"encoding"
                ) =>
            {
                return Err("invalid-commit");
            }
            Token::Message(_) => saw_message = true,
            _ => {}
        }
    }
    if !saw_message {
        return Err("invalid-commit");
    }
    work.charge(0)?;
    Ok(CommitFacts {
        tree,
        only_parent: if parent_count == 1 { parent } else { None },
    })
}

/// Visit provisional entries of one verified tree. Only a complete walk succeeds.
pub(super) fn visit_tree_entries<'a>(
    raw: &[u8],
    work: &mut Work<'a>,
    mut visit: impl FnMut(EntryRef<'_>, &mut Work<'a>) -> GitResult<()>,
) -> GitResult<usize> {
    if raw.len() > MAX_TREE_BYTES {
        return Err("tree-byte-cap");
    }
    work.charge(raw.len())?;
    let mut entries = TreeRefIter::from_bytes(raw, Sha1);
    let mut offset = 0;
    let mut count = 0;
    let mut previous = None;
    while offset < raw.len() {
        if count == MAX_TREE_ENTRIES {
            return Err("tree-entry-cap");
        }
        work.charge(1)?;
        let record = &raw[offset..];
        let mode_bytes = if record.starts_with(b"40000 ") {
            6
        } else if record.starts_with(b"100644 ") || record.starts_with(b"100755 ") {
            7
        } else {
            return Err("unsupported-tree-mode");
        };
        let entry = entries
            .next()
            .ok_or("invalid-tree")?
            .map_err(|_| "invalid-tree")?;
        let name_bytes = entry.filename.len();
        if name_bytes == 0 || name_bytes > MAX_TREE_NAME_BYTES || entry.oid.is_null() {
            return Err("invalid-tree-entry");
        }
        let end = entries.offset_to_next_entry(raw);
        if end > raw.len() || end != offset + mode_bytes + name_bytes + 1 + Sha1.len_in_bytes() {
            return Err("invalid-tree");
        }
        if previous.is_some_and(|prior| prior >= entry) {
            return Err("tree-entry-order");
        }
        previous = Some(entry);
        visit(entry, work)?;
        offset = end;
        count += 1;
    }
    work.charge(0)?;
    Ok(count)
}

pub(crate) fn read_error(error: ReadError) -> &'static str {
    match error {
        ReadError::Redirect => "redirect",
        ReadError::Kind => "nonregular",
        ReadError::Path => "unsafe-path",
        ReadError::Bytes => "stored-bytes-cap",
        ReadError::Aggregate => "aggregate-cap",
        ReadError::Memory => "memory-cap",
        ReadError::Deadline | ReadError::Interrupted => "deadline",
        ReadError::Changed => "input-changed",
        _ => "read-unavailable",
    }
}

pub(super) fn optional<'a>(
    root: &ContainedRoot,
    path: &str,
    limit: usize,
    budget: &mut ReadBudget<'_>,
    work: &mut Work<'a>,
) -> GitResult<Option<ReservedBytes<'a>>> {
    work.charge(1)?;
    let capacity = work.read_limit(limit, 0)?;
    match root.read_reserved(path, limit, budget, work.memory_budget(), capacity) {
        Ok(raw) => {
            work.charge(raw.as_ref().len())?;
            Ok(Some(raw))
        }
        Err(ReadError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(read_error(error)),
    }
}

#[derive(Default)]
pub(crate) struct ObservedRefs<'a> {
    inputs: Vec<ObservedRef<'a>>,
    _metadata: Option<Reservation<'a>>,
}

struct ObservedRef<'a> {
    path: String,
    limit: usize,
    raw: Option<ReservedBytes<'a>>,
}

impl<'a> ObservedRefs<'a> {
    fn read(
        &mut self,
        root: &ContainedRoot,
        path: String,
        limit: usize,
        budget: &mut ReadBudget<'_>,
        work: &mut Work<'a>,
    ) -> GitResult<Option<&[u8]>> {
        let _lookup = work.reserve_lookup(path.len())?;
        let raw = optional(root, &path, limit, budget, work)?;
        self.inputs.push(ObservedRef { path, limit, raw });
        Ok(self
            .inputs
            .last()
            .and_then(|entry| entry.raw.as_ref().map(AsRef::as_ref)))
    }

    pub(crate) fn confirm(
        &self,
        root: &ContainedRoot,
        budget: &mut ReadBudget<'_>,
        work: &mut Work<'a>,
    ) -> GitResult<()> {
        for entry in &self.inputs {
            let _lookup = work.reserve_lookup(entry.path.len())?;
            let current = optional(root, &entry.path, entry.limit, budget, work)?;
            work.charge(entry.raw.as_ref().map_or(0, |raw| raw.as_ref().len()))?;
            if current.as_ref().map(AsRef::as_ref) != entry.raw.as_ref().map(AsRef::as_ref) {
                return Err("refs-changed");
            }
        }
        work.charge(0)
    }

    pub(super) fn confirm_same_inputs(&self, other: &Self, work: &mut Work<'a>) -> GitResult<()> {
        if self.inputs.len() != other.inputs.len() {
            return Err("refs-changed");
        }
        for (first, second) in self.inputs.iter().zip(&other.inputs) {
            let left = first.raw.as_ref().map(AsRef::as_ref);
            let right = second.raw.as_ref().map(AsRef::as_ref);
            work.charge(
                first.path.len()
                    + second.path.len()
                    + left.map_or(0, <[u8]>::len)
                    + right.map_or(0, <[u8]>::len)
                    + 1,
            )?;
            if first.path != second.path || first.limit != second.limit || left != right {
                return Err("refs-changed");
            }
        }
        work.charge(0)
    }
}

pub(crate) fn exact_oid(raw: &[u8]) -> GitResult<ObjectId> {
    if raw.len() != 40
        || !raw
            .iter()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(c))
    {
        return Err("bad-oid");
    }
    ObjectId::from_hex(raw).map_err(|_| "bad-oid")
}

fn validate_branch(name: &str) -> GitResult<()> {
    let tail = name.strip_prefix("refs/heads/").ok_or("unsafe-ref")?;
    if name.len() > MAX_REF
        || tail.is_empty()
        || !name.is_ascii()
        || gix::refs::FullName::try_from(name).is_err()
    {
        return Err("unsafe-ref");
    }
    Ok(())
}

enum Target {
    Oid(ObjectId),
    Symbolic(String),
}

fn parse_target(bytes: &[u8]) -> GitResult<Target> {
    let line = bytes
        .strip_suffix(b"\r\n")
        .or_else(|| bytes.strip_suffix(b"\n"))
        .unwrap_or(bytes);
    if let Some(name) = line.strip_prefix(b"ref: ") {
        let name = std::str::from_utf8(name).map_err(|_| "unsafe-ref")?;
        validate_branch(name)?;
        Ok(Target::Symbolic(name.into()))
    } else {
        Ok(Target::Oid(exact_oid(line)?))
    }
}

fn packed_ref(
    bytes: &[u8],
    name: Option<&str>,
    budget: &ReadBudget<'_>,
) -> GitResult<Option<ObjectId>> {
    if bytes
        .split(|byte| *byte == b'\n')
        .take(MAX_REF_LINES + 1)
        .count()
        > MAX_REF_LINES
    {
        return Err("packed-ref-count");
    }
    let buffer =
        gix::refs::packed::Buffer::from_bytes(bytes, Sha1).map_err(|_| "bad-packed-refs")?;
    let mut selected = None;
    let mut previous = None;
    for entry in buffer.iter().map_err(|_| "bad-packed-refs")? {
        budget.checkpoint().map_err(read_error)?;
        let entry = entry.map_err(|_| "bad-packed-refs")?;
        let raw_name = entry.name.as_bstr();
        if previous.is_some_and(|prev| prev >= raw_name) {
            return Err("bad-packed-refs");
        }
        previous = Some(raw_name);
        if raw_name.starts_with(b"refs/replace/") {
            return Err("indirect-store");
        }
        let target = exact_oid(entry.target)?;
        if name.is_some_and(|name| raw_name == name) {
            if entry.object.is_some() {
                return Err("bad-packed-refs");
            }
            selected = Some(target);
        }
    }
    Ok(selected)
}

pub(crate) fn resolve_head<'a>(
    root: &ContainedRoot,
    budget: &mut ReadBudget<'_>,
    work: &mut Work<'a>,
) -> GitResult<(ObjectId, ObservedRefs<'a>)> {
    resolve_head_with_inputs(root, budget, work, ObservedRefs::default())
}

pub(super) fn resolve_paired_head<'a>(
    root: &ContainedRoot,
    budget: &mut ReadBudget<'_>,
    work: &mut Work<'a>,
) -> GitResult<(ObjectId, ObservedRefs<'a>)> {
    // Keep the retained frame and name allowance while the other sample is read.
    let metadata_bytes =
        MAX_REF_INPUTS * (size_of::<ObservedRef<'a>>() + 2 * (MAX_REF + ".git/".len()));
    // Existing symbolic parsing and hop detection own temporary names.
    let scratch_bytes = (MAX_HOPS + 2) * (2 * MAX_REF + 2 * size_of::<String>());
    let total = metadata_bytes + scratch_bytes;
    work.charge(total)?;
    if work.read_limit(total, 0)? != total {
        return Err("memory-cap");
    }
    let memory = work.memory_budget();
    let metadata = memory.reserve(metadata_bytes).map_err(|_| "memory-cap")?;
    let _scratch = memory.reserve(scratch_bytes).map_err(|_| "memory-cap")?;
    let mut inputs = Vec::new();
    inputs
        .try_reserve_exact(MAX_REF_INPUTS)
        .map_err(|_| "allocation")?;
    if inputs.capacity() != MAX_REF_INPUTS {
        return Err("memory-cap");
    }
    resolve_head_with_inputs(
        root,
        budget,
        work,
        ObservedRefs {
            inputs,
            _metadata: Some(metadata),
        },
    )
}

fn resolve_head_with_inputs<'a>(
    root: &ContainedRoot,
    budget: &mut ReadBudget<'_>,
    work: &mut Work<'a>,
    mut observed: ObservedRefs<'a>,
) -> GitResult<(ObjectId, ObservedRefs<'a>)> {
    for marker in [
        ".git/commondir",
        ".git/gitdir",
        ".git/config.worktree",
        ".git/info/grafts",
        ".git/objects/info/alternates",
        ".git/objects/info/http-alternates",
        ".git/refs/replace",
    ] {
        match observed.read(root, marker.into(), 0, budget, work) {
            Ok(None) => {}
            Ok(Some(_)) | Err("stored-bytes-cap" | "nonregular") => return Err("indirect-store"),
            Err(error) => return Err(error),
        }
    }
    let mut target = parse_target(
        observed
            .read(root, ".git/HEAD".into(), MAX_REF, budget, work)?
            .ok_or("missing-head")?,
    )?;
    let mut names = Vec::new();
    for _ in 0..=MAX_HOPS {
        match target {
            Target::Oid(head) => {
                if let Some(packed) = observed.read(
                    root,
                    ".git/packed-refs".into(),
                    MAX_PACKED_REFS,
                    budget,
                    work,
                )? {
                    packed_ref(packed, None, budget)?;
                }
                return Ok((head, observed));
            }
            Target::Symbolic(name) => {
                if names.len() >= MAX_HOPS || names.contains(&name) {
                    return Err("ref-hop-limit");
                }
                names.push(name.clone());
                if let Some(bytes) =
                    observed.read(root, format!(".git/{name}"), MAX_REF, budget, work)?
                {
                    target = parse_target(bytes)?;
                } else {
                    let packed = observed
                        .read(
                            root,
                            ".git/packed-refs".into(),
                            MAX_PACKED_REFS,
                            budget,
                            work,
                        )?
                        .ok_or("missing-ref")?;
                    let selected = packed_ref(packed, Some(&name), budget)?.ok_or("missing-ref")?;
                    return Ok((selected, observed));
                }
            }
        }
    }
    Err("ref-hop-limit")
}

struct Inflate<'a> {
    codec: Decompress,
    remaining: &'a [u8],
    finished: bool,
}

impl Inflate<'_> {
    fn pump(
        &mut self,
        output: &mut [u8],
        work: &mut Work<'_>,
        budget: &ReadBudget<'_>,
    ) -> GitResult<usize> {
        budget.checkpoint().map_err(read_error)?;
        let window = work.inflate_window()?;
        let in_limit = self.remaining.len().min(window);
        let out_limit = output.len().min(window);
        let before = (self.codec.total_in(), self.codec.total_out());
        let status = self
            .codec
            .decompress(
                &self.remaining[..in_limit],
                &mut output[..out_limit],
                FlushDecompress::None,
            )
            .map_err(|_| "bad-zlib")?;
        let consumed = usize::try_from(self.codec.total_in() - before.0).map_err(|_| "bad-zlib")?;
        let produced =
            usize::try_from(self.codec.total_out() - before.1).map_err(|_| "bad-zlib")?;
        if consumed > in_limit || produced > out_limit {
            return Err("bad-zlib");
        }
        work.charge(consumed + produced)?;
        self.remaining = &self.remaining[consumed..];
        self.finished = status == Status::StreamEnd;
        if self.finished && !self.remaining.is_empty() {
            return Err("trailing-zlib");
        }
        if consumed == 0 && produced == 0 && !self.finished {
            return Err("truncated-zlib");
        }
        Ok(produced)
    }
}

pub(crate) fn read_loose<'a>(
    root: &ContainedRoot,
    id: ObjectId,
    expected: Kind,
    byte_limit: usize,
    work: &mut Work<'a>,
    budget: &mut ReadBudget<'_>,
) -> GitResult<ReservedBytes<'a>> {
    if id.kind() != Sha1 {
        return Err("bad-oid");
    }
    let hex = id.to_hex().to_string();
    let (fanout, leaf) = hex.split_at(2);
    let capacity_limit = work.read_limit(usize::MAX, 0)?;
    let compressed = match root.read_reserved(
        &format!(".git/objects/{fanout}/{leaf}"),
        MAX_COMPRESSED,
        budget,
        work.memory_budget(),
        capacity_limit,
    ) {
        Ok(bytes) => bytes,
        Err(ReadError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err("missing-loose-object");
        }
        Err(error) => return Err(read_error(error)),
    };
    let mut stream = Inflate {
        codec: Decompress::new(),
        remaining: compressed.as_ref(),
        finished: false,
    };
    let mut hdr = [0; 32];
    let mut len = 0;
    let end = loop {
        len += stream.pump(&mut hdr[len..], work, budget)?;
        if let Some(end) = hdr[..len].iter().position(|byte| *byte == 0) {
            break end;
        }
        if stream.finished || len == hdr.len() {
            return Err("bad-loose-header");
        }
    };
    let space = hdr[..end]
        .iter()
        .position(|byte| *byte == b' ')
        .ok_or("bad-loose-header")?;
    if space == 0 || space + 1 >= end || !hdr[space + 1..end].iter().all(u8::is_ascii_digit) {
        return Err("bad-loose-header");
    }
    let (kind, size, header_len) =
        gix::objs::decode::loose_header(&hdr[..=end]).map_err(|_| "bad-loose-header")?;
    if gix::objs::encode::loose_header(kind, size).as_slice() != &hdr[..header_len] {
        return Err("bad-loose-header");
    }
    if kind != expected {
        return Err("object-kind");
    }
    let size = usize::try_from(size).map_err(|_| "declared-size-cap")?;
    if size > byte_limit {
        return Err("declared-size-cap");
    }
    work.reserve(size, 0)?;
    let mut body = Vec::new();
    body.try_reserve_exact(size).map_err(|_| "allocation-cap")?;
    let start = &hdr[header_len..len];
    if start.len() > size {
        return Err("loose-size-mismatch");
    }
    body.extend_from_slice(start);
    let mut chunk = [0; CHUNK];
    while !stream.finished {
        let len = (size - body.len()).saturating_add(1).min(chunk.len());
        let n = stream.pump(&mut chunk[..len], work, budget)?;
        if n > size - body.len() {
            return Err("loose-size-mismatch");
        }
        body.extend_from_slice(&chunk[..n]);
    }
    if body.len() != size {
        return Err("loose-size-mismatch");
    }
    verify_hash(&body, &hdr[..header_len], id, work)
        .map_err(|error| if error == "hash" { "object-oid" } else { error })?;
    work.hold_output(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn head_targets_are_exact_hashes_or_local_branch_refs() {
        let id = b"6b8589cc95ecc350aad48716afbcf8dab73b465d";
        assert!(matches!(parse_target(id), Ok(Target::Oid(oid)) if oid.as_bytes().len() == 20));
        assert!(
            matches!(parse_target(b"ref: refs/heads/main\r\n"), Ok(Target::Symbolic(name)) if name == "refs/heads/main")
        );
        assert!(matches!(
            parse_target(b"ref: refs/heads/../outside\n"),
            Err("unsafe-ref")
        ));
        assert!(matches!(
            parse_target(b"ref: refs/replace/main\n"),
            Err("unsafe-ref")
        ));
        assert!(matches!(
            parse_target(&id.to_ascii_uppercase()),
            Err("bad-oid")
        ));
    }
}
