use crate::sync::bundle::MAX_BUNDLE_SIZE;
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

    /// Single-component path segment (peer id): no separators, no `.` / `..`.
    fn is_safe_peer_id(peer_id: &str) -> bool {
        !peer_id.is_empty()
            && peer_id != "."
            && peer_id != ".."
            && !peer_id.contains('/')
            && !peer_id.contains('\\')
            && !peer_id.contains('\0')
    }

    /// Bundle file name only (no directory components).
    fn is_safe_bundle_name(name: &str) -> bool {
        !name.is_empty()
            && name != "."
            && name != ".."
            && !name.contains('/')
            && !name.contains('\\')
            && !name.contains('\0')
            && is_bundle_filename(name)
    }

    /// Regular file that is not a symlink (hostile drop folders may plant links).
    fn is_regular_non_symlink_file(path: &Path) -> bool {
        match fs::symlink_metadata(path) {
            Ok(meta) => meta.is_file() && !meta.file_type().is_symlink(),
            Err(_) => false,
        }
    }

    /// Directory that is not a symlink.
    fn is_regular_non_symlink_dir(path: &Path) -> bool {
        match fs::symlink_metadata(path) {
            Ok(meta) => meta.is_dir() && !meta.file_type().is_symlink(),
            Err(_) => false,
        }
    }

    /// Create `path` if missing, then require it is a non-symlink directory
    /// (rejects pre-planted junction/symlink destinations under the share).
    fn ensure_regular_dir(path: &Path) -> Result<()> {
        if path.exists() {
            if !Self::is_regular_non_symlink_dir(path) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!(
                        "path is not a regular directory (symlink/junction?): {}",
                        path.display()
                    ),
                )
                .into());
            }
            return Ok(());
        }
        fs::create_dir_all(path)?;
        if !Self::is_regular_non_symlink_dir(path) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "path is not a regular directory after create (symlink/junction?): {}",
                    path.display()
                ),
            )
            .into());
        }
        Ok(())
    }

    fn peer_bundle_path(&self, id: &IncomingBundle) -> Result<PathBuf> {
        if !Self::is_safe_peer_id(&id.peer_id) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("unsafe peer_id in bundle identity: {}", id.peer_id),
            )
            .into());
        }
        if !Self::is_safe_bundle_name(&id.name) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("unsafe bundle name in identity: {}", id.name),
            )
            .into());
        }
        let peer_dir = self.devices_dir().join(&id.peer_id);
        // Re-validate on every get/move (list→get TOCTOU: peer dir swapped to symlink).
        if !Self::is_regular_non_symlink_dir(&peer_dir) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "peer directory is missing, not a directory, or is a symlink: {}",
                    id.peer_id
                ),
            )
            .into());
        }
        Ok(peer_dir.join(&id.name))
    }

    /// Read at most `MAX_BUNDLE_SIZE` bytes (grows after open still bounded by take).
    fn read_bundle_capped(path: &Path) -> Result<Vec<u8>> {
        use std::io::Read;
        let mut file = fs::File::open(path)?;
        // Re-check after open: path must still be a non-symlink regular file.
        if !Self::is_regular_non_symlink_file(path) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "bundle path is not a regular non-symlink file after open",
            )
            .into());
        }
        let mut buf = Vec::new();
        let mut limited = (&mut file).take(MAX_BUNDLE_SIZE as u64 + 1);
        limited.read_to_end(&mut buf)?;
        if buf.len() > MAX_BUNDLE_SIZE {
            return Err(std::io::Error::other(format!(
                "Bundle exceeds maximum size ({} bytes cap)",
                MAX_BUNDLE_SIZE
            ))
            .into());
        }
        Ok(buf)
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
            if Self::is_regular_non_symlink_file(&path)
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
        Self::ensure_regular_dir(&outbox)?;

        // Same-volume temp under outbox/.tmp/ — never OS temp (EXDEV on NAS/USB).
        // Temp names use `.part` so they do not match is_bundle_filename.
        let tmp_dir = self.outbox_tmp_dir();
        Self::ensure_regular_dir(&tmp_dir)?;

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
            if peer_id == self.device_id || !Self::is_safe_peer_id(&peer_id) {
                continue; // Skip self + unsafe/symlink-named peer components
            }

            let peer_dir = device_entry.path();
            // Refuse symlinked peer dirs (hostile shared-folder escape).
            if !Self::is_regular_non_symlink_dir(&peer_dir) {
                continue;
            }
            for bundle_entry in fs::read_dir(&peer_dir)? {
                let bundle_entry = bundle_entry?;
                let path = bundle_entry.path();
                if Self::is_regular_non_symlink_file(&path)
                    && let Some(name) = path.file_name().and_then(|n| n.to_str())
                    && Self::is_safe_bundle_name(name)
                {
                    entries.push(IncomingBundle {
                        peer_id: peer_id.clone(),
                        name: name.to_string(),
                    });
                }
            }
        }
        entries.sort_by(|a, b| (&a.peer_id, &a.name).cmp(&(&b.peer_id, &b.name)));
        Ok(entries)
    }

    fn get_incoming(&self, id: &IncomingBundle) -> Result<Vec<u8>> {
        let path = self.peer_bundle_path(id)?;
        if !Self::is_regular_non_symlink_file(&path) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!(
                    "Bundle not found or not a regular file for peer '{}': {}",
                    id.peer_id, id.name
                ),
            )
            .into());
        }
        Self::read_bundle_capped(&path)
    }

    fn move_to_processed(&self, id: &IncomingBundle) -> Result<()> {
        let processed = self.processed_dir();
        Self::ensure_regular_dir(&processed)?;

        let src = self.peer_bundle_path(id)?;
        if !Self::is_regular_non_symlink_file(&src) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!(
                    "Bundle not found or not a regular file for peer '{}': {}",
                    id.peer_id, id.name
                ),
            )
            .into());
        }

        // Disambiguate processed names if two peers share a basename.
        let dest_name = format!("{}__{}", id.peer_id, id.name);
        let dest = processed.join(dest_name);
        if dest.exists() {
            if !Self::is_regular_non_symlink_file(&dest) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("processed dest is not a regular file: {}", dest.display()),
                )
                .into());
            }
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
        Self::ensure_regular_dir(&quarantine)?;

        let src = self.peer_bundle_path(id)?;
        if !Self::is_regular_non_symlink_file(&src) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!(
                    "Bundle not found or not a regular file for peer '{}': {}",
                    id.peer_id, id.name
                ),
            )
            .into());
        }

        let dest_name = format!("{}__{}", id.peer_id, id.name);
        let dest = quarantine.join(dest_name);
        if dest.exists() {
            if !Self::is_regular_non_symlink_file(&dest) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("quarantine dest is not a regular file: {}", dest.display()),
                )
                .into());
            }
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
        if !Self::is_regular_non_symlink_dir(&processed) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "processed path is not a regular directory (symlink/junction?): {}",
                    processed.display()
                ),
            )
            .into());
        }

        let mut count = 0;
        for entry in fs::read_dir(processed)? {
            let entry = entry?;
            let path = entry.path();
            // Only trim regular files — never follow symlink entries.
            if !Self::is_regular_non_symlink_file(&path) {
                continue;
            }
            let metadata = fs::symlink_metadata(&path)?;
            let modified = metadata.modified()?;
            if modified < older_than {
                fs::remove_file(path)?;
                count += 1;
            }
        }
        Ok(count)
    }
}
