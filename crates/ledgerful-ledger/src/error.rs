use miette::Diagnostic;
use thiserror::Error;

#[derive(Error, Debug, Diagnostic)]
pub enum LedgerError {
    #[error("Database error: {0}")]
    #[diagnostic(
        code(ledger::db_error),
        help("Check if the .ledgerful/state/ledger.db file is accessible and not corrupted.")
    )]
    Database(#[from] rusqlite::Error),

    #[error("Entity '{0}' already has a PENDING transaction")]
    #[diagnostic(
        code(ledger::conflict),
        help("Commit or rollback the existing transaction first, or use --force if you are sure.")
    )]
    Conflict(String),

    #[error("Transaction '{0}' not found")]
    #[diagnostic(
        code(ledger::not_found),
        help("Check the transaction ID. Use 'ledger status' to list active transactions.")
    )]
    NotFound(String),

    #[error("Transaction '{0}' is already {1}")]
    #[diagnostic(
        code(ledger::invalid_state),
        help("You cannot perform this action on a transaction in the {1} state.")
    )]
    InvalidState(String, String),

    #[error("Category '{0}' requires verification")]
    #[diagnostic(
        code(ledger::verification_required),
        help("Run 'ledgerful verify' or provide verification status/basis.")
    )]
    VerificationRequired(String),

    #[error("Empty entity path is not allowed")]
    #[diagnostic(
        code(ledger::empty_entity),
        help("Provide a valid file path or symbol name.")
    )]
    EmptyEntity,

    #[error("IO error: {0}")]
    #[diagnostic(code(ledger::io_error), help("Check file permissions and disk space."))]
    Io(#[from] std::io::Error),

    #[error("Config error: {0}")]
    #[diagnostic(
        code(ledger::config_error),
        help("Check your .ledgerful/config.toml for syntax errors.")
    )]
    Config(String),

    #[error("Validation error: {0}")]
    #[diagnostic(
        code(ledger::validation_error),
        help("Check the tech stack rules or provide more context in the commit message.")
    )]
    Validation(String),

    #[error("Rule violation: {0}")]
    #[diagnostic(
        code(ledger::rule_violation),
        help("Review the repository policy and architectural rules.")
    )]
    RuleViolation(String),

    #[error("Validator '{0}' failed: {1}")]
    #[diagnostic(
        code(ledger::validator_failed),
        help("The custom validator failed. Check the error message for specific details.")
    )]
    ValidatorFailed(String, String),

    #[error("Ledger re-sign cancelled: {0}")]
    #[diagnostic(
        code(ledger::re_sign_cancelled),
        help(
            "Re-sign is a destructive provenance operation. Review the dry-run output, then pass --yes if you accept the backup + re-sign plan."
        )
    )]
    ReSignCancelled(String),

    #[error("Ledger re-sign backup failed: {0}")]
    #[diagnostic(
        code(ledger::re_sign_backup_failed),
        help(
            "The ledger database backup could not be created or did not pass PRAGMA integrity_check. Resolve the disk/database issue before retrying."
        )
    )]
    ReSignBackupFailed(String),

    #[error("Ledger re-sign validation failed: {0}")]
    #[diagnostic(
        code(ledger::re_sign_validation),
        help(
            "No re-sign candidates matched the given criteria, or the requested transaction is not eligible for re-sign."
        )
    )]
    ReSignValidation(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_formatting() {
        let err = LedgerError::Conflict("src/main.rs".to_string());
        assert_eq!(
            format!("{}", err),
            "Entity 'src/main.rs' already has a PENDING transaction"
        );

        let err = LedgerError::NotFound("abc-123".to_string());
        assert_eq!(format!("{}", err), "Transaction 'abc-123' not found");
    }
}
