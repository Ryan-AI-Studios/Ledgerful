//! Bounded projection of the local configuration used by baseline analysis.
use crate::config::load::{parse_project_toml, validate_local_config};

#[derive(Debug, thiserror::Error)]
pub(crate) enum LocalConfigError {
    #[error("project configuration exceeded its byte limit")]
    Bytes,
    #[error("project configuration is not supported UTF-8 TOML")]
    Invalid,
}

/// Project the field consumed by the landed baseline extractor. The caller
/// supplies bytes from a contained local file, with no default-file fallback.
pub(crate) fn decode_index_stale_threshold_days(
    bytes: &[u8],
    byte_limit: usize,
) -> Result<u64, LocalConfigError> {
    if bytes.len() > byte_limit {
        return Err(LocalConfigError::Bytes);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| LocalConfigError::Invalid)?;
    let (mut config, _) = parse_project_toml(text).map_err(|_| LocalConfigError::Invalid)?;
    validate_local_config(&mut config, false).map_err(|_| LocalConfigError::Invalid)?;
    Ok(config.index.stale_threshold_days)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projects_the_shared_typed_index_default_and_local_override() {
        assert_eq!(decode_index_stale_threshold_days(b"", 1_024).unwrap(), 3);
        assert_eq!(
            decode_index_stale_threshold_days(
                b"[watch]\ndebounce_ms = 200\n[index]\nstale_threshold_days = 17\n",
                1_024,
            )
            .unwrap(),
            17
        );
    }

    #[test]
    fn rejects_oversized_malformed_and_duplicate_local_data() {
        assert!(matches!(
            decode_index_stale_threshold_days(b"[index]\n", 2),
            Err(LocalConfigError::Bytes)
        ));
        for malformed in [
            b"\xff".as_slice(),
            b"[index]\nstale_threshold_days = -1",
            b"[index]\nstale_threshold_days = 1\nstale_threshold_days = 2",
            b"[watch]\ndebounce_ms = 0\n[index]\nstale_threshold_days = 17\n",
        ] {
            assert!(matches!(
                decode_index_stale_threshold_days(malformed, 1_024),
                Err(LocalConfigError::Invalid)
            ));
        }
    }

    #[test]
    fn matches_legacy_sanitation_for_an_ignored_verify_step() {
        let config = b"[index]\nstale_threshold_days = 17\n[[verify.steps]]\ndescription = 'skip'\ncommand = ' '\n";
        assert_eq!(
            decode_index_stale_threshold_days(config, 1_024).unwrap(),
            17
        );
    }
}
