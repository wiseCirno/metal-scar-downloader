'use strict'

// A private JSON-lines transport. Upstream diagnostics must never reach IPC or disk.
const fs = require('node:fs')
const path = require('node:path')
const os = require('node:os')
const crypto = require('node:crypto')
const readline = require('node:readline')
const { createRequire } = require('node:module')
for (const name of ['log', 'info', 'debug', 'warn', 'error', 'trace']) console[name] = () => {}
const runtimeRequire = createRequire(path.join(__dirname, 'package.json'))
const methods = new Set(['captcha_sent', 'login_cellphone', 'login_status', 'logout', 'artist_songs', 'album', 'song_detail', 'song_download_url_v1', 'song_download_url', 'lyric'])
const MAX_LINE = 1024 * 1024
let api
let initializationError = false
try {
  const cache = os.tmpdir()
  fs.mkdirSync(cache, { recursive: true, mode: 0o700 })
  const identityPath = path.join(cache, 'desktop-device.json')
  let identity
  try { identity = JSON.parse(fs.readFileSync(identityPath, 'utf8')).deviceId } catch {}
  if (typeof identity !== 'string' || !/^[0-9a-f]{32}$/.test(identity)) {
    identity = crypto.randomBytes(16).toString('hex')
    fs.writeFileSync(identityPath, JSON.stringify({ deviceId: identity }), { mode: 0o600 })
  }
  global.deviceId = identity
  api = runtimeRequire('@neteasecloudmusicapienhanced/api')
} catch {
  initializationError = true
}

function send(message) {
  process.stdout.write(`${JSON.stringify(message)}\n`)
}
function normalize(response) {
  if (!response || typeof response.body !== 'object' || response.body === null || Buffer.isBuffer(response.body)) {
    return { error: 'NETEASE_RESPONSE' }
  }
  return {
    body: response.body,
    cookies: Array.isArray(response.cookie) ? response.cookie.filter(value => typeof value === 'string') : [],
  }
}
async function dispatch(request) {
  const { id, method, query } = request || {}
  if (!Number.isSafeInteger(id) || !methods.has(method) && method !== 'health' || !query || typeof query !== 'object' || Array.isArray(query)) {
    return { id: Number.isSafeInteger(id) ? id : 0, error: 'NETEASE_REQUEST' }
  }
  if (initializationError) return { id, error: 'NETEASE_API_UNAVAILABLE' }
  if (method === 'health') {
    const version = runtimeRequire('@neteasecloudmusicapienhanced/api/package.json').version
    return { id, body: { code: 200, version, node: process.versions.node }, cookies: [] }
  }
  // Only this application's read/auth API allowlist is exposed. Never enable unblock fallback.
  try {
    const response = await api[method]({ ...query, timeout: 25000 })
    return { id, ...normalize(response) }
  } catch (response) {
    // The upstream rejects with structured API responses as well as transport errors.
    if (response && response.body && typeof response.body === 'object') {
      return { id, ...normalize(response) }
    }
    return { id, error: 'NETEASE_NETWORK' }
  }
}

const lines = readline.createInterface({ input: process.stdin, crlfDelay: Infinity })
let chain = Promise.resolve()
lines.on('line', line => {
  chain = chain.then(async () => {
    if (Buffer.byteLength(line) > MAX_LINE) { send({ id: 0, error: 'NETEASE_REQUEST' }); return }
    let request
    try { request = JSON.parse(line) } catch { send({ id: 0, error: 'NETEASE_REQUEST' }); return }
    send(await dispatch(request))
  }).catch(() => send({ id: 0, error: 'NETEASE_API_UNAVAILABLE' }))
})
lines.on('close', () => { process.exit(0) })
process.on('uncaughtException', () => { process.exit(1) })
process.on('unhandledRejection', () => { process.exit(1) })
