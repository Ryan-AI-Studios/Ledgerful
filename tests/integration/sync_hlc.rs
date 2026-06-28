use ledgerful::sync::hlc::HLC;

#[test]
fn test_hlc_monotonic() {
    let node_id = "test-node";
    // We need an initial HLC to start. The plan says now(last_observed, node_id).
    // Let's assume there's a way to create an initial one or now handles a base case.
    // Given the signature, we'll try to use a "zero" HLC or a constructor if it exists.
    // For the Red commit, we'll try to use what the plan says will exist.

    let mut last = HLC {
        physical_ms: 0,
        logical: 0,
        node_id: node_id.to_string(),
    };

    for _ in 0..100 {
        let current = HLC::now(&last, node_id);
        assert!(
            current > last,
            "HLC must be strictly increasing: current={:?}, last={:?}",
            current,
            last
        );
        last = current;
    }
}

#[test]
fn test_hlc_now_increments_logical_on_same_millisecond() {
    let node_id = "test-node";
    let base = HLC {
        physical_ms: u64::MAX,
        logical: 0,
        node_id: node_id.to_string(),
    };
    let next = HLC::now(&base, node_id);
    assert_eq!(
        next.physical_ms,
        u64::MAX,
        "wall clock is below base, so physical_ms should stay at base"
    );
    assert_eq!(
        next.logical, 1,
        "logical must increment when wall clock hasn't advanced"
    );
    let next2 = HLC::now(&next, node_id);
    assert_eq!(
        next2.logical, 2,
        "logical must increment again on same millisecond"
    );
}

#[test]
fn test_hlc_observe_takes_max_of_local_and_remote() {
    let node_id = "node-1";
    let mut local = HLC {
        physical_ms: 100,
        logical: 5,
        node_id: node_id.to_string(),
    };

    let remote = HLC {
        physical_ms: 200,
        logical: 10,
        node_id: "node-2".to_string(),
    };

    local.observe(&remote);
    assert!(local.physical_ms >= 200);
    // If wall clock is < 200, it should be 200, logical 11
    // In our implementation, we don't mock wall clock yet, but we can assert basic properties.
}

#[test]
fn test_hlc_string_round_trip() {
    use std::str::FromStr;
    let hlcs = vec![
        HLC {
            physical_ms: 123456789,
            logical: 0,
            node_id: "node-a".to_string(),
        },
        HLC {
            physical_ms: 123456789,
            logical: 42,
            node_id: "node-b".to_string(),
        },
        HLC {
            physical_ms: 0,
            logical: 0,
            node_id: "z".to_string(),
        },
        HLC {
            physical_ms: u64::MAX,
            logical: u32::MAX,
            node_id: "max".to_string(),
        },
        HLC {
            physical_ms: 100,
            logical: 1,
            node_id: "complex-node-id".to_string(),
        },
    ];

    for hlc in hlcs {
        let s = hlc.to_string();
        let parsed = HLC::from_str(&s).expect("Failed to parse HLC string");
        assert_eq!(hlc, parsed);
    }
}

#[test]
fn test_hlc_rejects_invalid_strings() {
    use std::str::FromStr;
    let invalid = vec!["", "not-a-number", "123-abc-node", "123-0001", "123--node"];

    for s in invalid {
        assert!(
            HLC::from_str(s).is_err(),
            "Should have failed to parse: {}",
            s
        );
    }
}
