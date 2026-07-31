use crate::sync::transport::{IncomingBundle, Result, Transport, is_bundle_filename};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

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

    /// Same-volume temp directory under the outbox. Names inside must not match
    /// [`is_bundle_filename`] so list_* never treats temps as bundles.
    fn outbox_tmp_dir(&self) -> PathBuf {
        self.outbox_dir().join(".tmp")
    }

    fn peer_bundle_path(&self, id: &IncomingBundle) -> PathBuf {
        self.devices_dir().join(&id.peer_id).join(&id.name)
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
                && let Some(name) = path.file_name().and_then(|n| n.to_str())
                && is_bundle_filename(name)
            {
                entries.push(PathBuf::from(name));
            }
        }
        entries.sort();
        Ok(entries)
    }

    fn put_outgoing(&self, bundle: &Path) -> Result<()> {
        let filename = bundle.file_name().and_then(|n| n.to_str()).ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "Invalid bundle path")
        })?;
        let content = fs::read(bundle)?;
        self.put_outgoing_bytes(filename, &content)
    }

    fn put_outgoing_bytes(&self, name: &str, content: &[u8]) -> Result<()> {
        let outbox = self.outbox_dir();
        fs::create_dir_all(&outbox)?;

        // Same-volume temp under outbox/.tmp/ — never OS temp (EXDEV on NAS/USB).
        // Temp names use `.part` so they do not match is_bundle_filename.
        let tmp_dir = self.outbox_tmp_dir();
        fs::create_dir_all(&tmp_dir)?;

        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        // Last-dot extension is `part` — not gpg/lfbundle.
        let tmp_name = format!(".{name}.{nanos}.part");
        let tmp_path = tmp_dir.join(&tmp_name);
        let dest = outbox.join(name);

        fs::write(&tmp_path, content)?;

        // Windows: rename over existing may fail — remove first.
        if dest.exists() {
            fs::remove_file(&dest)?;
        }
        if let Err(e) = fs::rename(&tmp_path, &dest) {
            // Same-volume rename should succeed; fall back to copy+delete only if needed
            // (still same volume — both under outbox).
            if let Err(copy_err) = fs::copy(&tmp_path, &dest) {
                let _ = fs::remove_file(&tmp_path);
                return Err(std::io::Error::other(format!(
                    "Failed to finalize bundle put (rename: {e}; copy: {copy_err})"
                ))
                .into());
            }
            let _ = fs::remove_file(&tmp_path);
        }

        Ok(())
    }

    fn list_incoming(&self) -> Result<Vec<IncomingBundle>> {
        let devices = self.devices_dir();
        if !devices.exists() {
            return Ok(vec![]);
        }

        let mut entries = Vec::new();
        for device_entry in fs::read_dir(devices)? {
            let device_entry = device_entry?;
            let device_name = device_entry.file_name();
            let peer_id = device_name.to_string_lossy().into_owned();
            if peer_id == self.device_id {
                continue; // Skip self — peer-scoped list is SoT for self-skip
            }

            let peer_dir = device_entry.path();
            if peer_dir.is_dir() {
                for bundle_entry in fs::read_dir(&peer_dir)? {
                    let bundle_entry = bundle_entry?;
                    let path = bundle_entry.path();
                    if path.is_file()
                        && let Some(name) = path.file_name().and_then(|n| n.to_str())
                        && is_bundle_filename(name)
                    {
                        entries.push(IncomingBundle {
                            peer_id: peer_id.clone(),
                            name: name.to_string(),
                        });
                    }
                }
            }
        }
        entries.sort_by(|a, b| (&a.peer_id, &a.name).cmp(&(&b.peer_id, &b.name)));
        Ok(entries)
    }

    fn get_incoming(&self, id: &IncomingBundle) -> Result<Vec<u8>> {
        let path = self.peer_bundle_path(id);
        if !path.exists() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("Bundle not found for peer '{}': {}", id.peer_id, id.name),
            )
            .into());
        }
        Ok(fs::read(path)?)
    }

    fn move_to_processed(&self, id: &IncomingBundle) -> Result<()> {
        let processed = self.processed_dir();
        fs::create_dir_all(&processed)?;

        let src = self.peer_bundle_path(id);
        if !src.exists() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("Bundle not found for peer '{}': {}", id.peer_id, id.name),
            )
            .into());
        }

        // Disambiguate processed names if two peers share a basename.
        let dest_name = format!("{}__{}", id.peer_id, id.name);
        let dest = processed.join(dest_name);
        if dest.exists() {
            fs::remove_file(&dest)?;
        }
        if fs::rename(&src, &dest).is_err() {
            fs::copy(&src, &dest)?;
            fs::remove_file(src)?;
        }
        Ok(())
    }

    fn move_to_quarantine(&self, id: &IncomingBundle) -> Result<()> {
        let quarantine = self.quarantine_dir();
        fs::create_dir_all(&quarantine)?;

        let src = self.peer_bundle_path(id);
        if !src.exists() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("Bundle not found for peer '{}': {}", id.peer_id, id.name),
            )
            .into());
        }

        let dest_name = format!("{}__{}", id.peer_id, id.name);
        let dest = quarantine.join(dest_name);
        if dest.exists() {
            fs::remove_file(&dest)?;
        }
        if fs::rename(&src, &dest).is_err() {
            fs::copy(&src, &dest)?;
            fs::remove_file(src)?;
        }
        Ok(())
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
