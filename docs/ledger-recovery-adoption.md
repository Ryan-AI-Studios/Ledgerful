# Ledger recovery adoption

Eligible extra-genesis rows can be attested. They are not relinked.

The manifest domain is `ledger-recovery-manifest-v1`. The digest is SHA-256 of the origin URL, the stored head, and adopted rows sorted by tx id (`tx_id`, entry hash, `committed_at`). `repoIdentity` is `git remote get-url origin`. An absolute path is not an identity. Raw signatures and public keys are not stored.

Migration m56 creates `ledger_recovery_manifest` and is registered unconditionally. An older binary fails its schema preflight after m56 has run. An older binary that never sees m56 still exits 1 with `CHAIN_BREAK` on extra genesis.

`ledger recovery plan` is read-only. `ledger recovery apply --yes` takes a SQLite online backup, checks `PRAGMA integrity_check` and `PRAGMA foreign_key_check`, then appends one `MAINTENANCE` / `SECURITY` entry whose `prev_hash` is the previous head. The tx id is `adopt-` plus the manifest digest. A second apply is `alreadyApplied` and does not write again.

`verify --signatures --chain` is unchanged. `--accept-adoption` requires `--chain`. Success requires the stored manifest to match the current rows and prints historical continuity as not established. Forks, orphans, cycles, duplicate hashes, and invalid or unsigned signatures refuse adoption. Timestamp order is not authenticated chronology.

`ledger adopt` remains drift adoption. This command is `ledger recovery`.
