use crate::sync::error::SyncError;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

pub type Result<T> = std::result::Result<T, SyncError>;

pub mod dir;
pub use dir::DirTransport;

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
    fn put_outgoing(&self, bundle: &Path) -> Result<()>;
    fn list_incoming(&self) -> Result<Vec<PathBuf>>;
    fn get_incoming(&self, name: &str) -> Result<Vec<u8>>;
    fn move_to_processed(&self, name: &str) -> Result<()>;
    fn move_to_quarantine(&self, name: &str) -> Result<()>;
    fn trim_processed(&self, older_than: SystemTime) -> Result<usize>;
}

pub struct InMemoryTransport {
    pub outgoing: Arc<RwLock<HashMap<String, Vec<u8>>>>,
    pub incoming: Arc<RwLock<HashMap<String, Vec<u8>>>>,
    pub processed: Arc<RwLock<HashMap<String, Vec<u8>>>>,
    pub quarantine: Arc<RwLock<HashMap<String, Vec<u8>>>>,
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

    pub fn put_outgoing_bytes(&self, name: &str, content: &[u8]) -> Result<()> {
        self.outgoing
            .write()
            .insert(name.to_string(), content.to_vec());
        Ok(())
    }

    pub fn add_incoming_bytes(&self, name: &str, content: &[u8]) -> Result<()> {
        self.incoming
            .write()
            .insert(name.to_string(), content.to_vec());
        Ok(())
    }
}

impl Transport for InMemoryTransport {
    fn list_outgoing(&self) -> Result<Vec<PathBuf>> {
        Ok(self.outgoing.read().keys().map(PathBuf::from).collect())
    }

    fn put_outgoing(&self, bundle: &Path) -> Result<()> {
        // In a real transport, this would read from the local file and upload.
        // For InMemory, we might need a way to pass the bytes.
        // But the trait says &Path. This is slightly awkward for InMemory.
        // We'll assume the test uses put_outgoing_bytes for now,
        // or we implement reading from the path if needed.
        let content = std::fs::read(bundle)?;
        let name = bundle
            .file_name()
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "Invalid bundle path")
            })?
            .to_string_lossy()
            .to_string();

        self.outgoing.write().insert(name, content);
        Ok(())
    }

    fn list_incoming(&self) -> Result<Vec<PathBuf>> {
        Ok(self.incoming.read().keys().map(PathBuf::from).collect())
    }

    fn get_incoming(&self, name: &str) -> Result<Vec<u8>> {
        self.incoming.read().get(name).cloned().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, "Bundle not found in incoming").into()
        })
    }

    fn move_to_processed(&self, name: &str) -> Result<()> {
        let mut incoming = self.incoming.write();
        if let Some(content) = incoming.remove(name) {
            self.processed.write().insert(name.to_string(), content);
            Ok(())
        } else {
            Err(
                std::io::Error::new(std::io::ErrorKind::NotFound, "Bundle not found in incoming")
                    .into(),
            )
        }
    }

    fn move_to_quarantine(&self, name: &str) -> Result<()> {
        let mut incoming = self.incoming.write();
        if let Some(content) = incoming.remove(name) {
            self.quarantine.write().insert(name.to_string(), content);
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
        // Let's just say we don't trim for now.
        Ok(0)
    }
}
