import { load } from "@tauri-apps/plugin-store"

const DEFAULT_CONFIG = {
  download_path: "",
  concurrent_downloads: 3,
  download_instrumental: false,
  download_lyrics: false,
  id3_date_format: "year",
  artist_separator: ";",
  custom_album: "none",
  show_cover: true,
  language: "zh-Hans",
  netease_artist_id: "122546004",
  audio_quality: "exhigh",
}

export const CONFIG_CHANGED_EVENT = "app-config-changed"

const CONFIG_KEYS = Object.keys(DEFAULT_CONFIG)
const AUDIO_QUALITIES = new Set(["standard", "higher", "exhigh", "lossless", "hires"])
let storeInstance = null

async function getStore() {
  if (!storeInstance) storeInstance = await load("config.json", { autoSave: true })
  return storeInstance
}

function migrateConfig(config) {
  const migrated = { ...DEFAULT_CONFIG }
  if (config && typeof config === "object") {
    for (const key of CONFIG_KEYS) {
      if (key in config) migrated[key] = config[key]
    }
  }
  const artistId = String(migrated.netease_artist_id)
  migrated.netease_artist_id =
    /^\d{1,20}$/.test(artistId) && /[1-9]/.test(artistId) ? artistId : DEFAULT_CONFIG.netease_artist_id
  if (migrated.audio_quality === "higher") migrated.audio_quality = "exhigh"
  migrated.audio_quality = AUDIO_QUALITIES.has(migrated.audio_quality)
    ? migrated.audio_quality
    : DEFAULT_CONFIG.audio_quality
  if (![";", "/", ",", "、", " & "].includes(migrated.artist_separator)) migrated.artist_separator = ";"
  return migrated
}

async function readAndMigrate() {
  const store = await getStore()
  const existing = await store.get("app_config")
  const migrated = migrateConfig(existing)
  await store.set("app_config", migrated)
  return migrated
}

export async function initConfig() {
  await readAndMigrate()
}

export async function getConfig() {
  return readAndMigrate()
}

export async function saveConfig(config) {
  const store = await getStore()
  const nextConfig = migrateConfig(config)
  await store.set("app_config", nextConfig)
  window.dispatchEvent(new CustomEvent(CONFIG_CHANGED_EVENT, { detail: nextConfig }))
}

export async function resetConfig() {
  await saveConfig(DEFAULT_CONFIG)
}
