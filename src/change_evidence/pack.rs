//! Bounded decoding of verified objects from retained Git pack bytes.
use super::buffer::{MemoryBudget, Reservation, ReservedBytes};
use gix::ObjectId;
use gix::features::hash::crc32_update;
use gix::features::zlib::{Decompress, FlushDecompress, Status};
use gix::hash::Kind::Sha1;
use gix::objs::Kind;
use gix::odb::pack::data::{Entry, entry::Header};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

type PackResult<T> = Result<T, &'static str>;
pub(crate) const MAX_PACK_BYTES: usize = 16 * 1024 * 1024;
const MAX_ENTRIES: usize = 4_096;
pub(crate) const MAX_INDEX_BYTES: usize = 8 + 256 * 4 + MAX_ENTRIES * 28 + 40;
const MAX_OBJECT: usize = 8 * 1024 * 1024;
const MAX_DELTA_DEPTH: usize = 64;
const MAX_MEMORY: usize = 48 * 1024 * 1024;
const HASH_BYTES: usize = 20;
const CHUNK: usize = 4_096;

pub(crate) struct Work<'a> {
    deadline: Instant,
    interrupt: &'a AtomicBool,
    remaining: Option<usize>,
    memory: &'a MemoryBudget,
}

impl<'a> Work<'a> {
    pub(crate) fn new(
        deadline: Instant,
        interrupt: &'a AtomicBool,
        work: usize,
        memory: &'a MemoryBudget,
    ) -> Self {
        Self {
            deadline,
            interrupt,
            remaining: Some(work),
            memory,
        }
    }

    fn memory_remaining(&self) -> usize {
        let held = self.memory.limit() - self.memory.remaining();
        self.memory.limit().min(MAX_MEMORY).saturating_sub(held)
    }

    pub(super) fn memory_budget(&self) -> &'a MemoryBudget {
        self.memory
    }

    pub(crate) fn read_limit(&mut self, ceiling: usize, temporary: usize) -> PackResult<usize> {
        self.charge(0)?;
        Ok(ceiling.min(self.memory_remaining().saturating_sub(temporary)))
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu"))]
    pub(super) fn reserve_native_heap(
        &mut self,
        bytes: usize,
    ) -> PackResult<super::native_heap::NativeHeapTicket<'a>> {
        if self.read_limit(bytes, 0)? != bytes {
            return Err("memory-cap");
        }
        super::native_heap::NativeHeapTicket::reserve(
            self.memory,
            bytes,
            self.deadline,
            self.interrupt,
        )
        .map_err(|_| "memory-cap")
    }

    pub(super) fn reserve_lookup(&mut self, path_bytes: usize) -> PackResult<Reservation<'a>> {
        self.charge(4 * path_bytes + 1)?;
        // Allow for the existing native path lookup's temporary heap use.
        let bytes = 1_024 + 8 * path_bytes;
        if self.read_limit(bytes, 0)? != bytes {
            return Err("memory-cap");
        }
        self.memory.reserve(bytes).map_err(|_| "memory-cap")
    }

    pub(crate) fn charge(&mut self, amount: usize) -> PackResult<()> {
        if self.interrupt.load(Ordering::Acquire) || Instant::now() >= self.deadline {
            return Err("deadline");
        }
        self.remaining = self
            .remaining
            .and_then(|remaining| remaining.checked_sub(amount));
        self.remaining.map(|_| ()).ok_or("work-cap")
    }

    #[cfg(target_os = "linux")]
    pub(super) fn lend_native(&mut self, units: usize) -> PackResult<Work<'a>> {
        self.charge(units)?;
        Ok(Work::new(self.deadline, self.interrupt, units, self.memory))
    }

    pub(crate) fn inflate_window(&mut self) -> PackResult<usize> {
        self.charge(1)?;
        let window = (self.remaining.unwrap_or_default() / 2).min(CHUNK);
        if window == 0 {
            self.remaining = None;
            return Err("work-cap");
        }
        Ok(window)
    }

    fn require_work(&mut self, bytes: usize) -> PackResult<()> {
        self.charge(0)?;
        if self.remaining.is_none_or(|work| work < bytes) {
            self.remaining = None;
            return Err("work-cap");
        }
        Ok(())
    }

    pub(crate) fn reserve(&mut self, bytes: usize, live: usize) -> PackResult<()> {
        self.require_work(bytes)?;
        if live.saturating_add(bytes) > self.memory_remaining() {
            return Err("memory-cap");
        }
        Ok(())
    }

    pub(crate) fn hold_output(&self, bytes: Vec<u8>) -> PackResult<ReservedBytes<'a>> {
        if bytes.capacity() > self.memory_remaining() {
            return Err("memory-cap");
        }
        self.memory.hold(bytes).map_err(|_| "memory-cap")
    }
}

fn number(bytes: &[u8], at: usize) -> PackResult<usize> {
    let raw = bytes
        .get(at..at + 4)
        .ok_or("frame")?
        .try_into()
        .map_err(|_| "frame")?;
    Ok(u32::from_be_bytes(raw) as usize)
}

fn oid(bytes: &[u8]) -> PackResult<ObjectId> {
    let raw: [u8; HASH_BYTES] = bytes.try_into().map_err(|_| "oid")?;
    Ok(ObjectId::Sha1(raw))
}

pub(crate) fn verify_hash(
    bytes: &[u8],
    prefix: &[u8],
    expected: ObjectId,
    work: &mut Work<'_>,
) -> PackResult<()> {
    let mut hash = gix::hash::hasher(Sha1);
    work.charge(prefix.len())?;
    hash.update(prefix);
    for chunk in bytes.chunks(CHUNK) {
        work.charge(chunk.len())?;
        hash.update(chunk);
    }
    if hash.try_finalize().map_err(|_| "hash")? != expected {
        return Err("hash");
    }
    Ok(())
}

struct PackedObject {
    id: ObjectId,
    offset: usize,
    end: usize,
    crc: u32,
}

pub(crate) struct Pack<'a> {
    bytes: ReservedBytes<'a>,
    rows: Vec<PackedObject>,
    order: Vec<usize>,
    // Drop both vectors before refunding their shared reservation.
    _tables: Reservation<'a>,
}

impl<'a> Pack<'a> {
    pub(crate) fn admit(
        data: ReservedBytes<'a>,
        index: &ReservedBytes<'a>,
        expected: ObjectId,
        work: &mut Work<'a>,
    ) -> PackResult<Self> {
        const IDX_HEADER: usize = 8 + 256 * 4;
        let bytes = data.as_ref();
        let index = index.as_ref();
        if bytes.len() > MAX_PACK_BYTES || index.len() > MAX_INDEX_BYTES {
            return Err("pack-cap");
        }
        if bytes.get(..4) != Some(b"PACK") || !matches!(number(bytes, 4)?, 2 | 3) {
            return Err("pack-format");
        }
        if index.get(..4) != Some(b"\xfftOc") || number(index, 4)? != 2 {
            return Err("index-format");
        }
        let count = number(bytes, 8)?;
        if count > MAX_ENTRIES {
            return Err("entry-cap");
        }
        if number(index, IDX_HEADER - 4)? != count || index.len() != IDX_HEADER + count * 28 + 40 {
            return Err("index-size");
        }
        let footer = bytes
            .len()
            .checked_sub(HASH_BYTES)
            .filter(|end| *end >= 12)
            .ok_or("pack-size")?;
        let index_footer = index.len() - 40;
        if oid(&bytes[footer..])? != expected
            || oid(&index[index_footer..index_footer + HASH_BYTES])? != expected
        {
            return Err("pack-pair");
        }
        verify_hash(
            &index[..index.len() - HASH_BYTES],
            &[],
            oid(&index[index.len() - HASH_BYTES..])?,
            work,
        )?;
        verify_hash(&bytes[..footer], &[], expected, work)?;
        let table_bytes = count * (size_of::<PackedObject>() + size_of::<usize>());
        // Preserve the existing work preflight. The raw buffers already charge memory.
        work.require_work(bytes.len() + index.len() + table_bytes)?;
        work.reserve(table_bytes, 0)?;
        let tables = work
            .memory_budget()
            .reserve(table_bytes)
            .map_err(|_| "memory-cap")?;

        // The pinned default allocator gives fresh exact reservations the
        // requested logical capacity. Refuse unexpected capacity rather than
        // keeping any allocation that exceeds the MAX_MEMORY preflight.
        let mut rows = Vec::new();
        rows.try_reserve_exact(count).map_err(|_| "allocation")?;
        if rows.capacity() != count {
            return Err("memory-cap");
        }
        let mut fanout = [0usize; 256];
        let mut last = None;
        for i in 0..count {
            work.charge(1)?;
            let from = IDX_HEADER + i * HASH_BYTES;
            let id = oid(&index[from..from + HASH_BYTES])?;
            if last.is_some_and(|last| last >= id) {
                return Err("index-order");
            }
            last = Some(id);
            fanout[usize::from(id.as_bytes()[0])] += 1;
            let crc = number(index, IDX_HEADER + count * HASH_BYTES + 4 * i)? as u32;
            let offset = number(index, IDX_HEADER + count * (HASH_BYTES + 4) + 4 * i)?;
            if offset < 12 || offset >= footer {
                return Err("index-offset");
            }
            rows.push(PackedObject {
                id,
                offset,
                end: 0,
                crc,
            });
        }
        let mut cumulative = 0;
        for (i, bucket) in fanout.into_iter().enumerate() {
            work.charge(1)?;
            cumulative += bucket;
            if number(index, 8 + 4 * i)? != cumulative {
                return Err("index-fanout");
            }
        }
        let mut order = Vec::new();
        order.try_reserve_exact(count).map_err(|_| "allocation")?;
        if order.capacity() != count {
            return Err("memory-cap");
        }
        order.extend(0..count);
        work.charge(count * 12)?;
        order.sort_unstable_by_key(|position| rows[*position].offset);
        let mut boundary = footer;
        for &position in order.iter().rev() {
            let row = &mut rows[position];
            if row.offset >= boundary {
                return Err("index-offset");
            }
            row.end = boundary;
            boundary = row.offset;
        }
        if boundary != 12 {
            return Err("index-offset");
        }
        Ok(Self {
            bytes: data,
            rows,
            order,
            _tables: tables,
        })
    }

    fn find(&self, id: ObjectId) -> PackResult<usize> {
        self.rows
            .binary_search_by_key(&id, |row| row.id)
            .map_err(|_| "missing-object")
    }

    pub(crate) fn contains(&self, id: ObjectId) -> bool {
        self.find(id).is_ok()
    }

    // CRC, entry boundaries and decompressed bytes are checked for each visited entry.
    fn entry(&self, position: usize, work: &mut Work<'_>) -> PackResult<Entry> {
        let row = &self.rows[position];
        let span = &self.bytes.as_ref()[row.offset..row.end];
        let header_len = varint_width(span)?;
        let object_type = span[0] >> 4 & 7;
        let header_len = header_len
            + match object_type {
                1..=4 => 0,
                6 => varint_width(&span[header_len..])?,
                7 => HASH_BYTES,
                _ => return Err("entry-type"),
            };
        let header = span.get(..header_len).ok_or("entry-header")?;
        let entry =
            Entry::from_bytes(header, row.offset as u64, HASH_BYTES).map_err(|_| "entry-header")?;
        if entry.decompressed_size > MAX_OBJECT as u64 || entry.data_offset as usize >= row.end {
            return Err("entry-size");
        }
        let mut crc = 0;
        for chunk in span.chunks(CHUNK) {
            work.charge(chunk.len())?;
            crc = crc32_update(crc, chunk);
        }
        if crc != row.crc {
            return Err("entry-crc");
        }
        Ok(entry)
    }

    pub(crate) fn object(
        &self,
        id: ObjectId,
        expected: Kind,
        depth: usize,
        work: &mut Work<'a>,
    ) -> PackResult<ReservedBytes<'a>> {
        let mut position = self.find(id)?;
        let depth = depth.min(MAX_DELTA_DEPTH);
        let mut chain: Vec<(usize, Entry)> = Vec::new();
        let chain_bytes = depth * size_of::<(usize, Entry)>();
        work.reserve(chain_bytes, 0)?;
        chain.try_reserve_exact(depth).map_err(|_| "allocation")?;
        let (base, kind) = loop {
            work.charge(1)?;
            if chain.iter().any(|(previous, _)| *previous == position) {
                return Err("delta-cycle");
            }
            let entry = self.entry(position, work)?;
            if let Some(kind) = entry.header.as_kind() {
                break (entry, kind);
            }
            if chain.len() >= depth {
                return Err("delta-depth");
            }
            let next = match entry.header {
                Header::OfsDelta { base_distance } => {
                    let distance =
                        usize::try_from(base_distance).map_err(|_| "delta-base-offset")?;
                    let offset = self.rows[position]
                        .offset
                        .checked_sub(distance)
                        .filter(|offset| *offset < self.rows[position].offset)
                        .ok_or("delta-base-offset")?;
                    let order = self
                        .order
                        .binary_search_by_key(&offset, |index| self.rows[*index].offset)
                        .map_err(|_| "delta-base-offset")?;
                    self.order[order]
                }
                Header::RefDelta { base_id } => {
                    self.find(base_id).map_err(|_| "delta-base-external")?
                }
                _ => return Err("entry-type"),
            };
            chain.push((position, entry));
            position = next;
        };
        if kind != expected {
            return Err("object-kind");
        }
        let mut data = self.inflate(position, &base, chain_bytes, work)?;
        self.verify_object(position, kind, &data, work)?;
        while let Some((index, entry)) = chain.pop() {
            let delta = self.inflate(index, &entry, chain_bytes + data.capacity(), work)?;
            data = apply_delta(
                &data,
                &delta,
                chain_bytes + data.capacity() + delta.capacity(),
                work,
            )?;
            self.verify_object(index, kind, &data, work)?;
        }
        work.hold_output(data)
    }

    fn verify_object(
        &self,
        index: usize,
        kind: Kind,
        bytes: &[u8],
        work: &mut Work<'_>,
    ) -> PackResult<()> {
        let prefix = format!("{kind} {}\0", bytes.len());
        verify_hash(bytes, prefix.as_bytes(), self.rows[index].id, work).map_err(|error| {
            if error == "hash" {
                "object-hash"
            } else {
                error
            }
        })
    }

    fn inflate(
        &self,
        position: usize,
        entry: &Entry,
        live: usize,
        work: &mut Work<'_>,
    ) -> PackResult<Vec<u8>> {
        let bytes = &self.bytes.as_ref()[entry.data_offset as usize..self.rows[position].end];
        let size = entry.decompressed_size as usize;
        work.reserve(size, live)?;
        let mut result = Vec::new();
        result.try_reserve_exact(size).map_err(|_| "allocation")?;
        let mut inflater = Decompress::new();
        let mut output = [0u8; CHUNK];
        let mut offset = 0;
        loop {
            let window = work.inflate_window()?;
            let input = &bytes[offset..(offset + window).min(bytes.len())];
            let span = (size - result.len()).saturating_add(1).min(window);
            let status = inflater
                .decompress(input, &mut output[..span], FlushDecompress::None)
                .map_err(|_| "inflate")?;
            let used = inflater.total_in() as usize - offset;
            let produced = inflater.total_out() as usize - result.len();
            work.charge(used + produced)?;
            if produced > size - result.len() {
                return Err("inflate-size");
            }
            offset += used;
            result.extend_from_slice(&output[..produced]);
            if status == Status::StreamEnd {
                return if offset == bytes.len() && result.len() == size {
                    Ok(result)
                } else {
                    Err("inflate-size")
                };
            }
            if used == 0 && produced == 0 {
                return Err("inflate-truncated");
            }
        }
    }
}

fn varint_width(bytes: &[u8]) -> PackResult<usize> {
    bytes
        .iter()
        .take(5)
        .position(|byte| byte & 0x80 == 0)
        .map(|index| index + 1)
        .ok_or("entry-varint")
}

fn delta_size(input: &mut &[u8]) -> PackResult<usize> {
    let width = varint_width(input)?;
    let (field, rest) = input.split_at(width);
    *input = rest;
    let size = field
        .iter()
        .enumerate()
        .fold(0u64, |value, (position, byte)| {
            value | (u64::from(byte & 0x7f) << (7 * position))
        });
    if size > MAX_OBJECT as u64 || (width > 1 && field[width - 1] == 0) {
        return Err("delta-size");
    }
    Ok(size as usize)
}

fn take<'a>(bytes: &mut &'a [u8], len: usize) -> PackResult<&'a [u8]> {
    let (result, rest) = bytes.split_at_checked(len).ok_or("delta-truncated")?;
    *bytes = rest;
    Ok(result)
}

fn copy_field(bytes: &mut &[u8], flags: u8, width: usize) -> PackResult<usize> {
    let mut value = 0usize;
    for shift in 0..width {
        if flags & (1 << shift) != 0 {
            value |= usize::from(take(bytes, 1)?[0]) << (shift * 8);
        }
    }
    Ok(value)
}

fn apply_delta(
    base: &[u8],
    mut input: &[u8],
    live: usize,
    work: &mut Work<'_>,
) -> PackResult<Vec<u8>> {
    let source = delta_size(&mut input)?;
    let target = delta_size(&mut input)?;
    if source != base.len() {
        return Err("delta-base-size");
    }
    work.reserve(target, live)?;
    let mut output = Vec::new();
    output.try_reserve_exact(target).map_err(|_| "allocation")?;
    while let Some((&op, rest)) = input.split_first() {
        work.charge(1)?;
        input = rest;
        let bytes = match op {
            0 => return Err("delta-opcode"),
            1..=127 => take(&mut input, usize::from(op))?,
            _ => {
                let start = copy_field(&mut input, op & 0x0f, 4)?;
                let len = copy_field(&mut input, (op >> 4) & 7, 3)?;
                let len = if len == 0 { 65_536 } else { len };
                let end = start.checked_add(len).ok_or("delta-copy")?;
                base.get(start..end).ok_or("delta-copy")?
            }
        };
        if bytes.len() > target - output.len() {
            return Err("delta-size");
        }
        for chunk in bytes.chunks(CHUNK) {
            work.charge(chunk.len())?;
            output.extend_from_slice(chunk);
        }
    }
    if output.len() != target {
        return Err("delta-size");
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn applies_bounded_delta_copy_and_insert() {
        let cancelled = AtomicBool::new(false);
        let deadline = Instant::now() + Duration::from_secs(3);
        let memory = MemoryBudget::new(100);
        let mut work = Work::new(deadline, &cancelled, 100, &memory);
        assert_eq!(
            apply_delta(b"same-a", b"\x06\x06\x90\x05\x01b", 0, &mut work).unwrap(),
            b"same-b"
        );
        assert!(matches!(
            apply_delta(b"same-a", b"\x06\x06\x91\x10\x06", 0, &mut work),
            Err("delta-copy")
        ));
    }

    #[test]
    fn exhausted_work_remains_terminal_and_memory_refuses_before_allocation() {
        let cancelled = AtomicBool::new(false);
        let deadline = Instant::now() + Duration::from_secs(3);
        let memory = MemoryBudget::new(8);
        let mut work = Work::new(deadline, &cancelled, 10, &memory);
        assert_eq!(work.reserve(9, 0), Err("memory-cap"));
        assert_eq!(work.charge(11), Err("work-cap"));
        assert_eq!(work.charge(0), Err("work-cap"));
        let mut work = Work::new(deadline, &cancelled, 10, &memory);
        cancelled.store(true, Ordering::Release);
        assert_eq!(work.reserve(1, 0), Err("deadline"));
    }

    #[test]
    fn admitted_pack_tables_remain_charged_to_another_work_until_drop() {
        fn hash(bytes: &[u8]) -> ObjectId {
            let mut hasher = gix::hash::hasher(Sha1);
            hasher.update(bytes);
            hasher.try_finalize().unwrap()
        }

        let object = hash(b"blob 6\0hello\n");
        let mut data = b"PACK\0\0\0\x02\0\0\0\x01".to_vec();
        // Type and size of a six-byte blob followed by its compressed data.
        data.extend_from_slice(&[
            0x36, 120, 156, 203, 72, 205, 201, 201, 231, 2, 0, 8, 75, 2, 31,
        ]);
        let crc = crc32_update(0, &data[12..]);
        let pack_id = hash(&data);
        data.extend_from_slice(pack_id.as_bytes());

        let mut index = b"\xfftOc\0\0\0\x02".to_vec();
        for bucket in 0..=u8::MAX {
            index.extend_from_slice(&u32::from(bucket >= object.as_bytes()[0]).to_be_bytes());
        }
        index.extend_from_slice(object.as_bytes());
        index.extend_from_slice(&crc.to_be_bytes());
        index.extend_from_slice(&12u32.to_be_bytes());
        index.extend_from_slice(pack_id.as_bytes());
        let index_id = hash(&index);
        index.extend_from_slice(index_id.as_bytes());

        let memory = MemoryBudget::new(MAX_MEMORY + 1024);
        let data_capacity = data.capacity();
        let data = memory.hold(data).unwrap();
        let index = memory.hold(index).unwrap();
        let cancelled = AtomicBool::new(false);
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut first = Work::new(deadline, &cancelled, 10_000, &memory);
        let mut second = Work::new(deadline, &cancelled, 10_000, &memory);
        let table_bytes = size_of::<PackedObject>() + size_of::<usize>();
        let headroom = second.read_limit(usize::MAX, 0).unwrap();
        let _other = memory.reserve(headroom - table_bytes).unwrap();
        assert_eq!(second.read_limit(usize::MAX, 0), Ok(table_bytes));

        let pack = Pack::admit(data, &index, pack_id, &mut first).unwrap();
        assert!(pack.contains(object));
        assert_eq!(second.read_limit(usize::MAX, 0), Ok(0));
        assert_eq!(second.reserve(1, 0), Err("memory-cap"));

        drop(pack);
        assert_eq!(
            second.read_limit(usize::MAX, 0),
            Ok(data_capacity + table_bytes)
        );
    }

    #[test]
    fn caller_owned_output_counts_against_later_preflight_until_dropped() {
        let cancelled = AtomicBool::new(false);
        let deadline = Instant::now() + Duration::from_secs(3);
        let bytes = vec![1, 2, 3];
        let capacity = bytes.capacity();
        let memory = MemoryBudget::new(capacity + 4);
        let mut work = Work::new(deadline, &cancelled, 100, &memory);
        let _other = memory.reserve(2).unwrap();

        let output = work.hold_output(bytes).unwrap();
        assert_eq!(output.as_ref(), &[1, 2, 3]);
        assert_eq!(work.read_limit(100, 0), Ok(2));
        assert_eq!(work.reserve(3, 0), Err("memory-cap"));

        drop(output);
        assert_eq!(work.read_limit(100, 0), Ok(capacity + 2));
        assert_eq!(work.reserve(3, 0), Ok(()));
    }
}
