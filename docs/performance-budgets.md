# Performance budgets

This page records the regression ceilings enforced by `ledgerful bench --check`. They are
**predefined** ceilings derived from a recorded baseline on each measured host class
(`local-windows-2026-11` and `ci-windows-latest`) and one declared fixture band. They are not
product targets, not a wall-clock promise for every repository, and not a population claim.

The checker is documented in `docs/agent-output-contract.md` (`kind: "perfBudgetCheck"`).

## Host classes

| Class | Hardware | OS |
|---|---|---|
| `local-windows-2026-11` | AMD Ryzen 7 5800XT, 8 cores / 16 logical, 3.8 GHz base, 32 GiB RAM | Windows 11 Pro 10.0.26300 (build 26300), x86_64 |
| `ci-windows-latest` | GitHub-hosted `windows-latest` runner: image `windows-2025-vs2026` (image version `20261030.294.1`, runner `2.338.2`) on 2026-11-05; 2-4 vCPU, the image varies over time | Windows Server 2025, x86_64 |

`hostClass` is an operator declaration. The checker matches it to a row key; it cannot attest
the hardware. The hardware above is what makes the local claim auditable.

## Fixture band

The measured fixture is a clone of the engine repository. Put it under a **unique clean parent
directory** so the fixture's parent holds no unrelated repositories: `impact`/`review` run
federated sibling discovery over the repo root's parent, and a fixture cloned directly under a
crowded `%TEMP%` spends its 45 s history budget scanning unrelated directories instead of the
workload (observed 2026-11-05: `commitsWalked: 0`, 0 ms inner spans).

```powershell
$engine = 'C:\dev\ledgerful-wt-0564'
cargo build --release --bin ledgerful   # run from $engine at the measured HEAD
$root = Join-Path $env:TEMP ("perf-0564-" + [guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $root -Force | Out-Null
$fixture = Join-Path $root "fixture"
git clone --no-hardlinks $engine $fixture
# Precondition (blocking): doctor --json must exit 0 in the clone; a block finding
# makes doctor exit 1 even under --json, so a doctor-warm sample cannot be produced.
& (Join-Path $engine 'target\release\ledgerful.exe') -C $fixture doctor --json | Out-Null
if ($LASTEXITCODE -ne 0) { throw "reference fixture refused: doctor --json exited $LASTEXITCODE" }
```

A row accepts any fixture with **1000-3000 files** and **500-5000 commits** whose **root commit**
is `85ace228532eb523a4840c34d2ae59850b6cb6ae` (the measured clone: 1170 files / 678 commits).
`fixtureRootCommit` proves shared root ancestry, not content equality; a same-root fork or a
divergent branch inside the band is an accepted comparability risk, mitigated by the 25 %
latency headroom, the 50 % peak-RSS headroom and the advisory CI status. A receipt whose root
commit is `null` (unborn or multi-root fixture) can never qualify.

The two pilot sets disagree by an order of magnitude on `review` (2.2 s on a synthetic service
on 2026-10-18 vs 16.6 s on the engine tree on 2026-10-26), which is why the budget is keyed to a
fixture band rather than to "a repository": source count and commit depth dominate history work.

## Workloads

The child always receives `-C <fixture>` first. `--timeout 120` is a **measurement clock**, not
the shipped default.

| Slug | Child argv | Start state | B37 target (p95) |
|---|---|---|---|
| `index-cold` | `index` | cold | — (B38: 120 s for a medium default index, informational) |
| `status-warm` | `ledger status --compact` | warm | 1000 ms |
| `search-warm` | `search change-context --limit 3 --json` | warm | 1000 ms |
| `context-warm` | `change-context --json --paths src/main.rs --timeout 120` | warm | 3000 ms |
| `doctor-warm` | `doctor --json` | warm | 3000 ms |
| `hotspots-warm` | `hotspots --timeout 120 explain src/main.rs --json` | warm | 3000 ms |
| `impact-warm` | `impact --paths src/main.rs --json --timeout 120` | warm | 10 000 ms |
| `review-warm` | `review HEAD~5..HEAD --json --timeout 120` | warm | 10 000 ms |

`session` is not a workload: it composes context + doctor + hotspots and would not be
independent.

## Estimator

`percentile_sorted(sorted, pct)` is `sorted[round(pct / 100 x (n - 1))]`; the existing estimator
is reused unchanged. At n=20 the p95 is the 19th of 20 samples; at n=10 it is the maximum
observed. `p50Ms`/`p95Ms` come from the sorted `samplesMs`; `observedMs` preserves execution
order and is not sorted.

## Derivation

The **only** way a ceiling may be written is from a recorded baseline:

- `p50BudgetMs = max(50, ceil10(baselineP50Ms x 5 / 4))`
- `p95BudgetMs = max(50, ceil10(baselineP95Ms x 5 / 4))`
- `peakRssBudgetBytes = ceil16MiB((baselinePeakRssBytes x 3 + 1) / 2)`, declared **only** when
  the baseline p95 is at least 500 ms and every baseline sample observed at least one sampling
  tick (`rssMeasuredSamples == n`)

All arithmetic is integer (saturating multiply, truncating divide, then ceiling), so the table is
deterministic and no hand-rounded number enters it.

Worked example (`status-warm`): baseline p50 `410 ms` -> `410 x 5 / 4 = 512` -> `ceil10(512) = 520`;
baseline p95 `456 ms` -> `456 x 5 / 4 = 570` -> `ceil10(570) = 570`. Worked example
(`doctor-warm` RSS): baseline peak `47 779 840` bytes -> `(x 3 + 1) / 2 = 71 669 760` bytes ->
rounded up to 16 MiB -> `83 886 080` bytes.

## Local table (`local-windows-2026-11`)

Baseline: `2026-11-04`, `buildSha` `822ddc1730a7`, n=20 per row, run A of the adjacent A/B pair.
All run-B p95s agreed with run A within ±15 %. Every row's per-sample refresh cost
(`samplerWallMs / n`) was below `max(50 ms, 5 % of p50)`.

| Workload | n | p50 | p95 | p50 budget | p95 budget | peak RSS | RSS budget | B37 target | Target status |
|---|---|---|---|---|---|---|---|---|---|
| `index-cold` | 20 | 56 740 | 57 919 | 70 930 | 72 400 | 270 598 144 | 419 430 400 | — | `no-target` |
| `status-warm` | 20 | 410 | 456 | 520 | 570 | 13 979 648 | — | 1000 | `met` |
| `search-warm` | 20 | 415 | 473 | 520 | 600 | 13 971 456 | — | 1000 | `met` |
| `context-warm` | 20 | 1 733 | 1 905 | 2 170 | 2 390 | 46 272 512 | 83 886 080 | 3000 | `met` |
| `doctor-warm` | 20 | 838 | 880 | 1 050 | 1 100 | 47 779 840 | 83 886 080 | 3000 | `met` |
| `hotspots-warm` | 20 | 1 012 | 1 108 | 1 270 | 1 390 | 29 507 584 | 50 331 648 | 3000 | `met` |
| `impact-warm` | 20 | 1 105 | 1 192 | 1 390 | 1 490 | 44 527 616 | 67 108 864 | 10 000 | `met` |
| `review-warm` | 20 | 3 852 | 3 964 | 4 820 | 4 960 | 93 786 112 | 150 994 944 | 10 000 | `met` |

`status-warm` and `search-warm` carry no peak-RSS ceiling: their p95 is below 500 ms, so a
250 ms sampler tick can miss the whole sample. `index-cold` carries no B37 target; its 57.9 s p95
is above the shipped 25 s default, but `index` is not a budgeted analysis command and no
default-clock partial is claimed.

### Refresh cost per row (`samplerWallMs / n`)

| Workload | refresh/n | limit `max(50 ms, 5 % of p50)` |
|---|---|---|
| `index-cold` | 1 973 ms | 2 837 ms |
| `status-warm` | 9 ms | 50 ms |
| `search-warm` | 9 ms | 50 ms |
| `context-warm` | 53 ms | 86 ms |
| `doctor-warm` | 26 ms | 50 ms |
| `hotspots-warm` | 31 ms | 50 ms |
| `impact-warm` | 33 ms | 55 ms |
| `review-warm` | 119 ms | 192 ms |

### Current status (2026-11-05)

The committed ceilings are **unchanged** (no recalibration). The pre-repair evidence stands: the
independent eight-workload n=10 local proof at `4ff40b0`
(`receipts/independent-recovery-20261105/`) is `within-budget` for all eight, and the `674d995`
final-check set was recorded `within-budget` 8/8 by that leg's review (its raw receipts were
later overwritten by the `d2df1b6` set, so the surviving attestation is the recorded verdict and
review prose).

**Accepted current-source proof (2026-11-05T23:49Z).** A dedicated parent-run re-run at the measurement head `9b8ccd1` (`0.2.18 (9b8ccd13f6d5)`, binary SHA256 `eb9a7206602f907639f80d9ca0ac339c4c70fa0b03389ff94813c61a22a056a1`, built from the last code-bearing tree; the last code-bearing commit is `3d7bf1d`) produced a complete eight-workload n=10 set: `within-budget` for all eight, 0 violations / 0 unqualified, against the unchanged ceilings (impact p50 1108 / p95 1155 ms and peak RSS 38 477 824 B against 1390 / 1490 ms and 67 108 864 B). `run.json` records `complete=true`, eight native workloads exiting 0, `check_exit=0`, and equal start/end HEAD and binary hashes; the raw proof is `receipts/independent-recovery-20261105/parent-check-5807a735dc41479a9c8a630ab3aaf618/`. This satisfies the recorded current-source prerequisite for the unchanged Rust source. Documentation-only commits after `9b8ccd1` do not change the measured source; those commits' receipts are not the measurement of record, and this set remains it until a later code-bearing change.

**Retained red evidence (not erased).** At the deny-repair build `d2df1b6` a local check was red on `impact-warm`:
`p50 1690 ms` vs the `1390 ms` ceiling and `p95 1747 ms` vs `1490` (verdict and all eight
receipts in `receipts/final-check-failed-d2df1b6/`). A second full battery at `60edb36` reproduced it: `p50 1757 ms` / `p95 1789 ms`, with the other seven
workloads `within-budget` (`receipts/final-check-failed-60edb36/`). Documentation-only commits
after that measurement do not change the measured source; the dedicated `9b8ccd1` re-run above
later cleared it.

The failure also reproduced in four further probe runs: `impact-probe.json` p50 1704 at the
current fixture, `impact-refprobe-4ff40b0.json` p50 1705 at a fixture checked out where this row
previously passed, `impact-refprobe-674d995.json` p50 1670, and `impact-refprobe-lock135.json`
p50 1634 with `pulldown-cmark 0.13.5` rebuilt. That is most consistent with host/environment
variance in `impact-warm` rather than the dependency patch or a fixture-content change (host
variance is inferred from the probes, not directly measured). Every other workload is
`within-budget` in both batteries, and the failing receipts' ten samples are tightly clustered,
so this is a median shift rather than one slow sample. No ceiling was raised and no row was
re-baselined to make a check pass; the accepted `9b8ccd1` proof records no recalibration either.
The historical cause of the red medians remains unknown.

The earlier n=10 local check on build `ed24ee23d7bc` is **retained as historical evidence** of
the same variance class: it reported 7/8 `within-budget`, with `impact-warm` `over-budget`
(`p50 1763 ms` vs the `1390 ms` ceiling, `p95 1850` vs `1490`) after a host/workload shift that
reproduced across clones. No ceiling was raised then either. Evidence:
`conductor/0564-RepresentativePerformanceBudgets/receipts/check/` (failing verdict and the eight
n=10 receipts) and `receipts/rebaseline-2026-11-05/`; the conductor receipts are authoritative.

**Final gate verification (2026-11-06).** The closeout gates ran at `84ed90a`, a docs-only head
over the last code-bearing tree (`3d7bf1d`), so they describe the same Rust source as the accepted
`9b8ccd1` measurement. `cargo install --path .` installed `0.2.18 (84ed90adf23a)` and
`ledgerful verify --scope full` exited 0 on 3/3 commands: `cargo fmt --all -- --check` 3.7 s,
`cargo nextest run --workspace --all-features --profile slow` 895.0 s, and
`cargo test --workspace --all-features --doc` 58.0 s ("Verification passed"; transcript
`receipts/gates-84ed90a/verify-full.log`). `cargo deny check` was green (`advisories ok, bans ok,
licenses ok, sources ok`; `receipts/gates-84ed90a/deny.log`). The WSL workspace lane ran 5 426
tests: 5 421 passed, 5 failed, 11 skipped. The five failures are named known-environment issues,
not this source: four `impact::enrichment::coupling` tests cannot discover the repository through
the Windows worktree `.git` pointer from git-in-WSL, and `engine_scan_latency_guard` took 9 327 ms
against its untouched 6 000 ms debug budget (the known load-flaky in-suite guard; the literal is
byte-identical). `cargo clippy --all-targets --all-features -- -D warnings` is green at the last
code-bearing commit, and no Rust file changed after it.

### Evidence directories

All paths are track-relative under `C:\dev\coordinated\conductor\0564-RepresentativePerformanceBudgets\`.

- `receipts/check/` - the historical n=10 set at `ed24ee2`, including the failing `impact-warm` receipt.
- `receipts/rebaseline-2026-11-05/` - the n=20 rebaseline receipts from the same episode.
- `receipts/snapshots/` - the S1→S2→S3 fixture-mutation snapshots.
- `receipts/independent-recovery-20261105/` - the independent eight-workload proof at `4ff40b0` and the accepted `9b8ccd1` parent check under `parent-check-5807a735dc41479a9c8a630ab3aaf618/`.
- `receipts/ci/` - the Run 1 CI receipts and verdict.
- `receipts/epoch9-downloads/` - the Run 1 / Run 1-rerun / Run 1-rerun2 / Run 2 downloads.
- `receipts/final-check-failed-d2df1b6/` - the first post-repair final-check set (red on `impact-warm`) and the four impact probes.
- `receipts/final-check-failed-60edb36/` - the `60edb36` post-repair final-check set (also red on `impact-warm`) and its log.
- `receipts/gates-2604eb6/` - the fmt / clippy / nextest / deny gate logs from this leg.
- `receipts/gates-84ed90a/` - the `verify --scope full`, `cargo deny check` and WSL-lane transcripts from the closeout leg (`84ed90a`).
- `receipts/profile/` - the Phase 5 `impact`/`review` profile packets and timings.

## CI table (`ci-windows-latest`)

Baseline: `2026-11-05`, `buildSha` `85ada8cafb25` (the Run 1 PR merge commit), n=20 per row, on
`windows-2025-vs2026` (image version `20261030.294.1`; recorded from
[Run 1](https://github.com/Ryan-AI-Studios/Ledgerful/actions/runs/42080144098), job
`127430743893`). The measured Run 1 fixture is 1171 files / 685 commits at root
`85ace228532eb523a4840c34d2ae59850b6cb6ae`.

| Workload | n | p50 | p95 | p50 budget | p95 budget | peak RSS | RSS budget | B37 target | Target status |
|---|---|---|---|---|---|---|---|---|---|
| `status-warm` | 20 | 74 | 78 | 100 | 100 | — | — | 1000 | `met` |
| `search-warm` | 20 | 89 | 95 | 120 | 120 | — | — | 1000 | `met` |
| `context-warm` | 20 | 1 120 | 1 134 | 1 400 | 1 420 | 38 465 536 | 67 108 864 | 3000 | `met` |
| `doctor-warm` | 20 | 670 | 732 | 840 | 920 | 247 730 176 | 385 875 968 | 3000 | `met` |
| `hotspots-warm` | 20 | 602 | 617 | 760 | 780 | 32 133 120 | 50 331 648 | 3000 | `met` |
| `impact-warm` | 20 | 1 055 | 1 082 | 1 320 | 1 360 | 42 549 248 | 67 108 864 | 10 000 | `met` |
| `review-warm` | 20 | 6 835 | 6 907 | 8 550 | 8 640 | 141 189 120 | 218 103 808 | 10 000 | `met` |

`status-warm` and `search-warm` carry no peak-RSS ceiling for the same reason as their local
counterparts (p95 below 500 ms).

Four workflow runs produced the evidence. [Run 1](https://github.com/Ryan-AI-Studios/Ledgerful/actions/runs/42080144098)
measured the baseline before the CI rows existed, so its check reported `unqualified-host` while
still uploading receipts. [Run 1-rerun](https://github.com/Ryan-AI-Studios/Ledgerful/actions/runs/42085455909)
and [Run 1-rerun2](https://github.com/Ryan-AI-Studios/Ledgerful/actions/runs/42089475433) were
measured from empty trigger commits on the PR branch (identical merge tree, new immutable run
ids). Every workload's p95 agreed with Run 1 within ±25 % in at least one rerun; `search-warm`
(−30.5 % on Run 1-rerun) and `doctor-warm` (+165 % on Run 1-rerun2, two slow samples, 1944 and
1988 ms, in the n=20 set) were the two outliers, each settled by the other run. Rows derive from
Run 1 only and are never averaged. [Run 2](https://github.com/Ryan-AI-Studios/Ledgerful/actions/runs/42095089818)
(head `674d995`, measured merge build `e7cbbf5a0f37`) re-checked the committed rows against its
own build: all seven `within-budget`, 0 violations / 0 unqualified; its p95s were
73 / 76 / 1 018 / 720 / 470 / 891 / 5 942 ms for status / search / context / doctor / hotspots /
impact / review. Because the job is `continue-on-error`, the saved `perf-check.json` (not the job
badge) is the verdict evidence.

Measured refresh cost for the CI rows (`samplerWallMs / n`): `status-warm` 0 ms, `search-warm`
0 ms, `context-warm` 18 ms, `doctor-warm` 9 ms, `hotspots-warm` 8 ms, `impact-warm` 15 ms,
`review-warm` 101 ms. All are below `max(50 ms, 5 % of p50)` (56, 50, 50, 52 and 341 ms for
context, doctor, hotspots, impact and review).

The CI check is a sanity band, not a fine regression detector: a 25 % ceiling means a
run-to-run move inside that band passes by construction, and p50/peak RSS have no cross-run
agreement.

## Measurement clock vs shipped clocks

The child runs with `--timeout 120` (measurement clock) so a complete run is measured without
becoming a bounded partial at the shipped 25 s / 45 s clocks. The shipped clocks are not edited.
A ceiling must stay below 120 s. No workload's measured p95 is at or above the shipped 25 s
default except `index-cold`, and no default-clock partial-stop is claimed because none was
observed.

## Sampler refresh cost

`samplerWallMs` is the wall time spent inside the RSS sampler's refreshes, accumulated at
microsecond resolution and reported in whole milliseconds. It is a **refresh-cost diagnostic**,
not a measured contention effect. Phase 3 accepts a row only when `samplerWallMs / n` is below
`max(50 ms, 5 % of p50)`; the ceilings are derived from runs that include the sampler, so that
bounded monitoring cost is inside the budget. The measured per-sample refresh cost was
1 973 ms for `index-cold` (5 % of its 56.7 s p50 is 2 837 ms) and 9-119 ms for the warm rows.

## Limits

- One OS/arch/language profile, one fixture band, one declared host class per row. There is no
  baseline arm and no comparison to an unmeasured "expected" machine.
- No p99. At n=10 the p95 is the maximum observed, so one slow sample can false-alarm against the
  1.25x latency ceiling (a deliberate conservative trade).
- Headroom factors are stated separately: 25 % for p50/p95, 50 % for peak RSS
  (`(baseline x 3 + 1) / 2`, rounded up to 16 MiB).
- `peakRssBytes` is a **sampled peak of the summed descendant working set**. On Windows
  `Process::memory()` is the working set, so a shared DLL or mapped page counts once per process;
  it is comparable run to run, not private dirty bytes. 250 ms polling can miss a short-lived
  grandchild.
- The child wall cap equals the locked `--timeout 120` measurement clock (`CHILD_TIMEOUT`); the
  two values must move together, so a run at that boundary is a timeout hard error rather than a
  bounded partial.
- Largest measured child stdout against the 1 MiB `CHILD_MAX_OUTPUT_BYTES` capture cap:
  `impact-warm` 105 609 bytes and `review-warm` 1 865 bytes (self-timed profile packets, same
  argv). Both are far below the cap; a capture that hits it is a hard error, never a shortened
  sample.
- No content manifest; `fixtureRootCommit` proves shared root ancestry only.
- Each CI receipt measures the PR merge ref's own tree, so successive CI receipts can be
  different in-band trees; the 25 % headroom and advisory status are the mitigation.
- The PR path filter covers the measured command surfaces; a dependency change outside it is
  caught only by the weekly run, and GitHub's path-filter diff evaluation cap (up to 3 000
  changed files) means a very large PR may not trigger the PR run at all. The weekly run is the
  backstop for both.
- The job's measured battery duration is recorded with 2x headroom; a battery over 25 minutes
  stops the track for a higher limit or a split rather than closing against an unmeasured limit.
- The 120 s child cap aborts rather than excludes a slow setup; partial output is rejected, not
  timed; inconsistent receipts are `invalid-receipt`.
- `verify-performance.md` guard budgets are untouched.

## Checking receipts

```powershell
.\target\release\ledgerful bench --check output\bench\status-warm-local-check.json --json
```

Exit 0 only when every result is `within-budget`. `targetStatus` (`met` / `not-met` / `no-target`)
is informational on p95 and never changes the exit code. A receipt from a different build is
`foreign-build`; a receipt outside the fixture band or root is `fixture-out-of-band`; a receipt
with no row on this host class is `unqualified-host`.
