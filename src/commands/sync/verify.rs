use crate::state::storage::StorageManager;
use crate::sync::bundle::{Bundle, BundleParseError};
use crate::sync::peers::load_peer_keys;
use miette::{Result, miette};
use rusqlite::OptionalExtension;
use std::fs;
use std::io::Write;
use std::path::Path;
use zeroize::Zeroizing;

struct VerifyReport {
    verdict: &'static str,
    message: Option<String>,
    version: Option<u32>,
    device_id: Option<String>,
    bundle_hlc: Option<String>,
    entry_count: Option<usize>,
}

impl VerifyReport {
    fn fail(verdict: &'static str, message: impl Into<String>) -> Self {
        Self {
            verdict,
            message: Some(message.into()),
            version: None,
            device_id: None,
            bundle_hlc: None,
            entry_count: None,
        }
    }

    fn ok(bundle: &Bundle) -> Self {
        Self {
            verdict: "ok",
            message: None,
            version: Some(bundle.manifest.version),
            device_id: Some(bundle.manifest.device_id.clone()),
            bundle_hlc: Some(bundle.manifest.bundle_hlc.to_string()),
            entry_count: Some(bundle.manifest.entry_count),
        }
    }
}

pub fn handle(bundle_path: &str, json: bool) -> Result<()> {
    let path = Path::new(bundle_path);
    if !path.exists() {
        return finish(
            json,
            VerifyReport::fail(
                "missingFile",
                format!("Bundle file not found: {bundle_path}"),
            ),
        );
    }

    let team_secret: Zeroizing<String> = match std::env::var("LEDGERFUL_SYNC_SECRET") {
        Ok(s) => Zeroizing::new(s),
        Err(_) => {
            return finish(
                json,
                VerifyReport::fail(
                    "missingSecret",
                    "LEDGERFUL_SYNC_SECRET environment variable not set. It is required to verify bundles.",
                ),
            );
        }
    };

    let data = match fs::read(path) {
        Ok(d) => d,
        Err(e) => {
            return finish(
                json,
                VerifyReport::fail("missingFile", format!("Failed to read bundle: {e}")),
            );
        }
    };

    let zip_bytes = match Bundle::decrypt(&data, team_secret.as_bytes()) {
        Ok(z) => z,
        Err(e) => {
            return finish(
                json,
                VerifyReport::fail("decryptFailed", format!("Failed to decrypt bundle: {e}")),
            );
        }
    };

    let layout = match crate::commands::helpers::get_layout() {
        Ok(l) => l,
        Err(e) => {
            return finish(
                json,
                VerifyReport::fail("schemaInvalid", format!("Failed to resolve layout: {e}")),
            );
        }
    };
    let sync_dir = layout.state_dir.join("sync");
    let mut verify_keys = match load_peer_keys(sync_dir.as_std_path()) {
        Ok(k) => k,
        Err(e) => {
            return finish(
                json,
                VerifyReport::fail("schemaInvalid", format!("Failed to load peer keys: {e}")),
            );
        }
    };

    let own_pub_path = sync_dir.join("device.pub");
    if own_pub_path.exists() {
        let storage = match StorageManager::init_with_layout(&layout) {
            Ok(s) => s,
            Err(e) => {
                return finish(
                    json,
                    VerifyReport::fail("schemaInvalid", format!("Failed to open storage: {e}")),
                );
            }
        };
        let device_id: Option<String> = match storage
            .get_connection()
            .query_row("SELECT device_id FROM sync_state WHERE id = 1", [], |row| {
                row.get(0)
            })
            .optional()
        {
            Ok(id) => id,
            Err(e) => {
                return finish(
                    json,
                    VerifyReport::fail(
                        "schemaInvalid",
                        format!("Failed to query sync_state device_id: {e}"),
                    ),
                );
            }
        };

        if let Some(device_id) = device_id
            && !device_id.is_empty()
            && device_id != "unknown"
        {
            let key_bytes = match fs::read(own_pub_path.as_std_path()) {
                Ok(b) => b,
                Err(e) => {
                    return finish(
                        json,
                        VerifyReport::fail(
                            "schemaInvalid",
                            format!("Failed to read device.pub: {e}"),
                        ),
                    );
                }
            };
            if key_bytes.len() == 32 {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&key_bytes);
                if ed25519_dalek::VerifyingKey::from_bytes(&arr).is_ok() {
                    verify_keys.insert(device_id, arr);
                }
            }
        }
    }

    let bundle = match Bundle::parse(&zip_bytes, &verify_keys) {
        Ok(b) => b,
        Err(BundleParseError::UnknownDevice(msg)) => {
            return finish(json, VerifyReport::fail("unknownDevice", msg));
        }
        Err(BundleParseError::SignatureFailed(msg)) => {
            return finish(json, VerifyReport::fail("signatureFailed", msg));
        }
        Err(BundleParseError::IntegrityFailed(msg)) => {
            return finish(json, VerifyReport::fail("integrityFailed", msg));
        }
        Err(BundleParseError::SchemaInvalid(msg)) => {
            return finish(json, VerifyReport::fail("schemaInvalid", msg));
        }
    };

    finish(json, VerifyReport::ok(&bundle))
}

fn finish(json: bool, report: VerifyReport) -> Result<()> {
    let ok = report.verdict == "ok";
    if json {
        let mut envelope = serde_json::json!({
            "schemaVersion": 1,
            "ok": ok,
            "verdict": report.verdict,
        });
        if ok {
            envelope["version"] = serde_json::json!(report.version);
            envelope["deviceId"] = serde_json::json!(report.device_id);
            envelope["bundleHlc"] = serde_json::json!(report.bundle_hlc);
            envelope["entryCount"] = serde_json::json!(report.entry_count);
        } else if let Some(msg) = &report.message {
            envelope["message"] = serde_json::Value::String(msg.clone());
        }
        let mut stdout = std::io::stdout().lock();
        serde_json::to_writer(&mut stdout, &envelope)
            .map_err(|e| miette!("Failed to write verify JSON: {e}"))?;
        stdout
            .write_all(b"\n")
            .map_err(|e| miette!("Failed to write verify JSON newline: {e}"))?;
        if !ok {
            crate::output::requested_exit::request_exit(1);
            return Err(miette!("{}", diagnostic_display(&report)));
        }
        return Ok(());
    }

    if !ok {
        return Err(miette!("{}", diagnostic_display(&report)));
    }

    println!("Bundle Verification Success:");
    println!("  Version:        {}", report.version.unwrap_or(0));
    println!(
        "  Device ID:      {}",
        report.device_id.as_deref().unwrap_or("")
    );
    println!(
        "  Bundle HLC:     {}",
        report.bundle_hlc.as_deref().unwrap_or("")
    );
    println!("  Entry Count:    {}", report.entry_count.unwrap_or(0));
    println!("  Signature:      Valid (Ed25519)");
    println!("  Integrity:      Valid (SHA-256)");
    Ok(())
}

fn diagnostic_display(report: &VerifyReport) -> String {
    match &report.message {
        Some(msg) => format!("{}: {msg}", report.verdict),
        None => report.verdict.to_string(),
    }
}
