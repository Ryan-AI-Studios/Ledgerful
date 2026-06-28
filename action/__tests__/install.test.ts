import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest'
import * as exec from '@actions/exec'
import * as path from 'node:path'
import { ensureLedgerful } from '../src/install.js'

vi.mock('@actions/core', () => ({
  info: vi.fn(),
}))

vi.mock('@actions/exec', () => ({
  exec: vi.fn(),
}))

const execMock = vi.mocked(exec.exec)

describe('ensureLedgerful', () => {
  beforeEach(() => {
    execMock.mockReset()
    vi.stubEnv('GITHUB_ACTION_PATH', process.cwd())
  })

  afterEach(() => {
    vi.unstubAllEnvs()
  })

  it('installs the checked-out source when no version is requested', async () => {
    execMock
      .mockResolvedValueOnce(1)
      .mockResolvedValueOnce(0)

    await ensureLedgerful('')

    expect(execMock).toHaveBeenNthCalledWith(
      1,
      expect.any(String),
      ['ledgerful'],
      expect.objectContaining({ ignoreReturnCode: true, silent: true }),
    )
    expect(execMock).toHaveBeenNthCalledWith(
      2,
      'cargo',
      [
        'install',
        '--path',
        path.resolve(process.cwd(), '..'),
        '--locked',
        '--no-default-features',
      ],
      expect.objectContaining({ ignoreReturnCode: true }),
    )
  })

  it('installs an explicitly tagged version from the repository', async () => {
    execMock
      .mockResolvedValueOnce(1)
      .mockResolvedValueOnce(0)

    await ensureLedgerful('v1.2.3')

    expect(execMock).toHaveBeenNthCalledWith(
      2,
      'cargo',
      [
        'install',
        'ledgerful',
        '--git',
        'https://github.com/Ryan-AI-Studios/Ledgerful',
        '--tag',
        'v1.2.3',
        '--locked',
        '--no-default-features',
      ],
      expect.objectContaining({ ignoreReturnCode: true }),
    )
  })

  it('locates the checked-out source from the JavaScript entry point', async () => {
    vi.stubEnv('GITHUB_ACTION_PATH', '')
    const originalEntryPoint = process.argv[1]
    process.argv[1] = path.join(process.cwd(), 'dist', 'index.cjs')
    execMock
      .mockResolvedValueOnce(1)
      .mockResolvedValueOnce(0)

    try {
      await ensureLedgerful('')
    } finally {
      process.argv[1] = originalEntryPoint
    }

    expect(execMock).toHaveBeenNthCalledWith(
      2,
      'cargo',
      [
        'install',
        '--path',
        path.resolve(process.cwd(), '..'),
        '--locked',
        '--no-default-features',
      ],
      expect.objectContaining({ ignoreReturnCode: true }),
    )
  })
})
