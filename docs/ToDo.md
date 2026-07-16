# Ledgerful Engine — ToDo

Public-facing engineering todo list for the Ledgerful engine repo.

## Completed

- **Track 0066 — CI Job Graph Parallelism** (2026-07-16): Decoupled `test-slow`/`test-compile-fail` from the `test` job (`needs: [web-build, clippy]` instead of `[web-build, test]`) so the two slowest CI jobs overlap `test` instead of following it — ~29m → ~21.5m PR wall-clock. See `conductor/0066-CiJobGraphParallelism/`.
- **Track 0063 — CI Node 24 Action Upgrade** (2026-07-16): Upgraded all node20 actions to node24-native SHA-verified pins; removed `FORCE_JAVASCRIPT_ACTIONS_TO_NODE24` override. See `conductor/0063-CiNode24ActionUpgrade/`.
- **Track 0062 — Test Suite Health and Parallelism** (2026-07-16): Windows CI test parallelism + test-suite dedup. See `conductor/0062-TestSuiteHealthAndParallelism/`.
- **Track 0061 — Verify Fast Scope Reliability** (2026-07-15): `verify --scope fast` reliability fix. See `conductor/0061-VerifyFastScopeReliability/`.