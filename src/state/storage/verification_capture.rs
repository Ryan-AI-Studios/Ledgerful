use super::StorageManager;
use super::verification_capture_write::{
    InsertedAttempt, PreparedExecutedAttempt, write_executed_attempt,
};
#[cfg(target_os = "linux")]
use crate::change_evidence::pack_store::capture_db::NativeCaptureRecheck;
use crate::verify::engine::CompletedExecution;
use rusqlite::{Transaction, TransactionBehavior};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CaptureUnavailable {
    InvalidOrigin,
    StorageUnavailable,
    InvalidQualification,
    CaptureBudget,
    RevisionChanged,
    CheckoutUnavailable,
    PlatformUnavailable,
    PlanningStorageUnavailable,
    ScopeUnavailable,
    CapacityReached,
    AttemptWriteFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TransactionStage {
    Begin,
    RunInsert,
    ResultInsert,
    Savepoint,
    Rollback,
    Commit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VerificationCaptureCommit {
    /// The outer transaction committed, including the scoped writer's rows.
    Recorded {
        attempt_id: Uuid,
        run_id: i64,
        confirmed_samples: u8,
    },
    /// No capture was accepted. The legacy report may have been persisted.
    Unavailable(CaptureUnavailable),
    RolledBack(TransactionStage),
    /// No recorded acknowledgement is possible.
    Unconfirmed(TransactionStage),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CanonicalResultBinding {
    pub(crate) position: usize,
    pub(crate) result_id: i64,
}

enum NativeCheck {
    Admit,
    Confirm,
}

impl StorageManager {
    pub(crate) fn persist_verification_with_capture(
        &self,
        execution: &CompletedExecution<'_>,
        attempt: PreparedExecutedAttempt,
        #[cfg(target_os = "linux")] native: &mut NativeCaptureRecheck<'_>,
    ) -> VerificationCaptureCommit {
        self.persist_verification_transaction(execution, attempt, |check| {
            #[cfg(target_os = "linux")]
            match check {
                NativeCheck::Admit => native.admit(),
                NativeCheck::Confirm => native.check(),
            }
            #[cfg(not(target_os = "linux"))]
            match check {
                NativeCheck::Admit | NativeCheck::Confirm => true,
            }
        })
    }

    fn persist_verification_transaction(
        &self,
        execution: &CompletedExecution<'_>,
        attempt: PreparedExecutedAttempt,
        mut native_check: impl FnMut(NativeCheck) -> bool,
    ) -> VerificationCaptureCommit {
        if self.is_read_only {
            return VerificationCaptureCommit::Unavailable(CaptureUnavailable::StorageUnavailable);
        }
        let report = execution.report();
        let Some(terminal) = execution.terminal() else {
            return VerificationCaptureCommit::Unavailable(CaptureUnavailable::InvalidOrigin);
        };
        if !terminal.is_consistent()
            || terminal.completed() != report.results.len()
            || report.plan.as_ref().is_some_and(|plan| plan.refused)
            || terminal
                .outcomes()
                .iter()
                .zip(&report.results)
                .any(|(source, result)| {
                    source.exit_code != result.exit_code || source.duration_ms != result.duration_ms
                })
        {
            return VerificationCaptureCommit::Unavailable(CaptureUnavailable::InvalidOrigin);
        }
        // Use the same connection and canonical insert methods as legacy verify.
        // The connection is private to this execution. The nested writer receives
        // a savepoint and must leave transaction control to this function.
        let Ok(mut transaction) =
            Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)
        else {
            return VerificationCaptureCommit::Unconfirmed(TransactionStage::Begin);
        };

        if !native_check(NativeCheck::Admit) {
            return rollback(transaction, TransactionStage::Begin);
        }

        let plan_json = report
            .plan
            .as_ref()
            .and_then(|plan| serde_json::to_string(plan).ok());
        let run_id = match self.save_verification_run(
            &report.timestamp,
            plan_json.as_deref(),
            report.overall_pass,
            report.tx_id.as_deref(),
        ) {
            Ok(id) if id > 0 && self.conn.changes() == 1 => id,
            _ => return rollback(transaction, TransactionStage::RunInsert),
        };

        let mut bindings = Vec::with_capacity(terminal.outcomes().len());
        for (position, result) in report.results.iter().enumerate() {
            let result_id = match self.save_verification_result(
                run_id,
                &result.command,
                result.exit_code,
                result.duration_ms,
                result.truncated,
                report.tx_id.as_deref(),
            ) {
                Ok(()) if self.conn.changes() == 1 => self.conn.last_insert_rowid(),
                _ => return rollback(transaction, TransactionStage::ResultInsert),
            };
            if result_id <= 0 {
                return rollback(transaction, TransactionStage::ResultInsert);
            }
            if let Some(outcome) = terminal.outcomes().get(position) {
                bindings.push(CanonicalResultBinding {
                    position: outcome.position,
                    result_id,
                });
            }
        }

        let outcome = match write_in_savepoint(&mut transaction, run_id, &bindings, attempt, || {
            native_check(NativeCheck::Confirm)
        }) {
            Ok(outcome) => outcome,
            Err(_) => return rollback(transaction, TransactionStage::Savepoint),
        };

        if transaction.commit().is_err() {
            return VerificationCaptureCommit::Unconfirmed(TransactionStage::Commit);
        }
        match outcome {
            Ok(receipt) => VerificationCaptureCommit::Recorded {
                attempt_id: receipt.attempt_id(),
                run_id,
                confirmed_samples: receipt.confirmed_samples(),
            },
            Err(reason) => VerificationCaptureCommit::Unavailable(reason),
        }
    }
}

fn write_in_savepoint(
    transaction: &mut Transaction<'_>,
    run_id: i64,
    bindings: &[CanonicalResultBinding],
    attempt: PreparedExecutedAttempt,
    confirm_native: impl FnOnce() -> bool,
) -> rusqlite::Result<Result<InsertedAttempt, CaptureUnavailable>> {
    let savepoint = transaction.savepoint()?;
    let result = write_executed_attempt(&savepoint, attempt, run_id, bindings);
    let result = if result.is_ok() && !confirm_native() {
        Err(CaptureUnavailable::ScopeUnavailable)
    } else {
        result
    };
    match result {
        Ok(_) => savepoint.commit()?,
        // finish() performs ROLLBACK TO and RELEASE and reports either error.
        Err(_) => savepoint.finish()?,
    }
    Ok(result)
}

fn rollback(transaction: Transaction<'_>, stage: TransactionStage) -> VerificationCaptureCommit {
    if transaction.rollback().is_ok() {
        VerificationCaptureCommit::RolledBack(stage)
    } else {
        VerificationCaptureCommit::Unconfirmed(stage)
    }
}

#[cfg(test)]
#[path = "verification_capture_tests.rs"]
mod tests;
