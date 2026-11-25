use super::*;
use crate::commands::architecture::test_support::{COMPONENTS, edge_set};

const FROM: &str = "src/a/endpoint.rs";
const TO: &str = "src/b/endpoint.rs";
const HEAD: &str = "0123456789012345678901234567890123456789";

fn request(context: Option<String>) -> ProposalRequest {
    ProposalRequest {
        from: FROM.to_owned(),
        to: TO.to_owned(),
        kind: FILE_CALL.to_owned(),
        head: HEAD.to_owned(),
        context,
    }
}

fn policy(unrelated_rule_id: &str, decision_id: &str) -> String {
    format!(
        "{COMPONENTS}\n[[rules]]\nid='{unrelated_rule_id}'\neffect='forbid'\nfrom='b'\nto='c'\n\
        [[rules]]\nid='{decision_id}'\neffect='forbid'\nfrom='a'\nto='b'\n"
    )
}

fn snapshot(policy: &str, pair_count: usize) -> Snapshot {
    let mut edges = edge_set(&[(TO, FROM, 1)]);
    edges.identity.index_head_hash = Some(HEAD.to_owned());
    edges.coverage.edges_resolved_cross_file = pair_count as u64;
    // Repeated component-level b→c relationships with distinct file pairs.
    // The exact projected cap remains below the 2,000-file ceiling.
    for index in 0..pair_count - 1 {
        edges.edges.push(FileEdge {
            src_path: format!("src/b/f{:03}.rs", index / 100),
            dst_path: format!("src/c/f{:03}.rs", index % 100),
            weight: 1,
        });
    }
    Snapshot {
        raw_config: policy.as_bytes().to_vec(),
        index_config_digest: blake3::hash(b"fixture config"),
        scope: [1; 32],
        edges,
    }
}

fn assert_refusal(packet: &ProposalPacket, reason: &str) {
    assert_eq!(packet.status, "unknown");
    assert_eq!(packet.permission, "unknown");
    assert_eq!(packet.reason_codes, [reason]);
    assert!(packet.observed_context.is_none());
    assert!(packet.projection.is_none());
}

#[test]
fn architecture_proposal_unrelated_long_forbid_does_not_expand_projection_at_pair_cap() {
    let id = "r".repeat(64);
    // Landed 0602 admits rule identifiers beyond the proposal wire limit. The
    // near-cap identifier matches every unrelated pair in this fixture.
    let policy = policy(&"x".repeat(60 * 1024), &id);
    let prepare = |pairs| {
        evaluate_snapshot(request(None), snapshot(&policy, pairs), &mut || Ok(()))
            .observed_context
            .unwrap()
            .token
    };
    let replay = |pairs| {
        evaluate_snapshot(
            request(Some(prepare(pairs))),
            snapshot(&policy, pairs),
            &mut || Ok(()),
        )
    };

    let packet = replay(MAX_PAIRS - 1);
    assert_eq!(packet.status, "evaluated");
    assert_eq!(packet.permission, "unknown");
    let projection = packet.projection.unwrap();
    assert_eq!(projection.rule_decision, "forbid");
    assert_eq!(projection.rule_id.as_deref(), Some(id.as_str()));
    assert_eq!(projection.cycle_effect, "introduced");
    assert_eq!(projection.members.unwrap(), ["a", "b"]);
    assert_refusal(&replay(MAX_PAIRS), "LIMIT_EXCEEDED");
}

#[test]
fn architecture_proposal_refuses_oversize_winning_rule_identifier() {
    let policy = policy("unrelated", &"r".repeat(65));
    let packet = evaluate_snapshot(request(None), snapshot(&policy, 2), &mut || Ok(()));
    assert_refusal(&packet, "UNREPRESENTABLE_IDENTIFIER");
}

#[test]
fn architecture_proposal_mid_projection_expiry_discards_all_fields() {
    const BASE_PAIRS: usize = 32;
    let policy = policy("unrelated", "requested");
    let mut preparation_checks = 0;
    let prepared = evaluate_snapshot(request(None), snapshot(&policy, BASE_PAIRS), &mut || {
        preparation_checks += 1;
        Ok(())
    });
    let token = prepared.observed_context.unwrap().token;
    // Replay spends the prepare checks again, then one check per copied edge
    // plus the two bounds around that copy. Stop inside component assignment.
    let expiry = preparation_checks + BASE_PAIRS + 2 + 3;
    let mut checks = 0;
    let packet = evaluate_snapshot(
        request(Some(token)),
        snapshot(&policy, BASE_PAIRS),
        &mut || {
            checks += 1;
            if checks == expiry {
                Err("LIMIT_EXCEEDED")
            } else {
                Ok(())
            }
        },
    );
    assert_eq!(checks, expiry);
    assert_refusal(&packet, "LIMIT_EXCEEDED");
}
