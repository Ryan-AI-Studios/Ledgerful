import * as exec from '@actions/exec'
import * as core from '@actions/core'
import { existsSync } from 'node:fs'
import { join } from 'node:path'

function stripAnsi(str: string): string {
  return str.replace(/\x1B\[[0-9;]*[mGKHF]/g, '')
}

export async function runLedgerful(args: string[], cwd: string): Promise<string> {
  let stdout = ''
  let stderr = ''
  const exitCode = await exec.exec('ledgerful', args, {
    cwd,
    ignoreReturnCode: true,
    listeners: {
      stdout: (data: Buffer) => { stdout += data.toString() },
      stderr: (data: Buffer) => { stderr += data.toString() },
    },
    silent: true,
  })
  if (exitCode !== 0) {
    throw new Error(`ledgerful ${args.join(' ')} failed (exit ${exitCode}):\n${stripAnsi(stderr).slice(0, 1000)}`)
  }
  return stripAnsi(stdout)
}

export async function initProject(cwd: string): Promise<void> {
  const cgDir = join(cwd, '.ledgerful')
  if (existsSync(cgDir)) {
    core.debug('.ledgerful/ already exists, skipping init')
    return
  }
  core.info('Initializing Ledgerful project...')
  await runLedgerful(['init'], cwd)
}

export async function scanImpact(cwd: string, baseRef: string): Promise<string> {
  const args = ['scan', '--impact', '--json']
  if (baseRef) {
    args.push('--base-ref', baseRef)
  }
  core.info(`Running: ledgerful ${args.join(' ')}`)
  return runLedgerful(args, cwd)
}
