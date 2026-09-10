//! `update --binary`: Latest archive for release installs; cargo for engine dogfood.

use crate::commands::doctor::binary_latest::{GITHUB_OWNER_REPO, parse_release_tag_name};
use crate::commands::doctor::is_ledgerful_engine_worktree;
use crate::util::network::network_disabled_from_env;
use crate::util::path::ensure_path_within_root;
use miette::{IntoDiagnostic, Result, miette};
use owo_colors::{OwoColorize, Stream, Style};
use sha2::{Digest, Sha256};
use std::env;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use tracing::info;

/// Same names as 0201 `ARCHIVE_*` (copy; do not mutate pins).
pub(crate) const ARCHIVE_WINDOWS: &str = "ledgerful-x86_64-pc-windows-msvc.zip";
pub(crate) const ARCHIVE_LINUX: &str = "ledgerful-x86_64-unknown-linux-gnu.tar.gz";
pub(crate) const ARCHIVE_MAC_X64: &str = "ledgerful-x86_64-apple-darwin.tar.gz";
pub(crate) const ARCHIVE_MAC_ARM: &str = "ledgerful-aarch64-apple-darwin.tar.gz";

const GITHUB_API_VERSION: &str = "2022-11-28";
const METADATA_TIMEOUT: Duration = Duration::from_secs(2);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_SIDECAR_BYTES: u64 = 8 * 1024;
const MAX_ARCHIVE_BYTES: u64 = 80 * 1024 * 1024;
const MAX_EXTRACTED_BYTES: u64 = 150 * 1024 * 1024;
const GITHUB_DOWNLOAD_BASE: &str = "https://github.com";

const ZIP_FEATURES: bool = cfg!(any(feature = "export", feature = "web", feature = "sync"));

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BinaryUpdateMode {
    EngineCargo { root: PathBuf },
    ReleaseArchive,
}

#[derive(Debug, Clone)]
pub(crate) struct BinaryUpdateCtx {
    pub force: bool,
    pub force_unlock: bool,
    pub dry_run: bool,
    pub dest: Option<PathBuf>,
    pub fail_current_exe: bool,
    pub api_base: String,
    pub download_base: String,
    pub layout_root: Option<PathBuf>,
    pub cwd: Option<PathBuf>,
    pub target_triple: Option<String>,
}

impl BinaryUpdateCtx {
    fn production(force: bool, force_unlock: bool, dry_run: bool) -> Self {
        Self {
            force,
            force_unlock,
            dry_run,
            dest: None,
            fail_current_exe: false,
            api_base: crate::commands::doctor::binary_latest::GITHUB_API_BASE.to_string(),
            download_base: GITHUB_DOWNLOAD_BASE.to_string(),
            layout_root: None,
            cwd: None,
            target_triple: None,
        }
    }
}

pub(crate) fn execute_binary_update(force: bool, force_unlock: bool, dry_run: bool) -> Result<()> {
    let outcome =
        execute_binary_update_impl(&BinaryUpdateCtx::production(force, force_unlock, dry_run))?;
    if !outcome.is_empty() {
        print!("{outcome}");
        if !outcome.ends_with('\n') {
            println!();
        }
    }
    Ok(())
}

pub(crate) fn execute_binary_update_impl(ctx: &BinaryUpdateCtx) -> Result<String> {
    let mode = resolve_mode(ctx)?;
    match mode {
        BinaryUpdateMode::EngineCargo { root } => execute_engine_cargo(ctx, &root),
        BinaryUpdateMode::ReleaseArchive => execute_release_archive(ctx),
    }
}

pub(crate) fn binary_update_mode(layout_root: &Path, cwd: &Path) -> BinaryUpdateMode {
    if is_ledgerful_engine_worktree(layout_root) {
        BinaryUpdateMode::EngineCargo {
            root: layout_root.to_path_buf(),
        }
    } else if is_ledgerful_engine_worktree(cwd) {
        BinaryUpdateMode::EngineCargo {
            root: cwd.to_path_buf(),
        }
    } else {
        BinaryUpdateMode::ReleaseArchive
    }
}

pub(crate) fn archive_name_for_triple(triple: &str) -> Option<&'static str> {
    match triple {
        "x86_64-pc-windows-msvc" => Some(ARCHIVE_WINDOWS),
        "x86_64-unknown-linux-gnu" => Some(ARCHIVE_LINUX),
        "x86_64-apple-darwin" => Some(ARCHIVE_MAC_X64),
        "aarch64-apple-darwin" => Some(ARCHIVE_MAC_ARM),
        _ => None,
    }
}

pub(crate) fn parse_sha256_sidecar_body(body: &str) -> Option<String> {
    let token = body.split_whitespace().next()?;
    if token.len() == 64 && token.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(token.to_ascii_lowercase())
    } else {
        None
    }
}

fn resolve_mode(ctx: &BinaryUpdateCtx) -> Result<BinaryUpdateMode> {
    let cwd = match &ctx.cwd {
        Some(p) => p.clone(),
        None => env::current_dir().into_diagnostic()?,
    };
    let layout_root = match &ctx.layout_root {
        Some(p) => p.clone(),
        None => crate::commands::helpers::get_layout_or_cwd_if_not_git()?
            .root
            .into_std_path_buf(),
    };
    Ok(binary_update_mode(&layout_root, &cwd))
}

fn latest_page_url() -> String {
    format!("https://github.com/{GITHUB_OWNER_REPO}/releases/latest")
}

fn user_agent() -> String {
    format!("ledgerful/{}", env!("CARGO_PKG_VERSION"))
}

fn host_target_triple() -> &'static str {
    if cfg!(all(windows, target_arch = "x86_64")) {
        "x86_64-pc-windows-msvc"
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "x86_64-unknown-linux-gnu"
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "x86_64-apple-darwin"
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "aarch64-apple-darwin"
    } else {
        "unsupported"
    }
}

fn ctx_triple(ctx: &BinaryUpdateCtx) -> &str {
    ctx.target_triple
        .as_deref()
        .unwrap_or_else(|| host_target_triple())
}

fn archive_or_unsupported(ctx: &BinaryUpdateCtx) -> Result<&'static str> {
    let triple = ctx_triple(ctx);
    archive_name_for_triple(triple).ok_or_else(|| {
        miette!(
            "This platform ({triple}) has no published Ledgerful archive.\n\
             Published archives:\n  {}\n  {}\n  {}\n  {}\nSee {}",
            ARCHIVE_WINDOWS,
            ARCHIVE_LINUX,
            ARCHIVE_MAC_X64,
            ARCHIVE_MAC_ARM,
            latest_page_url()
        )
    })
}

fn resolve_release_dest(ctx: &BinaryUpdateCtx) -> Result<PathBuf> {
    if ctx.fail_current_exe {
        return Err(release_current_exe_err());
    }
    if let Some(dest) = &ctx.dest {
        return Ok(dest.clone());
    }
    // Legitimate: replace this install. Release path has no cwd-relative fallback.
    // nosemgrep: rust.lang.security.current-exe.current-exe
    env::current_exe().map_err(|_| release_current_exe_err())
}

fn release_current_exe_err() -> miette::Report {
    miette!(
        "Could not resolve this install's executable path. \
         Refusing a cwd-relative ledgerful.exe.\nSee {}",
        latest_page_url()
    )
}

fn resolve_engine_dest(ctx: &BinaryUpdateCtx) -> PathBuf {
    if let Some(dest) = &ctx.dest {
        return dest.clone();
    }
    // Legitimate: self-path for in-place cargo dogfood of this install.
    // nosemgrep: rust.lang.security.current-exe.current-exe
    env::current_exe().unwrap_or_else(|e| {
        tracing::warn!("Failed to get current executable path: {e}. Falling back to 'ledgerful'.");
        PathBuf::from(if cfg!(windows) {
            "ledgerful.exe"
        } else {
            "ledgerful"
        })
    })
}

fn execute_engine_cargo(ctx: &BinaryUpdateCtx, engine_root: &Path) -> Result<String> {
    let bin_path = resolve_engine_dest(ctx);
    let display_path = bin_path.display().to_string();

    if ctx.dry_run {
        return Ok(format!(
            "{} Would replace binary at {} with current source build (cargo install --path .).\n",
            "DRY-RUN".if_supports_color(Stream::Stdout, |s| s.style(Style::new().yellow().bold())),
            display_path.if_supports_color(Stream::Stdout, |s| s.cyan())
        ));
    }

    if ctx.force_unlock {
        crate::platform::process_policy::force_unlock_processes()?;
    }

    if fs::metadata(engine_root.join("Cargo.toml")).is_err() {
        return Err(miette!(
            "Engine cargo update requires Cargo.toml in the resolved engine worktree."
        ));
    }

    let header = format!(
        "Replacing {} with current source build...\n",
        display_path.if_supports_color(Stream::Stdout, |s| s.cyan())
    );
    print!("{header}");
    let _ = std::io::Write::flush(&mut std::io::stdout());
    info!(
        "Running 'cargo install --path .' in {}",
        engine_root.display()
    );

    if fs::OpenOptions::new().write(true).open(&bin_path).is_err() {
        println!("Warning: Ledgerful binary is currently locked by another process.");
        println!("Please close any other running instances or daemon processes before continuing.");
        println!("(Attempting shadow-copy anyway...)");
    }

    let old_path_opt = shadow_copy_path(&bin_path);

    let mut cmd = cargo_install_command();
    cmd.current_dir(engine_root);
    cmd.args(["install", "--path", "."]);
    if ctx.force {
        cmd.arg("--force");
    }

    let status = cmd.status().into_diagnostic()?;

    if status.success() {
        let mut rest = format!(
            "{} Ledgerful updated successfully.\n",
            "DONE".if_supports_color(Stream::Stdout, |s| s.style(Style::new().green().bold()))
        );
        if let Some(old_path) = old_path_opt {
            info!("Stale binary moved to: {}", old_path.display());
            rest.push_str(&format!(
                "{} Stale binary will be cleaned up on next startup.\n",
                "INFO:".if_supports_color(Stream::Stdout, |s| s.style(Style::new().blue().bold()))
            ));
        }
        Ok(rest)
    } else {
        if let Some(old_path) = old_path_opt {
            let _ = fs::rename(old_path, bin_path);
        }
        Err(miette!("Update failed. See above for errors."))
    }
}

fn execute_release_archive(ctx: &BinaryUpdateCtx) -> Result<String> {
    let archive = archive_or_unsupported(ctx)?;
    let dest = resolve_release_dest(ctx)?;
    let display_path = dest.display().to_string();

    if ctx.dry_run {
        return release_dry_run(ctx, archive, &display_path);
    }

    if ctx.force_unlock {
        crate::platform::process_policy::force_unlock_processes()?;
    }

    if network_disabled_from_env() {
        return Err(miette!(
            "Network disabled (LEDGERFUL_NO_NETWORK). Cannot download Latest.\nSee {}",
            latest_page_url()
        ));
    }

    if !ZIP_FEATURES && archive.ends_with(".zip") {
        return Err(miette!(
            "This build was compiled without zip support. Cannot extract {archive}.\nSee {}",
            latest_page_url()
        ));
    }

    let tag = fetch_latest_tag(&ctx.api_base).map_err(|e| {
        miette!(
            "Failed to resolve GitHub Latest ({e}).\nSee {}",
            latest_page_url()
        )
    })?;
    let download_base = ctx.download_base.trim_end_matches('/');
    let archive_url =
        format!("{download_base}/{GITHUB_OWNER_REPO}/releases/download/{tag}/{archive}");
    let sidecar_url = format!("{archive_url}.sha256");

    let sidecar_bytes = http_get_bytes(&sidecar_url, MAX_SIDECAR_BYTES).map_err(|e| {
        miette!(
            "Failed to download {archive}.sha256 ({e}).\nSee {}",
            latest_page_url()
        )
    })?;
    let sidecar_text = String::from_utf8_lossy(&sidecar_bytes);
    let expected = parse_sha256_sidecar_body(&sidecar_text).ok_or_else(|| {
        miette!(
            "Published {archive}.sha256 sidecar is missing a 64-hex SHA-256 token.\nSee {}",
            latest_page_url()
        )
    })?;

    let archive_bytes = http_get_bytes(&archive_url, MAX_ARCHIVE_BYTES).map_err(|e| {
        miette!(
            "Failed to download {archive} ({e}).\nSee {}",
            latest_page_url()
        )
    })?;
    let actual = sha256_hex(&archive_bytes);
    if actual != expected {
        return Err(miette!(
            "Published SHA-256 does not match the downloaded archive. \
             Destination left unchanged.\nSee {}",
            latest_page_url()
        ));
    }

    let payload = extract_release_payload(archive, &archive_bytes, &dest)?;
    replace_dest_bytes(&dest, &payload)?;

    Ok(format!(
        "{} Ledgerful updated successfully from GitHub Latest {tag}.\n",
        "DONE".if_supports_color(Stream::Stdout, |s| s.style(Style::new().green().bold()))
    ))
}

fn release_dry_run(ctx: &BinaryUpdateCtx, archive: &str, display_path: &str) -> Result<String> {
    let prefix = format!(
        "{} Would replace binary at {}",
        "DRY-RUN".if_supports_color(Stream::Stdout, |s| s.style(Style::new().yellow().bold())),
        display_path.if_supports_color(Stream::Stdout, |s| s.cyan())
    );
    match fetch_latest_tag(&ctx.api_base) {
        Ok(tag) => {
            let download_base = ctx.download_base.trim_end_matches('/');
            let archive_url =
                format!("{download_base}/{GITHUB_OWNER_REPO}/releases/download/{tag}/{archive}");
            Ok(format!(
                "{prefix} from GitHub Latest {tag}:\n  {archive_url}\n  {archive_url}.sha256\n"
            ))
        }
        Err(_) => Ok(format!(
            "{prefix} from GitHub Latest:\n  {}\n  {}\n  {}.sha256\n  {}\n  {}.sha256\n  {}\n  {}.sha256\n  {}\n  {}.sha256\n",
            latest_page_url(),
            ARCHIVE_WINDOWS,
            ARCHIVE_WINDOWS,
            ARCHIVE_LINUX,
            ARCHIVE_LINUX,
            ARCHIVE_MAC_X64,
            ARCHIVE_MAC_X64,
            ARCHIVE_MAC_ARM,
            ARCHIVE_MAC_ARM
        )),
    }
}

fn fetch_latest_tag(api_base: &str) -> Result<String, String> {
    if network_disabled_from_env() {
        return Err("network disabled".to_string());
    }
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(METADATA_TIMEOUT)
        .timeout_read(METADATA_TIMEOUT)
        .build();
    let url = format!(
        "{}/repos/{GITHUB_OWNER_REPO}/releases/latest",
        api_base.trim_end_matches('/')
    );
    let json = github_get_json(&agent, &url)?;
    parse_release_tag_name(&json).ok_or_else(|| "empty tag_name".to_string())
}

fn github_get_json(agent: &ureq::Agent, url: &str) -> Result<serde_json::Value, String> {
    let ua = user_agent();
    let resp = agent
        .get(url)
        .set("User-Agent", &ua)
        .set("Accept", "application/vnd.github+json")
        .set("X-GitHub-Api-Version", GITHUB_API_VERSION)
        .call()
        .map_err(|e| match e {
            ureq::Error::Status(code, _resp) => format!("HTTP {code}"),
            ureq::Error::Transport(inner) => inner.to_string(),
        })?;
    resp.into_json().map_err(|_| "invalid JSON".to_string())
}

fn http_get_bytes(url: &str, max_len: u64) -> Result<Vec<u8>, String> {
    if network_disabled_from_env() {
        return Err("network disabled".to_string());
    }
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(DOWNLOAD_TIMEOUT)
        .build();
    let ua = user_agent();
    let resp = agent
        .get(url)
        .timeout(DOWNLOAD_TIMEOUT)
        .set("User-Agent", &ua)
        .call()
        .map_err(|e| match e {
            ureq::Error::Status(code, _resp) => format!("HTTP {code}"),
            ureq::Error::Transport(inner) => inner.to_string(),
        })?;
    let mut reader = resp.into_reader().take(max_len.saturating_add(1));
    let mut buf = Vec::new();
    reader.read_to_end(&mut buf).map_err(|e| e.to_string())?;
    if (buf.len() as u64) > max_len {
        return Err(format!("body exceeds {max_len} bytes"));
    }
    Ok(buf)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

fn extract_release_payload(archive: &str, bytes: &[u8], dest: &Path) -> Result<Vec<u8>> {
    if archive.ends_with(".zip") {
        extract_zip_root_exe(bytes)
    } else if archive.ends_with(".tar.gz") {
        extract_unix_tarball(archive, bytes, dest)
    } else {
        Err(miette!(
            "Unsupported archive name {archive}.\nSee {}",
            latest_page_url()
        ))
    }
}

#[cfg(any(feature = "export", feature = "web", feature = "sync"))]
fn extract_zip_root_exe(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|e| miette!("Failed to open Latest zip: {e}\nSee {}", latest_page_url()))?;
    let mut file = archive.by_name("ledgerful.exe").map_err(|e| {
        miette!(
            "Latest zip is missing ledgerful.exe at the archive root ({e}).\nSee {}",
            latest_page_url()
        )
    })?;
    if file.name() != "ledgerful.exe" {
        return Err(miette!(
            "Latest zip must contain ledgerful.exe at the archive root.\nSee {}",
            latest_page_url()
        ));
    }
    if file.size() > MAX_ARCHIVE_BYTES {
        return Err(miette!(
            "ledgerful.exe in Latest zip exceeds the size cap.\nSee {}",
            latest_page_url()
        ));
    }
    let mut payload = Vec::new();
    file.read_to_end(&mut payload).into_diagnostic()?;
    Ok(payload)
}

#[cfg(not(any(feature = "export", feature = "web", feature = "sync")))]
fn extract_zip_root_exe(_bytes: &[u8]) -> Result<Vec<u8>> {
    Err(miette!(
        "This build was compiled without zip support. Cannot extract a Windows archive.\nSee {}",
        latest_page_url()
    ))
}

fn unix_tar_member(stem: &str) -> String {
    format!("{stem}/ledgerful")
}

fn extract_unix_tarball(archive: &str, bytes: &[u8], dest: &Path) -> Result<Vec<u8>> {
    extract_unix_tarball_with_cap(archive, bytes, dest, MAX_EXTRACTED_BYTES)
}

fn extract_unix_tarball_with_cap(
    archive: &str,
    bytes: &[u8],
    dest: &Path,
    max_extracted: u64,
) -> Result<Vec<u8>> {
    let stem = archive
        .strip_suffix(".tar.gz")
        .ok_or_else(|| miette!("Invalid tarball name {archive}"))?;
    let member = unix_tar_member(stem);
    let staging = unique_staging_dir(dest)?;
    fs::create_dir_all(&staging).into_diagnostic()?;
    let archive_path = staging.join("archive.tar.gz");
    let write_result = (|| -> Result<Vec<u8>> {
        fs::write(&archive_path, bytes).into_diagnostic()?;
        let status = Command::new("tar")
            .arg("-xzf")
            .arg(&archive_path)
            .arg(&member)
            .current_dir(&staging)
            .status()
            .map_err(|_| {
                miette!(
                    "tar is required to extract {archive}.\nSee {}",
                    latest_page_url()
                )
            })?;
        let nested = staging.join(stem).join("ledgerful");
        let meta = match fs::symlink_metadata(&nested) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(miette!(
                    "Latest tarball is missing {member}.\nSee {}",
                    latest_page_url()
                ));
            }
            Err(e) => return Err(e).into_diagnostic(),
            Ok(m) => m,
        };
        if !status.success() {
            return Err(miette!(
                "tar failed to extract {archive}.\nSee {}",
                latest_page_url()
            ));
        }
        let stem_dir = staging.join(stem);
        if let Ok(stem_meta) = fs::symlink_metadata(&stem_dir)
            && stem_meta.file_type().is_symlink()
        {
            return Err(miette!(
                "Latest tarball member {member} is not a regular file.\nSee {}",
                latest_page_url()
            ));
        }
        if meta.file_type().is_symlink() || !meta.file_type().is_file() {
            return Err(miette!(
                "Latest tarball member {member} is not a regular file.\nSee {}",
                latest_page_url()
            ));
        }
        if let Err(e) = ensure_path_within_root(&staging, &nested) {
            return Err(miette!("{e}"));
        }
        if meta.len() > max_extracted {
            return Err(miette!(
                "ledgerful in Latest tarball exceeds the extracted size cap.\nSee {}",
                latest_page_url()
            ));
        }
        let file = fs::File::open(&nested).into_diagnostic()?;
        let mut limited = file.take(max_extracted.saturating_add(1));
        let mut payload = Vec::new();
        limited.read_to_end(&mut payload).into_diagnostic()?;
        if payload.len() as u64 > max_extracted {
            return Err(miette!(
                "ledgerful in Latest tarball exceeds the extracted size cap.\nSee {}",
                latest_page_url()
            ));
        }
        Ok(payload)
    })();
    let _ = fs::remove_dir_all(&staging);
    write_result
}

fn unique_staging_dir(dest: &Path) -> Result<PathBuf> {
    let parent = dest.parent().ok_or_else(|| {
        miette!(
            "Destination path has no parent directory for extract staging.\nSee {}",
            latest_page_url()
        )
    })?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    Ok(parent.join(format!(".ledgerful-upd-{}-{nanos}", std::process::id())))
}

fn replace_dest_bytes(dest: &Path, payload: &[u8]) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let name = dest
            .file_name()
            .ok_or_else(|| miette!("Destination path has no file name"))?;
        let sibling = dest.with_file_name(format!(".{}.new", name.to_string_lossy()));
        fs::write(&sibling, payload).into_diagnostic()?;
        fs::set_permissions(&sibling, fs::Permissions::from_mode(0o755)).into_diagnostic()?;
        fs::rename(&sibling, dest).into_diagnostic()?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let old = shadow_copy_path(dest);
        if let Err(e) = fs::write(dest, payload) {
            if let Some(old_path) = old {
                let _ = fs::rename(old_path, dest);
            }
            return Err(e).into_diagnostic();
        }
        Ok(())
    }
}

fn cargo_install_command() -> Command {
    #[cfg(debug_assertions)]
    if let Some(path) = env::var_os("LEDGERFUL_TEST_CARGO_COMMAND") {
        return Command::new(path);
    }
    Command::new("cargo")
}

fn shadow_copy_path(bin_path: &Path) -> Option<PathBuf> {
    let mut old_path = bin_path.to_path_buf();
    let stem = bin_path.file_stem()?.to_string_lossy();
    match bin_path.extension() {
        Some(ext) => old_path.set_file_name(format!("{stem}.old.{}", ext.to_string_lossy())),
        None => old_path.set_file_name(format!("{stem}.old")),
    }
    if fs::rename(bin_path, &old_path).is_ok() {
        Some(old_path)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    #[cfg(any(feature = "export", feature = "web", feature = "sync"))]
    use std::io::Write;

    mod env_guard {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/integration/common/env_guard.rs"
        ));
    }
    use env_guard::TempEnv;

    fn engine_fixture(root: &Path) {
        fs::create_dir_all(root.join("src").join("cli").join("args")).unwrap();
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"ledgerful\"\nversion = \"0.2.12\"\n",
        )
        .unwrap();
        fs::write(
            root.join("src").join("cli").join("args").join("mod.rs"),
            "// stub",
        )
        .unwrap();
    }

    fn cargo_sentinel(dir: &Path) -> (PathBuf, PathBuf) {
        let sentinel = dir.join("cargo_called.txt");
        #[cfg(windows)]
        {
            let stub = dir.join("fake-cargo.cmd");
            fs::write(
                &stub,
                format!(
                    "@echo off\r\necho CARGO_CALLED>\"{}\"\r\nexit /b 1\r\n",
                    sentinel.display()
                ),
            )
            .unwrap();
            (stub, sentinel)
        }
        #[cfg(not(windows))]
        {
            let stub = dir.join("fake-cargo");
            fs::write(
                &stub,
                format!(
                    "#!/bin/sh\necho CARGO_CALLED > \"{}\"\nexit 1\n",
                    sentinel.display()
                ),
            )
            .unwrap();
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
            (stub, sentinel)
        }
    }

    fn release_ctx(tmp: &Path, dest: &Path, api: &str, download: &str) -> BinaryUpdateCtx {
        BinaryUpdateCtx {
            force: false,
            force_unlock: false,
            dry_run: false,
            dest: Some(dest.to_path_buf()),
            fail_current_exe: false,
            api_base: api.to_string(),
            download_base: download.to_string(),
            layout_root: Some(tmp.to_path_buf()),
            cwd: Some(tmp.to_path_buf()),
            target_triple: Some("x86_64-pc-windows-msvc".to_string()),
        }
    }

    fn mock_latest<'a>(server: &'a httpmock::MockServer, tag: &str) -> httpmock::Mock<'a> {
        let ua = user_agent();
        server.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path("/repos/Ryan-AI-Studios/Ledgerful/releases/latest")
                .header("User-Agent", ua.as_str());
            then.status(200)
                .header("content-type", "application/json")
                .body(format!(
                    r#"{{"tag_name":"{tag}","target_commitish":"main"}}"#
                ));
        })
    }

    #[cfg(any(feature = "export", feature = "web", feature = "sync"))]
    fn fixture_zip(payload: &[u8]) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            zip.start_file("ledgerful.exe", options).unwrap();
            zip.write_all(payload).unwrap();
            zip.finish().unwrap();
        }
        buf
    }

    #[test]
    fn update_binary_dry_run_engine_worktree_names_cargo() {
        let tmp = tempfile::tempdir().unwrap();
        engine_fixture(tmp.path());
        let dest = tmp.path().join("ledgerful.exe");
        let ctx = BinaryUpdateCtx {
            force: false,
            force_unlock: false,
            dry_run: true,
            dest: Some(dest),
            fail_current_exe: false,
            api_base: "http://127.0.0.1:9".to_string(),
            download_base: "http://127.0.0.1:9".to_string(),
            layout_root: Some(tmp.path().to_path_buf()),
            cwd: Some(tmp.path().to_path_buf()),
            target_triple: None,
        };
        let out = execute_binary_update_impl(&ctx).expect("engine dry-run");
        assert!(
            out.contains("cargo install --path ."),
            "engine dry-run must name cargo: {out}"
        );
        assert!(
            !out.contains("/releases/download/"),
            "engine dry-run must not name a download URL: {out}"
        );
    }

    #[test]
    fn update_binary_dry_run_engine_subdir_names_cargo() {
        let tmp = tempfile::tempdir().unwrap();
        engine_fixture(tmp.path());
        let sub = tmp.path().join("src").join("commands");
        fs::create_dir_all(&sub).unwrap();
        let dest = tmp.path().join("ledgerful.exe");
        let ctx = BinaryUpdateCtx {
            force: false,
            force_unlock: false,
            dry_run: true,
            dest: Some(dest),
            fail_current_exe: false,
            api_base: "http://127.0.0.1:9".to_string(),
            download_base: "http://127.0.0.1:9".to_string(),
            layout_root: Some(tmp.path().to_path_buf()),
            cwd: Some(sub),
            target_triple: None,
        };
        assert!(matches!(
            binary_update_mode(tmp.path(), &tmp.path().join("src").join("commands")),
            BinaryUpdateMode::EngineCargo { .. }
        ));
        let out = execute_binary_update_impl(&ctx).expect("subdir dry-run");
        assert!(
            out.contains("cargo install --path ."),
            "engine subdir dry-run must name cargo: {out}"
        );
    }

    #[test]
    fn update_binary_dry_run_release_install_names_latest_zip() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("dest.exe");
        let api = httpmock::MockServer::start();
        let download = httpmock::MockServer::start();
        let latest = mock_latest(&api, "v0.2.12");
        let peel = api.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path("/repos/Ryan-AI-Studios/Ledgerful/commits/v0.2.12");
            then.status(200).body("{}");
        });
        let sidecar = download.mock(|when, then| {
            when.method(httpmock::Method::GET).path(
                "/Ryan-AI-Studios/Ledgerful/releases/download/v0.2.12/ledgerful-x86_64-pc-windows-msvc.zip.sha256",
            );
            then.status(200).body("deadbeef");
        });
        let mut ctx = release_ctx(tmp.path(), &dest, &api.base_url(), &download.base_url());
        ctx.dry_run = true;
        let out = execute_binary_update_impl(&ctx).expect("release dry-run");
        assert!(
            out.contains(&format!(
                "{}/Ryan-AI-Studios/Ledgerful/releases/download/v0.2.12/{ARCHIVE_WINDOWS}",
                download.base_url()
            )),
            "dry-run must name constructed zip URL on download base: {out}"
        );
        assert!(
            !out.contains(&format!(
                "{}/Ryan-AI-Studios/Ledgerful/releases/download/",
                api.base_url()
            )),
            "download URLs must not use the API base: {out}"
        );
        assert!(
            out.contains(&format!("{ARCHIVE_WINDOWS}.sha256")),
            "dry-run must name sidecar URL: {out}"
        );
        assert!(
            !out.contains("/download/main/"),
            "must not use target_commitish: {out}"
        );
        latest.assert_calls(1);
        peel.assert_calls(0);
        sidecar.assert_calls(0);
    }

    #[test]
    #[serial(env)]
    fn update_binary_release_dry_run_zero_http_when_no_network() {
        let _net = TempEnv::set("LEDGERFUL_NO_NETWORK", "1");
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("dest.exe");
        let server = httpmock::MockServer::start();
        let latest = mock_latest(&server, "v0.2.12");
        let mut ctx = release_ctx(tmp.path(), &dest, &server.base_url(), &server.base_url());
        ctx.dry_run = true;
        let out = execute_binary_update_impl(&ctx).expect("no-network dry-run succeeds");
        latest.assert_calls(0);
        assert!(
            out.contains(&latest_page_url()),
            "no-network dry-run must name Latest page: {out}"
        );
    }

    #[test]
    #[serial(env)]
    fn update_binary_release_dry_run_no_network_prints_latest_page() {
        let _net = TempEnv::set("LEDGERFUL_NO_NETWORK", "1");
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("dest.exe");
        let mut ctx = release_ctx(
            tmp.path(),
            &dest,
            "http://127.0.0.1:9",
            "http://127.0.0.1:9",
        );
        ctx.dry_run = true;
        let out = execute_binary_update_impl(&ctx).expect("no-network dry-run");
        assert!(out.contains("https://github.com/Ryan-AI-Studios/Ledgerful/releases/latest"));
        assert!(out.contains(ARCHIVE_WINDOWS));
        assert!(out.contains(&format!("{ARCHIVE_WINDOWS}.sha256")), "{out}");
        assert!(out.contains(ARCHIVE_LINUX));
        assert!(out.contains(&format!("{ARCHIVE_LINUX}.sha256")), "{out}");
        assert!(out.contains(&format!("{ARCHIVE_MAC_X64}.sha256")), "{out}");
        assert!(out.contains(&format!("{ARCHIVE_MAC_ARM}.sha256")), "{out}");
        assert!(
            !out.contains("/releases/download/"),
            "must not invent a download tag URL: {out}"
        );
    }

    #[test]
    fn update_binary_never_uses_target_commitish() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("dest.exe");
        let server = httpmock::MockServer::start();
        let _latest = mock_latest(&server, "v0.2.12");
        let mut ctx = release_ctx(tmp.path(), &dest, &server.base_url(), &server.base_url());
        ctx.dry_run = true;
        let out = execute_binary_update_impl(&ctx).expect("dry-run");
        assert!(out.contains("/download/v0.2.12/"));
        assert!(!out.contains("/download/main/"));
        assert!(!out.contains("target_commitish"));
    }

    #[test]
    fn update_binary_unsupported_target_is_miette_without_http() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("dest.exe");
        let server = httpmock::MockServer::start();
        let latest = mock_latest(&server, "v0.2.12");
        let mut ctx = release_ctx(tmp.path(), &dest, &server.base_url(), &server.base_url());
        ctx.target_triple = Some("aarch64-unknown-linux-gnu".to_string());
        let err = execute_binary_update_impl(&ctx).expect_err("unsupported");
        let msg = format!("{err:?}");
        assert!(msg.contains(ARCHIVE_WINDOWS), "{msg}");
        assert!(msg.contains(ARCHIVE_LINUX), "{msg}");
        assert!(msg.contains(ARCHIVE_MAC_X64), "{msg}");
        assert!(msg.contains(ARCHIVE_MAC_ARM), "{msg}");
        assert!(msg.contains(&latest_page_url()), "{msg}");
        latest.assert_calls(0);
    }

    #[test]
    fn update_binary_release_current_exe_fail_is_hard_error() {
        let tmp = tempfile::tempdir().unwrap();
        let server = httpmock::MockServer::start();
        let latest = mock_latest(&server, "v0.2.12");
        let mut ctx = release_ctx(
            tmp.path(),
            &tmp.path().join("unused.exe"),
            &server.base_url(),
            &server.base_url(),
        );
        ctx.dest = None;
        ctx.fail_current_exe = true;
        let err = execute_binary_update_impl(&ctx).expect_err("hard error");
        let msg = format!("{err:?}");
        assert!(msg.contains(&latest_page_url()), "{msg}");
        assert!(
            msg.contains("cwd-relative") || msg.contains("executable path"),
            "{msg}"
        );
        latest.assert_calls(0);
    }

    #[test]
    #[serial(env)]
    fn update_binary_release_no_network_names_latest_url() {
        let _net = TempEnv::set("LEDGERFUL_NO_NETWORK", "1");
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("dest.exe");
        fs::write(&dest, b"keep-me").unwrap();
        let ctx = release_ctx(
            tmp.path(),
            &dest,
            "http://127.0.0.1:9",
            "http://127.0.0.1:9",
        );
        let err = execute_binary_update_impl(&ctx).expect_err("live no-network");
        let msg = format!("{err:?}");
        assert!(msg.contains(&latest_page_url()), "{msg}");
        assert!(!msg.contains("cargo install"), "{msg}");
        assert_eq!(fs::read(&dest).unwrap(), b"keep-me");
    }

    #[cfg(any(feature = "export", feature = "web", feature = "sync"))]
    #[test]
    #[serial(env)]
    fn update_binary_release_verifies_sha256_sidecar_body() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("dest.exe");
        fs::write(&dest, b"old-bytes").unwrap();
        let payload = b"fresh-ledgerful-exe";
        let zip = fixture_zip(payload);
        let hash = sha256_hex(&zip);
        let sidecar = format!("{hash}  {ARCHIVE_WINDOWS}\r\n");
        let api = httpmock::MockServer::start();
        let download = httpmock::MockServer::start();
        let latest = mock_latest(&api, "v0.2.12");
        download.mock(|when, then| {
            when.method(httpmock::Method::GET).path(format!(
                "/Ryan-AI-Studios/Ledgerful/releases/download/v0.2.12/{ARCHIVE_WINDOWS}.sha256"
            ));
            then.status(200).body(sidecar);
        });
        download.mock(|when, then| {
            when.method(httpmock::Method::GET).path(format!(
                "/Ryan-AI-Studios/Ledgerful/releases/download/v0.2.12/{ARCHIVE_WINDOWS}"
            ));
            then.status(200).body(zip);
        });
        let ctx = release_ctx(tmp.path(), &dest, &api.base_url(), &download.base_url());
        let out = execute_binary_update_impl(&ctx).expect("live replace");
        assert!(out.contains("v0.2.12"), "{out}");
        assert_eq!(fs::read(&dest).unwrap(), payload);
        latest.assert_calls(1);
    }

    #[cfg(any(feature = "export", feature = "web", feature = "sync"))]
    #[test]
    #[serial(env)]
    fn update_binary_release_refuses_hash_mismatch() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("dest.exe");
        fs::write(&dest, b"untouched").unwrap();
        let zip = fixture_zip(b"payload");
        let server = httpmock::MockServer::start();
        let _latest = mock_latest(&server, "v0.2.12");
        server.mock(|when, then| {
            when.method(httpmock::Method::GET).path(format!(
                "/Ryan-AI-Studios/Ledgerful/releases/download/v0.2.12/{ARCHIVE_WINDOWS}.sha256"
            ));
            then.status(200).body(
                "0000000000000000000000000000000000000000000000000000000000000000  archive.zip\n",
            );
        });
        server.mock(|when, then| {
            when.method(httpmock::Method::GET).path(format!(
                "/Ryan-AI-Studios/Ledgerful/releases/download/v0.2.12/{ARCHIVE_WINDOWS}"
            ));
            then.status(200).body(zip);
        });
        let (stub, sentinel) = cargo_sentinel(tmp.path());
        let _cargo = TempEnv::set(
            "LEDGERFUL_TEST_CARGO_COMMAND",
            stub.to_str().expect("utf8 stub"),
        );
        let ctx = release_ctx(tmp.path(), &dest, &server.base_url(), &server.base_url());
        let err = execute_binary_update_impl(&ctx).expect_err("mismatch");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("unchanged"),
            "mismatch must refuse replace: {msg}"
        );
        assert_eq!(fs::read(&dest).unwrap(), b"untouched");
        assert!(!sentinel.exists(), "hash mismatch must not invoke cargo");
    }

    #[cfg(any(feature = "export", feature = "web", feature = "sync"))]
    #[test]
    #[serial(env)]
    fn update_binary_release_does_not_call_cargo() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("dest.exe");
        fs::write(&dest, b"old").unwrap();
        let zip = fixture_zip(b"new-exe");
        let hash = sha256_hex(&zip);
        let server = httpmock::MockServer::start();
        let _latest = mock_latest(&server, "v0.2.12");
        server.mock(|when, then| {
            when.method(httpmock::Method::GET).path(format!(
                "/Ryan-AI-Studios/Ledgerful/releases/download/v0.2.12/{ARCHIVE_WINDOWS}.sha256"
            ));
            then.status(200)
                .body(format!("{hash}  {ARCHIVE_WINDOWS}\n"));
        });
        server.mock(|when, then| {
            when.method(httpmock::Method::GET).path(format!(
                "/Ryan-AI-Studios/Ledgerful/releases/download/v0.2.12/{ARCHIVE_WINDOWS}"
            ));
            then.status(200).body(zip);
        });
        let (stub, sentinel) = cargo_sentinel(tmp.path());
        let _cargo = TempEnv::set(
            "LEDGERFUL_TEST_CARGO_COMMAND",
            stub.to_str().expect("utf8 stub"),
        );
        let ctx = release_ctx(tmp.path(), &dest, &server.base_url(), &server.base_url());
        execute_binary_update_impl(&ctx).expect("release path");
        assert!(!sentinel.exists(), "release path must not invoke cargo");
        assert_eq!(fs::read(&dest).unwrap(), b"new-exe");
    }

    #[test]
    fn hermetic_temp_is_release_on_both_roots() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            binary_update_mode(tmp.path(), tmp.path()),
            BinaryUpdateMode::ReleaseArchive
        );
    }

    #[test]
    fn sidecar_body_takes_first_64_hex_token() {
        assert_eq!(
            parse_sha256_sidecar_body(
                "58228f717d650bba81c0b00fa5fef0be577199b86751ddc6dd8f6acac0f728cb  ledgerful-x86_64-pc-windows-msvc.zip\r\n"
            )
            .as_deref(),
            Some("58228f717d650bba81c0b00fa5fef0be577199b86751ddc6dd8f6acac0f728cb")
        );
        assert!(parse_sha256_sidecar_body("not-a-hash").is_none());
        assert!(parse_sha256_sidecar_body("").is_none());
    }

    fn linux_release_ctx(tmp: &Path, dest: &Path, api: &str, download: &str) -> BinaryUpdateCtx {
        let mut ctx = release_ctx(tmp, dest, api, download);
        ctx.target_triple = Some("x86_64-unknown-linux-gnu".to_string());
        ctx
    }

    fn fixture_tar_gz(stem: &str, files: &[(&str, &[u8])]) -> Vec<u8> {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join(stem);
        fs::create_dir_all(&root).unwrap();
        for (rel, bytes) in files {
            let path = root.join(rel);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(path, bytes).unwrap();
        }
        let archive = tmp.path().join("archive.tar.gz");
        let status = Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg(stem)
            .current_dir(tmp.path())
            .status()
            .expect("spawn tar");
        assert!(status.success(), "tar -czf fixture failed");
        fs::read(&archive).expect("read fixture archive")
    }

    fn fixture_linux_release_tar_gz(payload: &[u8]) -> Vec<u8> {
        let stem = ARCHIVE_LINUX
            .strip_suffix(".tar.gz")
            .expect("linux archive suffix");
        fixture_tar_gz(
            stem,
            &[
                ("ledgerful", payload),
                ("README.md", b"readme"),
                ("LICENSE", b"license"),
                ("web/index.html", b"<html></html>"),
            ],
        )
    }

    /// Prebuilt ustar+gzip: `{ARCHIVE_LINUX stem}/ledgerful` is a symlink to `/etc/passwd`.
    /// Host `tar` lists this without needing Windows symlink privilege.
    const SYMLINK_MEMBER_TAR_GZ: &[u8] = &[
        0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0a, 0xcb, 0x49, 0x4d, 0x49, 0x4f,
        0x2d, 0x4a, 0x2b, 0xcd, 0xd1, 0xad, 0xb0, 0x30, 0x8b, 0x37, 0x33, 0xd1, 0x2d, 0xcd, 0xcb,
        0xce, 0xcb, 0x2f, 0xcf, 0xd3, 0xcd, 0xc9, 0xcc, 0x2b, 0xad, 0xd0, 0x4d, 0xcf, 0x2b, 0xd5,
        0xcf, 0x81, 0x29, 0x61, 0x20, 0x13, 0x18, 0x18, 0x18, 0x18, 0x98, 0x9b, 0x9b, 0x83, 0x69,
        0x03, 0x03, 0x03, 0x74, 0x1a, 0x93, 0x6d, 0x64, 0x60, 0x64, 0x62, 0xc4, 0xa0, 0x60, 0xa4,
        0x9f, 0x5a, 0x92, 0xac, 0x5f, 0x90, 0x58, 0x5c, 0x5c, 0x9e, 0xc2, 0x40, 0x2b, 0x50, 0x5a,
        0x5c, 0x92, 0x58, 0xa4, 0xa0, 0xa0, 0x40, 0x33, 0x0b, 0x46, 0xc1, 0x28, 0x18, 0x05, 0xa3,
        0x80, 0x61, 0x50, 0x02, 0x00, 0x7e, 0x23, 0x33, 0x51, 0x00, 0x06, 0x00, 0x00,
    ];

    #[test]
    fn update_binary_unix_member_path_uses_posix_slash() {
        let stem = ARCHIVE_LINUX
            .strip_suffix(".tar.gz")
            .expect("linux archive suffix");
        let member = unix_tar_member(stem);
        assert_eq!(member, format!("{stem}/ledgerful"));
        assert!(member.contains('/'), "{member}");
        assert!(!member.contains('\\'), "{member}");
    }

    #[test]
    #[serial(env)]
    fn update_binary_unix_extract_reads_nested_binary_when_archive_has_legit_extras() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("dest");
        fs::write(&dest, b"old-bytes").unwrap();
        let payload = b"fresh-linux-ledgerful";
        let tarball = fixture_linux_release_tar_gz(payload);
        let hash = sha256_hex(&tarball);
        let sidecar = format!("{hash}  {ARCHIVE_LINUX}\n");
        let api = httpmock::MockServer::start();
        let download = httpmock::MockServer::start();
        let latest = mock_latest(&api, "v0.2.12");
        download.mock(|when, then| {
            when.method(httpmock::Method::GET).path(format!(
                "/Ryan-AI-Studios/Ledgerful/releases/download/v0.2.12/{ARCHIVE_LINUX}.sha256"
            ));
            then.status(200).body(sidecar);
        });
        download.mock(|when, then| {
            when.method(httpmock::Method::GET).path(format!(
                "/Ryan-AI-Studios/Ledgerful/releases/download/v0.2.12/{ARCHIVE_LINUX}"
            ));
            then.status(200).body(tarball);
        });
        let ctx = linux_release_ctx(tmp.path(), &dest, &api.base_url(), &download.base_url());
        let out = execute_binary_update_impl(&ctx).expect("unix extract with extras");
        assert!(out.contains("v0.2.12"), "{out}");
        assert_eq!(fs::read(&dest).unwrap(), payload);
        latest.assert_calls(1);
    }

    #[test]
    #[serial(env)]
    fn update_binary_unix_extract_refuses_hash_mismatch() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("dest");
        fs::write(&dest, b"untouched").unwrap();
        let tarball = fixture_linux_release_tar_gz(b"payload");
        let server = httpmock::MockServer::start();
        let _latest = mock_latest(&server, "v0.2.12");
        server.mock(|when, then| {
            when.method(httpmock::Method::GET).path(format!(
                "/Ryan-AI-Studios/Ledgerful/releases/download/v0.2.12/{ARCHIVE_LINUX}.sha256"
            ));
            then.status(200).body(
                "0000000000000000000000000000000000000000000000000000000000000000  archive.tar.gz\n",
            );
        });
        server.mock(|when, then| {
            when.method(httpmock::Method::GET).path(format!(
                "/Ryan-AI-Studios/Ledgerful/releases/download/v0.2.12/{ARCHIVE_LINUX}"
            ));
            then.status(200).body(tarball);
        });
        let (stub, sentinel) = cargo_sentinel(tmp.path());
        let _cargo = TempEnv::set(
            "LEDGERFUL_TEST_CARGO_COMMAND",
            stub.to_str().expect("utf8 stub"),
        );
        let ctx = linux_release_ctx(tmp.path(), &dest, &server.base_url(), &server.base_url());
        let err = execute_binary_update_impl(&ctx).expect_err("mismatch");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("unchanged"),
            "mismatch must refuse replace: {msg}"
        );
        assert_eq!(fs::read(&dest).unwrap(), b"untouched");
        assert!(!sentinel.exists(), "hash mismatch must not invoke cargo");
    }

    #[test]
    fn update_binary_unix_extract_missing_member_names_stem() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("dest");
        let stem = ARCHIVE_LINUX
            .strip_suffix(".tar.gz")
            .expect("linux archive suffix");
        let tarball = fixture_tar_gz(stem, &[("README.md", b"only extras")]);
        let err = extract_unix_tarball(ARCHIVE_LINUX, &tarball, &dest)
            .expect_err("missing nested binary");
        let msg = format!("{err:?}");
        assert!(
            msg.contains(&format!("{stem}/ledgerful")),
            "missing member must name stem path: {msg}"
        );
        assert!(
            !msg.contains("tar failed to extract"),
            "missing member must not be generic tar failed: {msg}"
        );
    }

    #[test]
    fn update_binary_unix_extract_refuses_symlink_member() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("dest");
        fs::write(&dest, b"keep").unwrap();
        let err = extract_unix_tarball(ARCHIVE_LINUX, SYMLINK_MEMBER_TAR_GZ, &dest)
            .expect_err("symlink member");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("not a regular file") || msg.contains("missing"),
            "symlink member must be refused (regular-file or missing after tar cannot lay the link): {msg}"
        );
        assert_eq!(fs::read(&dest).unwrap(), b"keep");
    }

    #[test]
    fn update_binary_unix_extract_refuses_oversize_payload() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("dest");
        let stem = ARCHIVE_LINUX
            .strip_suffix(".tar.gz")
            .expect("linux archive suffix");
        let tarball = fixture_tar_gz(stem, &[("ledgerful", b"0123456789abcdef")]);
        let err =
            extract_unix_tarball_with_cap(ARCHIVE_LINUX, &tarball, &dest, 8).expect_err("oversize");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("extracted size cap"),
            "oversize must name the cap: {msg}"
        );
    }
}
