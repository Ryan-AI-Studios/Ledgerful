use crate::policy::load as policy_load;
use crate::state::layout::Layout;
use miette::Result;

pub fn execute_config_verify(json: bool, section: Option<&str>, verbose: bool) -> Result<()> {
    let current_dir = std::env::current_dir()
        .map_err(|e| miette::miette!("Failed to get current directory: {e}"))?;
    let layout = Layout::new(current_dir.to_string_lossy().as_ref());

    let mut success = true;
    let mut errors = Vec::new();

    if !json {
        println!("Verifying Ledgerful configuration...");
    }

    // Verify config.toml
    let config = match crate::config::load_config(&layout) {
        Ok(cfg) => {
            if !json {
                println!("  ✅ config.toml is valid");
            }
            Some(cfg)
        }
        Err(e) => {
            if !json {
                println!("  ❌ config.toml is invalid:\n    {e}");
            }
            errors.push(format!("config.toml is invalid: {e}"));
            success = false;
            None
        }
    };

    // Verify rules.toml
    match policy_load::load_rules(&layout) {
        Ok(_) => {
            if !json {
                println!("  ✅ rules.toml is valid");
            }
        }
        Err(e) => {
            if !json {
                println!("  ❌ rules.toml is invalid:\n    {e}");
            }
            errors.push(format!("rules.toml is invalid: {e}"));
            success = false;
        }
    }

    // TA14 R5: Validate provider priority list
    if let Some(cfg) = &config {
        let providers = &cfg.ask.providers.priority;
        if !providers.is_empty() {
            for (idx, entry) in providers.iter().enumerate() {
                if entry.timeout_secs.is_some_and(|t| t == 0) {
                    let msg = format!("ask.providers.priority[{}]: timeout_secs must be > 0", idx);
                    if !json {
                        println!("  ❌ {msg}");
                    }
                    errors.push(msg);
                    success = false;
                }
            }
            if !json && success {
                println!(
                    "  ✅ ask.providers.priority is valid ({} provider(s))",
                    providers.len()
                );
            }
        }
    }

    // Report config sections
    if let (true, Some(cfg)) = (success, &config) {
        match crate::commands::config_verify::render_verify_report(cfg, json, section, verbose) {
            Ok(report) => {
                if json {
                    println!("{report}");
                } else {
                    println!("\nResolved Settings:");
                    println!("{report}");
                }
            }
            Err(e) => {
                errors.push(e.to_string());
                success = false;
            }
        }
    }

    if success {
        if !json {
            println!("\nAll configurations are valid.");
        }
        Ok(())
    } else {
        if json {
            let err_json = serde_json::json!({
                "success": false,
                "errors": errors
            });
            println!(
                "{}",
                serde_json::to_string_pretty(&err_json).unwrap_or_default()
            );
        }
        Err(miette::miette!("Configuration verification failed."))
    }
}
