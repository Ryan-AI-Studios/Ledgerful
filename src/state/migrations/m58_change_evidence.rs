use rusqlite_migration::M;

/// Revision-bound evidence storage, registered for every build.
pub fn m58_change_evidence() -> Vec<M<'static>> {
    vec![M::up(
        r#"
CREATE INDEX idx_change_evidence_mapping_tested_file ON test_mapping(tested_file_id);
CREATE TABLE change_evidence_scopes (
    repo_scope TEXT NOT NULL PRIMARY KEY,
    scope_guard BLOB NOT NULL UNIQUE CHECK(length(scope_guard) = 32),
    created_at TEXT NOT NULL
);
CREATE TABLE change_evidence_names (
    repo_scope TEXT NOT NULL REFERENCES change_evidence_scopes(repo_scope),
    baseline_name TEXT NOT NULL REFERENCES analysis_baselines(name),
    PRIMARY KEY(repo_scope, baseline_name)
) WITHOUT ROWID;
CREATE TABLE change_evidence_attestations (
    repo_scope TEXT NOT NULL,
    base TEXT NOT NULL,
    baseline_name TEXT NOT NULL,
    baseline_digest TEXT NOT NULL,
    attestation_version INTEGER NOT NULL,
    input_identity_json TEXT NOT NULL,
    attestation_digest TEXT NOT NULL,
    observed_at TEXT NOT NULL,
    PRIMARY KEY(repo_scope, base, baseline_name, baseline_digest),
    FOREIGN KEY(repo_scope, baseline_name) REFERENCES change_evidence_names(repo_scope, baseline_name)
) WITHOUT ROWID;
CREATE TABLE change_evidence_prepared (
    repo_scope TEXT NOT NULL REFERENCES change_evidence_scopes(repo_scope),
    head TEXT NOT NULL,
    base TEXT NOT NULL,
    baseline_name TEXT NOT NULL,
    baseline_digest TEXT NOT NULL,
    generation INTEGER NOT NULL CHECK(generation > 0),
    payload_version INTEGER NOT NULL,
    payload_json TEXT NOT NULL,
    payload_digest TEXT NOT NULL,
    captured_at TEXT NOT NULL,
    PRIMARY KEY(repo_scope, head, base, baseline_name, baseline_digest)
) WITHOUT ROWID;
CREATE TABLE change_evidence_attempts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    repo_scope TEXT NOT NULL REFERENCES change_evidence_scopes(repo_scope),
    attempt_id TEXT NOT NULL UNIQUE,
    head TEXT NOT NULL,
    attempt_kind TEXT NOT NULL CHECK(attempt_kind IN ('executed', 'dry-run', 'refused')),
    originally_bound INTEGER NOT NULL CHECK(originally_bound IN (0, 1)),
    run_id INTEGER REFERENCES verification_runs(id) ON DELETE SET NULL,
    sample_count INTEGER NOT NULL CHECK(sample_count BETWEEN 0 AND 32),
    payload_version INTEGER NOT NULL,
    payload_json TEXT NOT NULL,
    payload_digest TEXT NOT NULL,
    observed_from TEXT NOT NULL,
    observed_to TEXT NOT NULL,
    UNIQUE(repo_scope, attempt_id),
    CHECK((attempt_kind = 'executed' AND originally_bound = 1)
       OR (attempt_kind IN ('dry-run', 'refused') AND originally_bound = 0
           AND run_id IS NULL AND sample_count = 0))
);
CREATE UNIQUE INDEX idx_change_evidence_attempt_run ON change_evidence_attempts(run_id) WHERE run_id IS NOT NULL;
CREATE INDEX idx_change_evidence_attempt_scope_head ON change_evidence_attempts(repo_scope, head, id DESC);
CREATE TABLE change_evidence_samples (
    repo_scope TEXT NOT NULL,
    attempt_id TEXT NOT NULL,
    position INTEGER NOT NULL CHECK(position >= 0),
    result_id INTEGER NOT NULL,
    PRIMARY KEY(repo_scope, attempt_id, position),
    UNIQUE(repo_scope, attempt_id, result_id),
    FOREIGN KEY(repo_scope, attempt_id) REFERENCES change_evidence_attempts(repo_scope, attempt_id) ON DELETE CASCADE
) WITHOUT ROWID;
CREATE INDEX idx_change_evidence_ledger_tx ON ledger_entries(tx_id);
CREATE TRIGGER change_evidence_attestation_immutable BEFORE UPDATE ON change_evidence_attestations
BEGIN SELECT RAISE(ABORT, 'change-evidence-attestation-immutable'); END;
CREATE TRIGGER change_evidence_names_cap BEFORE INSERT ON change_evidence_names
WHEN (SELECT COUNT(*) FROM change_evidence_names WHERE repo_scope=NEW.repo_scope) >= 128
BEGIN SELECT RAISE(ABORT, 'change-evidence-name-capacity'); END;
CREATE TRIGGER change_evidence_prepared_cap BEFORE INSERT ON change_evidence_prepared
WHEN (SELECT COUNT(*) FROM change_evidence_prepared WHERE repo_scope=NEW.repo_scope) >= 32
BEGIN SELECT RAISE(ABORT, 'change-evidence-prepared-capacity'); END;
CREATE TRIGGER change_evidence_attempt_cap BEFORE INSERT ON change_evidence_attempts
WHEN (SELECT COUNT(*) FROM change_evidence_attempts WHERE repo_scope=NEW.repo_scope) >= 256
BEGIN SELECT RAISE(ABORT, 'change-evidence-attempt-capacity'); END;
CREATE TRIGGER change_evidence_attempt_bound_on_insert BEFORE INSERT ON change_evidence_attempts
WHEN NEW.originally_bound=1 AND NEW.run_id IS NULL
BEGIN SELECT RAISE(ABORT, 'change-evidence-run-required'); END;
CREATE TRIGGER change_evidence_attempt_immutable BEFORE UPDATE OF repo_scope,attempt_id,attempt_kind,originally_bound,sample_count ON change_evidence_attempts
WHEN OLD.repo_scope IS NOT NEW.repo_scope OR OLD.attempt_id IS NOT NEW.attempt_id
  OR OLD.attempt_kind IS NOT NEW.attempt_kind OR OLD.originally_bound IS NOT NEW.originally_bound
  OR OLD.sample_count IS NOT NEW.sample_count
BEGIN SELECT RAISE(ABORT, 'change-evidence-immutable-binding'); END;
CREATE TRIGGER change_evidence_attempt_no_rebind BEFORE UPDATE OF run_id ON change_evidence_attempts
WHEN OLD.run_id IS NOT NEW.run_id AND NOT (OLD.run_id IS NOT NULL AND NEW.run_id IS NULL
    AND NOT EXISTS(SELECT 1 FROM verification_runs WHERE id=OLD.run_id))
BEGIN SELECT RAISE(ABORT, 'change-evidence-run-rebind'); END;
CREATE TRIGGER change_evidence_sample_cap BEFORE INSERT ON change_evidence_samples
WHEN (SELECT COUNT(*) FROM change_evidence_samples WHERE repo_scope=NEW.repo_scope AND attempt_id=NEW.attempt_id) >= 32
BEGIN SELECT RAISE(ABORT, 'change-evidence-sample-capacity'); END;
CREATE TRIGGER change_evidence_sample_owned BEFORE INSERT ON change_evidence_samples
WHEN NOT EXISTS(SELECT 1 FROM change_evidence_attempts a JOIN verification_results r
    ON r.id=NEW.result_id AND r.run_id=a.run_id WHERE a.repo_scope=NEW.repo_scope
    AND a.attempt_id=NEW.attempt_id AND a.originally_bound=1 AND a.run_id IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'change-evidence-sample-owner'); END;
CREATE TRIGGER change_evidence_sample_immutable BEFORE UPDATE ON change_evidence_samples
BEGIN SELECT RAISE(ABORT, 'change-evidence-sample-immutable'); END;
"#,
    )]
}
