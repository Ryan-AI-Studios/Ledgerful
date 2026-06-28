import { describe, it, expect } from 'vitest'
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { join, dirname } from 'node:path'
import { buildComment, COMMENT_MARKER, formatAge } from '../src/comment.js'
import { parseImpactPacket } from '../src/impact.js'
import type { ImpactPacket } from '../src/types.js'

const __filename = fileURLToPath(import.meta.url)
const __dirname = dirname(__filename)
const fixturesDir = join(__dirname, '..', 'fixtures')

function readFixture<T>(name: string): T {
  return JSON.parse(readFileSync(join(fixturesDir, name), 'utf-8')) as T
}

const HIGH_SHA = 'abc123def456789'
const LOW_SHA = '111222333444555'

describe('buildComment', () => {
  it('HIGH risk comment contains marker', () => {
    const packet = readFixture<ImpactPacket>('impact-high.json')
    const comment = buildComment(packet, HIGH_SHA)
    expect(comment).toContain(COMMENT_MARKER)
  })

  it('HIGH risk comment contains risk badge', () => {
    const packet = readFixture<ImpactPacket>('impact-high.json')
    const comment = buildComment(packet, HIGH_SHA)
    expect(comment).toContain('🔴 HIGH')
  })

  it('LOW risk comment contains correct badge', () => {
    const packet = readFixture<ImpactPacket>('impact-low.json')
    const comment = buildComment(packet, LOW_SHA)
    expect(comment).toContain('🟢 LOW')
  })

  it('clean tree comment does not render empty details blocks', () => {
    const packet = readFixture<ImpactPacket>('impact-clean.json')
    const comment = buildComment(packet, HIGH_SHA)
    // No changed files, no couplings, no ci predictions, no hotspots => no <details>
    expect(comment).not.toContain('<details>')
  })

  it('coupling details block present when couplings > 0', () => {
    const packet = readFixture<ImpactPacket>('impact-high.json')
    const comment = buildComment(packet, HIGH_SHA)
    expect(comment).toContain('Temporal Couplings')
    expect(comment).toContain('<details>')
  })

  it('coupling details block absent when couplings == 0', () => {
    const packet = readFixture<ImpactPacket>('impact-low.json')
    const comment = buildComment(packet, LOW_SHA)
    // The metrics table contains "Temporal Couplings | 0" but no <details> block for couplings
    expect(comment).not.toContain('🔗 Temporal Couplings')
  })

  it('file list truncated at 20', () => {
    const files = Array.from({ length: 25 }, (_, i) => ({
      path: `src/file${i}.ts`,
      status: 'Modified',
      isStaged: true,
    }))
    const packet: ImpactPacket = {
      riskLevel: 'LOW',
      riskReasons: [],
      treeClean: false,
      changes: files,
      temporalCouplings: [],
      hotspots: [],
      ciPredictions: [],
    }
    const comment = buildComment(packet, HIGH_SHA)
    expect(comment).toContain('(5 more files not shown)')
  })

  it('comment includes short sha in footer', () => {
    const packet = readFixture<ImpactPacket>('impact-high.json')
    const comment = buildComment(packet, HIGH_SHA)
    expect(comment).toContain('abc123d')
  })

  it('ci predictions block present when predictions > 0', () => {
    const packet = readFixture<ImpactPacket>('impact-high.json')
    const comment = buildComment(packet, HIGH_SHA)
    expect(comment).toContain('CI Predictions')
  })

  it('hotspots block present when hotspots > 0', () => {
    const packet = readFixture<ImpactPacket>('impact-high.json')
    const comment = buildComment(packet, HIGH_SHA)
    expect(comment).toContain('Hotspots')
  })

  it('does not crash when ciPredictions is absent from raw JSON (real Rust output omits it via skip_serializing_if)', () => {
    const raw = JSON.stringify({
      riskLevel: 'low',
      riskReasons: [],
      treeClean: false,
      changes: [],
      temporalCouplings: [],
      hotspots: [],
    })
    const packet = parseImpactPacket(raw)
    expect(() => buildComment(packet, LOW_SHA)).not.toThrow()
  })
})

describe('formatAge', () => {
  it('returns unknown for invalid date string', () => {
    expect(formatAge('invalid')).toBe('unknown')
  })

  it('returns unknown for empty string', () => {
    expect(formatAge('')).toBe('unknown')
  })

  it('returns a time string for a valid ISO date', () => {
    const result = formatAge(new Date().toISOString())
    expect(result).toMatch(/^\d+[mhd] ago$/)
  })
})
