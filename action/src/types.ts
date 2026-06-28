export type RiskLevel = 'HIGH' | 'MEDIUM' | 'LOW' | 'TRIVIAL'

export const VALID_RISK_THRESHOLDS: readonly RiskLevel[] = ['TRIVIAL', 'LOW', 'MEDIUM', 'HIGH']
export const VALID_FAIL_ON_RISK: readonly (RiskLevel | '')[] = ['', 'LOW', 'MEDIUM', 'HIGH']

export interface ChangedFile {
  path: string
  status: string
  isStaged: boolean
  oldPath?: string
}

export interface TemporalCoupling {
  fileA: string
  fileB: string
  score: number
}

export interface Hotspot {
  path: string
  score: number
  displayScore?: number
  complexity?: number
  frequency?: number
}

export interface CIPrediction {
  jobName: string
  platform: string
  failureProbability: number
  explanation?: string | null
}

export interface ImpactPacket {
  riskLevel: RiskLevel
  riskReasons: string[]
  treeClean: boolean
  changes: ChangedFile[]
  temporalCouplings: TemporalCoupling[]
  hotspots: Hotspot[]
  ciPredictions: CIPrediction[]
}

export interface ActionInputs {
  githubToken: string
  projectPath: string
  baseRef: string
  riskThreshold: RiskLevel
  failOnRisk: RiskLevel | ''
  postOnClean: boolean
}
