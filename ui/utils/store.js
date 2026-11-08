import { invoke } from "@tauri-apps/api/core"
import { listen } from "@tauri-apps/api/event"
import { create } from "zustand"
import packageJson from "#/package.json"
import { getConfig } from "@/utils/config"
import { safeNeteaseErrorCode, translate } from "@/utils/i18n"
import { isInstrumental } from "@/utils/library"
import { extractYears } from "@/utils/year"

const pendingStates = new Set(["queued", "downloading", "processing", "cancelling"])
export const isPendingDownload = (item) => pendingStates.has(item?.state)
export const useAppStore = create((set, get) => ({
  hasUpdate: false,
  songList: null,
  years: [],
  status: null,
  loading: false,
  isDownloading: false,
  neteaseLoggedIn: false,
  neteaseAuthRevision: 0,
  activeBatchId: null,
  downloadIds: [],
  downloadItems: {},
  downloadsOpen: false,
  setDownloadsOpen: (downloadsOpen) => set({ downloadsOpen }),
  setNeteaseLoggedIn: (neteaseLoggedIn) =>
    set((state) => ({ neteaseLoggedIn: neteaseLoggedIn === true, neteaseAuthRevision: state.neteaseAuthRevision + 1 })),
  refreshNeteaseAuthStatus: async () => {
    const revision = get().neteaseAuthRevision
    try {
      const status = await invoke("netease_auth_status")
      if (get().neteaseAuthRevision !== revision) return get().neteaseLoggedIn
      const loggedIn = status?.logged_in === true
      set({ neteaseLoggedIn: loggedIn })
      return loggedIn
    } catch {
      if (get().neteaseAuthRevision !== revision) return get().neteaseLoggedIn
      set({ neteaseLoggedIn: false })
      return false
    }
  },
  patchDownload: (id, patch) =>
    set((state) => {
      const item = state.downloadItems[id]
      if (!item) return {}
      return { downloadItems: { ...state.downloadItems, [id]: { ...item, ...patch } } }
    }),
  clearDownloadHistory: () =>
    set((state) => {
      const downloadIds = state.downloadIds.filter((id) => isPendingDownload(state.downloadItems[id]))
      return { downloadIds, downloadItems: Object.fromEntries(downloadIds.map((id) => [id, state.downloadItems[id]])) }
    }),
  cancelDownloads: async (songId) => {
    const { activeBatchId, downloadItems, downloadIds } = get()
    if (!activeBatchId) return
    const ids = downloadIds.filter(
      (id) =>
        (!songId || id === songId) && isPendingDownload(downloadItems[id]) && downloadItems[id].state !== "cancelling",
    )
    if (!ids.length) return
    set((state) => ({
      downloadItems: {
        ...state.downloadItems,
        ...Object.fromEntries(ids.map((id) => [id, { ...state.downloadItems[id], state: "cancelling" }])),
      },
    }))
    try {
      await invoke("cancel_downloads", { batchId: activeBatchId, songIds: songId ? ids : null })
    } catch (error) {
      if (get().activeBatchId !== activeBatchId) return
      set((state) => ({
        downloadItems: {
          ...state.downloadItems,
          ...Object.fromEntries(
            ids.filter((id) => state.downloadItems[id]?.state === "cancelling").map((id) => [id, downloadItems[id]]),
          ),
        },
        status: {
          type: "error",
          message: `${translate(get().downloadLanguage || "zh-Hans", "cancelFailed")}: ${String(error)}`,
        },
      }))
    }
  },

  initAppData: async () => {
    get().refreshNeteaseAuthStatus()
    invoke("load_latest_version")
      .then((latestVersion) => {
        if (latestVersion && latestVersion.trim() !== packageJson.version.trim()) set({ hasUpdate: true })
      })
      .catch((error) => console.error("检查更新失败:", error))
    const config = await getConfig()
    const language = config.language || "zh-Hans"
    const localSongs = await invoke("load_local_music").catch(() => null)
    if (!localSongs?.length) {
      set({ status: { type: "default", message: translate(language, "emptyStatus") } })
      return
    }
    set({
      songList: localSongs,
      years: extractYears(localSongs),
      status: { type: "default", message: translate(language, "loaded", { count: localSongs.length }) },
    })
  },
  refreshSongList: async () => {
    if (get().loading || get().isDownloading) return null
    const language = (await getConfig()).language || "zh-Hans"
    set({ loading: true, status: { type: "default", message: translate(language, "refreshing") } })
    try {
      const result = await invoke("refresh_music_list")
      set({
        songList: result,
        years: extractYears(result),
        status: { type: "default", message: translate(language, "refreshed", { count: result.length }) },
      })
      return result
    } catch (error) {
      set({ status: { type: "error", message: translate(language, safeNeteaseErrorCode(error) || "operationFailed") } })
      return null
    } finally {
      set({ loading: false })
    }
  },
  reloadLocalMusic: async () => {
    const songs = await invoke("load_local_music")
    set({ songList: songs, years: extractYears(songs) })
    return songs
  },
  updateSongStatus: (songId, downloaded, quality) =>
    set((state) => ({
      songList:
        state.songList?.map((song) =>
          song.id === songId
            ? { ...song, download: downloaded, downloaded_quality: quality ?? song.downloaded_quality }
            : song,
        ) ?? state.songList,
    })),
  setAllSongStatuses: (downloaded) =>
    set((state) => ({
      songList: state.songList?.map((song) => ({ ...song, download: downloaded })) ?? state.songList,
    })),

  downloadSongs: async (targets) => {
    if (get().neteaseLoggedIn !== true || get().isDownloading || get().loading) return null
    const batchId = crypto.randomUUID()
    const successSongIds = []
    const failures = []
    const unlisteners = []
    let failureLogPath = ""
    let language = "zh-Hans"
    let progressTimer
    let progressUpdates = new Map()
    set({ isDownloading: true, activeBatchId: batchId, downloadsOpen: true })
    const flushProgress = () => {
      if (!progressUpdates.size) return
      const updates = progressUpdates
      progressUpdates = new Map()
      set((state) => {
        const items = { ...state.downloadItems }
        for (const [id, payload] of updates) {
          if (!isPendingDownload(items[id]) || items[id].state === "cancelling") continue
          items[id] = {
            ...items[id],
            progress: Math.max(0, Math.min(99, Number(payload.progress) || 0)),
            state: payload.progress >= 100 ? "processing" : "downloading",
          }
        }
        return { downloadItems: items }
      })
    }
    try {
      const config = await getConfig()
      language = config.language || "zh-Hans"
      set({ downloadLanguage: language })
      const songs = [
        ...new Map(
          (targets || (get().songList || []).filter((song) => !song.download))
            .filter((song) => config.download_instrumental || !isInstrumental(song))
            .map((song) => [song.id, song]),
        ).values(),
      ]
      if (!songs.length) {
        set({ status: { type: "default", message: translate(language, "nothingToDownload") } })
        return { successCount: 0, failCount: 0, cancelledCount: 0, successSongIds }
      }
      set({
        status: { type: "default", message: translate(language, "taskDownloading") },
        downloadIds: songs.map((song) => song.id),
        downloadItems: Object.fromEntries(
          songs.map((song) => [
            song.id,
            { song, batchId, state: "queued", progress: 0, quality: "", error: "", logPath: "" },
          ]),
        ),
      })
      unlisteners.push(
        await listen("download-progress", ({ payload }) => {
          if (payload.batch_id !== batchId) return
          const { song_id: id, stage, quality, error, log_path, log_error } = payload
          const item = get().downloadItems[id]
          if (!item) return
          if (stage === "start" || stage === "quality") {
            if (item.state !== "cancelling")
              get().patchDownload(id, { state: "downloading", ...(quality ? { quality } : {}) })
          } else if (stage === "processing") {
            progressUpdates.delete(id)
            if (item.state !== "cancelling") get().patchDownload(id, { state: "processing", progress: 99 })
          } else if (stage === "complete" && payload.success) {
            progressUpdates.delete(id)
            if (!successSongIds.includes(id)) successSongIds.push(id)
            get().patchDownload(id, { state: "completed", progress: 100, quality })
            get().updateSongStatus(id, true, quality)
          } else if (stage === "cancelled") {
            progressUpdates.delete(id)
            get().patchDownload(id, { state: "cancelled", progress: 0 })
          } else if (stage === "error") {
            progressUpdates.delete(id)
            if (log_path) failureLogPath = log_path
            const reason = translate(language, safeNeteaseErrorCode(error) || "operationFailed")
            failures.push(`${item.song.title}: ${reason}${log_error ? ` · ${log_error}` : ""}`)
            get().patchDownload(id, {
              state: "failed",
              error: reason,
              logPath: log_path || "",
              logError: log_error || "",
            })
          }
        }),
      )
      unlisteners.push(
        await listen("download-single-progress", ({ payload }) => {
          if (payload.batch_id !== batchId || !["download", "start"].includes(payload.stage)) return
          progressUpdates.set(payload.song_id, payload)
          if (!progressTimer)
            progressTimer = setTimeout(() => {
              progressTimer = undefined
              flushProgress()
            }, 120)
        }),
      )
      const [successCount, failCount, cancelledCount = 0] = await invoke("download_music", {
        batchId,
        songIds: songs.map((song) => song.id),
      })
      set({
        status: {
          type: failCount ? "warning" : "default",
          message: [
            translate(language, "downloadDone", { success: successCount, failed: failCount }),
            cancelledCount ? translate(language, "cancelledCount", { count: cancelledCount }) : "",
            failureLogPath,
          ]
            .filter(Boolean)
            .join(" · "),
        },
      })
      return { successCount, failCount, cancelledCount, successSongIds }
    } catch (error) {
      const reason = translate(language, safeNeteaseErrorCode(error) || "operationFailed")
      set((state) => ({
        downloadItems: Object.fromEntries(
          Object.entries(state.downloadItems).map(([id, item]) => [
            id,
            isPendingDownload(item) ? { ...item, state: "failed", error: reason } : item,
          ]),
        ),
        status: { type: "error", message: [failures.at(-1) || reason, failureLogPath].filter(Boolean).join(" · ") },
      }))
      return null
    } finally {
      clearTimeout(progressTimer)
      flushProgress()
      await get()
        .reloadLocalMusic()
        .catch(() => {})
      for (const unlisten of unlisteners) unlisten()
      set({ isDownloading: false, activeBatchId: null })
    }
  },
}))
