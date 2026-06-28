use crate::sync::transport::{Result, Transport};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub struct DirTransport {
    root: PathBuf,
    device_id: String,
}

impl DirTransport {
    pub fn new(root: &Path, device_id: &str) -> Self {
        Self {
            root: root.to_path_buf(),
            device_id: device_id.to_string(),
        }
    }

    fn outbox_dir(&self) -> PathBuf {
        self.root.join("devices").join(&self.device_id)
    }

    fn processed_dir(&self) -> PathBuf {
        self.root
            .join("devices")
            .join(&self.device_id)
            .join("processed")
    }

    fn quarantine_dir(&self) -> PathBuf {
        self.root
            .join("devices")
            .join(&self.device_id)
            .join("quarantine")
    }

    fn devices_dir(&self) -> PathBuf {
        self.root.join("devices")
    }
}

impl Transport for DirTransport {
    fn list_outgoing(&self) -> Result<Vec<PathBuf>> {
        let dir = self.outbox_dir();
        if !dir.exists() {
            return Ok(vec![]);
        }

        let mut entries = Vec::new();
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_file()
                && path.extension().and_then(|s| s.to_str()) == Some("gpg")
                && let Some(name) = path.file_name()
            {
                entries.push(PathBuf::from(name));
            }
        }
        Ok(entries)
    }

    fn put_outgoing(&self, bundle: &Path) -> Result<()> {
        let outbox = self.outbox_dir();
        fs::create_dir_all(&outbox)?;

        let filename = bundle.file_name().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "Invalid bundle path")
        })?;

        let dest = outbox.join(filename);

        // Atomic move if possible, or copy + delete
        if fs::rename(bundle, &dest).is_err() {
            fs::copy(bundle, &dest)?;
            fs::remove_file(bundle)?;
        }

        Ok(())
    }

    fn list_incoming(&self) -> Result<Vec<PathBuf>> {
        let devices = self.devices_dir();
        if !devices.exists() {
            return Ok(vec![]);
        }

        let mut entries = Vec::new();
        for device_entry in fs::read_dir(devices)? {
            let device_entry = device_entry?;
            let device_name = device_entry.file_name();
            if device_name.to_string_lossy() == self.device_id {
                continue; // Skip self
            }

            let peer_dir = device_entry.path();
            if peer_dir.is_dir() {
                for bundle_entry in fs::read_dir(peer_dir)? {
                    let bundle_entry = bundle_entry?;
                    let path = bundle_entry.path();
                    if path.is_file()
                        && path.extension().and_then(|s| s.to_str()) == Some("gpg")
                        && let Some(name) = path.file_name()
                    {
                        entries.push(PathBuf::from(name));
                    }
                }
            }
        }
        Ok(entries)
    }

    fn get_incoming(&self, name: &str) -> Result<Vec<u8>> {
        // Search in all peer directories
        let devices = self.devices_dir();
        for device_entry in fs::read_dir(devices)? {
            let device_entry = device_entry?;
            if device_entry.file_name().to_string_lossy() == self.device_id {
                continue;
            }

            let path = device_entry.path().join(name);
            if path.exists() {
                return Ok(fs::read(path)?);
            }
        }

        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Bundle not found in any peer directory",
        )
        .into())
    }

    fn move_to_processed(&self, name: &str) -> Result<()> {
        let processed = self.processed_dir();
        fs::create_dir_all(&processed)?;

        // Find it in any peer directory
        let devices = self.devices_dir();
        for device_entry in fs::read_dir(devices)? {
            let device_entry = device_entry?;
            if device_entry.file_name().to_string_lossy() == self.device_id {
                continue;
            }

            let src = device_entry.path().join(name);
            if src.exists() {
                let dest = processed.join(name);
                if fs::rename(&src, &dest).is_err() {
                    fs::copy(&src, &dest)?;
                    fs::remove_file(src)?;
                }
                return Ok(());
            }
        }

        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Bundle not found in any peer directory",
        )
        .into())
    }

    fn move_to_quarantine(&self, name: &str) -> Result<()> {
        let quarantine = self.quarantine_dir();
        fs::create_dir_all(&quarantine)?;

        // Find it in any peer directory
        let devices = self.devices_dir();
        for device_entry in fs::read_dir(devices)? {
            let device_entry = device_entry?;
            if device_entry.file_name().to_string_lossy() == self.device_id {
                continue;
            }

            let src = device_entry.path().join(name);
            if src.exists() {
                let dest = quarantine.join(name);
                if fs::rename(&src, &dest).is_err() {
                    fs::copy(&src, &dest)?;
                    fs::remove_file(src)?;
                }
                return Ok(());
            }
        }

        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Bundle not found in any peer directory",
        )
        .into())
    }

    fn trim_processed(&self, older_than: SystemTime) -> Result<usize> {
        let processed = self.processed_dir();
        if !processed.exists() {
            return Ok(0);
        }

        let mut count = 0;
        for entry in fs::read_dir(processed)? {
            let entry = entry?;
            let metadata = entry.metadata()?;
            let modified = metadata.modified()?;
            if modified < older_than {
                fs::remove_file(entry.path())?;
                count += 1;
            }
        }
        Ok(count)
    }
}
