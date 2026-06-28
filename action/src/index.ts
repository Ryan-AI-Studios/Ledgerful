import * as core from '@actions/core'
import * as github from '@actions/github'
import { ensureLedgerful } from './install.js'
import { initProject, scanImpact } from './runner.js'
import { parseImpactPacket, isRiskAtOrAbove } from './impact.js'
import { buildComment } from './comment.js'
import { upsertComment } from './github.js'
import type { RiskLevel, ActionInputs } from './types.js'
import { VALID_RISK_THRESHOLDS, VALID_FAIL_ON_RISK } from './types.js'

function readInputs(): ActionInputs {
  return {
    githubToken: core.getInput('github-token', { required: true }),
    projectPath: core.getInput('project-path') || '.',
    baseRef: core.getInput('base-ref'),
    riskThreshold: (core.getInput('risk-threshold') || 'TRIVIAL') as RiskLevel,
    failOnRisk: core.getInput('fail-on-risk') as RiskLevel | '',
    postOnClean: core.getInput('post-on-clean') === 'true',
  }
}

async function run(): Promise<void> {
  const inputs = readInputs()

  // Validate riskThreshold and failOnRisk before doing any work
  if (!(VALID_RISK_THRESHOLDS as readonly string[]).includes(inputs.riskThreshold)) {
    core.setFailed(`Invalid risk-threshold '${inputs.riskThreshold}'. Must be one of: ${VALID_RISK_THRESHOLDS.join(', ')}`)
    return
  }
  if (!(VALID_FAIL_ON_RISK as readonly string[]).includes(inputs.failOnRisk)) {
    core.setFailed(`Invalid fail-on-risk '${inputs.failOnRisk}'. Must be one of: ${VALID_FAIL_ON_RISK.filter(v => v !== '').join(', ')} (or empty to disable)`)
    return
  }

  const ctx = github.context

  // Only operate on pull_request events
  if (ctx.eventName !== 'pull_request') {
    core.warning(`Ledgerful action is designed for pull_request events; got '${ctx.eventName}'. Skipping.`)
    return
  }

  const prNumber = ctx.payload.pull_request?.number
  if (!prNumber) {
    core.setFailed('Could not determine PR number from context')
    return
  }

  const baseRef = inputs.baseRef || (ctx.payload.pull_request?.base?.sha ?? '')
  const headSha = ctx.payload.pull_request?.head?.sha ?? ctx.sha

  core.info(`PR #${prNumber}: comparing ${headSha.slice(0, 7)} against base ${baseRef.slice(0, 7)}`)

  // Step 1: Install Ledgerful
  await ensureLedgerful(core.getInput('ledgerful-version') || '')

  // Step 2: Init project
  await initProject(inputs.projectPath)

  // Step 3: Scan
  const rawJson = await scanImpact(inputs.projectPath, baseRef)

  // Step 4: Parse
  const packet = parseImpactPacket(rawJson)

  // Step 5: Set outputs
  core.setOutput('overall-risk', packet.riskLevel)
  core.setOutput('changed-files-count', String(packet.changes.length))

  // Step 6: Check threshold
  if (!isRiskAtOrAbove(packet.riskLevel, inputs.riskThreshold)) {
    core.info(`Risk ${packet.riskLevel} is below threshold ${inputs.riskThreshold}. Skipping comment.`)
    return
  }

  // Step 7: Skip if clean and post-on-clean is false
  if (packet.treeClean && !inputs.postOnClean) {
    core.info('Working tree is clean. Skipping comment (set post-on-clean: true to override).')
    return
  }

  // Step 8: Build and post comment
  const body = buildComment(packet, headSha)
  const octokit = github.getOctokit(inputs.githubToken)
  const { owner, repo } = ctx.repo
  const commentUrl = await upsertComment(octokit, owner, repo, prNumber, body)
  core.setOutput('comment-url', commentUrl)
  core.info(`Risk comment posted: ${commentUrl}`)

  // Step 9: Fail on risk threshold
  if (inputs.failOnRisk && isRiskAtOrAbove(packet.riskLevel, inputs.failOnRisk as RiskLevel)) {
    core.setFailed(`Ledgerful: overall risk is ${packet.riskLevel} (fail-on-risk: ${inputs.failOnRisk})`)
  }
}

run().catch(core.setFailed)
