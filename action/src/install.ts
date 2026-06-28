import * as core from '@actions/core'
import * as exec from '@actions/exec'
import * as path from 'node:path'

async function which(bin: string): Promise<boolean> {
  try {
    const isWindows = process.platform === 'win32'
    const cmd = isWindows ? 'where' : 'which'
    const exitCode = await exec.exec(cmd, [bin], { ignoreReturnCode: true, silent: true })
    return exitCode === 0
  } catch {
    return false
  }
}

async function cargoInstall(version: string): Promise<void> {
  const args = ['install']
  if (version) {
    args.push(
      'ledgerful',
      '--git', 'https://github.com/Ryan-AI-Studios/Ledgerful',
      '--tag', version,
    )
  } else {
    const actionRoot = process.env.GITHUB_ACTION_PATH
      ? path.resolve(process.env.GITHUB_ACTION_PATH, '..')
      : path.resolve(path.dirname(process.argv[1]), '..', '..')
    args.push('--path', actionRoot)
  }
  args.push('--locked', '--no-default-features')
  core.info(`Installing Ledgerful via cargo (this may take several minutes on first run)...`)
  core.info('Tip: Cache ~/.cargo/registry and ~/.cargo/git with actions/cache to speed up subsequent runs.')
  const exitCode = await exec.exec('cargo', args, { ignoreReturnCode: true })
  if (exitCode !== 0) {
    throw new Error('cargo install ledgerful failed. Ensure the Rust toolchain is installed and the repository has a committed Cargo.lock.')
  }
}

export async function ensureLedgerful(version: string): Promise<void> {
  if (await which('ledgerful')) {
    core.info('ledgerful already in PATH, skipping install')
    return
  }
  await cargoInstall(version)
}
