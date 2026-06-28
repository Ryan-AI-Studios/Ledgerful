#[cfg(feature = "sync")]
mod tests {
    use ledgerful::sync::transport::{Transport, InMemoryTransport};
    use std::path::Path;
    use tempfile::tempdir;
    use std::fs;

    #[test]
    fn test_in_memory_transport_sanity() {
        let transport = InMemoryTransport::new();
        
        assert!(transport.list_outgoing().unwrap().is_empty());
        assert!(transport.list_incoming().unwrap().is_empty());

        // Test put_outgoing via a real file because the trait takes &Path
        let tmp = tempdir().unwrap();
        let bundle_file = tmp.path().join("bundle.zip.gpg");
        fs::write(&bundle_file, b"content").unwrap();
        
        transport.put_outgoing(&bundle_file).unwrap();
        
        let outgoing = transport.list_outgoing().unwrap();
        assert_eq!(outgoing.len(), 1);
        assert!(outgoing[0].to_string_lossy().contains("bundle.zip.gpg"));

        // Test incoming
        transport.add_incoming_bytes("peer_bundle.zip.gpg", b"peer content").unwrap();
        let incoming = transport.list_incoming().unwrap();
        assert_eq!(incoming.len(), 1);
        
        let content = transport.get_incoming("peer_bundle.zip.gpg").unwrap();
        assert_eq!(content, b"peer content");

        // Test move to processed
        transport.move_to_processed("peer_bundle.zip.gpg").unwrap();
        assert!(transport.list_incoming().unwrap().is_empty());
        
        // Test move to quarantine
        transport.add_incoming_bytes("bad_bundle.zip.gpg", b"bad content").unwrap();
        transport.move_to_quarantine("bad_bundle.zip.gpg").unwrap();
        assert!(transport.list_incoming().unwrap().is_empty());
    }
}
