import type { ImpactPacket, RiskLevel } from './types.js'

const RISK_ORDER: Record<RiskLevel, number> = {
  TRIVIAL: 0,
  LOW: 1,
  MEDIUM: 2,
  HIGH: 3,
}

export function parseImpactPacket(raw: string): ImpactPacket {
  let obj: unknown
  try {
    obj = JSON.parse(raw)
  } catch {
    throw new Error('invalid JSON from ledgerful scan: ' + String(raw).slice(0, 200))
  }
  if (typeof obj !== 'object' || obj === null) {
    throw new Error('invalid JSON from ledgerful scan: expected object')
  }
  if (typeof (obj as Record<string, unknown>)['riskLevel'] !== 'string') {
    throw new Error('invalid impact packet: riskLevel field is missing or not a string')
  }
  const packet = obj as ImpactPacket
  // Rust outputs lowercase risk levels; normalize to uppercase for TypeScript comparisons
  packet.riskLevel = (packet.riskLevel as string).toUpperCase() as RiskLevel
  // ciPredictions is skip_serializing_if = "Vec::is_empty" on the Rust side, so it
  // is omitted from the JSON entirely (not even `[]`) when there are no predictions.
  packet.ciPredictions = packet.ciPredictions ?? []
  return packet
}

export function isRiskAtOrAbove(level: RiskLevel, threshold: RiskLevel): boolean {
  return (RISK_ORDER[level] ?? -1) >= (RISK_ORDER[threshold] ?? -1)
}