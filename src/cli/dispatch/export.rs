use crate::cli::args::ExportCommands;
use miette::{IntoDiagnostic, Result};

/// Check if the current repo has a DEMO_MARKER file, indicating it was
/// created by `ledgerful demo`. If so, exports must self-identify as demo
/// artifacts to prevent synthetic evidence from being mistaken for real.
pub(super) fn is_demo_repo(layout: &crate::state::layout::Layout) -> bool {
    layout.root.join(".ledgerful").join("DEMO_MARKER").exists()
}

#[cfg(feature = "export")]
pub(super) fn dispatch_export(command: ExportCommands) -> Result<()> {
    use crate::export::soc2::generate_soc2_export_with_options;
    use crate::export::soc2_control::generate_soc2_control_export;
    use owo_colors::{OwoColorize, Stream, Style};

    match command {
        ExportCommands::Evidence {
            profile,
            out,
            force,
            control,
        } => {
            if profile != "soc2" {
                return Err(miette::miette!(
                    "unknown export profile: {profile}; currently only 'soc2' is supported"
                ));
            }

            // Shared state_dir for linked worktrees; Layout::new only when not in a repo.
            // Resolve errors after git discover fail closed (no private state invent).
            let layout = crate::commands::helpers::get_layout_or_cwd_if_not_git()?;

            let is_demo = is_demo_repo(&layout);
            let keys_dir = if is_demo {
                Some(
                    layout
                        .root
                        .join(".ledgerful")
                        .join("keys")
                        .as_std_path()
                        .to_path_buf(),
                )
            } else {
                None
            };
            let default_name = if is_demo {
                "ledgerful-DEMO-evidence.zip"
            } else {
                "ledgerful-soc2-evidence.zip"
            };
            let path = out.unwrap_or_else(|| std::path::PathBuf::from(default_name));

            let validated = validate_export_evidence_path(&path, force)?;

            let zip_bytes = if control.is_empty() {
                generate_soc2_export_with_options(&layout, is_demo, keys_dir.as_deref(), None)?
            } else {
                generate_soc2_control_export(&layout, is_demo, keys_dir.as_deref(), &control)?
            };

            std::fs::write(&validated, &zip_bytes).into_diagnostic()?;

            println!(
                "{} SOC2 evidence exported to {}",
                "SUCCESS:"
                    .if_supports_color(Stream::Stdout, |s| s.style(Style::new().green().bold())),
                validated.display()
            );
            Ok(())
        }
        ExportCommands::Head { out, force, stdout } => {
            use crate::export::head::{
                HeadExportDest, prepare_chain_head_export, resolve_head_export_dest,
                serialize_chain_head,
            };
            use std::io::Write;

            // Resolve before path validation: `-` is a legal file name but means stdout.
            let dest = resolve_head_export_dest(out, stdout, force)?;
            let layout = crate::commands::helpers::get_layout_or_cwd_if_not_git()?;
            let head = prepare_chain_head_export(&layout)?;
            let json = serialize_chain_head(&head)?;

            match dest {
                HeadExportDest::Stdout => {
                    // Exact serialize_chain_head bytes — no SUCCESS, no extra trailing NL.
                    std::io::stdout()
                        .lock()
                        .write_all(&json)
                        .into_diagnostic()?;
                    Ok(())
                }
                HeadExportDest::File { path, force } => {
                    let validated = validate_export_evidence_path(&path, force)?;
                    std::fs::write(&validated, &json).into_diagnostic()?;

                    println!(
                        "{} Chain head exported to {}",
                        "SUCCESS:".if_supports_color(Stream::Stdout, |s| {
                            s.style(Style::new().green().bold())
                        }),
                        validated.display()
                    );
                    Ok(())
                }
            }
        }
    }
}

#[cfg(not(feature = "export"))]
pub(super) fn dispatch_export(_command: ExportCommands) -> Result<()> {
    Err(miette::miette!(
        "export feature is not enabled in this build; rebuild with --features export"
    ))
}

/// Validate an export-evidence output path with 0032 path-safety discipline.
///
/// Differences from `validate_export_path` (used for `ledger export-provenance`):
///
/// - The path does **not** have to be inside the repository. Users may export
///   evidence to an absolute path such as `~/Desktop/ledgerful-soc2-evidence.zip`.
/// - If the current directory is inside a git repository, we still refuse to write
///   to `Cargo.toml`, `src/`, or `.ledgerful/state/` inside that repository, and
///   we re-check the canonicalized path after symlink resolution so a repo-local
///   symlink cannot escape into a protected location.
/// - Refuses to overwrite an existing file without `--force`.
///
/// Shared by `export evidence` and `export head`.
pub(super) fn validate_export_evidence_path(
    path: &std::path::Path,
    force: bool,
) -> miette::Result<std::path::PathBuf> {
    let cwd = std::env::current_dir()
        .map_err(|e| miette::miette!("failed to determine current directory: {e}"))?;
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    let cleaned = path_clean::PathClean::clean(&absolute);

    // Reject directory-only targets and paths with no file component.
    let file_name_os = cleaned.file_name();
    let file_name_valid = file_name_os
        .and_then(|n| n.to_str().map(|s| !s.is_empty()))
        .unwrap_or(false);
    if cleaned.is_dir() || !file_name_valid {
        return Err(miette::miette!(
            "invalid path: no file component; must specify a file, not a directory"
        ));
    }

    // Resolve symlinks/junctions for the safety-boundary comparison.
    let canonical = if cleaned.exists() {
        std::fs::canonicalize(&cleaned)
            .map_err(|e| miette::miette!("failed to resolve path: {e}"))?
    } else {
        match cleaned.parent() {
            Some(parent) => {
                let base = std::fs::canonicalize(parent)
                    .map_err(|e| miette::miette!("failed to resolve parent directory: {e}"))?;
                base.join(file_name_os.unwrap_or_default())
            }
            None => cleaned.clone(),
        }
    };

    let canonical = strip_verbatim_prefix(&canonical);

    // If we can discover a repo root, apply source/state protections to the
    // canonicalized path. This is the post-canonicalization symlink re-check:
    // a path that pointed at `src/` or `.ledgerful/state/` via a symlink/junction
    // will resolve to a canonical path under those directories and be rejected.
    if let Ok(repo_root) = crate::commands::helpers::get_repo_root() {
        let repo_root_std = repo_root.as_std_path();
        let canonical_repo_root = std::fs::canonicalize(repo_root_std)
            .map_err(|e| miette::miette!("failed to resolve repo root: {e}"))?;
        let canonical_repo_root = strip_verbatim_prefix(&canonical_repo_root);

        let cargo_toml_path = strip_verbatim_prefix(&canonical_repo_root.join("Cargo.toml"));
        if canonical == cargo_toml_path {
            return Err(miette::miette!("refusing to write to Cargo.toml"));
        }

        let src_dir = strip_verbatim_prefix(&canonical_repo_root.join("src"));
        if canonical.starts_with(&src_dir) {
            return Err(miette::miette!("refusing to write inside src/"));
        }

        let state_dir =
            strip_verbatim_prefix(&canonical_repo_root.join(".ledgerful").join("state"));
        if canonical.starts_with(&state_dir) {
            return Err(miette::miette!(
                "refusing to write inside .ledgerful/state/"
            ));
        }
    }

    if canonical.exists() && !force {
        return Err(miette::miette!(
            "{} already exists; use --force to overwrite",
            canonical.display()
        ));
    }

    Ok(canonical)
}

/// Strip the Windows "\\?\" verbatim prefix so that canonical paths compare
/// naturally with user-provided paths. On non-Windows platforms this is a no-op.
pub(super) fn strip_verbatim_prefix(path: &std::path::Path) -> std::path::PathBuf {
    #[cfg(windows)]
    {
        use std::path::Component;
        let mut components = path.components();
        if let Some(Component::Prefix(prefix)) = components.next()
            && let Some(disk) = prefix
                .as_os_str()
                .to_str()
                .and_then(|s| s.strip_prefix(r"\\?\"))
        {
            let rest = components.as_path();
            return std::path::Path::new(disk).join(rest);
        }
    }
    path.to_path_buf()
}
