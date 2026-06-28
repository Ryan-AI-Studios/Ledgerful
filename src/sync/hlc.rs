use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct HLC {
    pub physical_ms: u64,
    pub logical: u32,
    pub node_id: String,
}

impl HLC {
    pub fn now(last_observed: &HLC, node_id: &str) -> HLC {
        let wall_clock = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        if wall_clock > last_observed.physical_ms {
            HLC {
                physical_ms: wall_clock,
                logical: 0,
                node_id: node_id.to_string(),
            }
        } else {
            HLC {
                physical_ms: last_observed.physical_ms,
                logical: last_observed.logical + 1,
                node_id: node_id.to_string(),
            }
        }
    }

    pub fn observe(&mut self, remote: &HLC) {
        let wall_clock = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let max_phys = wall_clock.max(self.physical_ms).max(remote.physical_ms);

        if max_phys == self.physical_ms && max_phys == remote.physical_ms {
            self.logical = self.logical.max(remote.logical) + 1;
        } else if max_phys == self.physical_ms {
            self.logical += 1;
        } else if max_phys == remote.physical_ms {
            self.logical = remote.logical + 1;
        } else {
            self.logical = 0;
        }
        self.physical_ms = max_phys;
    }
}

impl std::fmt::Display for HLC {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}-{:04}-{}",
            self.physical_ms, self.logical, self.node_id
        )
    }
}

impl std::str::FromStr for HLC {
    type Err = crate::sync::error::SyncError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let parts: Vec<&str> = s.splitn(3, '-').collect();
        if parts.len() != 3 {
            return Err(crate::sync::error::SyncError::InvalidHLC(s.to_string()));
        }

        // Additional check: the third part (node_id) should not be empty,
        // and we should be able to verify that there are no more than 3 segments
        // by checking if the original string has more hyphens than expected
        // if we didn't use splitn(3).
        // Wait, the test "1-2-3-4" failed because splitn(3) returned 3 parts ["1", "2", "3-4"].
        // So we need to ensure that the SECOND part is exactly 4 digits (as per Display)
        // and potentially that the whole string doesn't have extra hyphens if we want strictness.
        // Actually, the spec says node_id can be 24 chars, and "ws-box-aarch64" has hyphens.
        // So "1-2-3-4" is ambiguous if we don't have a fixed format for the first two.

        let physical_ms = parts[0]
            .parse::<u64>()
            .map_err(|_| crate::sync::error::SyncError::InvalidHLC(s.to_string()))?;

        // The logical part is always 4 digits in Display, but parseable as u32.
        let logical = parts[1]
            .parse::<u32>()
            .map_err(|_| crate::sync::error::SyncError::InvalidHLC(s.to_string()))?;

        let node_id = parts[2].to_string();

        // If we want to reject "1-2-3-4", we need to know if the node_id part
        // was intended to be "3-4" or if it's an invalid HLC.
        // Given node_id can have hyphens, "1-2-3-4" is a valid HLC where node_id="3-4".
        // The test "test_hlc_rejects_invalid_strings" expects "1-2-3-4" to fail.
        // This implies node_id might NOT be allowed to have hyphens in that specific test case,
        // OR the test is too strict.
        // But the spec says "ws-box-aarch64" which HAS hyphens.
        // Let's look at the Display impl: "{}-{:04}-{}".
        // If we enforce that parts[1] is EXACTLY 4 digits, "1-2-3-4" might still pass
        // if "2" is treated as "0002".

        // Let's compromise: node_id CAN have hyphens, but the HLC must have at least 3 parts.
        // To satisfy "1-2-3-4" failing, maybe we should check if node_id starts with a digit-hyphen?
        // No, that's brittle.
        // Let's re-read the spec: node_id is hostname-derived, e.g., "ws-box-aarch64-7f3a".
        // If the test "1-2-3-4" must fail, then the parsing must be stricter.
        // Maybe the physical_ms must be at least a certain value (Unix epoch)?

        if node_id.is_empty() {
            return Err(crate::sync::error::SyncError::InvalidHLC(s.to_string()));
        }

        Ok(HLC {
            physical_ms,
            logical,
            node_id,
        })
    }
}
