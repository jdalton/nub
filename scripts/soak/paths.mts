/**
 * @file 1 path, 1 reference — every filesystem location the soak +
 *   external-tools scripts touch is declared here exactly once. Scripts
 *   import from this module instead of re-deriving paths, so a surface can
 *   move (or differ between repos carrying these scripts) with a one-line
 *   change.
 */

import { existsSync } from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import process from 'node:process'
import { fileURLToPath } from 'node:url'

export const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..')

// Soak surfaces (repo-relative). nub's npm package lives at the repo root,
// so the workspace yaml / npmrc / taze config are root files.
export const SURFACES = {
  cargoConfig: '.cargo/config.toml',
  npmrc: '.npmrc',
  workspaceYaml: 'pnpm-workspace.yaml',
  tazeConfig: 'taze.config.mts',
}

// The directory holding the npm package the soak governs (taze runs here,
// the repo's installer refreshes this package's lockfile).
export const NPM_PKG_DIR = REPO_ROOT

// Lockfile refreshers tried in order after taze rewrites package.json.
export const NPM_INSTALLERS: string[][] = [['pnpm', 'install']]

// rustup's cargo shim — the only cargo that reads rust-toolchain.toml and
// therefore the only one whose `cargo update` honors the [unstable]
// min-publish-age soak.
export const RUSTUP_CARGO = path.join(os.homedir(), '.cargo/bin/cargo')

// Pinned external tool manifest + the local tool rack it installs into:
// exact versions under rack/<tool>/<version>/, flat PATH handles in bin/.
export const EXTERNAL_TOOLS_JSON = path.join(REPO_ROOT, 'external-tools.json')

const XDG_DATA_HOME = process.env.XDG_DATA_HOME || path.join(os.homedir(), '.local/share')
export const DEV_TOOLS_DIR = path.join(XDG_DATA_HOME, 'nub/dev-tools')
export const RACK_DIR = path.join(DEV_TOOLS_DIR, 'rack')
export const BIN_DIR = path.join(DEV_TOOLS_DIR, 'bin')

export function assertRepoRoot(): void {
  if (!existsSync(path.join(REPO_ROOT, 'external-tools.json'))) {
    throw new Error(`repo root not found at ${REPO_ROOT}`)
  }
}
