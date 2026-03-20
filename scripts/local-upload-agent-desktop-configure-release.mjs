import fs from 'node:fs/promises'
import path from 'node:path'

const root = process.cwd()
const tauriConfigPath = path.join(root, 'apps/local-upload-agent/src-tauri/tauri.conf.json')

const updaterEndpoint = process.env.CHUANGCUT_AGENT_UPDATER_ENDPOINT?.trim() ?? ''
const updaterPubkey = process.env.CHUANGCUT_AGENT_UPDATER_PUBKEY?.trim() ?? ''
const windowsThumbprint = process.env.WINDOWS_CERTIFICATE_THUMBPRINT?.trim() ?? ''
const windowsDigestAlgorithm = process.env.WINDOWS_DIGEST_ALGORITHM?.trim() || 'sha256'
const windowsTimestampUrl = process.env.WINDOWS_TIMESTAMP_URL?.trim() ?? ''

const raw = await fs.readFile(tauriConfigPath, 'utf8')
const config = JSON.parse(raw)
const updaterConfigured = Boolean(updaterEndpoint && updaterPubkey)

config.bundle ??= {}
config.bundle.createUpdaterArtifacts = updaterConfigured
config.bundle.windows ??= {}

if (windowsThumbprint && windowsTimestampUrl) {
  config.bundle.windows.certificateThumbprint = windowsThumbprint
  config.bundle.windows.digestAlgorithm = windowsDigestAlgorithm
  config.bundle.windows.timestampUrl = windowsTimestampUrl
}

config.plugins ??= {}

if (updaterConfigured) {
  config.plugins.updater = {
    pubkey: updaterPubkey,
    endpoints: [updaterEndpoint],
    windows: {
      installMode: 'passive',
    },
  }
} else {
  delete config.plugins.updater
  if (Object.keys(config.plugins).length === 0) {
    delete config.plugins
  }
}

await fs.writeFile(tauriConfigPath, `${JSON.stringify(config, null, 2)}\n`)
