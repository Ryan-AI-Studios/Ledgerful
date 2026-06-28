use miette::{Result, miette};
use std::fs;

use ed25519_dalek::SigningKey;
use rand::rngs::OsRng;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use zeroize::Zeroize;

pub fn handle(force: bool, with_secret: Option<String>) -> Result<()> {
    let cg_dir = std::env::current_dir()
        .map_err(|e| miette!("Failed to get current dir: {}", e))?
        .join(".ledgerful");
    let sync_dir = cg_dir.join("sync");

    if !sync_dir.exists() {
        fs::create_dir_all(&sync_dir).map_err(|e| miette!("Failed to create sync dir: {}", e))?;
    }

    let key_path = sync_dir.join("device.key");
    if key_path.exists() && !force {
        return Err(miette!(
            "device.key already exists. Use --force to overwrite."
        ));
    }

    let signing_key = SigningKey::generate(&mut OsRng);
    let key_bytes = signing_key.to_bytes();

    fs::write(&key_path, key_bytes).map_err(|e| miette!("Failed to write device.key: {}", e))?;

    #[cfg(unix)]
    {
        let mut perms = fs::metadata(&key_path).unwrap().permissions();
        perms.set_mode(0o600);
        fs::set_permissions(&key_path, perms).unwrap();
    }

    let pub_path = sync_dir.join("device.pub");
    let pub_key = signing_key.verifying_key().to_bytes();
    fs::write(&pub_path, pub_key).map_err(|e| miette!("Failed to write device.pub: {}", e))?;

    #[cfg(unix)]
    {
        let mut perms = fs::metadata(&pub_path).unwrap().permissions();
        perms.set_mode(0o644);
        fs::set_permissions(&pub_path, perms).unwrap();
    }

    let mut secret = match with_secret {
        Some(s) => s,
        None => {
            if let Ok(s) = std::env::var("LEDGERFUL_SYNC_SECRET") {
                s
            } else {
                rpassword::prompt_password("Enter 12-word team secret: ")
                    .map_err(|e| miette!("Failed to read secret: {}", e))?
            }
        }
    };

    let device_id = format!(
        "device-{}",
        uuid::Uuid::new_v4()
            .to_string()
            .chars()
            .take(8)
            .collect::<String>()
    );

    let config_path = cg_dir.join("config.toml");
    if config_path.exists() {
        let mut config_str = fs::read_to_string(&config_path)
            .map_err(|e| miette!("Failed to read config: {}", e))?;
        if !config_str.contains("[sync]") {
            config_str.push_str("\n[sync]\n");
        }
        if config_str.contains("device_id =") {
            // Replace existing
        } else {
            config_str.push_str(&format!("device_id = \"{}\"\n", device_id));
        }
        fs::write(&config_path, config_str)
            .map_err(|e| miette!("Failed to write config: {}", e))?;
    }

    secret.zeroize();

    println!(
        "Sync initialized successfully. Device key saved to {:?}",
        key_path
    );
    Ok(())
}
