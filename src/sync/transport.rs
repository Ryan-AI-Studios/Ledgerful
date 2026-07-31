use crate::sync::error::SyncError;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

pub type Result<T> = std::result::Result<T, SyncError>;

pub mod dir;
pub use dir::DirTransport;

/// Written extension for new bundles (honest name — not OpenPGP).
pub const BUNDLE_EXT: &str = "lfbundle";

/// True when the last-dot extension is `lfbundle` (new) or `gpg` (legacy dual-read).
/// Live pre-0112 filter was last-dot `gpg` (any `*.gpg`), not only `*.zip.gpg`.
pub fn is_bundle_filename(name: &str) -> bool {
    Path::new(name)
        .extension()
        .and_then(|s| s.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case(BUNDLE_EXT) || ext.eq_ignore_ascii_case("gpg"))
}

/// Resolved identity for an incoming peer bundle.
///
/// Must be threaded end-to-end through get → apply → move so a bare-name re-search
/// cannot archive a different peer's same-named file after reading another.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct IncomingBundle {
    pub peer_id: String,
    pub name: String,
}

#[derive(Debug, Clone)]
pub enum SyncTarget {
    Dir(PathBuf),
}

impl SyncTarget {
    pub fn parse(s: &str) -> Result<Self> {
        if let Some(path_str) = s.strip_prefix("dir://") {
            // Normalize path: handle dir:///C:/... on Windows
            let path = if path_str.starts_with('/') && path_str.get(2..3) == Some(":") {
                PathBuf::from(&path_str[1..])
            } else {
                PathBuf::from(path_str)
            };
            Ok(SyncTarget::Dir(path))
        } else {
            Err(SyncError::UnsupportedTarget(s.to_string()))
        }
    }

    pub fn connect(&self, device_id: &str) -> Box<dyn Transport> {
        match self {
            SyncTarget::Dir(path) => Box::new(DirTransport::new(path, device_id)),
        }
    }
}

pub trait Transport: Send + Sync {
    fn list_outgoing(&self) -> Result<Vec<PathBuf>>;
    /// Path-based put (reads file). Prefer [`Self::put_outgoing_bytes`] for same-volume
    /// atomic write without an OS-temp staging file.
    fn put_outgoing(&self, bundle: &Path) -> Result<()>;
    /// Write encrypted bundle bytes under the device outbox using a **same-volume**
    /// temp + rename (never OS temp → share, which fails EXDEV on NAS/USB mounts).
    fn put_outgoing_bytes(&self, name: &str, content: &[u8]) -> Result<()>;
    fn list_incoming(&self) -> Result<Vec<IncomingBundle>>;
    fn get_incoming(&self, id: &IncomingBundle) -> Result<Vec<u8>>;
    fn move_to_processed(&self, id: &IncomingBundle) -> Result<()>;
    fn move_to_quarantine(&self, id: &IncomingBundle) -> Result<()>;
    fn trim_processed(&self, older_than: SystemTime) -> Result<usize>;
}

type PeerBundleMap = HashMap<(String, String), Vec<u8>>;

pub struct InMemoryTransport {
    pub outgoing: Arc<RwLock<HashMap<String, Vec<u8>>>>,
    /// Keyed by (peer_id, name) so identity is unambiguous.
    pub incoming: Arc<RwLock<PeerBundleMap>>,
    pub processed: Arc<RwLock<PeerBundleMap>>,
    pub quarantine: Arc<RwLock<PeerBundleMap>>,
}

impl Default for InMemoryTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryTransport {
    pub fn new() -> Self {
        Self {
            outgoing: Arc::new(RwLock::new(HashMap::new())),
            incoming: Arc::new(RwLock::new(HashMap::new())),
            processed: Arc::new(RwLock::new(HashMap::new())),
            quarantine: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Seed an incoming peer bundle for tests.
    pub fn add_incoming_bytes(&self, peer_id: &str, name: &str, content: &[u8]) -> Result<()> {
        self.incoming
            .write()
            .insert((peer_id.to_string(), name.to_string()), content.to_vec());
        Ok(())
    }
}

impl Transport for InMemoryTransport {
    fn list_outgoing(&self) -> Result<Vec<PathBuf>> {
        let mut names: Vec<PathBuf> = self
            .outgoing
            .read()
            .keys()
            .filter(|n| is_bundle_filename(n))
            .map(PathBuf::from)
            .collect();
        names.sort();
        Ok(names)
    }

    fn put_outgoing(&self, bundle: &Path) -> Result<()> {
        let content = std::fs::read(bundle)?;
        let name = bundle
            .file_name()
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "Invalid bundle path")
            })?
            .to_string_lossy()
            .to_string();
        self.put_outgoing_bytes(&name, &content)
    }

    fn put_outgoing_bytes(&self, name: &str, content: &[u8]) -> Result<()> {
        self.outgoing
            .write()
            .insert(name.to_string(), content.to_vec());
        Ok(())
    }

    fn list_incoming(&self) -> Result<Vec<IncomingBundle>> {
        let mut entries: Vec<IncomingBundle> = self
            .incoming
            .read()
            .keys()
            .filter(|(_, name)| is_bundle_filename(name))
            .map(|(peer_id, name)| IncomingBundle {
                peer_id: peer_id.clone(),
                name: name.clone(),
            })
            .collect();
        entries.sort_by(|a, b| (&a.peer_id, &a.name).cmp(&(&b.peer_id, &b.name)));
        Ok(entries)
    }

    fn get_incoming(&self, id: &IncomingBundle) -> Result<Vec<u8>> {
        self.incoming
            .read()
            .get(&(id.peer_id.clone(), id.name.clone()))
            .cloned()
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::NotFound, "Bundle not found in incoming")
                    .into()
            })
    }

    fn move_to_processed(&self, id: &IncomingBundle) -> Result<()> {
        let key = (id.peer_id.clone(), id.name.clone());
        let mut incoming = self.incoming.write();
        if let Some(content) = incoming.remove(&key) {
            self.processed.write().insert(key, content);
            Ok(())
        } else {
            Err(
                std::io::Error::new(std::io::ErrorKind::NotFound, "Bundle not found in incoming")
                    .into(),
            )
        }
    }

    fn move_to_quarantine(&self, id: &IncomingBundle) -> Result<()> {
        let key = (id.peer_id.clone(), id.name.clone());
        let mut incoming = self.incoming.write();
        if let Some(content) = incoming.remove(&key) {
            self.quarantine.write().insert(key, content);
            Ok(())
        } else {
            Err(
                std::io::Error::new(std::io::ErrorKind::NotFound, "Bundle not found in incoming")
                    .into(),
            )
        }
    }

    fn trim_processed(&self, _older_than: SystemTime) -> Result<usize> {
        // For InMemory, we don't have timestamps on the entries unless we store them.
        Ok(0)
    }
}
