use crate::commands::helpers::get_layout;
use crate::config::load::load_config;
use miette::{IntoDiagnostic, Result};
use rpassword::read_password;
use std::env;

pub fn handle(_once: bool) -> Result<()> {
    let layout = get_layout()?;
    let config = load_config(&layout)?;

    let team_secret = if let Ok(secret) = env::var("LEDGERFUL_SYNC_SECRET") {
        secret
    } else {
        println!("Enter team sync secret (12-word phrase):");
        read_password().into_diagnostic()?
    };

    if team_secret.trim().is_empty() {
        return Err(miette::miette!("Team secret cannot be empty."));
    }

    crate::sync::run(&config, layout.root.as_std_path(), team_secret.as_bytes())?;

    Ok(())
}
