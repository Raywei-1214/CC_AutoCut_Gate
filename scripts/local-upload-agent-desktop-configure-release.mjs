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

config.bundle ??= {}
config.bundle.createUpdaterArtifacts = true
config.bundle.windows ??= {}

if (windowsThumbprint && windowsTimestampUrl) {
  config.bundle.windows.certificateThumbprint = windowsThumbprint
  config.bundle.windows.digestAlgorithm = windowsDigestAlgorithm
  config.bundle.windows.timestampUrl = windowsTimestampUrl
}

config.plugins ??= {}
config.plugins.updater = {
  pubkey: updaterPubkey,
  endpoints: updaterEndpoint ? [updaterEndpoint] : [],
  windows: {
    installMode: 'passive',
  },
}

await fs.writeFile(tauriConfigPath, `${JSON.stringify(config, null, 2)}\n`)
