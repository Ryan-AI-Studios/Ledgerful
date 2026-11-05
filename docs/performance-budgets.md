# Performance budgets

This page records the regression ceilings enforced by `ledgerful bench --check`. They are
**predefined** ceilings derived from recorded baselines on two declared host classes and one
declared fixture band. They are not product targets, not a wall-clock promise for every
repository, and not a population claim.

The checker is documented in `docs/agent-output-contract.md` (`kind: "perfBudgetCheck"`).

## Host classes

| Class | Hardware | OS |
|---|---|---|
| `local-windows-2026-11` | AMD Ryzen 7 5800XT, 8 cores / 16 logical, 3.8 GHz base, 32 GiB RAM | Windows 11 Pro 10.0.26300 (build 26300), x86_64 |
| `ci-windows-latest` | GitHub-hosted `windows-latest` runner (2-4 vCPU; the runner image varies) | Windows Server, x86_64 |

`hostClass` is an operator declaration. The checker matches it to a row key; it cannot attest
the hardware. The hardware above is what makes the local claim auditable.

## Fixture band

The measured fixture is a clone of the engine repository:

```powershell
$fixture = Join-Path $env:TEMP ("perf-fixture-0564-" + [guid]::NewGuid().ToString("N"))
git clone --no-hardlinks C:\dev\ledgerful-wt-0564 $fixture
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

## CI table (`ci-windows-latest`)

The seven warm CI rows are measured by the advisory `perf-budget.yml` workflow on a PR (Run 1),
agreed against a second run (Run 1-rerun) within ±25 %, and verified by Run 2 after the rows are
committed. **The CI rows are not yet committed**; they land with the Phase 4 CI measurement leg.
No CI numbers are claimed here.

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

- One OS/arch/language profile, one fixture band, one declared host class per row.
- No p99. At n=10 the p95 is the maximum observed, so one slow sample can false-alarm against the
  1.25x latency ceiling (a deliberate conservative trade).
- Headroom factors are stated separately: 25 % for p50/p95, 50 % for peak RSS
  (`(baseline x 3 + 1) / 2`, rounded up to 16 MiB).
- `peakRssBytes` is a **sampled peak of the summed descendant working set**. On Windows
  `Process::memory()` is the working set, so a shared DLL or mapped page counts once per process;
  it is comparable run to run, not private dirty bytes. 250 ms polling can miss a short-lived
  grandchild.
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
