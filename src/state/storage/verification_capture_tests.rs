use super::*;
use crate::change_evidence::attempt_projection::{
    AttemptKind, Label, Prediction, Projection, Sample, Scope, Step,
};
use crate::commands::verify::AttemptPayload;
use crate::state::storage::in_memory_storage;
use crate::state::storage::verification::AttemptIdentity;
use crate::verify::results::{VerificationReport, VerificationResult};
use crate::verify::selection_evidence::{PredictionState, SelectionBasis, StepKindJson, StepState};
use rusqlite::hooks::{AuthAction, AuthContext, Authorization, TransactionOperation};

const TIMESTAMP: &str = "2026-11-20T21:00:00Z";
const COMMAND: &str = "git --version";

fn storage() -> StorageManager {
    let storage = in_memory_storage();
    storage
        .conn
        .pragma_update(None, "foreign_keys", true)
        .unwrap();
    storage
}

fn execution() -> CompletedExecution<'static> {
    let results = [11, 22]
        .into_iter()
        .map(|duration_ms| VerificationResult {
            command: COMMAND.into(),
            exit_code: 0,
            duration_ms,
            stdout_summary: String::new(),
            stderr_summary: String::new(),
            truncated: false,
            timestamp: TIMESTAMP.into(),
        })
        .collect();
    let mut report = VerificationReport::new(None, results);
    report.timestamp = TIMESTAMP.into();
    CompletedExecution::for_storage_test(report)
}

fn attempt(attempt_id: Uuid) -> PreparedExecutedAttempt {
    let observed = TIMESTAMP.parse().unwrap();
    let identity = AttemptIdentity {
        repo_scope: None,
        authorize_first_scope: true,
        scope_guard: [1; 32],
        attempt_id,
        head: "a".repeat(40),
        observed_from: observed,
        observed_to: observed,
    };
    let projection = Projection {
        schema_version: 1,
        selection_schema_version: 1,
        attempt_kind: AttemptKind::Executed,
        scope_requested: Scope::Full,
        scope_executed: Scope::Full,
        basis: SelectionBasis::ExplicitSteps,
        prediction: Prediction {
            state: PredictionState::NotApplicable,
            warning_count: 0,
            impact_freshness: None,
        },
        steps: [11, 22]
            .into_iter()
            .enumerate()
            .map(|(index, duration)| Step {
                index: index as u64,
                command: Label::GitVersion,
                kind: StepKindJson::NonTest,
                state: StepState::Passed,
                exit_code: Some(0),
                duration_ms: Some(duration),
                timed_out: Some(false),
            })
            .collect(),
        omissions: vec![],
        limitation_codes: vec!["selection-soundness-not-established"],
        partial: false,
        sample: Sample {
            complete: true,
            omitted: 0,
        },
    };
    PreparedExecutedAttempt::new(
        identity,
        AttemptPayload::from_storage_test_projection(projection),
    )
    .unwrap()
}

fn persist(storage: &StorageManager, attempt_id: Uuid) -> VerificationCaptureCommit {
    storage.persist_verification_transaction(&execution(), attempt(attempt_id), |_| true)
}

fn row_counts(storage: &StorageManager) -> [i64; 5] {
    storage
        .conn
        .query_row(
            "SELECT (SELECT count(*) FROM verification_runs),
                (SELECT count(*) FROM verification_results),
                (SELECT count(*) FROM change_evidence_scopes),
                (SELECT count(*) FROM change_evidence_attempts),
                (SELECT count(*) FROM change_evidence_samples)",
            [],
            |row| {
                Ok([
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ])
            },
        )
        .unwrap()
}

#[test]
fn equal_times_and_duplicate_commands_keep_exact_run_and_result_positions() {
    let storage = storage();
    let mut run_ids = Vec::new();
    for attempt_id in [Uuid::new_v4(), Uuid::new_v4()] {
        let VerificationCaptureCommit::Recorded {
            attempt_id: recorded_id,
            run_id,
            confirmed_samples,
        } = persist(&storage, attempt_id)
        else {
            panic!("capture failed");
        };
        assert_eq!(recorded_id, attempt_id);
        assert_eq!(confirmed_samples, 2);
        run_ids.push(run_id);

        let mut stmt = storage
            .conn
            .prepare(
                "SELECT s.position, r.id, r.run_id, r.command, r.duration_ms, v.timestamp
             FROM change_evidence_samples s
             JOIN change_evidence_attempts a USING (repo_scope, attempt_id)
             JOIN verification_results r ON r.id = s.result_id
             JOIN verification_runs v ON v.id = a.run_id
             WHERE a.attempt_id = ?1 AND a.run_id = ?2
             ORDER BY s.position",
            )
            .unwrap();
        let rows: Vec<(i64, i64, i64, String, i64, String)> = stmt
            .query_map(rusqlite::params![attempt_id.to_string(), run_id], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert_ne!(rows[0].1, rows[1].1);
        for ((position, _, owner, command, duration_ms, timestamp), expected) in
            rows.into_iter().zip([(0, 11), (1, 22)])
        {
            assert_eq!((position, duration_ms), expected);
            assert_eq!(owner, run_id);
            assert_eq!(command, COMMAND);
            assert_eq!(timestamp, TIMESTAMP);
        }
    }
    assert_ne!(run_ids[0], run_ids[1]);
    assert_eq!(row_counts(&storage), [2, 4, 1, 2, 4]);
}

#[test]
fn begin_failure_leaves_the_existing_transaction_untouched() {
    let storage = storage();
    let transaction = storage.conn.unchecked_transaction().unwrap();
    assert_eq!(
        persist(&storage, Uuid::new_v4()),
        VerificationCaptureCommit::Unconfirmed(TransactionStage::Begin)
    );
    assert!(!storage.conn.is_autocommit());
    assert_eq!(row_counts(&storage), [0; 5]);
    transaction.rollback().unwrap();
}

#[test]
fn canonical_run_or_second_result_insert_failure_rolls_back_the_run() {
    for (table, predicate, stage) in [
        ("verification_runs", "1", TransactionStage::RunInsert),
        (
            "verification_results",
            "NEW.duration_ms = 22",
            TransactionStage::ResultInsert,
        ),
    ] {
        let storage = storage();
        storage
            .conn
            .execute_batch(&format!(
            "CREATE TEMP TRIGGER fixture_reject_insert BEFORE INSERT ON {table} WHEN {predicate}
             BEGIN SELECT RAISE(ABORT, 'fixture rejected insert'); END;"
        ))
            .unwrap();
        assert_eq!(
            persist(&storage, Uuid::new_v4()),
            VerificationCaptureCommit::RolledBack(stage)
        );
        assert!(storage.conn.is_autocommit());
        assert_eq!(row_counts(&storage), [0; 5]);
    }
}

#[test]
fn duplicate_attempt_insert_keeps_both_canonical_runs() {
    let storage = storage();
    let id = Uuid::new_v4();
    assert!(matches!(
        persist(&storage, id),
        VerificationCaptureCommit::Recorded { .. }
    ));
    assert_eq!(
        persist(&storage, id),
        VerificationCaptureCommit::Unavailable(CaptureUnavailable::AttemptWriteFailed)
    );
    assert_eq!(row_counts(&storage), [2, 4, 1, 1, 2]);
}

fn reject_second_sample(storage: &StorageManager) {
    storage
        .conn
        .execute_batch(
            "CREATE TEMP TRIGGER fixture_reject_second_sample
         BEFORE INSERT ON change_evidence_samples WHEN NEW.position = 1
         BEGIN SELECT RAISE(ABORT, 'fixture rejected sample'); END;",
        )
        .unwrap();
}

#[test]
fn savepoint_rollback_keeps_the_run_and_discards_scope_attempt_and_sample() {
    let storage = storage();
    reject_second_sample(&storage);
    assert_eq!(
        persist(&storage, Uuid::new_v4()),
        VerificationCaptureCommit::Unavailable(CaptureUnavailable::AttemptWriteFailed)
    );
    assert!(storage.conn.is_autocommit());
    assert_eq!(row_counts(&storage), [1, 2, 0, 0, 0]);
}

#[test]
fn failed_savepoint_rollback_forces_outer_rollback() {
    let mut storage = storage();
    reject_second_sample(&storage);
    storage
        .conn
        .authorizer(Some(|ctx: AuthContext<'_>| match ctx.action {
            AuthAction::Savepoint {
                operation: TransactionOperation::Rollback,
                ..
            } => Authorization::Deny,
            _ => Authorization::Allow,
        }))
        .unwrap();

    assert_eq!(
        persist(&storage, Uuid::new_v4()),
        VerificationCaptureCommit::RolledBack(TransactionStage::Savepoint)
    );
    assert!(storage.conn.is_autocommit());
    assert_eq!(row_counts(&storage), [0; 5]);
}

#[test]
fn failed_outer_commit_returns_no_recorded_acknowledgement_or_rows() {
    let mut storage = storage();
    storage.conn.commit_hook(Some(|| true)).unwrap();

    assert_eq!(
        persist(&storage, Uuid::new_v4()),
        VerificationCaptureCommit::Unconfirmed(TransactionStage::Commit)
    );
    assert!(storage.conn.is_autocommit());
    assert_eq!(row_counts(&storage), [0; 5]);
}
