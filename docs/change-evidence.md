# Revision-bound change evidence

Ledgerful attaches prepared baseline comparisons and explicitly captured verification attempts to exact Git revisions in the local dashboard. Use a standalone repository with its project-local Ledgerful state under `.ledgerful`. Comparison uses a full SHA-1 child commit and its first and only parent.

## Prepare a comparison

At the baseline revision, update the index and create a named baseline, or select an existing one. Attest it while that revision is checked out.

```powershell
ledgerful index --full
ledgerful baseline create parent --json
ledgerful change-evidence attest-baseline parent --json
```

After committing the child revision, update the index and prepare the same baseline.

```powershell
ledgerful index --full
ledgerful change-evidence prepare parent --json
```

The acknowledgements report the exact revision and baseline digest when established. A successful commit returns exit 0. An incomparable or unavailable result returns exit 1 with stable reason codes.

The dashboard uses the full parent and child commit IDs and the opaque local repository ID. It checks the checkout, discovery policy, config, and saved index generation before serving current comparison rows. Reprepare after a relevant change or once an observation is more than seven days old. The current proof describes the checkout and index at the time of observation. A later edit can invalidate that proof. Preparation reports an index input hash mismatch as `index-input-hash-mismatch`. Git and verification evidence can remain available for a historical revision. A partial Git section with `unsupported-tree-path` omits names outside the evidence path format and retains verified representable changes.

On Linux, comparison supports an absent system Git configuration and the stock hosted profile with Git's safe-directory setting and optional stock Git LFS entries. Ledgerful observes that fixed file for index discovery. Repository ownership, attribute and raw-input checks still apply. An unsupported system policy can return `index-discovery-unavailable`.

## Capture verification

Run verification with explicit capture enabled.

```powershell
ledgerful verify --scope full --capture-change-evidence --json 'git --version'
ledgerful verify --scope fast --capture-change-evidence --dry-run --json
```

The JSON response includes `changeEvidenceCapture`. A recorded plan-only attempt has its own attempt ID. An executed attempt adds a run ID after its canonical run and capture commit. An executed attempt binds its stored command-level observations to the checkout sampled before and after verification. A dirty or moved checkout can leave capture unavailable. Execution evidence describes actual command outcomes and their limits. A lost canonical run or sample appears as `run-record-lost`, with outcome and transaction details omitted.

A repository supports up to 128 attested names, 32 prepared keys, and 256 attempts. A new capture at a limit returns an explicit refusal.

## Serve or disable evidence

`ledgerful web start` serves evidence through the authenticated local dashboard. `GET /api/projects` advertises readiness for the primary project, `GET /api/changes` supplies eligible revision keys, and `GET /api/change-evidence` accepts an exact `repoScope`, `head`, and `base`. Supply `baselineName` and `baselineDigest` together when selecting a baseline. The [OpenAPI contract](api/openapi.json) describes the response sections and status codes. The daemon validates a saved comparison in a passive read. It sends `Cache-Control: no-store`, accepts queries up to 512 bytes, and admits at most two evidence workers. The response deadline is 3.5 seconds, the SQLite busy ceiling is 100 milliseconds, and serialized responses are capped at 256 KiB. Typed limits and partial states describe the evidence available within those bounds.

Use `ledgerful web start --disable-change-evidence` to disable serving while retaining captured records and the rest of the dashboard. Re-enable serving by restarting without that flag. Use an engine that supports the migrated schema throughout.

If you need to recover from a pre-migration backup, work from a copy of that backup in a separate checkout and keep the current database for recovery. The restored copy contains evidence saved through the backup date.
