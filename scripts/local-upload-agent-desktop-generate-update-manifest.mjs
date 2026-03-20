import fs from 'node:fs/promises'
import path from 'node:path'

const args = new Map()
for (const argument of process.argv.slice(2)) {
  const [key, value] = argument.split('=')
  args.set(key.replace(/^--/, ''), value)
}

const assetsDir = path.resolve(args.get('assets-dir') ?? 'release-assets')
const repo = args.get('repo')
const tag = args.get('tag')
const version = args.get('version')
const baseUrl = args.get('base-url')?.trim() ?? ''
const notes = args.get('notes') ?? 'See GitHub release notes.'

if (!version) {
  throw new Error('缺少 --version 参数')
}

if (!baseUrl && (!repo || !tag)) {
  throw new Error('缺少发布地址：请提供 --base-url，或同时提供 --repo / --tag')
}

async function findFile(suffix) {
  const entries = await fs.readdir(assetsDir, { recursive: true, withFileTypes: true })
  const entry = entries.find((item) => item.isFile() && item.name.endsWith(suffix))
  if (!entry) {
    throw new Error(`未找到文件：${suffix}`)
  }

  return path.join(entry.parentPath, entry.name)
}

function normalizeBaseUrl(value) {
  return value.replace(/\/+$/, '')
}

function encodeFileName(filePath) {
  return encodeURIComponent(path.basename(filePath))
}

function buildAssetUrl(filePath) {
  if (baseUrl) {
    return `${normalizeBaseUrl(baseUrl)}/${encodeFileName(filePath)}`
  }

  return `https://github.com/${repo}/releases/download/${tag}/${encodeFileName(filePath)}`
}

const macUpdaterBundle = await findFile('.app.tar.gz')
const macUpdaterSignature = await findFile('.app.tar.gz.sig')
const windowsUpdaterBundle = await findFile('.msi.zip')
const windowsUpdaterSignature = await findFile('.msi.zip.sig')

const manifest = {
  version,
  notes,
  pub_date: new Date().toISOString(),
  platforms: {
    'darwin-aarch64': {
      url: buildAssetUrl(macUpdaterBundle),
      signature: (await fs.readFile(macUpdaterSignature, 'utf8')).trim(),
    },
    'windows-x86_64': {
      url: buildAssetUrl(windowsUpdaterBundle),
      signature: (await fs.readFile(windowsUpdaterSignature, 'utf8')).trim(),
    },
  },
}

await fs.writeFile(path.join(assetsDir, 'latest.json'), `${JSON.stringify(manifest, null, 2)}\n`)
