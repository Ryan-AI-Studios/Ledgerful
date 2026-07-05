---
name: codex-review
description: Use this skill when you want a cross-model code review, a second opinion on changes, or an independent audit before committing. Trigger when the user asks for a review, a second pair of eyes, cross-model review, Codex review, Claude review, or wants GPT/Claude/Codex to examine code. Also trigger before final verification on high-risk changes.
---

# Cross-Model Review (Codex + Claude)

Different AI models catch different issues. The primary reviewer is OpenAI
Codex (`codex exec`); the fallback when Codex is exhausted or rate-limited
is Anthropic Claude (`claude -p`). Both produce independent reviews of the
same diff. This is especially valuable before committing high-risk changes,
after substantial refactors, or when the Ledgerful impact report shows
elevated risk.

## When To Use

- Before committing high-risk changes (ARCHITECTURE, FEATURE, BUGFIX categories)
- After a substantial refactor spanning multiple files
- When Ledgerful reports `riskLevel: High` or broad temporal couplings
- After implementing a full phase from the Ledger incorporation plan
- When you want a second opinion on design decisions
- Before creating a PR

## Primary: Codex Review (One-Shot)

```powershell
$null | codex exec -C "." -s read-only -m gpt-5.4 -o review.md "Review the current phase of work. Compare the current git diff against the base branch, identify bugs, regressions, missing tests, risky patterns, and unclear assumptions. Do not modify files. Give findings ordered by severity (critical/high/medium/low), then list the most important follow-up checks."
```

Key flags:

| Flag | Purpose |
|------|---------|
| `-C <path>` | Set workspace root (use "." for current dir) |
| `-s read-only` | Prevent the reviewer from modifying files |
| `-m gpt-5.4` | Use GPT-5.4 for the review (different training than Claude) |
| `-o review.md` | Write final review text to file |
| `--json` | Machine-readable output (for CI integration) |

#### SAMPLE Commands - alter based on the exact review you need.

### Codex Targeted Review

Review specific files or a specific diff:

```powershell
$null | codex exec -C "C:\dev\Ledgerful" -s read-only -m gpt-5.4 -o review.md "Review ONLY these files: src/ledger/transaction.rs src/ledger/db.rs. Check for: unsafe patterns, missing error handling, inconsistent status transitions, and SQL injection risks. Do not modify files."
```

Review a specific commit range:

```powershell
$null | codex exec -C "C:\dev\Ledgerful" -s read-only -m gpt-5.4 -o review.md "Review the changes between HEAD~5 and HEAD. Focus on: does the transaction lifecycle handle all edge cases? Are there any paths where a PENDING transaction could become orphaned? Do not modify files."
```

## Fallback: AGY Review (when Codex is exhausted) 
agy -p "Run 'git diff HEAD~3..HEAD --stat' then 'git diff HEAD~3..HEAD' in this repo and review the changes.
  Write the review to .\agy-review.md. Output P0/P1/P2 findings with file:line, issue, fix. Focus: Rust 2024 idioms,
  miette errors, determinism contract, unwrap/expect safety. Read-only review — do not edit files."

## Fallback: OpenCode Review (when Codex is exhausted)
If Claude or AGY hangs or errors out, OpenCode's CLI agent can be run automatically:
```powershell
opencode run --auto "Run 'git diff HEAD~3..HEAD' and review the changes against the plan. Output P0/P1/P2 findings to .\review.md. Do not modify source files."
```

## Fallback: Claude Review (when Codex is exhausted)

When Codex hits a usage limit, rate limit, or is unavailable, fall back to
Anthropic Claude for the cross-model pass. Claude is a different model family
(Anthropic vs OpenAI) and catches a different set of issues.


```powershell
claude -p "Run 'git diff HEAD~3..HEAD --stat' then 'git diff HEAD~3..HEAD' in this repo and review the changes. Output P0/P1/P2 findings with file:line, issue, fix. Focus: Rust 2024 idioms, miette errors, determinism contract, unwrap/expect safety. Read-only review — do not edit files." --allowedTools "Read,Edit,Bash" --output-format json 2>&1 | Out-File -FilePath claude-review.md -Encoding utf8 -Width 4096
```

### If it hangs anyway

A hang with zero output is *not* automatically "needs a token" — treat it as
a symptom to diagnose, not a known cause:

1. **Check for a stale/conflicting interactive `claude` process first.** An
   already-running interactive session can hold a lock on the credential
   file that a nested headless call blocks on. Find and consider closing it
   (`Get-Process claude` / Task Manager) before assuming an auth problem.
2. **Check the stored login is actually valid**, not expired — run a plain
   interactive `claude` and confirm it doesn't prompt to re-login.
3. **Only if both of those check out**, fall back to an explicit credential
   that bypasses the stored-login path entirely:
   ```powershell
   claude setup-token   # one-time; mints a ~1-year token off your subscription
   $env:CLAUDE_CODE_OAUTH_TOKEN = "<token from setup-token>"
   ```
   `$env:` assignments are session-scoped — to avoid re-pasting it every new
   terminal, persist it once instead:
   ```powershell
   [Environment]::SetEnvironmentVariable("CLAUDE_CODE_OAUTH_TOKEN", "<token>", "User")
   ```
   An `ANTHROPIC_API_KEY` also works and takes precedence if you'd rather
   bill per-token through the Console instead of riding the subscription.

> [!IMPORTANT]
> - Claude `-p` is non-interactive but can be slow (2-10 min) on substantive prompts even when auth is fine. Give it a generous timeout (600000ms+); there's also a built-in background-wait cap (~10 min by default since v2.1.182), overridable via `CLAUDE_CODE_PRINT_BG_WAIT_CEILING_MS`.
> - The `--allowedTools "Read,Edit,Bash"` flags are required so Claude can run `git diff` and read files.
> - Claude writes the full output at the end (not streamed), so an empty file during execution is normal for the first couple minutes — don't kill it prematurely.
> - If reviewing a committed range, pre-stage the diff (`git diff HEAD~3..HEAD > review-diff.txt`) and tell Claude to read that file instead of running git — more reliable on Windows.

### Claude Flags

| Flag | Purpose |
|------|---------|
| `-p "<prompt>"` | Non-interactive print mode (required) |
| `--allowedTools "Read,Edit,Bash"` | Tool allowlist (required for git/file access) |
| `--output-format json` | Structured output; easier to parse than raw text (optional) |
| `--bare` | Skip OAuth/keychain reads entirely — pairs with an explicit `ANTHROPIC_API_KEY` in CI-style environments with no user profile (optional) |
| `--dangerously-skip-permissions` | Bypass permission prompts (optional, faster) |

### Environment / Auth (only needed if the zero-setup path fails)

| Variable | Purpose |
|---|---|
| `CLAUDE_CODE_OAUTH_TOKEN` | Long-lived token from `claude setup-token`, rides your existing subscription. |
| `ANTHROPIC_API_KEY` | Console API key; takes precedence over subscription OAuth if set. Bills per-token separately from your subscription. |
| `CLAUDE_CODE_PRINT_BG_WAIT_CEILING_MS` | Override the ~10 min default background-wait cap for `-p` (`0` = unlimited) |

### Choosing Codex vs Claude

| Situation | Use |
|---|---|
| Codex available, under limit | **Codex** (primary) |
| Codex rate-limited / usage exhausted | **Claude** (fallback) |
| Both available | **Codex first**, then Claude for a second independent pass if the first found issues |
| Neither available | Continue with native gates (build/lint/test); report the missing cross-model signal |

## Ledgerful-Aware Review

Include Ledgerful signals in the review prompt so Codex can prioritize its findings:

```powershell
$null | codex exec -C "C:\dev\Ledgerful" -s read-only -m gpt-5.4 -o review.md "Run 'ledgerful impact --summary' to see the current risk level. Then review the git diff with that risk context. Focus on: (1) files with high hotspot scores, (2) temporally coupled files that weren't changed but might need updates, (3) protected paths. Do not modify files."
```

## Interactive Review

For deeper investigation where you want back-and-forth:

```powershell
codex -C "C:\dev\Ledgerful" -m gpt-5.4
```

Then inside the TUI:

```
/review
```

This opens an interactive review session. Use `/model` to switch models mid-session if needed.

## Review Profiles

For frequent reviews, create a profile in `~/.codex/config.toml`:

```toml
[profiles.deep-review]
model = "gpt-5.4"
sandbox = "read-only"
ask_for_approval = "never"
```

Then invoke:

```powershell
$null | codex exec -p deep-review -C "C:\dev\Ledgerful" -o review.md "Review the current diff for bugs, regressions, and missing tests. Do not modify files."
```

## Reading the Output

After a one-shot review, read the output:

```powershell
Get-Content review.md -TotalCount 200
```

Remove or gitignore the scratch file before finishing:

```powershell
Remove-Item review.md
```

The review should contain findings ordered by severity. Address critical and high findings before committing. Medium and low findings can be tracked as follow-up.

## Integration with Ledgerful Workflow

1. Run `ledgerful scan --impact` — get risk signals
2. Make your changes
3. Run `ledgerful impact` — see blast radius
4. Run cross-model review (Codex primary, Claude fallback) — get findings
5. Address critical/high findings
6. Run `ledgerful verify` — run configured verification
7. Commit with `ledgerful ledger commit`

## Safety Notes

- Always use `-s read-only` for reviews (Codex) or instruct Claude not to edit files. The reviewer should never modify files.
- Do not pass secrets, API keys, or `.env` contents in review prompts.
- Cross-model output is written by a different model — its suggestions may not align with this project's conventions (Rust 2024, miette errors, determinism contract). Evaluate suggestions against the coding-core skill before applying.
- Review output is advisory, not authoritative. You still make the final call.

## Troubleshooting

| Symptom | Fix |
|---|---|
| Codex output file is empty | Check if it's blocking on stdin (`< NUL` required in CI/agents) |
| Codex: `You've hit your usage limit` | Fall back to Claude (see above); Codex resets at the stated time |
| Codex command hangs | Codex may be waiting on stdin; run from a normal terminal first or redirect `< NUL` |
| Claude `-p` hangs with empty output, no auth error | Do NOT assume "needs a token" — verified empirically that a logged-in machine needs no env var at all. Check for a stale/conflicting interactive `claude` process first (it can hold a credential-file lock); confirm the stored login isn't expired by running plain interactive `claude`; only then fall back to `CLAUDE_CODE_OAUTH_TOKEN`/`ANTHROPIC_API_KEY` (see "If it hangs anyway" above). |
| Claude `-p` still hangs after ruling out stale processes, expired login, and setting an explicit token | Kill the process and retry; suspect a Windows stdin/pipe issue. Pre-stage the diff to a file and tell Claude to read it instead of running git. |
| Claude output is truncated or mangled | Ensure the prompt is a single quoted string; spaces in unquoted args get split on Windows |

## Cost Awareness

Each `codex exec` or `claude -p` call consumes API tokens. For routine low-risk changes, skip the cross-model review. Reserve it for:

- High-risk or high-complexity changes
- Phase completion reviews (L1, L2, etc.)
- Pre-PR reviews
- When you're uncertain about a design decision