//! Validates the published SQLite snapshot while its canonical inputs are held read-only.

const INDEX_HEADER: usize = 48;
pub(super) const WAL_HEADER: usize = 32;
const FRAME_HEADER: usize = 24;
const VERSION: u32 = 3_007_000;
const MAX_PAGE_NUMBER: u32 = 0xffff_fffe;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SnapshotError {
    ShortIndex,
    HeaderMismatch,
    Uninitialized,
    IndexChecksum,
    IndexVersion,
    EmptyEpoch,
    PageSize,
    ShortWal,
    WalMagic,
    WalVersion,
    WalChecksum,
    SaltMismatch,
    PrefixTooLarge,
    ShortPrefix,
    FramePage,
    FrameSalt,
    FrameChecksum,
    FinalCommit,
    CachedChecksum,
    DatabaseHeader,
    DatabaseTooLarge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ValidatedSnapshot {
    wal_bytes: u64,
    database_bytes: u64,
}

impl ValidatedSnapshot {
    pub(super) fn wal_bytes(self) -> u64 {
        self.wal_bytes
    }
    pub(super) fn database_bytes(self) -> u64 {
        self.database_bytes
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct FrameReadPlan {
    pub(super) frames: u32,
    pub(super) frame_bytes: usize,
}

struct IndexHeader {
    frames: u32,
    database_pages: u32,
    page_size: u32,
    big_endian: bool,
    cached_checksum: [u32; 2],
    salt: [u8; 8],
}

fn ne32(b: &[u8]) -> u32 {
    u32::from_ne_bytes(b.try_into().unwrap())
}
fn ne16(b: &[u8]) -> u16 {
    u16::from_ne_bytes(b.try_into().unwrap())
}
fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes(b.try_into().unwrap())
}

fn native_page_size(encoded: u16) -> u32 {
    if encoded == 1 {
        65_536
    } else {
        u32::from(encoded)
    }
}

fn is_page_size(size: u32) -> bool {
    (512..=65_536).contains(&size) && size.is_power_of_two()
}

fn checksum(bytes: &[u8], mut sum: [u32; 2], big_endian: bool) -> [u32; 2] {
    let (pairs, remainder) = bytes.as_chunks::<8>();
    debug_assert!(remainder.is_empty());
    for pair in pairs {
        let word = |b: &[u8]| {
            let a: [u8; 4] = b.try_into().unwrap();
            if big_endian {
                u32::from_be_bytes(a)
            } else {
                u32::from_le_bytes(a)
            }
        };
        sum[0] = sum[0].wrapping_add(word(&pair[..4])).wrapping_add(sum[1]);
        sum[1] = sum[1].wrapping_add(word(&pair[4..])).wrapping_add(sum[0]);
    }
    sum
}

fn read_index(shm: &[u8]) -> Result<IndexHeader, SnapshotError> {
    use SnapshotError::*;
    let headers = shm.get(..2 * INDEX_HEADER).ok_or(ShortIndex)?;
    let (h, alternate) = headers.split_at(INDEX_HEADER);
    if h != alternate {
        return Err(HeaderMismatch);
    }
    if h[12] != 1 {
        return Err(Uninitialized);
    }
    if ne32(&h[0..4]) != VERSION {
        return Err(IndexVersion);
    }
    if checksum(&h[..40], [0, 0], cfg!(target_endian = "big"))
        != [ne32(&h[40..44]), ne32(&h[44..48])]
    {
        return Err(IndexChecksum);
    }
    if h[13] > 1 {
        return Err(WalMagic);
    }
    Ok(IndexHeader {
        frames: ne32(&h[16..20]),
        database_pages: ne32(&h[20..24]),
        page_size: native_page_size(ne16(&h[14..16])),
        big_endian: h[13] != 0,
        cached_checksum: [ne32(&h[24..28]), ne32(&h[28..32])],
        salt: h[32..40].try_into().unwrap(),
    })
}

fn database_extent(pages: u32, page_size: u32, limit: u64) -> Result<u64, SnapshotError> {
    if pages == 0 || pages > MAX_PAGE_NUMBER {
        return Err(SnapshotError::DatabaseHeader);
    }
    let extent = u64::from(pages) * u64::from(page_size);
    if extent > limit {
        return Err(SnapshotError::DatabaseTooLarge);
    }
    Ok(extent)
}

/// A checked MAIN-only snapshot after a normal checkpoint or clean reopen.
/// The caller must hold the same admitted MAIN, WAL, and SHM files through use.
pub(super) fn verify_empty(
    shm: &[u8],
    main_header: &[u8],
    main_len: u64,
    wal_len: u64,
    expected_page_size: u32,
    max_database_bytes: u64,
) -> Result<ValidatedSnapshot, SnapshotError> {
    use SnapshotError::*;
    let index = read_index(shm)?;
    if index.frames != 0 || wal_len != 0 {
        return Err(EmptyEpoch);
    }
    let main = main_header.get(..100).ok_or(DatabaseHeader)?;
    if &main[..16] != b"SQLite format 3\0"
        || main[18..20] != [2, 2]
        || be32(&main[24..28]) != be32(&main[92..96])
    {
        return Err(DatabaseHeader);
    }
    let pages = be32(&main[28..32]);
    let page_size = native_page_size(u16::from_be_bytes(main[16..18].try_into().unwrap()));
    if !is_page_size(page_size) || page_size != expected_page_size {
        return Err(PageSize);
    }
    match (index.database_pages, index.page_size) {
        (0, 0) => {}
        hint if hint == (pages, page_size) => {}
        _ => return Err(DatabaseHeader),
    }
    let database_bytes = database_extent(pages, page_size, max_database_bytes)?;
    if database_bytes != main_len {
        return Err(DatabaseHeader);
    }
    Ok(ValidatedSnapshot {
        database_bytes,
        wal_bytes: 0,
    })
}

/// Incremental validation of the exact published WAL prefix. Keep the same
/// admitted files read-only and stable through validation and subsequent use.
pub(super) struct SnapshotValidator {
    plan: FrameReadPlan,
    index: IndexHeader,
    wal_bytes: u64,
    database_bytes: u64,
    sum: [u32; 2],
    seen: u32,
    final_commit: u32,
}

impl SnapshotValidator {
    pub(super) fn begin(
        shm: &[u8],
        wal_header: &[u8],
        wal_len: u64,
        expected_page_size: u32,
        max_prefix_bytes: u64,
        max_database_bytes: u64,
    ) -> Result<Self, SnapshotError> {
        use SnapshotError::*;
        let index = read_index(shm)?;
        if index.frames == 0 {
            return Err(EmptyEpoch);
        }
        let size = index.page_size;
        if !is_page_size(size) || size != expected_page_size {
            return Err(PageSize);
        }
        let header = wal_header.get(..WAL_HEADER).ok_or(ShortWal)?;
        let magic = be32(&header[..4]);
        if magic & !1 != 0x377f0682 || index.big_endian != (magic & 1 != 0) {
            return Err(WalMagic);
        }
        if be32(&header[4..8]) != VERSION {
            return Err(WalVersion);
        }
        if be32(&header[8..12]) != size {
            return Err(PageSize);
        }
        if index.salt != header[16..24] {
            return Err(SaltMismatch);
        }
        let sum = checksum(&header[..24], [0, 0], index.big_endian);
        if sum != [be32(&header[24..28]), be32(&header[28..32])] {
            return Err(WalChecksum);
        }
        let frame_bytes = size as usize + FRAME_HEADER;
        let wal_bytes = u64::from(index.frames) * frame_bytes as u64 + WAL_HEADER as u64;
        if wal_bytes > max_prefix_bytes {
            return Err(PrefixTooLarge);
        }
        if wal_bytes > wal_len {
            return Err(ShortPrefix);
        }
        let database_bytes = database_extent(index.database_pages, size, max_database_bytes)?;
        let plan = FrameReadPlan {
            frames: index.frames,
            frame_bytes,
        };
        Ok(Self {
            plan,
            index,
            wal_bytes,
            database_bytes,
            sum,
            seen: 0,
            final_commit: 0,
        })
    }

    /// Only a bounded read plan is available until every frame has been verified.
    pub(super) fn read_plan(&self) -> FrameReadPlan {
        self.plan
    }

    pub(super) fn push_frame(&mut self, frame: &[u8]) -> Result<(), SnapshotError> {
        use SnapshotError::*;
        if self.seen >= self.plan.frames || frame.len() != self.plan.frame_bytes {
            return Err(ShortPrefix);
        }
        if !(1..=MAX_PAGE_NUMBER).contains(&be32(&frame[..4])) {
            return Err(FramePage);
        }
        let committed_pages = be32(&frame[4..8]);
        if committed_pages > MAX_PAGE_NUMBER {
            return Err(FinalCommit);
        }
        if frame[8..16] != self.index.salt {
            return Err(FrameSalt);
        }
        let sum = checksum(&frame[..8], self.sum, self.index.big_endian);
        let sum = checksum(&frame[FRAME_HEADER..], sum, self.index.big_endian);
        if sum != [be32(&frame[16..20]), be32(&frame[20..24])] {
            return Err(FrameChecksum);
        }
        self.sum = sum;
        self.final_commit = committed_pages;
        self.seen += 1;
        Ok(())
    }

    pub(super) fn finish(self) -> Result<ValidatedSnapshot, SnapshotError> {
        use SnapshotError::*;
        if self.seen != self.plan.frames {
            return Err(ShortPrefix);
        }
        if self.final_commit == 0 || self.final_commit != self.index.database_pages {
            return Err(FinalCommit);
        }
        if self.sum != self.index.cached_checksum {
            return Err(CachedChecksum);
        }
        Ok(ValidatedSnapshot {
            wal_bytes: self.wal_bytes,
            database_bytes: self.database_bytes,
        })
    }
}

#[cfg(test)]
fn validate(
    shm: &[u8],
    wal: &[u8],
    expected_page_size: u32,
    limit: usize,
) -> Result<ValidatedSnapshot, SnapshotError> {
    let mut validator = SnapshotValidator::begin(
        shm,
        wal,
        wal.len() as u64,
        expected_page_size,
        limit as u64,
        u64::MAX,
    )?;
    let plan = validator.read_plan();
    let end = WAL_HEADER + plan.frame_bytes * plan.frames as usize;
    for frame in wal[WAL_HEADER..end].chunks_exact(plan.frame_bytes) {
        validator.push_frame(frame)?;
    }
    validator.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn putbe(b: &mut [u8], v: u32) {
        b.copy_from_slice(&v.to_be_bytes());
    }
    fn putne(b: &mut [u8], v: u32) {
        b.copy_from_slice(&v.to_ne_bytes());
    }
    fn fixture(big: bool, page: usize) -> (Vec<u8>, Vec<u8>) {
        let mut wal = vec![0; WAL_HEADER + 2 * (FRAME_HEADER + page)];
        putbe(&mut wal[0..4], 0x377f0682 | u32::from(big));
        putbe(&mut wal[4..8], VERSION);
        putbe(&mut wal[8..12], page as u32);
        wal[16..24].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        let mut sum = checksum(&wal[..24], [0, 0], big);
        putbe(&mut wal[24..28], sum[0]);
        putbe(&mut wal[28..32], sum[1]);
        for i in 0..2 {
            let start = WAL_HEADER + i * (FRAME_HEADER + page);
            let f = &mut wal[start..start + FRAME_HEADER + page];
            putbe(&mut f[..4], (i + 1) as u32);
            putbe(&mut f[4..8], if i == 1 { 2 } else { 0 });
            f[8..16].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
            f[FRAME_HEADER..].fill((i + 17) as u8);
            sum = checksum(&f[..8], sum, big);
            sum = checksum(&f[FRAME_HEADER..], sum, big);
            putbe(&mut f[16..20], sum[0]);
            putbe(&mut f[20..24], sum[1]);
        }
        let mut h = [0u8; INDEX_HEADER];
        putne(&mut h[..4], VERSION);
        h[12] = 1;
        h[13] = u8::from(big);
        h[14..16].copy_from_slice(&(if page == 65536 { 1 } else { page as u16 }).to_ne_bytes());
        putne(&mut h[16..20], 2);
        putne(&mut h[20..24], 2);
        putne(&mut h[24..28], sum[0]);
        putne(&mut h[28..32], sum[1]);
        h[32..40].copy_from_slice(&wal[16..24]);
        let c = checksum(&h[..40], [0, 0], cfg!(target_endian = "big"));
        putne(&mut h[40..44], c[0]);
        putne(&mut h[44..48], c[1]);
        (h.repeat(2), wal)
    }
    fn fix_index(shm: &mut [u8]) {
        let c = checksum(&shm[..40], [0, 0], cfg!(target_endian = "big"));
        putne(&mut shm[40..44], c[0]);
        putne(&mut shm[44..48], c[1]);
        let (a, b) = shm.split_at_mut(48);
        b.copy_from_slice(a);
    }
    #[test]
    fn validates_both_checksum_orders_and_page_sizes() {
        for big in [false, true] {
            for page in [512, 4096, 65536] {
                let (s, mut w) = fixture(big, page);
                let end = w.len();
                w.extend([9; 7]);
                assert_eq!(
                    validate(&s, &w, page as u32, end).unwrap().wal_bytes(),
                    end as u64
                );
            }
        }
    }
    #[test]
    fn streaming_requires_every_frame_once() {
        let (s, w) = fixture(true, 512);
        let make = || {
            SnapshotValidator::begin(&s, &w[..32], w.len() as u64, 512, w.len() as u64, u64::MAX)
                .unwrap()
        };
        let mut v = make();
        assert_eq!(v.finish(), Err(SnapshotError::ShortPrefix));
        v = make();
        assert_eq!(v.push_frame(&w[32..40]), Err(SnapshotError::ShortPrefix));
        v.push_frame(&w[32..568]).unwrap();
        v.push_frame(&w[568..1104]).unwrap();
        assert_eq!(v.push_frame(&w[568..1104]), Err(SnapshotError::ShortPrefix));
        assert_eq!(v.finish().unwrap().database_bytes(), 1024);
    }
    #[test]
    fn rejects_corruptions_and_resource_bounds() {
        let (s, w) = fixture(false, 512);
        assert_eq!(
            validate(&s[..95], &w, 512, usize::MAX),
            Err(SnapshotError::ShortIndex)
        );
        let mut x = s.clone();
        x[48] ^= 1;
        assert_eq!(
            validate(&x, &w, 512, usize::MAX),
            Err(SnapshotError::HeaderMismatch)
        );
        let mut x = s.clone();
        x[8] ^= 1;
        x[56] ^= 1;
        assert_eq!(
            validate(&x, &w, 512, usize::MAX),
            Err(SnapshotError::IndexChecksum)
        );
        let mut x = s.clone();
        putne(&mut x[16..20], 0);
        fix_index(&mut x);
        assert_eq!(
            validate(&x, &w, 512, usize::MAX),
            Err(SnapshotError::EmptyEpoch)
        );
        assert_eq!(
            validate(&s, &w, 4096, usize::MAX),
            Err(SnapshotError::PageSize)
        );
        assert_eq!(
            validate(&s, &w, 512, w.len() - 1),
            Err(SnapshotError::PrefixTooLarge)
        );
        assert_eq!(
            validate(&s, &w[..w.len() - 1], 512, usize::MAX),
            Err(SnapshotError::ShortPrefix)
        );
        let mut x = w.clone();
        x[16] ^= 1;
        assert_eq!(
            validate(&s, &x, 512, usize::MAX),
            Err(SnapshotError::SaltMismatch)
        );
        let mut x = w.clone();
        x[32 + 24] ^= 1;
        assert_eq!(
            validate(&s, &x, 512, usize::MAX),
            Err(SnapshotError::FrameChecksum)
        );
        let mut x = s.clone();
        putne(&mut x[20..24], 3);
        fix_index(&mut x);
        assert_eq!(
            validate(&x, &w, 512, usize::MAX),
            Err(SnapshotError::FinalCommit)
        );
        let mut x = s.clone();
        x[24] ^= 1;
        fix_index(&mut x);
        assert_eq!(
            validate(&x, &w, 512, usize::MAX),
            Err(SnapshotError::CachedChecksum)
        );
    }
}
