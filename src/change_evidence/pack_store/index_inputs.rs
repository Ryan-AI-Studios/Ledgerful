//! Match same-snapshot indexed witnesses to verified regular blobs and raw checkout files.
use super::{
    FileModePolicy, MAX_TRACKED_BLOB_BYTES, Observation, PackResult, charge_proposal_limit,
    exact_oid,
};
use crate::commands::baseline::extract::capture_snapshot::CaptureSnapshot;
use crate::index::orchestrator::PARSER_VERSION;
use crate::util::fs::{StrictDecodeError, decode_bytes_for_capture};
use gix::ObjectId;
use std::cell::Cell;
use std::mem::size_of;

#[cfg(any(windows, target_os = "linux"))]
use super::super::baseline_discovery_policy::{DiscoveryEntry, RuleFrame, classify_selected};
#[cfg(any(windows, target_os = "linux"))]
use std::collections::{BTreeMap, BTreeSet};

#[cfg(any(windows, target_os = "linux"))]
type HeadRules<'work, 'path> = BTreeMap<&'path str, [Option<RuleFrame<'work, 'path>>; 2]>;

#[derive(Debug, Eq, PartialEq)]
pub(super) struct MatchedIndexInputs {
    pub(super) rows: usize,
    pub(super) digest: blake3::Hash,
}

#[derive(Clone, Copy)]
pub(super) struct ProposalSourceBudget<'a> {
    pub(super) entries: &'a Cell<usize>,
    pub(super) bytes: &'a Cell<usize>,
}

impl<'a> Observation<'a> {
    pub(super) fn match_index_inputs(
        &mut self,
        head: ObjectId,
        tree: ObjectId,
        snapshot: &CaptureSnapshot,
    ) -> PackResult<MatchedIndexInputs> {
        self.match_index_inputs_with(head, tree, snapshot, None)
    }

    pub(super) fn match_index_inputs_with(
        &mut self,
        head: ObjectId,
        tree: ObjectId,
        snapshot: &CaptureSnapshot,
        proposal: Option<ProposalSourceBudget<'_>>,
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
        let walk = if let Some(budget) = proposal {
            self.observe_staged_index()?;
            self.walk_tree_with_limit(tree, super::TreePaths::ChangedFiles, Some(budget.entries))?
        } else {
            self.match_staged_tree(tree)?
        };
        let witnesses = snapshot.witnesses();
        let rows = witnesses.len();
        if rows > walk.files() {
            return Err("index-input-membership-unproven");
        }
        #[cfg(any(windows, target_os = "linux"))]
        if proposal.is_some() {
            self.confirm_proposal_head_membership(&walk, snapshot)?;
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
            if let Some(budget) = proposal {
                if entry.checkout_ambiguous {
                    return Err("index-input-path-not-in-head");
                }
                self.confirm_proposal_staged_path(&walk, witness.path.as_bytes())?;
                let total = expected_size.checked_mul(2).ok_or("proposal-source-cap")?;
                charge_proposal_limit(budget.bytes, total, "proposal-source-cap")?;
            }
            let _lookup = self.work.reserve_lookup(entry.path.len())?;
            let (blob, actual_mtime) = self.match_tracked_blob(
                witness.path,
                entry.id,
                entry.executable,
                policy,
                None,
                proposal.map(|_| expected_size),
            )?;
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

    fn confirm_proposal_staged_path(
        &mut self,
        walk: &super::TreeIndexWalk<'a>,
        path: &[u8],
    ) -> PackResult<()> {
        let index = self
            .staged_index
            .as_ref()
            .and_then(|index| index.as_ref().ok())
            .ok_or("staged-index-unobserved")?;
        let entries = index.state.entries();
        let entry = entries
            .binary_search_by(|entry| {
                let indexed: &[u8] = entry.path(&index.state).as_ref();
                indexed.cmp(path)
            })
            .map_err(|_| "staged-tree-mismatch")?;
        walk.compare_file(path, entries[entry].mode, entries[entry].id, &mut self.work)
    }

    #[cfg(any(windows, target_os = "linux"))]
    fn confirm_proposal_head_membership(
        &mut self,
        walk: &super::TreeIndexWalk<'a>,
        snapshot: &CaptureSnapshot,
    ) -> PackResult<()> {
        use crate::index::staleness::is_supported_code_path;
        let indexed: BTreeSet<_> = snapshot.witnesses().map(|row| row.path).collect();
        let mut admitted_rules = HeadRules::new();
        let mut omitted_rules = HeadRules::new();
        let mut external = None;
        for (path, _, _) in walk.tracked_files() {
            self.work.charge(path.len() + 1)?;
            let path = std::str::from_utf8(path).map_err(|_| "index-input-membership-unproven")?;
            if !is_supported_code_path(path) {
                continue;
            }
            let in_index = indexed.contains(path);
            let excluded = if in_index {
                // A source newly unmasked by local rules is extra, even when
                // its tracked bytes match and it has just been fully indexed.
                self.was_excluded_at_head(walk, path, &mut admitted_rules, None)?
            } else {
                if external.is_none() {
                    let raw = self.read_proposal_rule(".git/info/exclude")?;
                    external = Some(
                        RuleFrame::compile(b"", raw.as_deref(), &mut self.work)
                            .map_err(|_| "index-input-membership-unproven")?,
                    );
                }
                self.was_excluded_at_head(walk, path, &mut omitted_rules, external.as_ref())?
            };
            if excluded == in_index {
                return Err("index-input-membership-unproven");
            }
        }
        Ok(())
    }

    /// Reuse strict discovery precedence over verified committed rule blobs.
    /// For omissions, also establish that applicable live and staged rules
    /// agree with HEAD and that local exclude cannot reverse the exclusion.
    #[cfg(any(windows, target_os = "linux"))]
    fn was_excluded_at_head<'tree>(
        &mut self,
        walk: &super::TreeIndexWalk<'a>,
        path: &'tree str,
        cache: &mut HeadRules<'a, 'tree>,
        external: Option<&RuleFrame<'a, 'static>>,
    ) -> PackResult<bool> {
        let mut parents = Vec::new();
        let mut parent = "";
        let ends = path
            .match_indices('/')
            .map(|(end, _)| (end, true))
            .chain(std::iter::once((path.len(), false)));
        for (end, directory) in ends {
            let candidate = &path[..end];
            if let std::collections::btree_map::Entry::Vacant(entry) = cache.entry(parent) {
                let verify_live = external.is_some();
                entry.insert([
                    self.committed_rule(walk, parent, ".ignore", verify_live)?,
                    self.committed_rule(walk, parent, ".gitignore", verify_live)?,
                ]);
            }
            parents.push(parent);
            let mut selected = None;
            let rules_by_parent: &HeadRules<'a, 'tree> = cache;
            let frames = (0..2).flat_map(|lane| {
                parents.iter().rev().filter_map(move |parent| {
                    rules_by_parent
                        .get(*parent)
                        .and_then(|rules| rules[lane].as_ref())
                })
            });
            for frame in frames {
                if let Some(matches) = frame
                    .decide(candidate.as_bytes(), directory, &mut self.work)
                    .map_err(|_| "index-input-membership-unproven")?
                {
                    selected = Some(matches);
                    break;
                }
            }
            if classify_selected(candidate, directory, false, selected, &mut self.work)
                .map_err(|_| "index-input-membership-unproven")?
                == DiscoveryEntry::Prune
            {
                let live = match (selected, external) {
                    (None, Some(external)) => external
                        .decide(candidate.as_bytes(), directory, &mut self.work)
                        .map_err(|_| "index-input-membership-unproven")?,
                    _ => return Ok(true),
                };
                return classify_selected(candidate, directory, false, live, &mut self.work)
                    .map(|entry| entry == DiscoveryEntry::Prune)
                    .map_err(|_| "index-input-membership-unproven");
            }
            parent = candidate;
        }
        Ok(false)
    }

    #[cfg(any(windows, target_os = "linux"))]
    fn committed_rule<'tree>(
        &mut self,
        walk: &super::TreeIndexWalk<'a>,
        parent: &'tree str,
        name: &str,
        verify_live: bool,
    ) -> PackResult<Option<RuleFrame<'a, 'tree>>> {
        use super::super::root_ignore::MAX_IGNORE_FILE_BYTES;

        self.work.charge(parent.len() + name.len() + 1)?;
        let rule = if parent.is_empty() {
            name.to_owned()
        } else {
            format!("{parent}/{name}")
        };
        let live = if verify_live {
            self.read_proposal_rule(&rule)?
        } else {
            None
        };
        let Some(file) = walk.exact_file(rule.as_bytes(), &mut self.work)? else {
            if live.is_some()
                || walk
                    .expected_entry(rule.as_bytes(), &mut self.work)?
                    .is_some()
            {
                return Err("index-input-membership-unproven");
            }
            return Ok(None);
        };
        if file.checkout_ambiguous {
            return Err("index-input-membership-unproven");
        }
        let raw = self.object_with_pack_limit(
            file.id,
            gix::objs::Kind::Blob,
            MAX_IGNORE_FILE_BYTES,
            Some(MAX_IGNORE_FILE_BYTES),
        )?;
        if verify_live {
            self.confirm_proposal_staged_path(walk, rule.as_bytes())?;
            if live.as_deref() != Some(raw.as_ref()) {
                return Err("index-input-membership-unproven");
            }
        }
        RuleFrame::compile(parent.as_bytes(), Some(raw.as_ref()), &mut self.work)
            .map(Some)
            .map_err(|_| "index-input-membership-unproven")
    }

    #[cfg(any(windows, target_os = "linux"))]
    fn read_proposal_rule(&mut self, path: &str) -> PackResult<Option<Vec<u8>>> {
        use super::super::root_ignore::MAX_IGNORE_FILE_BYTES;
        match self.root.read(path, MAX_IGNORE_FILE_BYTES, &mut self.io) {
            Ok(raw) => Ok(Some(raw)),
            Err(super::ReadError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(None)
            }
            Err(error) => Err(super::git_read_error(error)),
        }
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
            let (blob, _) = self.match_tracked_blob(
                witness.path,
                entry.id,
                entry.executable,
                policy,
                None,
                None,
            )?;
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
