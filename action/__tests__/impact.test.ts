import { describe, it, expect } from 'vitest'
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { join, dirname } from 'node:path'
import { parseImpactPacket, isRiskAtOrAbove } from '../src/impact.js'

const __filename = fileURLToPath(import.meta.url)
const __dirname = dirname(__filename)
const fixturesDir = join(__dirname, '..', 'fixtures')

function readFixture(name: string): string {
  return readFileSync(join(fixturesDir, name), 'utf-8')
}

describe('parseImpactPacket', () => {
  it('parses HIGH risk packet from fixture and uppercases riskLevel', () => {
    const packet = parseImpactPacket(readFixture('impact-high.json'))
    expect(packet.riskLevel).toBe('HIGH')
    expect(packet.changes.length).toBe(3)
  })

  it('parses LOW risk packet from fixture', () => {
    const packet = parseImpactPacket(readFixture('impact-low.json'))
    expect(packet.riskLevel).toBe('LOW')
    expect(packet.changes.length).toBe(1)
  })

  it('parses clean tree packet', () => {
    const packet = parseImpactPacket(readFixture('impact-clean.json'))
    expect(packet.treeClean).toBe(true)
    expect(packet.changes.length).toBe(0)
  })

  it('tolerates missing optional fields', () => {
    const raw = JSON.stringify({ riskLevel: 'low', riskReasons: [], treeClean: false, changes: [], temporalCouplings: [], hotspots: [], ciPredictions: [] })
    expect(() => parseImpactPacket(raw)).not.toThrow()
    const packet = parseImpactPacket(raw)
    expect(packet.riskLevel).toBe('LOW')
  })

  it('defaults ciPredictions to [] when the key is absent from the JSON (Rust skip_serializing_if = "Vec::is_empty")', () => {
    const raw = JSON.stringify({ riskLevel: 'low', riskReasons: [], treeClean: false, changes: [], temporalCouplings: [], hotspots: [] })
    const packet = parseImpactPacket(raw)
    expect(packet.ciPredictions).toEqual([])
  })

  it('throws on invalid JSON', () => {
    expect(() => parseImpactPacket('not json {')).toThrow(/invalid JSON/)
  })

  it('throws when riskLevel is missing', () => {
    const raw = JSON.stringify({ changes: [] })
    expect(() => parseImpactPacket(raw)).toThrow(/riskLevel/)
  })
})

describe('isRiskAtOrAbove', () => {
  it('HIGH >= MEDIUM is true', () => {
    expect(isRiskAtOrAbove('HIGH', 'MEDIUM')).toBe(true)
  })

  it('LOW >= HIGH is false', () => {
    expect(isRiskAtOrAbove('LOW', 'HIGH')).toBe(false)
  })

  it('identical levels returns true', () => {
    expect(isRiskAtOrAbove('MEDIUM', 'MEDIUM')).toBe(true)
    expect(isRiskAtOrAbove('HIGH', 'HIGH')).toBe(true)
    expect(isRiskAtOrAbove('LOW', 'LOW')).toBe(true)
    expect(isRiskAtOrAbove('TRIVIAL', 'TRIVIAL')).toBe(true)
  })

  it('TRIVIAL >= LOW is false', () => {
    expect(isRiskAtOrAbove('TRIVIAL', 'LOW')).toBe(false)
  })

  it('HIGH >= TRIVIAL is true', () => {
    expect(isRiskAtOrAbove('HIGH', 'TRIVIAL')).toBe(true)
  })
})
