//! `ledgerful sync setup` — readiness checklist + gated `--enable` (0113).
//!
//! Default: print checklist + Next (exit 0 even when incomplete).
//! `--enable`: strict refuse matrix; sibling `config.toml.bak` then enable.
//! Never prompts for the team secret. Never writes secret material.

use super::readiness::{ReadinessReport, collect_readiness};
use crate::commands::helpers::get_layout;
use crate::config::load::load_config;
use miette::{Result, miette};
use std::fs;
use std::io::Write;

/// Handle `sync setup` / `sync setup --enable` / `--json`.
pub fn handle(enable: bool, json: bool) -> Result<()> {
    let layout = get_layout()?;
    let config = load_config(&layout)?;
    let report = collect_readiness(&layout, &config)?;

    if enable {
        return handle_enable(&layout, &report, json);
    }

    if json {
        emit_json(&report)?;
        return Ok(());
    }

    print_checklist(&report);
    Ok(())
}

fn handle_enable(
    layout: &crate::state::layout::Layout,
    report: &ReadinessReport,
    json: bool,
) -> Result<()> {
    if report.enabled {
        if json {
            // Re-collect would still show enabled; emit current report.
            emit_json(report)?;
        } else {
            println!("Team sync is already enabled ([sync].enabled = true).");
            println!("  Next: {}", report.next_action);
        }
        return Ok(());
    }

    let failures = report.enable_failures();
    if !failures.is_empty() {
        if json {
            // Still pure JSON, but non-zero exit via error after? Spec: non-zero + list failed gates.
            // For agents: emit diagnostic JSON on stdout would be wrong for error path.
            // Refuse with miette error (stderr); no config mutation.
            return Err(refuse_error(&failures));
        }
        eprintln!("Refusing to enable team sync — incomplete readiness:");
        for f in &failures {
            eprintln!("  ✗ {f}");
        }
        eprintln!();
        eprintln!("Run `ledgerful sync setup` for the full checklist.");
        eprintln!("See docs/team-sync.md");
        return Err(miette!(
            "sync setup --enable refused: {} gate(s) failed",
            failures.len()
        ));
    }

    // Success path: sibling backup then quiet mutate (setup owns stdout messaging).
    backup_config_toml(layout)?;
    crate::commands::config::execute_config_set_in_quiet(layout, "sync.enabled=true")?;

    // Re-collect after enable for accurate next-action / JSON.
    let config = load_config(layout)?;
    let after = collect_readiness(layout, &config)?;

    if json {
        // Pure readiness JSON only — quiet set guarantees no "Set …" prefix.
        emit_json(&after)?;
    } else {
        println!();
        println!("Team sync enabled ([sync].enabled = true).");
        if layout.config_file().exists() {
            let bak = config_bak_path(layout);
            if bak.exists() {
                println!("  Backup: {bak}");
            }
        }
        println!("  Next:   {}", after.next_action);
        println!("See docs/team-sync.md");
    }
    Ok(())
}

fn refuse_error(failures: &[&str]) -> miette::Error {
    let list = failures
        .iter()
        .map(|f| format!("  - {f}"))
        .collect::<Vec<_>>()
        .join("\n");
    miette!("sync setup --enable refused — failed gates:\n{list}")
}

/// Copy `config.toml` → sibling `config.toml.bak` once when the source exists.
fn backup_config_toml(layout: &crate::state::layout::Layout) -> Result<()> {
    let config_path = layout.config_file();
    if !config_path.exists() {
        return Ok(());
    }
    let bak = config_bak_path(layout);
    fs::copy(config_path.as_std_path(), bak.as_std_path()).map_err(|e| {
        miette!(
            "Failed to write config backup {} before enabling sync: {e}",
            bak
        )
    })?;
    Ok(())
}

fn config_bak_path(layout: &crate::state::layout::Layout) -> camino::Utf8PathBuf {
    layout.state_dir.join("config.toml.bak")
}

fn emit_json(report: &ReadinessReport) -> Result<()> {
    let value = report.to_json_value();
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, &value)
        .map_err(|e| miette!("Failed to write readiness JSON: {e}"))?;
    stdout
        .write_all(b"\n")
        .map_err(|e| miette!("Failed to write readiness JSON newline: {e}"))?;
    Ok(())
}

fn print_checklist(report: &ReadinessReport) {
    println!("Team Sync Setup [Available — opt-in shared-folder v1]");
    println!();
    println!("Readiness checklist:");
    println!(
        "  [{}] Initialized (device.key + device.pub + SoT device_id)",
        mark(report.initialized)
    );
    if let Some(ref id) = report.device_id {
        println!("       Device ID: {id}");
    }
    match (&report.peer_count, &report.peers_error) {
        (_, Some(e)) => println!("  [!] Peers: error — {e}"),
        (Some(n), None) => println!("  [{}] Peers >= 1 (count: {n})", mark(*n >= 1)),
        (None, None) => println!("  [!] Peers: unknown"),
    }
    println!("  [{}] Target set", mark(report.target_set));
    if report.target_set {
        let display = if report.target.trim().is_empty() {
            "(empty)"
        } else {
            report.target.as_str()
        };
        println!("       Target: {display}");
    }
    println!(
        "  [{}] Target parseable (SyncTarget::parse)",
        mark(report.target_parse_ok)
    );
    println!(
        "  [{}] Target reachable ({})",
        mark(report.target_reachable.is_reachable()),
        report.target_reachable.as_str()
    );
    println!(
        "  [{}] Secret env set (LEDGERFUL_SYNC_SECRET){}",
        mark(report.secret_env_set),
        secret_hint(report.secret_env_set)
    );
    println!("  [{}] Enabled ([sync].enabled)", mark(report.enabled));
    match report.quarantine_note.as_deref() {
        Some(note) => println!("  Quarantined (this device): {note}"),
        None => println!("  Quarantined (this device): {}", report.quarantine_count),
    }
    println!();
    println!("  Readiness: {}", report.readiness.as_str());
    println!("  Next:      {}", report.next_action);
    println!();
    println!("Notes:");
    println!("  • setup never enables sync unless you pass --enable");
    println!("  • --enable refuses without init + ≥1 peer + parseable reachable target");
    println!("  • team secret is never written to config.toml or disk by setup");
    println!("  • mutual trust needs two accept cycles (A→B and B→A)");
    println!("See docs/team-sync.md");
}

fn mark(ok: bool) -> char {
    if ok { 'x' } else { ' ' }
}

fn secret_hint(secret_env_set: bool) -> String {
    if secret_env_set {
        return String::new();
    }
    // Presence messaging only — never prompt (agents must not hang).
    if crate::util::term::is_interactive() {
        " — run will prompt".to_string()
    } else {
        " — set LEDGERFUL_SYNC_SECRET for non-interactive execution".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::sync::readiness::{ReadinessKind, TargetReachable, collect_readiness};
    use crate::config::model::{Config, SyncConfig};
    use crate::state::layout::Layout;
    use crate::state::storage::StorageManager;
    use ed25519_dalek::SigningKey;
    use std::fs;
    use tempfile::tempdir;

    fn write_keys_and_sot(layout: &Layout, device_id: &str) {
        let sync_dir = layout.state_dir.join("sync");
        fs::create_dir_all(sync_dir.as_std_path()).unwrap();
        let sk = SigningKey::generate(&mut rand::rng());
        fs::write(sync_dir.join("device.key").as_std_path(), sk.to_bytes()).unwrap();
        fs::write(
            sync_dir.join("device.pub").as_std_path(),
            sk.verifying_key().to_bytes(),
        )
        .unwrap();
        layout.ensure_state_dir().unwrap();
        let storage = StorageManager::init_with_layout(layout).unwrap();
        storage
            .get_connection()
            .execute(
                "INSERT INTO sync_state (id, device_id) VALUES (1, ?1)
                 ON CONFLICT(id) DO UPDATE SET device_id = excluded.device_id",
                [device_id],
            )
            .unwrap();
    }

    fn add_peer(layout: &Layout, peer_id: &str) {
        let sk = SigningKey::generate(&mut rand::rng());
        crate::sync::peers::trust_peer(
            layout.state_dir.join("sync").as_std_path(),
            peer_id,
            &sk.verifying_key().to_bytes(),
            false,
        )
        .unwrap();
    }

    fn cfg(enabled: bool, target: &str) -> Config {
        Config {
            sync: SyncConfig {
                enabled,
                target: target.to_string(),
                ..SyncConfig::default()
            },
            ..Config::default()
        }
    }

    #[test]
    fn enable_failures_empty_when_green() {
        let tmp = tempdir().unwrap();
        let share = tmp.path().join("share");
        fs::create_dir_all(&share).unwrap();
        let layout = Layout::new(camino::Utf8Path::from_path(tmp.path()).unwrap());
        write_keys_and_sot(&layout, "device-setup01");
        add_peer(&layout, "device-peer-s1");
        // Materialize a real config.toml so bak + set can run.
        layout.ensure_state_dir().unwrap();
        let config_path = layout.config_file();
        fs::write(
            config_path.as_std_path(),
            format!(
                "[sync]\nenabled = false\ntarget = \"dir://{}\"\n",
                share.display().to_string().replace('\\', "/")
            ),
        )
        .unwrap();

        let target = format!("dir://{}", share.display().to_string().replace('\\', "/"));
        let report = collect_readiness(&layout, &cfg(false, &target)).unwrap();
        assert!(report.can_enable());
        assert_eq!(report.readiness, ReadinessKind::Disabled);

        backup_config_toml(&layout).unwrap();
        assert!(config_bak_path(&layout).exists());
        crate::commands::config::execute_config_set_in(&layout, "sync.enabled=true").unwrap();
        let after_cfg = crate::config::load::load_config(&layout).unwrap();
        assert!(after_cfg.sync.enabled);
        let after = collect_readiness(&layout, &after_cfg).unwrap();
        assert_eq!(after.readiness, ReadinessKind::Ready);
        assert_eq!(after.target_reachable, TargetReachable::Yes);
    }

    #[test]
    fn setup_never_calls_secret_prompt_apis() {
        // Source-level guard: production code (above tests) must not prompt.
        let src = include_str!("setup.rs");
        let prod = src.split("#[cfg(test)]").next().unwrap_or(src);
        assert!(
            !prod.contains("rpassword::")
                && !prod.contains("prompt_password")
                && !prod.contains("read_password"),
            "setup production code must never prompt for secret"
        );
    }
}
