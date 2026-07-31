#[cfg(feature = "sync")]
mod tests {
    use ledgerful::sync::transport::{InMemoryTransport, IncomingBundle, Transport};
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_in_memory_transport_sanity() {
        let transport = InMemoryTransport::new();

        assert!(transport.list_outgoing().unwrap().is_empty());
        assert!(transport.list_incoming().unwrap().is_empty());

        let tmp = tempdir().unwrap();
        let bundle_file = tmp.path().join("bundle.lfbundle");
        fs::write(&bundle_file, b"content").unwrap();

        transport.put_outgoing(&bundle_file).unwrap();

        let outgoing = transport.list_outgoing().unwrap();
        assert_eq!(outgoing.len(), 1);
        assert!(outgoing[0].to_string_lossy().contains("bundle.lfbundle"));

        transport
            .add_incoming_bytes("peer-1", "peer_bundle.lfbundle", b"peer content")
            .unwrap();
        let incoming = transport.list_incoming().unwrap();
        assert_eq!(incoming.len(), 1);

        let content = transport.get_incoming(&incoming[0]).unwrap();
        assert_eq!(content, b"peer content");

        transport.move_to_processed(&incoming[0]).unwrap();
        assert!(transport.list_incoming().unwrap().is_empty());

        let bad = IncomingBundle {
            peer_id: "peer-1".to_string(),
            name: "bad_bundle.lfbundle".to_string(),
        };
        transport
            .add_incoming_bytes(&bad.peer_id, &bad.name, b"bad content")
            .unwrap();
        transport.move_to_quarantine(&bad).unwrap();
        assert!(transport.list_incoming().unwrap().is_empty());
    }
}
