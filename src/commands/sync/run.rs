use crate::commands::helpers::get_layout;
use crate::config::load::load_config;
use miette::{IntoDiagnostic, Result};
use std::env;
use zeroize::Zeroizing;

/// Run team sync once the user has opted in.
///
/// Disabled path returns **before** the secret prompt and before the engine,
/// with a clear opt-in explanation. Engine still treats `!enabled` as a silent
/// no-op for defense-in-depth.
pub fn handle(_once: bool) -> Result<()> {
    let layout = get_layout()?;
    let config = load_config(&layout)?;

    if !config.sync.enabled {
        println!("Team sync is disabled ([sync].enabled = false).");
        println!(
            "This is the default forever-opt-in posture — no silent team merge. \
See docs/team-sync.md."
        );
        println!();
        println!("When ready (after init + mutual pairing):");
        println!("  1. ledgerful sync init");
        println!("  2. ledgerful sync setup          # readiness checklist");
        println!("  3. Set [sync].target, then ledgerful sync setup --enable");
        println!("  4. ledgerful sync run --once");
        return Ok(());
    }

    let sync_dir = layout.state_dir.join("sync");
    let key_path = sync_dir.join("device.key");
    if !key_path.exists() {
        return Err(miette::miette!(
            "Sync is enabled but not initialized (missing device.key at {}). \
Run `ledgerful sync init` first. Or `ledgerful sync setup` for a readiness checklist.",
            key_path
        ));
    }

    if config.sync.target.trim().is_empty() {
        return Err(miette::miette!(
            "Sync is enabled but [sync].target is empty. \
Set a shared-folder target, e.g. `ledgerful config set sync.target=\"dir:///path/to/shared\"`. \
Or run `ledgerful sync setup` for a readiness checklist."
        ));
    }

    // Same prompt pattern as init (`prompt_password`) for consistent UX.
    let team_secret: Zeroizing<String> = if let Ok(secret) = env::var("LEDGERFUL_SYNC_SECRET") {
        Zeroizing::new(secret)
    } else {
        Zeroizing::new(
            rpassword::prompt_password("Enter team sync secret (12-word phrase): ")
                .into_diagnostic()?,
        )
    };

    if team_secret.trim().is_empty() {
        return Err(miette::miette!("Team secret cannot be empty."));
    }

    // Engine expects the `.ledgerful` state dir (keys at state_dir/sync, DB at state_dir/state).
    crate::sync::run(
        &config,
        layout.state_dir.as_std_path(),
        team_secret.as_bytes(),
    )?;

    Ok(())
}
