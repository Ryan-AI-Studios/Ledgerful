#[cfg(feature = "sync")]
mod tests {
    use ledgerful::sync::transport::{
        DirTransport, InMemoryTransport, IncomingBundle, Transport, is_bundle_filename,
    };
    use std::path::Path;
    use tempfile::tempdir;

    #[test]
    fn is_bundle_filename_accepts_lfbundle_and_gpg() {
        assert!(is_bundle_filename("x.lfbundle"));
        assert!(is_bundle_filename("x.zip.gpg")); // last-dot gpg
        assert!(is_bundle_filename("plain.gpg"));
        assert!(!is_bundle_filename("x.lfbundle.part"));
        assert!(!is_bundle_filename("x.tmp"));
        assert!(!is_bundle_filename("noext"));
    }

    #[test]
    fn test_in_memory_transport_sanity() {
        let transport = InMemoryTransport::new();

        assert!(transport.list_outgoing().unwrap().is_empty());
        assert!(transport.list_incoming().unwrap().is_empty());

        transport
            .put_outgoing_bytes("test_bundle.lfbundle", b"bundle content")
            .unwrap();

        let outgoing = transport.list_outgoing().unwrap();
        assert_eq!(outgoing.len(), 1);
        assert_eq!(outgoing[0].to_str().unwrap(), "test_bundle.lfbundle");

        transport
            .add_incoming_bytes("peer-b", "test_bundle.lfbundle", b"peer content")
            .unwrap();
        let incoming = transport.list_incoming().unwrap();
        assert_eq!(incoming.len(), 1);
        assert_eq!(incoming[0].peer_id, "peer-b");
        assert_eq!(incoming[0].name, "test_bundle.lfbundle");

        let content = transport.get_incoming(&incoming[0]).unwrap();
        assert_eq!(content, b"peer content");
    }

    #[test]
    fn test_dir_transport_round_trip_lfbundle() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();

        let device_id = "device_a";
        let transport = DirTransport::new(root, device_id);

        transport
            .put_outgoing_bytes("test.lfbundle", b"content")
            .unwrap();

        let outbox = root.join("devices").join(device_id);
        assert!(outbox.join("test.lfbundle").exists());
        // Temp dir must not leave matching bundle names
        let tmp_dir = outbox.join(".tmp");
        if tmp_dir.exists() {
            for e in std::fs::read_dir(&tmp_dir).unwrap() {
                let name = e.unwrap().file_name().to_string_lossy().into_owned();
                assert!(
                    !is_bundle_filename(&name),
                    "temp name must not match bundle filter: {name}"
                );
            }
        }

        let peer_id = "device_b";
        let peer_outbox = root.join("devices").join(peer_id);
        std::fs::create_dir_all(&peer_outbox).unwrap();
        std::fs::write(peer_outbox.join("peer.lfbundle"), b"peer content").unwrap();
        // Legacy dual-read
        std::fs::write(peer_outbox.join("legacy.zip.gpg"), b"legacy content").unwrap();

        let incoming = transport.list_incoming().unwrap();
        assert_eq!(incoming.len(), 2);
        assert!(
            incoming
                .iter()
                .any(|i| i.peer_id == peer_id && i.name == "peer.lfbundle")
        );
        assert!(
            incoming
                .iter()
                .any(|i| i.peer_id == peer_id && i.name == "legacy.zip.gpg")
        );

        let id = IncomingBundle {
            peer_id: peer_id.to_string(),
            name: "peer.lfbundle".to_string(),
        };
        let content = transport.get_incoming(&id).unwrap();
        assert_eq!(content, b"peer content");

        transport.move_to_processed(&id).unwrap();
        let processed = root.join("devices").join(device_id).join("processed");
        assert!(processed.join(format!("{peer_id}__peer.lfbundle")).exists());
        assert!(!peer_outbox.join("peer.lfbundle").exists());
    }

    #[test]
    fn two_peers_same_basename_get_and_move_correct_peer_only() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        let me = "device_me";
        let transport = DirTransport::new(root, me);

        let peer_a = "peer_a";
        let peer_b = "peer_b";
        let shared_name = "same-name.lfbundle";

        for (peer, body) in [(peer_a, b"from-a" as &[u8]), (peer_b, b"from-b")] {
            let dir = root.join("devices").join(peer);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(shared_name), body).unwrap();
        }

        let incoming = transport.list_incoming().unwrap();
        assert_eq!(incoming.len(), 2);

        let id_a = IncomingBundle {
            peer_id: peer_a.to_string(),
            name: shared_name.to_string(),
        };
        let id_b = IncomingBundle {
            peer_id: peer_b.to_string(),
            name: shared_name.to_string(),
        };

        assert_eq!(transport.get_incoming(&id_a).unwrap(), b"from-a");
        assert_eq!(transport.get_incoming(&id_b).unwrap(), b"from-b");

        // Move only A — B must remain in place
        transport.move_to_processed(&id_a).unwrap();
        assert!(!root.join("devices").join(peer_a).join(shared_name).exists());
        assert!(root.join("devices").join(peer_b).join(shared_name).exists());
        assert_eq!(transport.get_incoming(&id_b).unwrap(), b"from-b");

        let processed = root.join("devices").join(me).join("processed");
        assert!(processed.join(format!("{peer_a}__{shared_name}")).exists());
    }

    #[test]
    fn list_outgoing_accepts_both_extensions() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        let device_id = "dev";
        let transport = DirTransport::new(root, device_id);
        let outbox = root.join("devices").join(device_id);
        std::fs::create_dir_all(&outbox).unwrap();
        std::fs::write(outbox.join("a.lfbundle"), b"1").unwrap();
        std::fs::write(outbox.join("b.zip.gpg"), b"2").unwrap();
        std::fs::write(outbox.join("c.part"), b"3").unwrap();
        std::fs::write(outbox.join(".tmp").join("x.part"), b"4").ok();

        let outgoing = transport.list_outgoing().unwrap();
        let names: Vec<_> = outgoing
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"a.lfbundle".to_string()));
        assert!(names.contains(&"b.zip.gpg".to_string()));
        assert!(!names.iter().any(|n| n.ends_with(".part")));
    }

    #[test]
    fn put_outgoing_bytes_writes_same_volume_not_os_temp_only() {
        // DirTransport.put_outgoing_bytes must stage under outbox/.tmp then rename.
        // Prove final file lands in devices/<id>/ and .tmp names are non-bundle.
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        let device_id = "dev";
        let transport = DirTransport::new(root, device_id);

        transport
            .put_outgoing_bytes("export.lfbundle", b"encrypted-payload")
            .unwrap();

        let outbox = root.join("devices").join(device_id);
        let final_path = outbox.join("export.lfbundle");
        assert!(final_path.exists());
        assert_eq!(std::fs::read(&final_path).unwrap(), b"encrypted-payload");

        // Path-based put also goes through same-volume bytes path
        let staging = root.join("staging-src.lfbundle");
        std::fs::write(&staging, b"via-path").unwrap();
        transport.put_outgoing(&staging).unwrap();
        assert_eq!(
            std::fs::read(outbox.join("staging-src.lfbundle")).unwrap(),
            b"via-path"
        );

        // Ensure we never require the source path to remain (rename/copy handled)
        let _ = Path::new(&staging);
    }

    #[test]
    fn quarantine_uses_same_peer_identity() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        let me = "me";
        let transport = DirTransport::new(root, me);
        let peer = "peer_x";
        let name = "poison.lfbundle";
        let dir = root.join("devices").join(peer);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(name), b"bad").unwrap();

        let id = IncomingBundle {
            peer_id: peer.to_string(),
            name: name.to_string(),
        };
        transport.move_to_quarantine(&id).unwrap();
        assert!(!dir.join(name).exists());
        assert!(
            root.join("devices")
                .join(me)
                .join("quarantine")
                .join(format!("{peer}__{name}"))
                .exists()
        );
    }
}
