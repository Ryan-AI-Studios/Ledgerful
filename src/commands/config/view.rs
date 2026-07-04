use crate::config::model::VerifyMode;
use crate::state::layout::Layout;
use miette::Result;
use owo_colors::OwoColorize;

pub fn execute_config_view(json: bool, section: Option<String>, key: Option<String>) -> Result<()> {
    let current_dir = std::env::current_dir()
        .map_err(|e| miette::miette!("Failed to get current directory: {e}"))?;
    let layout = Layout::new(current_dir.to_string_lossy().as_ref());
    let config = crate::config::load_config(&layout)?;

    let mut val = serde_json::to_value(&config)
        .map_err(|e| miette::miette!("Failed to serialize config: {e}"))?;

    // Redact secret fields (api_key, token, etc.) before any output
    crate::config::redact::redact_config_value(&mut val);

    // TA32: Inject effective verification mode into view output
    let rules = crate::policy::load::load_rules(&layout).unwrap_or_default();

    if let Some(verify_obj) = val.get_mut("verify").and_then(|v| v.as_object_mut()) {
        let effective_mode = config.verify.effective_mode();
        let mode_str = match effective_mode {
            VerifyMode::Auto => "auto",
            VerifyMode::Explicit => "explicit",
        };
        verify_obj.insert(
            "effective_mode".to_string(),
            serde_json::Value::String(mode_str.to_string()),
        );

        let rules_source = if rules.was_legacy_default {
            "historical-rules-fallback"
        } else if effective_mode == VerifyMode::Auto {
            "auto-policy"
        } else {
            "explicit-config"
        };
        verify_obj.insert(
            "rules_source".to_string(),
            serde_json::Value::String(rules_source.to_string()),
        );
    }

    if !json && rules.was_legacy_default {
        println!(
            "{}",
            "ℹ️  Detected historical rules; using automatic fallback policy.".blue()
        );
    }

    let filtered = if let Some(sec) = &section {
        let sec_key = val
            .as_object()
            .and_then(|obj| obj.keys().find(|k| k.eq_ignore_ascii_case(sec)).cloned());
        if let Some(sk) = sec_key {
            let sec_val = &val[&sk];
            if let Some(k) = &key {
                let k_key = sec_val.as_object().and_then(|obj| {
                    obj.keys()
                        .find(|inner_k| inner_k.eq_ignore_ascii_case(k))
                        .cloned()
                });
                if let Some(kk) = k_key {
                    sec_val[&kk].clone()
                } else {
                    return Err(miette::miette!("Key '{}' not found in section '{}'", k, sk));
                }
            } else {
                sec_val.clone()
            }
        } else {
            return Err(miette::miette!("Section '{}' not found in config", sec));
        }
    } else if let Some(k) = &key {
        let top_key = val.as_object().and_then(|obj| {
            obj.keys()
                .find(|inner_k| inner_k.eq_ignore_ascii_case(k))
                .cloned()
        });
        if let Some(tk) = top_key {
            val[&tk].clone()
        } else {
            return Err(miette::miette!("Key '{}' not found in top-level config", k));
        }
    } else {
        val
    };

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&filtered)
                .map_err(|e| miette::miette!("Failed to serialize filtered config to JSON: {e}"))?
        );
    } else {
        if filtered.is_string() {
            println!(
                "{}",
                filtered
                    .as_str()
                    .ok_or_else(|| miette::miette!("expected string value"))?
            );
        } else if filtered.is_number() || filtered.is_boolean() || filtered.is_null() {
            println!("{}", filtered);
        } else {
            println!(
                "{}",
                serde_json::to_string_pretty(&filtered)
                    .map_err(|e| miette::miette!("Failed to serialize: {e}"))?
            );
        }
    }
    Ok(())
}
