# Ledgerful V1 Engineering Principles Review

## Scope

This review evaluates **Ledgerful Implementation Plan v1** against the following constraints:

* **Follow SRP**: Keep modules focused on one task.
* **Be Idiomatic Rust**: Use `Result` and `match`, avoid `unwrap`.
* **Stay KISS/YAGNI**: Don't build the abstraction until it's needed.
* **Prefer Determinism**: Ensure outputs are predictable and testable.
* **Favor Error Visibility**: Use `anyhow`/`miette` to provide actionable user errors.

---

## Overall Verdict

**Status: Mostly aligned, but needs tightening in a few places before implementation.**

The v2 plan is directionally strong. It already leans toward deterministic behavior, conservative defaults, and good module boundaries. The main risk areas are:

1. a few modules are still broad enough that implementers could violate SRP
2. the plan still leaves room for premature abstraction in platform/state/index layers
3. the plan should state Rust error-handling expectations more explicitly
4. the plan should more aggressively discourage “smart” fallbacks that reduce determinism

---

## 1. SRP Review

### Current Strengths

The v2 plan already separates major concerns better than most implementation plans. In particular, it separates:

* CLI routing
* platform handling
* git scanning
* watching/debounce
* indexing
* impact scoring
* policy evaluation
* verification
* Gemini wrapping

That is a good foundation.

### Remaining Risks

A few modules are still broad enough to invite kitchen-sink implementations:

#### A. `platform/`

Risk:

* `detect`, `shell`, `paths`, `env`, and `process_policy` are fine as a split, but implementers may still start putting general subprocess behavior and validation logic there.

Recommendation:

* treat `platform/` strictly as environment-specific normalization and detection only
* keep generic process spawning in `util/process.rs` or `verify/runner.rs`
* keep business decisions out of `platform/`

#### B. `index/`

Risk:

* `symbols`, `references`, `runtime_usage`, `normalize`, and `storage` could become a catch-all semantic engine.

Recommendation:

* keep `symbols` limited to declaration extraction
* keep `references` limited to lightweight file-local or parser-derived relationships
* keep `runtime_usage` limited to env/config/runtime access detection
* do not let `index/` become “global program intelligence” in v1

#### C. `state/`

Risk:

* `db`, `migrations`, `reports`, and `locks` are good, but implementers may mix layout, persistence, recovery, and report formatting.

Recommendation:

* `layout` should only know paths and directory structure
* `db` should only know persistence API
* `migrations` should only know schema upgrades
* `reports` should only know report read/write
* `reset` logic should remain in the command layer or a narrow recovery helper

#### D. `impact/`

Risk:

* `relationships`, `reasoning`, and `score` could blur together.

Recommendation:

* `relationships` computes input facts only
* `score` assigns tier/weights only
* `reasoning` formats human-readable explanations only

### SRP Verdict

**Pass with conditions.** The plan is structurally good, but should add stricter module role statements to reduce implementation drift.

### Module facades and public surface

Packet and config-model keep private submodules behind `pub use` facades. Do not
promote internal helpers (serialization, env resolvers) to `pub mod` without a
clear reason. Product-stable surfaces are **OpenAPI** (`docs/api/openapi.json` +
drift test), **`scan --pr` `PrScanReport` JSON**, **policy-check JSON**, the
**public-ledger export allowlist**, and the **CLI** — not Rust `pub mod` paths.
Integration tests may deep-import `ledgerful::…`; that does not make those paths
a supported external library API. Enforcement is PR review.

---

## 2. Idiomatic Rust Review

### Current Strengths

The plan already prefers:

* Rust-first implementation
* explicit diagnostics
* bounded subprocess handling
* deterministic local state

Those are compatible with idiomatic Rust.

### Missing Explicitness

The plan should say this plainly:

* public fallible functions should return `Result<T, E>`
* user-facing command handlers should return `miette::Result<()>` or an equivalent top-level diagnostic result
* internal libraries may use `anyhow::Result<T>` for app-level composition where typed errors are not worth the complexity
* `unwrap`, `expect`, and unchecked assumptions should be forbidden in production code except in tests or impossible-by-construction cases that are documented

### Recommended Addition

Add a short implementation rule section:

* Prefer `Result` propagation with `?`
* Use `match` for branch clarity when multiple failure modes need user-visible handling
* Use `Option` only when absence is expected and non-exceptional
* Convert lower-level errors into actionable command-level diagnostics with context
* Avoid panics in normal runtime paths

### Idiomatic Rust Verdict

**Needs explicit reinforcement.** The plan is compatible with idiomatic Rust, but it should say so directly to prevent sloppy agent-generated code.

---

## 3. KISS / YAGNI Review

### Current Strengths

The v2 plan does a lot right here:

* rejects Python as required runtime for v1
* rejects MCP/server/cloud architecture for v1
* rejects whole-program overanalysis in v1
* rejects autonomous git flows in v1
* introduces a basic impact-packet phase early

That is exactly the right shape.

### Remaining Risks

There are still a few places where implementers may overbuild.

#### A. SQLite too early for every concern

The plan now formalizes SQLite, which is fine, but v1 should not force every internal datum into the DB immediately.

Recommendation:

* use JSON file reports first where simpler
* only move data into SQLite when there is a concrete durability/querying need
* keep DB usage narrow in early phases

#### B. Locks subsystem

`state/locks.rs` may be unnecessary early unless concurrent command execution becomes a real issue.

Recommendation:

* mark locking as conditional
* do not build an elaborate lock manager before a real race exists

#### C. `process_policy.rs`

This might be premature if it becomes its own abstraction layer before process execution patterns stabilize.

Recommendation:

* keep a minimal command execution policy model at first
* only split dedicated process-policy logic once at least two subsystems need it

#### D. Reference analysis

The plan should explicitly prohibit building repo-wide call graph ambitions in v1.

Recommendation:

* say that file-local and changed-file-adjacent analysis is sufficient for v1

### KISS/YAGNI Verdict

**Mostly pass, but tighten anti-overengineering instructions.** The biggest remaining risk is implementers using the plan as permission to build clever infrastructure too early.

---

## 4. Determinism Review

### Current Strengths

This is one of the strongest parts of the plan.

The v2 plan already emphasizes:

* deterministic risk scoring
* targeted verification
* explainable reasoning
* inspectable reports
* JSON packet output
* graceful degradation
* stable phase boundaries

That is excellent.

### Remaining Gaps

The plan should state these determinism requirements more concretely:

#### A. Stable ordering

All emitted file lists, symbol lists, reasons, commands, and report sections should be sorted deterministically.

#### B. Stable packet schema

The impact packet should have a versioned schema and stable field order in tests.

#### C. No silent fallback heuristics

If a parser fails, the tool should record partial results explicitly rather than quietly inventing replacement behavior.

#### D. Deterministic default verification plans

Given the same repo state and config, the verification plan must always be identical.

#### E. Clock sensitivity

Timestamps should not be embedded into comparison-sensitive test fixtures unless explicitly normalized.

### Recommended Addition

Add a “determinism contract” section:

* sort outputs before presentation/persistence where possible
* version packet schemas
* never suppress parse or scan failure silently
* annotate partial data explicitly
* normalize volatile fields in tests

### Determinism Verdict

**Strong pass with a few useful hardening additions.**

---

## 5. Error Visibility Review

### Current Strengths

The plan already names `anyhow` and `miette`, and it repeatedly asks for clear diagnostics and graceful failure.

### What Is Missing

It should define where each belongs.

Recommended rule of thumb:

* `miette` at command boundaries and user-visible diagnostics
* `anyhow` for internal orchestration where rich user display is not needed yet
* `thiserror` only for stable internal error enums when that adds clarity

The plan should also require:

* command errors must explain what failed, why it matters, and what the user can do next
* errors should name the path, command, or dependency involved when safe to do so
* missing tools should produce actionable setup guidance
* config parse errors should include file path and failing key when feasible
* verification failures should distinguish command failure from tool-not-found from timeout

### Example Quality Bar

Bad:

* “Failed to verify”

Good:

* “Verification command `cargo test watcher` exited with status 101 in package `ledgerful`. Review stderr summary below. Full output saved to `.ledgerful/reports/latest-verify.json`."

### Error Visibility Verdict

**Pass, but it needs a sharper operational standard.**

---

## 6. Concrete Improvements Recommended for V2

### Add a new section: Rust Implementation Rules

Include:

* no `unwrap`/`expect` in production paths
* prefer `Result` + `?`
* use `match` for explicit branching on expected failure modes
* `miette` for command/user-facing diagnostics
* `anyhow` for internal composition

### Add a new section: Determinism Contract

Include:

* stable sorting for emitted collections
* stable packet schema versioning
* explicit partial-result annotation
* no silent fallback behavior
* normalized test fixtures for volatile fields

### Tighten SRP in module descriptions

Explicitly constrain:

* `platform/` to platform adaptation only
* `index/` to changed-file intelligence only
* `state/` to persistence/layout/report storage only
* `impact/` to fact assembly, scoring, and explanation only

### Tighten KISS/YAGNI wording

Add explicit “do not build yet” statements for:

* lock manager sophistication
* repo-wide call graphing
* generalized plugin systems
* abstraction layers with only one implementation
* DB-first design where flat-file state is enough in early phases

### Tighten error expectations

Require actionable errors to include:

* what failed
* where it failed
* likely cause when known
* next step for the user

---

## 7. Final Verdict by Principle

### Follow SRP

**Verdict: Pass with tightening needed**
The structure is good, but several modules need narrower role statements.

### Be Idiomatic Rust

**Verdict: Partial pass**
Compatible with idiomatic Rust, but the plan should explicitly ban `unwrap` in production paths and require `Result`-driven error propagation.

### Stay KISS/YAGNI

**Verdict: Mostly pass**
Good overall restraint, but needs firmer warnings against premature DB, locking, and semantic-engine abstractions.

### Prefer Determinism

**Verdict: Strong pass**
One of the best parts of the plan. It should still add a stable ordering/schema/testing contract.

### Favor Error Visibility

**Verdict: Pass with sharpening**
The intent is good, but the expected structure of actionable errors should be spelled out more explicitly.

---

## Recommended Disposition

**Adopt v2, but revise it once more before implementation to add:**

1. Rust implementation rules
2. Determinism contract
3. tighter SRP boundaries
4. stronger YAGNI guardrails
5. more explicit error quality requirements

---

## Test Tiers

Ledgerful uses `cargo-nextest` tier profiles defined in `.config/nextest.toml`
to separate fast PR feedback from heavy/nightly validation.

| Tier | Profile | What it runs | Target | When |
|---|---|---|---|---|
| 1 | `default` | Fast unit + integration tests, excludes `__slow` tests | `<60s` wall | Every PR, local dev |
| 2 | `ci` | Same as `default`, with retries (`count=2`) and 60s slow-timeout | `<60s` wall | CI / pre-push gate |
| 3 | `slow` | Only tests whose name ends in `__slow`; 300s slow-timeout | nightly | On-demand / nightly / pre-release |
| 4 | doctests | `cargo test --workspace --all-features --doc` | seconds | Every PR |

Note: compile-fail tier removed in 0067; `--profile compile-fail` no longer exists.

### Naming convention

Any test whose function name ends in `__slow` belongs to the slow tier. This is
a **filter-based** convention, not `#[ignore]`, so slow tests remain runnable
without special flags and tier selection lives entirely in `nextest.toml`.

**Important:** tracks that rename tests (for example TA7) must preserve an
existing `__slow` suffix. If a slow test is renamed and the suffix is dropped,
it silently re-enters the fast tier and breaks the `<60s` PR target.

### Running tiers locally

```bash
# Fast PR loop (excludes slow)
cargo nextest run --workspace --all-features --profile default

# CI gate (retries, 60s timeout)
cargo nextest run --workspace --all-features --profile ci

# Slow tier only (13 tests marked __slow)
cargo nextest run --workspace --all-features --profile slow

# Doctests (nextest cannot run doctests on stable)
cargo test --workspace --all-features --doc

# Full local suite (ci + slow + doctests)
ledgerful verify --scope full
```

### Pre-push verify gate tier mapping

`ledgerful verify --scope fast` runs the `ci` profile:

```
cargo nextest run --workspace --all-features --profile ci
```

`ledgerful verify --scope full` adds the slow and doctest commands to the plan:

```
cargo nextest run --workspace --all-features --profile slow
cargo test --workspace --all-features --doc
```

For docs-only commits (no `.rs` source changes), the gate skips test execution
and runs only format and clippy checks. If nextest is not installed, the gate
falls back to `cargo test -j 1 --all-features -- --test-threads=1`.

---

## Coverage & Mutation Testing

Install the additional Cargo subcommands locally (these are **not**
linkable crate dependencies, so they live outside `Cargo.toml`):

```bash
cargo install cargo-llvm-cov --locked --version 0.8.7
cargo install cargo-mutants --locked --version 27.1.0
```

### Running coverage locally

Generate an HTML coverage report and open it in your browser:

```bash
cargo llvm-cov nextest --workspace --all-features --html --open
```

To print a text summary instead:

```bash
cargo llvm-cov nextest --workspace --all-features --text
```

The HTML report is written to `target/llvm-cov/html/` by default; when using
`--output-dir coverage/` it is written under that directory.

### Running mutation testing locally

Target a single critical module first (fastest feedback loop):

```bash
cargo mutants --workspace --in-place --file 'src/ledger/**'
```

To run against all three critical modules:

```bash
cargo mutants --workspace --in-place \
  --file 'src/ledger/**' \
  --file 'src/impact/**' \
  --file 'src/verify/**'
```

Results are written to `mutants.out/outcomes.json` and `mutants.out/mutants.log`.

### When CI runs each

* **Coverage** — on every push to `main` (not on PRs, to keep the PR loop fast).
  CI uses the precompiled binary via `taiki-e/install-action@cargo-llvm-cov`
  rather than compiling `cargo-llvm-cov` from source.
* **Mutation testing** — nightly at 04:00 UTC via the `schedule` trigger.
  CI uses the precompiled binary via `taiki-e/install-action@cargo-mutants`.
  The job is capped at 120 minutes and targets only `src/ledger/`,
  `src/impact/`, and `src/verify/` to keep runtime reasonable.

### Skipping untestable functions

If a generated mutant is impossible to kill because the function is
intentionally untestable (for example a pure `Display` impl or a formatting
helper), add the attribute:

```rust
#[mutants::skip]
fn intentionally_untestable_function() {
    // ...
}
```

Use this sparingly and document why the function cannot be meaningfully
unit-tested.

---

## Init Secret and Binary-Alias Safety

Starter config generation has a separate security boundary from trusted
runtime-default loading. Init selects `LEDGERFUL_DEFAULT_CONFIG`, then the
user-level template, then the built-in template; it uses `toml_edit` to remove
shared-policy secret keys and credentialed structured URLs while preserving
unrelated formatting. Sanitized TOML is reparsed as `Config`, checked again for
secret assignments, staged in the destination directory, flushed, and
published with create-if-absent semantics. Existing configs win races.

Binary alias refresh is confined to the canonical executable's directory.
Source and staged content are compared by length and BLAKE3 hash. Windows uses
same-directory `MoveFileExW` replacement and a rename-aside/rollback fallback
for sharing violations; Linux/glibc uses `renameat2(RENAME_NOREPLACE)`.
Other Unix targets use hard-link create-if-absent and fail closed when the
filesystem lacks that atomic primitive. Symlink or reparse-point aliases are
refused. Operation-owned `.new`/`.old` artifacts use an exact lowercase UUID
name. Deferred old-executable cleanup requires a regular working alias,
reports failures, and never scans or mutates unrelated `PATH` locations.
