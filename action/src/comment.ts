import type { ImpactPacket, RiskLevel } from './types.js'

export const COMMENT_MARKER = '<!-- ledgerful-risk-report:v1 -->'

const RISK_BADGE: Record<RiskLevel, string> = {
  HIGH: '🔴 HIGH',
  MEDIUM: '🟡 MEDIUM',
  LOW: '🟢 LOW',
  TRIVIAL: '⚪ TRIVIAL',
}

const RISK_EMOJI: Record<RiskLevel, string> = {
  HIGH: '🔴',
  MEDIUM: '🟡',
  LOW: '🟢',
  TRIVIAL: '⚪',
}

function riskBadge(level: string): string {
  return RISK_BADGE[level as RiskLevel] ?? `⚠️ ${level}`
}

export function formatAge(isoDate: string): string {
  const ts = new Date(isoDate).getTime()
  if (isNaN(ts)) return 'unknown'
  const ms = Date.now() - ts
  const mins = Math.floor(ms / 60000)
  if (mins < 60) return `${mins}m ago`
  const hrs = Math.floor(mins / 60)
  if (hrs < 24) return `${hrs}h ago`
  return `${Math.floor(hrs / 24)}d ago`
}

function summaryLine(packet: ImpactPacket): string {
  const hasCouplings = packet.temporalCouplings.length > 0
  const fileCount = packet.changes.length
  let line = ''
  if (hasCouplings) {
    line = `${packet.temporalCouplings.length} temporal coupling(s) detected — verify related files before merging.`
  } else {
    line = `${fileCount} file(s) changed with ${packet.riskLevel} risk.`
  }
  if (packet.riskLevel === 'HIGH') {
    line = `⚠️ Attention required. ${line}`
  }
  return line
}

export function buildComment(rawPacket: ImpactPacket, sha: string): string {
  // Normalize riskLevel to uppercase in case called with raw Rust output (lowercase)
  const packet: ImpactPacket = {
    ...rawPacket,
    riskLevel: (rawPacket.riskLevel as string).toUpperCase() as RiskLevel,
  }
  const badge = riskBadge(packet.riskLevel)
  const emoji = RISK_EMOJI[packet.riskLevel as RiskLevel] ?? '⚠️'
  const now = new Date().toUTCString()
  const sha7 = sha.slice(0, 7)

  const shownFiles = packet.changes.slice(0, 20)
  const hiddenFiles = packet.changes.length - shownFiles.length

  const fileRows = shownFiles
    .map(f => `| \`${f.path}\` | ${f.status} |`)
    .join('\n')
  const fileFooter = hiddenFiles > 0 ? `\n\n_(${hiddenFiles} more files not shown)_` : ''
  const filesBlock = packet.changes.length > 0
    ? `<details>\n<summary>📂 Changed Files (${packet.changes.length})</summary>\n\n| File | Status |\n|---|---|\n${fileRows}${fileFooter}\n\n</details>`
    : ''

  const couplingRows = packet.temporalCouplings
    .map(c => `| \`${c.fileA}\` ↔ \`${c.fileB}\` | ${Math.round(c.score * 100)}% |`)
    .join('\n')
  const couplingsBlock = packet.temporalCouplings.length > 0
    ? `<details>\n<summary>🔗 Temporal Couplings (${packet.temporalCouplings.length})</summary>\n\n| Files | Coupling |\n|---|---|\n${couplingRows}\n\n</details>`
    : ''

  const hotspotRows = packet.hotspots
    .map(h => `| \`${h.path}\` | ${Math.round(h.score * 100)}% |`)
    .join('\n')
  const hotspotsBlock = packet.hotspots.length > 0
    ? `<details>\n<summary>🔥 Hotspots (${packet.hotspots.length})</summary>\n\n| File | Score |\n|---|---|\n${hotspotRows}\n\n</details>`
    : ''

  const predRows = packet.ciPredictions
    .map(p => `| \`${p.jobName}\` | ${Math.round(p.failureProbability * 100)}% | ${p.explanation ?? '—'} |`)
    .join('\n')
  const predictionsBlock = packet.ciPredictions.length > 0
    ? `<details>\n<summary>🔮 CI Predictions (${packet.ciPredictions.length})</summary>\n\n| Job | Probability | Reason |\n|---|---|---|\n${predRows}\n\n</details>`
    : ''

  const reasonsBlock = packet.riskReasons.length > 0
    ? `**Risk Reasons:** ${packet.riskReasons.join(', ')}`
    : ''

  const sections = [reasonsBlock, filesBlock, couplingsBlock, hotspotsBlock, predictionsBlock].filter(Boolean).join('\n\n')

  const metricsTable = [
    `| Overall Risk | ${badge} |`,
    `| Changed Files | ${packet.changes.length} |`,
    `| Temporal Couplings | ${packet.temporalCouplings.length} |`,
    `| Hotspots | ${packet.hotspots.length} |`,
    `| CI Predictions | ${packet.ciPredictions.length} |`,
  ].join('\n')

  return [
    COMMENT_MARKER,
    `## Ledgerful Risk Analysis ${emoji} ${packet.riskLevel}`,
    '',
    `> ${summaryLine(packet)}`,
    '',
    '| Metric | Value |',
    '|---|---|',
    metricsTable,
    '',
    sections,
    '',
    '---',
    `*[Ledgerful](https://github.com/Ryan-AI-Studios/Ledgerful) · commit \`${sha7}\` · ${now}*`,
  ].filter(line => line !== undefined).join('\n')
}
