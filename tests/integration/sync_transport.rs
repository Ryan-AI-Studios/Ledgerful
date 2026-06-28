#[cfg(feature = "sync")]
mod tests {
    use ledgerful::sync::transport::{InMemoryTransport, Transport};
    use std::path::Path;

    use tempfile::tempdir;

    #[test]
    fn test_in_memory_transport_sanity() {
        let transport = InMemoryTransport::new();

        // Initial state: empty
        assert!(transport.list_outgoing().unwrap().is_empty());
        assert!(transport.list_incoming().unwrap().is_empty());

        // Put outgoing
        let _bundle_path = Path::new("test_bundle.zip.gpg");
        transport
            .put_outgoing_bytes("test_bundle.zip.gpg", b"bundle content")
            .unwrap();

        let outgoing = transport.list_outgoing().unwrap();
        assert_eq!(outgoing.len(), 1);
        assert_eq!(outgoing[0].to_str().unwrap(), "test_bundle.zip.gpg");

        // In memory transport simulates another device by making outgoing available as incoming?
        // Actually the spec says:
        // "The DirTransport implementation automatically uses <target>/devices/<device_id>/ as the outbox
        // and <target>/devices/*/ (all peer subdirectories) as the inbox."
        // So InMemoryTransport should probably support multiple devices or a way to simulate peers.
    }

    #[test]
    fn test_dir_transport_round_trip() {
        use ledgerful::sync::transport::DirTransport;
        let tmp = tempdir().unwrap();
        let root = tmp.path();

        let device_id = "device_a";
        let transport = DirTransport::new(root, device_id);

        // Put outgoing
        let bundle_file = root.join("test.zip.gpg");
        std::fs::write(&bundle_file, b"content").unwrap();
        transport.put_outgoing(&bundle_file).unwrap();

        // Check if it's in the outbox
        let outbox = root.join("devices").join(device_id);
        assert!(outbox.join("test.zip.gpg").exists());

        // Simulate a peer bundle in their outbox
        let peer_id = "device_b";
        let peer_outbox = root.join("devices").join(peer_id);
        std::fs::create_dir_all(&peer_outbox).unwrap();
        std::fs::write(peer_outbox.join("peer.zip.gpg"), b"peer content").unwrap();

        // List incoming should find it
        let incoming = transport.list_incoming().unwrap();
        assert_eq!(incoming.len(), 1);
        assert!(incoming[0].to_string_lossy().contains("peer.zip.gpg"));

        // Get incoming
        let content = transport.get_incoming("peer.zip.gpg").unwrap();
        assert_eq!(content, b"peer content");

        // Move to processed
        transport.move_to_processed("peer.zip.gpg").unwrap();
        let processed = root.join("devices").join(device_id).join("processed");
        assert!(processed.join("peer.zip.gpg").exists());
        assert!(!peer_outbox.join("peer.zip.gpg").exists());
    }
}
