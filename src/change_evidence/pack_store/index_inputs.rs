//! Match same-snapshot indexed witnesses to verified regular blobs and raw checkout files.
use super::{FileModePolicy, MAX_TRACKED_BLOB_BYTES, Observation, PackResult, exact_oid};
use crate::commands::baseline::extract::capture_snapshot::CaptureSnapshot;
use crate::index::orchestrator::PARSER_VERSION;
use crate::util::fs::{StrictDecodeError, decode_bytes_for_capture};
use gix::ObjectId;
use std::mem::size_of;

#[derive(Debug, Eq, PartialEq)]
pub(super) struct MatchedIndexInputs {
    pub(super) rows: usize,
    pub(super) digest: blake3::Hash,
}

impl Observation<'_> {
    pub(super) fn match_index_inputs(
        &mut self,
        head: ObjectId,
        tree: ObjectId,
        snapshot: &CaptureSnapshot,
    ) -> PackResult<MatchedIndexInputs> {
        let generation = &snapshot.generation;
        if generation
            .head_hash
            .as_deref()
            .map(str::as_bytes)
            .map(exact_oid)
            != Some(Ok(head))
            || generation.last_indexed_at.is_none()
        {
            return Err("index-generation-mismatch");
        }
        if generation.parser_version.as_deref() != Some(PARSER_VERSION) {
            return Err("index-parser-version-mismatch");
        }
        let policy = FileModePolicy::from_local(self.observe_local_git_config()?.file_mode)?;
        let walk = self.match_staged_tree(tree)?;
        let witnesses = snapshot.witnesses();
        let rows = witnesses.len();
        if rows > walk.files() {
            return Err("index-input-membership-unproven");
        }
        let mut max_path_len = 0;
        for witness in witnesses {
            self.work
                .charge(1 + witness.path.len() + witness.parser_version.len())?;
            if witness.parser_version != PARSER_VERSION {
                return Err("index-parser-version-mismatch");
            }
            let entry = walk
                .exact_file(witness.path.as_bytes(), &mut self.work)?
                .ok_or("index-input-path-not-in-head")?;
            let expected_size = witness
                .file_size
                .and_then(|size| usize::try_from(size).ok())
                .ok_or("index-input-stamp-missing")?;
            let expected_mtime = witness.mtime_ns.ok_or("index-input-stamp-missing")?;
            let _lookup = self.work.reserve_lookup(entry.path.len())?;
            let (blob, actual_mtime) =
                self.match_tracked_blob(witness.path, entry.id, entry.executable, policy, None)?;
            if blob.as_ref().len() != expected_size || actual_mtime != Some(expected_mtime) {
                return Err("index-input-stamp-mismatch");
            }
            let hash = self.hash_index_blob(blob.as_ref())?;
            if hash.to_hex().as_str() != witness.content_hash {
                return Err("index-input-hash-mismatch");
            }
            max_path_len = max_path_len.max(witness.path.len());
        }
        drop(walk);

        // Verified hashes and parser versions are byte-identical to these SQL
        // rows. Reuse the landed sort, tuple framing and hash without cloning.
        let sort_work =
            rows * (max_path_len + 1) * (4 * (usize::BITS - rows.leading_zeros()) as usize + 3);
        self.work
            .charge(sort_work + rows * (PARSER_VERSION.len() + 67))?;
        let capacity = 3 * rows * size_of::<usize>() + 128;
        if self.work.read_limit(capacity, 0)? != capacity {
            return Err("memory-cap");
        }
        let _canonical = self
            .work
            .memory_budget()
            .reserve(capacity)
            .map_err(|_| "memory-cap")?;
        let digest = snapshot.recompute_input_digest();
        if digest != snapshot.input_digest {
            return Err("index-input-digest-mismatch");
        }
        let hex = digest
            .strip_prefix("blake3:")
            .ok_or("index-input-digest-mismatch")?;
        let digest = blake3::Hash::from_hex(hex).map_err(|_| "index-input-digest-mismatch")?;
        self.work.charge(0)?;
        Ok(MatchedIndexInputs { rows, digest })
    }

    /// Look for positive evidence of legacy content drift after exact path
    /// membership. This can explain a negative verdict across index generations.
    /// It never returns permission to compare or bind old hashes to the new HEAD.
    pub(super) fn has_confirmed_index_content_drift(
        &mut self,
        tree: ObjectId,
        snapshot: &CaptureSnapshot,
    ) -> PackResult<bool> {
        let policy = FileModePolicy::from_local(self.observe_local_git_config()?.file_mode)?;
        let walk = self.match_staged_tree(tree)?;
        for witness in snapshot.witnesses() {
            self.work.charge(1 + witness.path.len())?;
            let entry = walk
                .exact_file(witness.path.as_bytes(), &mut self.work)?
                .ok_or("index-input-path-not-in-head")?;
            let _lookup = self.work.reserve_lookup(entry.path.len())?;
            let (blob, _) =
                self.match_tracked_blob(witness.path, entry.id, entry.executable, policy, None)?;
            let hash = self.hash_index_blob(blob.as_ref())?;
            if hash.to_hex().as_str() != witness.content_hash {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn hash_index_blob(&mut self, raw: &[u8]) -> PackResult<blake3::Hash> {
        self.work.charge(6 * raw.len() + 1)?;
        // The UTF-16 path owns the vector of code units and can grow its UTF-8
        // output. Reserve both old and new output capacities during growth.
        let capacity = if raw.starts_with(&[0xff, 0xfe]) || raw.starts_with(&[0xfe, 0xff]) {
            8 * raw.len() + 64
        } else {
            raw.len()
        };
        if self.work.read_limit(capacity, 0)? != capacity {
            return Err("memory-cap");
        }
        let _decoder = self
            .work
            .memory_budget()
            .reserve(capacity)
            .map_err(|_| "memory-cap")?;
        let text = decode_bytes_for_capture(raw, MAX_TRACKED_BLOB_BYTES, MAX_TRACKED_BLOB_BYTES)
            .map_err(|error| match error {
                StrictDecodeError::InvalidEncoding => "unsupported-input-encoding",
                StrictDecodeError::RawByteLimit | StrictDecodeError::DecodedByteLimit => {
                    "index-input-text-cap"
                }
            })?;
        let digest = blake3::hash(text.as_bytes());
        self.work.charge(0)?;
        Ok(digest)
    }
}
