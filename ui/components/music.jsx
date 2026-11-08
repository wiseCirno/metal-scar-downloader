import { invoke } from "@tauri-apps/api/core"
import { openUrl } from "@tauri-apps/plugin-opener"
import { memo } from "react"
import { AlbumCover } from "@/components/cover"
import { DropdownMenu } from "@/components/menu"
import { cn } from "@/utils/cn"
import { useI18n } from "@/utils/i18n"
import { useAppStore } from "@/utils/store"

function isNeteaseMusicUrl(value) {
  try {
    const url = new URL(value)
    return url.protocol === "https:" && url.hostname === "music.163.com"
  } catch {
    return false
  }
}

export const MusicItem = memo(function MusicItem({ song, showCover, selected, onSelect }) {
  const { t } = useI18n()
  const loading = useAppStore((state) => state.loading)
  const isDownloading = useAppStore((state) => state.isDownloading)
  const neteaseLoggedIn = useAppStore((state) => state.neteaseLoggedIn)
  const downloadSongs = useAppStore((state) => state.downloadSongs)
  const updateSongStatus = useAppStore((state) => state.updateSongStatus)
  const task = useAppStore((state) => state.downloadItems[song.id])
  const progress = task && ["downloading", "processing", "cancelling"].includes(task.state) ? task.progress : null

  const setDownloaded = async (downloaded) => {
    await invoke("set_download_status", { songId: song.id, downloaded })
    updateSongStatus(song.id, downloaded)
  }
  const select = (event) => {
    if (!event.target.closest("button, [role='menu']")) onSelect(song.id, event)
  }
  const handleKeyDown = (event) => {
    if ((event.key === "Enter" || event.key === " ") && !event.target.closest("button, [role='menu']")) {
      event.preventDefault()
      onSelect(song.id, event)
    }
  }

  return (
    <div
      role="option"
      tabIndex={0}
      aria-selected={selected}
      className={cn(
        "relative flex items-center gap-4 h-fit p-4 border bg-background/35 transition-colors [&>button]:shrink-0",
        selected && "border-lime-400/70 bg-lime-400/5",
      )}
      onClick={select}
      onKeyDown={handleKeyDown}
    >
      {showCover && <AlbumCover song={song} className="size-20 shrink-0" />}
      <span className="flex-1 flex flex-col gap-1 min-w-0">
        <span className="text-base font-semibold wrap-anywhere" title={song.title}>
          {song.title}
        </span>
        {song.download && song.downloaded_quality && (
          <span className="text-xs text-lime-400">
            {t(
              {
                standard: "qualityStandard",
                higher: "qualityHigher",
                exhigh: "qualityExhigh",
                lossless: "qualityLossless",
                hires: "qualityHires",
              }[song.downloaded_quality] || "audioQuality",
            )}
          </span>
        )}
        <span className="flex flex-wrap items-center gap-x-3 gap-y-1 text-secondary">
          {song.vip && (
            <span className="shrink-0 whitespace-nowrap text-amber-400" title={t("vipHint")}>
              {t("vipMark")}
            </span>
          )}
          {(song.instrumental || /instrumental/i.test(song.title || "")) && (
            <span className="shrink-0 whitespace-nowrap text-amber-400">{t("instrumentalMark")}</span>
          )}
          <span className="shrink-0 whitespace-nowrap">{song.publish}</span>
          <span className="w-px h-3 shrink-0 bg-secondary rotate-16" aria-hidden="true" />
          <span className="min-w-0 flex-1 basis-24 truncate" title={song.artist || song.album_title}>
            {song.artist || song.album_title}
          </span>
        </span>
      </span>
      {song.download && (
        <div className="absolute top-0 right-0 border-t-10 border-l-10 border-t-lime-400 border-l-transparent pointer-events-none" />
      )}
      {progress !== null && (
        <div className="absolute left-0 bottom-0 h-0.75 bg-lime-400 transition" style={{ width: `${progress}%` }} />
      )}
      <DropdownMenu
        items={[
          {
            label: song.download ? t("redownload") : t("download"),
            disabled: neteaseLoggedIn !== true || loading || isDownloading,
            onClick: () => downloadSongs([song]),
          },
          {
            label: song.download ? t("markPending") : t("markDownloaded"),
            onClick: () => setDownloaded(!song.download),
          },
          {
            label: t("openTrack"),
            disabled: !isNeteaseMusicUrl(song.music_url),
            onClick: () => openUrl(song.music_url),
          },
        ]}
      />
    </div>
  )
})
