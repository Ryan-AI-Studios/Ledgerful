//! Admit the supported local Git configuration from caller-owned bytes.
use gix::bstr::ByteSlice;
use gix::config::{self, tree::Core};

pub(crate) const MAX_LOCAL_GIT_CONFIG_BYTES: usize = 16 * 1_024;

const CORE: [&str; 9] = [
    "repositoryformatversion",
    "bare",
    "filemode",
    "ignorecase",
    "symlinks",
    "autocrlf",
    "eol",
    "safecrlf",
    "logallrefupdates",
];
const EXTENSIONS: [&str; 2] = ["objectformat", "refstorage"];
const REMOTE: [&str; 2] = ["url", "fetch"];
const BRANCH: [&str; 2] = ["remote", "merge"];

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct LocalGitPolicy {
    pub(crate) file_mode: Option<bool>,
    pub(crate) ignore_case: Option<bool>,
    pub(crate) symlinks: Option<bool>,
}

#[derive(Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum LocalGitConfigError {
    #[error("local Git configuration exceeded its byte limit")]
    Bytes,
    #[error("local Git configuration contains an invalid value")]
    Invalid,
    #[error("local Git configuration repeats a singleton field")]
    Duplicate,
    #[error("local Git configuration is outside the supported repository policy")]
    Unsupported,
}

/// The caller supplies only the bounded contained `.git/config` snapshot.
/// Successful admission establishes local policy for SHA-1 and files refs.
pub(crate) fn admit_local_git_config(bytes: &[u8]) -> Result<LocalGitPolicy, LocalGitConfigError> {
    if bytes.len() > MAX_LOCAL_GIT_CONFIG_BYTES {
        return Err(LocalGitConfigError::Bytes);
    }
    std::str::from_utf8(bytes).map_err(|_| LocalGitConfigError::Invalid)?;
    if bytes.contains(&0) {
        return Err(LocalGitConfigError::Invalid);
    }
    let config = config::File::from_bytes_no_includes(
        bytes,
        config::file::Metadata::api(),
        config::file::init::Options {
            lossy: true,
            ..Default::default()
        },
    )
    .map_err(|_| LocalGitConfigError::Invalid)?;
    check_fields(&config)?;

    let format_version = config
        .integer("core.repositoryformatversion")
        .transpose()
        .map_err(|_| LocalGitConfigError::Invalid)?;
    if !matches!(format_version, Some(0 | 1)) || boolean(&config, "core.bare")? != Some(false) {
        return Err(LocalGitConfigError::Unsupported);
    }
    for (key, supported) in [
        ("extensions.objectformat", "sha1"),
        ("extensions.refstorage", "files"),
    ] {
        if let Some(value) = config.string(key)
            && (format_version != Some(1) || value.as_ref() != supported)
        {
            return Err(LocalGitConfigError::Unsupported);
        }
    }

    config
        .string("core.autocrlf")
        .map(|value| Core::AUTO_CRLF.try_into_autocrlf(value))
        .transpose()
        .map_err(|_| LocalGitConfigError::Invalid)?;
    config
        .string("core.eol")
        .map(|value| Core::EOL.try_into_eol(value))
        .transpose()
        .map_err(|_| LocalGitConfigError::Invalid)?;
    config
        .string("core.safecrlf")
        .map(|value| Core::SAFE_CRLF.try_into_safecrlf(value))
        .transpose()
        .map_err(|_| LocalGitConfigError::Invalid)?;
    Core::LOG_ALL_REF_UPDATES
        .try_into_ref_updates(config.boolean("core.logallrefupdates"))
        .map_err(|_| LocalGitConfigError::Invalid)?;

    Ok(LocalGitPolicy {
        file_mode: boolean(&config, "core.filemode")?,
        ignore_case: boolean(&config, "core.ignorecase")?,
        symlinks: boolean(&config, "core.symlinks")?,
    })
}

#[cfg(any(windows, target_os = "linux"))]
pub(super) const MAX_DISCOVERY_HOME_CONFIG_BYTES: usize = 8 * 1024;

/// Commands and pathname values in this file aren't resolved here. The pinned
/// ignore resolver searches raw config lines, so a typed parse alone is insufficient.
#[cfg(any(windows, target_os = "linux"))]
pub(super) fn admit_discovery_home_config(bytes: &[u8]) -> bool {
    if bytes.len() > MAX_DISCOVERY_HOME_CONFIG_BYTES
        || std::str::from_utf8(bytes).is_err()
        || bytes.contains(&0)
        || bytes
            .windows(b"excludesfile".len())
            .any(|part| part.eq_ignore_ascii_case(b"excludesfile"))
    {
        return false;
    }
    let Ok(config) = config::File::from_bytes_no_includes(
        bytes,
        config::file::Metadata::api(),
        config::file::init::Options {
            lossy: true,
            ..Default::default()
        },
    ) else {
        return false;
    };
    for section in config.sections() {
        let header = section.header();
        if header.subsection_name().is_some() {
            return false;
        }
        let Ok(section_name) = header.name().to_str() else {
            return false;
        };
        let keys: &[&str] = if section_name.eq_ignore_ascii_case("user") {
            &["name", "email", "signingkey"]
        } else if section_name.eq_ignore_ascii_case("init") {
            &["defaultbranch"]
        } else if section_name.eq_ignore_ascii_case("core") {
            &[
                "editor",
                "pager",
                "autocrlf",
                "eol",
                "filemode",
                "safecrlf",
                "ignorecase",
            ]
        } else {
            return false;
        };
        for key in section.body().value_names() {
            if !keys
                .iter()
                .any(|allowed| key.as_ref().eq_ignore_ascii_case(allowed))
            {
                return false;
            }
        }
    }
    true
}

fn check_fields(config: &config::File<'_>) -> Result<(), LocalGitConfigError> {
    let mut seen_core = [false; CORE.len()];
    let mut seen_extensions = [false; EXTENSIONS.len()];
    for section in config.sections() {
        let header = section.header();
        let name = header
            .name()
            .to_str()
            .map_err(|_| LocalGitConfigError::Invalid)?;
        let (allowed, mut seen): (&[&str], Option<&mut [bool]>) = match header.subsection_name() {
            None if name.eq_ignore_ascii_case("core") => (&CORE, Some(&mut seen_core)),
            None if name.eq_ignore_ascii_case("extensions") => {
                (&EXTENSIONS, Some(&mut seen_extensions))
            }
            Some(sub) if !sub.is_empty() && name.eq_ignore_ascii_case("remote") => (&REMOTE, None),
            Some(sub) if !sub.is_empty() && name.eq_ignore_ascii_case("branch") => (&BRANCH, None),
            _ => return Err(LocalGitConfigError::Unsupported),
        };
        for value_name in section.body().value_names() {
            let slot = allowed
                .iter()
                .position(|key| value_name.as_ref().eq_ignore_ascii_case(key))
                .ok_or(LocalGitConfigError::Unsupported)?;
            if let Some(seen) = seen.as_deref_mut() {
                if std::mem::replace(&mut seen[slot], true) {
                    return Err(LocalGitConfigError::Duplicate);
                }
                if section.body().value(allowed[slot]).is_none() {
                    return Err(LocalGitConfigError::Invalid);
                }
            }
        }
    }
    Ok(())
}

fn boolean(config: &config::File<'_>, key: &str) -> Result<Option<bool>, LocalGitConfigError> {
    config
        .boolean(key)
        .transpose()
        .map_err(|_| LocalGitConfigError::Invalid)
}
