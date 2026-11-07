import { Dialog, DialogBackdrop, DialogPanel, DialogTitle } from "@headlessui/react"
import { Check, Download, ListMusic, LoaderCircle, Trash2, X } from "lucide-react"
import { Virtuoso } from "react-virtuoso"
import { Button } from "@/components/button"
import { AlbumCover } from "@/components/cover"
import { cn } from "@/utils/cn"
import { useI18n } from "@/utils/i18n"
import { isPendingDownload, useAppStore } from "@/utils/store"

const qualityKeys = {
  hires: "qualityHires",
  lossless: "qualityLossless",
  exhigh: "qualityExhigh",
  standard: "qualityStandard",
}
const stateKeys = {
  queued: "taskQueued",
  downloading: "taskDownloading",
  processing: "taskProcessing",
  cancelling: "taskCancelling",
  completed: "taskCompleted",
  failed: "taskFailed",
  cancelled: "taskCancelled",
}

export function DownloadLauncher() {
  const { t } = useI18n()
  const ids = useAppStore((state) => state.downloadIds)
  const items = useAppStore((state) => state.downloadItems)
  const status = useAppStore((state) => state.status)
  const setOpen = useAppStore((state) => state.setDownloadsOpen)
  const finished = ids.filter((id) => !isPendingDownload(items[id])).length
  const running = ids.some((id) => isPendingDownload(items[id]))
  return (
    <div className="flex flex-1 items-center gap-3 min-w-0 py-1">
      <button
        type="button"
        onClick={() => setOpen(true)}
        aria-label={t("downloadQueue")}
        title={t("downloadQueue")}
        className="flex-center size-10 shrink-0 border border-secondary bg-background/70 cursor-pointer transition-colors hover:border-secondary/70 hover:text-primary/70"
      >
        {running ? <LoaderCircle className="size-4 animate-spin" /> : <ListMusic className="size-4" />}
      </button>
      <span className="min-w-0 flex-1">
        <span className="flex items-center gap-2 text-sm font-medium">
          {t("downloadQueue")}
          {ids.length > 0 && (
            <span className="text-xs text-secondary tabular-nums">
              {finished} / {ids.length}
            </span>
          )}
        </span>
        <span
          className={cn(
            "block truncate text-xs text-secondary mt-0.5",
            status?.type === "error" && "text-red-400",
            status?.type === "warning" && "text-amber-400",
          )}
        >
          {status?.message || t("queueEmptyHint")}
        </span>
      </span>
    </div>
  )
}

function DownloadRow({ id }) {
  const { t } = useI18n()
  const item = useAppStore((state) => state.downloadItems[id])
  const cancel = useAppStore((state) => state.cancelDownloads)
  if (!item) return null
  const active = isPendingDownload(item)
  return (
    <article className="flex gap-4 py-4 px-6 border-b border-border/50">
      <AlbumCover song={item.song} className="size-14 shrink-0" />
      <div className="min-w-0 flex-1">
        <div className="flex items-start gap-3">
          <div className="min-w-0 flex-1">
            <p className="truncate font-medium" title={item.song.title}>
              {item.song.title}
            </p>
            <p
              className="truncate text-xs text-secondary mt-1"
              title={`${item.song.artist} · ${item.song.album_title}`}
            >
              {item.song.artist} · {item.song.album_title}
            </p>
          </div>
          <span
            className={cn(
              "shrink-0 text-xs whitespace-nowrap pt-0.5",
              item.state === "failed"
                ? "text-red-400"
                : item.state === "completed"
                  ? "text-lime-300"
                  : "text-secondary",
            )}
          >
            {t(stateKeys[item.state])}
          </span>
        </div>
        <div className="flex items-center gap-3 mt-3">
          <div
            role="progressbar"
            aria-label={item.song.title}
            aria-valuemin={0}
            aria-valuemax={100}
            aria-valuenow={Math.round(item.progress)}
            className="h-1.5 bg-border/50 overflow-hidden flex-1"
          >
            <div
              className={cn(
                "h-full transition-[width] duration-150",
                item.state === "failed" ? "bg-red-400" : item.state === "cancelled" ? "bg-muted" : "bg-lime-400",
              )}
              style={{ width: `${item.progress}%` }}
            />
          </div>
          <span className="text-[11px] text-secondary tabular-nums shrink-0">{Math.round(item.progress)}%</span>
          {item.quality && (
            <span className="text-[11px] text-secondary shrink-0">
              {t(qualityKeys[item.quality] || "audioQuality")}
            </span>
          )}
        </div>
        {item.error && <p className="mt-2 text-xs text-red-300 select-text break-words">{item.error}</p>}
        {item.logPath && (
          <p className="mt-1 text-[11px] text-secondary select-text break-all">
            {t("errorLog")}: {item.logPath}
          </p>
        )}
        {item.logError && <p className="mt-1 text-xs text-amber-400 select-text break-words">{item.logError}</p>}
      </div>
      {active ? (
        <button
          type="button"
          aria-label={`${t("cancelTask")} · ${item.song.title}`}
          title={t("cancelTask")}
          disabled={item.state === "cancelling"}
          onClick={() => cancel(id)}
          className="flex-center size-7 shrink-0 hover:bg-red-400/10 text-secondary hover:text-red-400 disabled:opacity-30 cursor-pointer"
        >
          <X className="size-4" />
        </button>
      ) : (
        <span className="flex-center size-7 shrink-0 text-lime-400">
          {item.state === "completed" && <Check className="size-4" />}
        </span>
      )}
    </article>
  )
}

export function DownloadPanel() {
  const { t } = useI18n()
  const open = useAppStore((state) => state.downloadsOpen)
  const setOpen = useAppStore((state) => state.setDownloadsOpen)
  const ids = useAppStore((state) => state.downloadIds)
  const items = useAppStore((state) => state.downloadItems)
  const status = useAppStore((state) => state.status)
  const cancel = useAppStore((state) => state.cancelDownloads)
  const clear = useAppStore((state) => state.clearDownloadHistory)
  const pending = ids.filter((id) => isPendingDownload(items[id]))
  const done = ids.filter((id) => items[id]?.state === "completed").length
  const allCancelling = pending.length > 0 && pending.every((id) => items[id]?.state === "cancelling")
  return (
    <Dialog open={open} onClose={() => setOpen(false)} className="relative z-70">
      <DialogBackdrop className="fixed inset-0 bg-black/65" />
      <div className="fixed inset-0 flex items-end sm:items-center justify-center p-4 sm:p-8">
        <DialogPanel className="w-full max-w-3xl border bg-background shadow-2xl flex flex-col h-[min(76vh,720px)] overflow-hidden">
          <div className="px-6 pt-5 pb-4 border-b flex items-center gap-4">
            <span className="flex-center size-10 bg-lime-400/10 text-lime-300">
              <Download className="size-5" />
            </span>
            <div className="flex-1 min-w-0">
              <DialogTitle className="font-semibold text-lg">{t("downloadQueue")}</DialogTitle>
              <p className="text-xs text-secondary mt-0.5">
                {t("queueSummary")
                  .replace("{completed}", done)
                  .replace("{pending}", pending.length)
                  .replace("{total}", ids.length)}
              </p>
            </div>
            <button
              type="button"
              aria-label={t("close")}
              onClick={() => setOpen(false)}
              className="p-2 text-secondary hover:text-primary cursor-pointer"
            >
              <X className="size-5" />
            </button>
          </div>
          {ids.length ? (
            <Virtuoso
              className="flex-1 min-h-0"
              data={ids}
              computeItemKey={(_, id) => id}
              itemContent={(_, id) => <DownloadRow id={id} />}
            />
          ) : (
            <div className="flex-1 flex-center flex-col gap-3 text-secondary">
              <ListMusic className="size-10 opacity-40" />
              <span>{t("queueEmpty")}</span>
              <span className="text-xs">{t("queueEmptyHint")}</span>
            </div>
          )}
          {status && (
            <p
              role="status"
              className={cn(
                "px-6 py-3 text-xs border-t break-words select-text max-h-24 overflow-y-auto",
                status.type === "error"
                  ? "text-red-400"
                  : status.type === "warning"
                    ? "text-amber-400"
                    : "text-secondary",
              )}
            >
              {status.message}
            </p>
          )}
          <div className="px-6 py-4 border-t flex justify-between items-center gap-4">
            <button
              type="button"
              onClick={clear}
              disabled={!ids.some((id) => !isPendingDownload(items[id]))}
              className="flex items-center gap-2 text-xs text-secondary hover:text-primary disabled:opacity-30 cursor-pointer"
            >
              <Trash2 className="size-3.5" />
              {t("clearFinished")}
            </button>
            <Button
              type="button"
              disabled={!pending.length || allCancelling}
              onClick={() => cancel()}
              className="text-red-300 border-red-400/40 hover:border-red-400"
            >
              {allCancelling ? t("taskCancelling") : t("cancelAllDownloads")}
            </Button>
          </div>
        </DialogPanel>
      </div>
    </Dialog>
  )
}
