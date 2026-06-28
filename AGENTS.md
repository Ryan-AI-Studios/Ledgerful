powershell{
  forbid:"&& | [[ | ]] | then | fi | done | echo -e"
  prefer:"Get-ChildItem | Get-Content | Test-Path | Join-Path | Copy-Item | Remove-Item"
  rules:
    - use $_ and object properties for pipelines
    - use backslashes for shell-level Windows paths
    - avoid Bash shims for complex logic
    - chain commands with ; or separate lines
}

ledgerful{
  before:
    - ledgerful doctor
    - ledgerful audit
    - ledgerful ledger status --compact
    - ledgerful scan --impact for meaningful code/config/policy edits
    - read .ledgerful/reports/latest-impact.json if present
  edit:
    - do not edit .ledgerful state files
    - inspect hotspots
    - inspect temporal couplings >70%
  after:
    - ledgerful verify --scope full
    - cargo install --path . after Ledgerful source edits
    - report risk, verification, pending tx, drift
  skip_for:
    - format-only
    - scratch files
    - binary/media-only
    - lockfile-only dependency churn
    - explicit user bypass
  fail:
    unavailable:"continue with native checks; report missing signals"
    drift:"reconcile/adopt before continuing unless user says otherwise"
    verify:"report exact failed command and justified fallback"
}

ledger{
  start:"ledgerful ledger start <entity> --category <CATEGORY> --message <intent>"
  commit:"ledgerful ledger commit <tx-id> --summary <what> --reason <why>"
  status:"ledgerful ledger status --compact"
  hooks:
    - pre-commit: ledgerful ledger status --compact --exit-code
    - pre-push: ledgerful ledger status --compact --exit-code
  stale_sidecar:"after git commit, if ledger status shows 1 pending, run ledger commit immediately"
  categories:"ARCHITECTURE | FEATURE | INFRA | SECURITY | REFACTOR | BUGFIX | DOCS | CHORE"
}

verify{
  scope:"targeted during work; full before finalizing"
  required:
    - cargo fmt --all -- --check
    - cargo clippy --all-targets --all-features -- -D warnings
    - cargo nextest run --lib --bins --workspace
    - ledgerful verify --scope full
  conditional:
    - cargo nextest run --test integration
    - cargo test --doc -p <crate>
  targeted_during_work:"ledgerful verify --scope fast for quick scoped feedback"
  hygiene:
    - no secrets
    - no .env commits
    - remove temporary output before finish unless required
    - use output/ for temporary generated output
  never:
    - --no-verify unless user explicitly requests
}

rust{
  edition:"2024"
  forbid:
    - unwrap() in production
    - expect() in production
  errors:
    user_facing:"thiserror + miette::Diagnostic"
    internal:"anyhow allowed"
  invariants:
    - deterministic output for same repo state/config
    - local-first/offline capable
    - network degrades gracefully
    - preserve Windows paths
    - prefer camino for UTF-8 paths
    - tempfile::tempdir() for SQLite tests
    - no shared global test state
}

boundaries{
  platform:"environment normalization/detection only"
  index:"changed-file symbols/imports only"
  state:"persistence/layout/migrations only"
  impact:"fact assembly/scoring/explanation only"
  ledger:"transaction lifecycle/enforcement/search only"
  search:"search owns search"
}

determinism{
  require:
    - sort emitted collections
    - version packet schemas
    - annotate partial failures
    - never silently swallow parse/scan failures
    - normalize volatile fixture fields
  test_when:
    - impact tracks
    - verify tracks
    - ledger tracks
}

kg{
  backend:"CozoDB"
  state:".ledgerful/state/ledger.cozo"
  prefer_over_grep:
    - ledgerful index --incremental
    - ledgerful search "<query>"
    - ledgerful ask "<question>"
    - ledgerful ask --semantic "<question>"
  surfaces:
    - ledgerful endpoints --changed / --json
    - ledgerful services diff
    - ledgerful data-models impact --changed
    - ledgerful config schema
    - ledgerful config diff
    - ledgerful observability diff
    - ledgerful observability coverage
    - ledgerful hotspots trend
    - ledgerful hotspots explain
    - ledgerful security boundaries
    - ledgerful security impact --changed
    - ledgerful ledger graph <tx-id>
}

aibrains{
  preflight:"ai-brains preflight --summary"
  pre_edit:"ai-brains preflight --summary before risky edits"
  query:"ai-brains sync query \"<query>\""
  recall:"ai-brains recall \"<query>\" --semantic"
  pin:"ai-brains pin \"<DECISION/CONSTRAINT/HOTSPOT: message>\""
}

git{
  forbid:
    - push to main/master
    - force-push without explicit approval
    - destructive operations without explicit approval
    - committing secrets/.env
  require:
    - inspect diff before commit
    - commit only intentional files
    - keep unrelated fixes separate where practical
    - clear ledger status before push
}

review{
  log:"C:\dev\coordinated\conductor\<track>/review.md"
  critical_high:"must be verified_fixed before clearance"
  regression_caused_by_work:"high; never deferrable"
  medium:"fix by default; defer only under implement skill limits"
  closure:"code change alone is not closure"
}

contracts{
  required_when:
    - /api/* payload changed
    - config gate changed
    - daemon behavior changed
  update:
    - docs/Frontend-Notes.md
    - C:\dev\ledgerful-frontend\docs\Backend-Notes.md
    - affected frontend types/components
  missing:"high finding"
  template:"E1 empty-state string|null ripple"
}

stop_before:
  - destructive git operation
  - force-push
  - push to main/master
  - missing secrets
  - unavailable external service with no mock
  - ambiguous/conflicting specs not resolvable from code+plan
  - broad unrelated failures
  - unsafe dependency upgrade
  - scope exceeds current track

unrelated_failures{
  fix_only_if:"obvious + low-risk + blocking validation"
  otherwise:"document and report"
  commit:"separate where practical"
}
