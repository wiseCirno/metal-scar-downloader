import { Disc3, ImageOff } from "lucide-react"
import { memo, useEffect, useState } from "react"
import { cn } from "@/utils/cn"
import { useI18n } from "@/utils/i18n"

const MAX_LOADING = 4
const MAX_CACHED = 160
const covers = new Map()
let waiting = []
let loading = 0

export function thumbnailUrl(value, size = 160) {
  if (!value) return ""
  const source = value.replaceAll("{w}", String(size)).replaceAll("{h}", String(size))
  try {
    const url = new URL(source)
    if (url.protocol !== "https:") return ""
    if (url.hostname.endsWith(".music.126.net")) url.searchParams.set("param", `${size}y${size}`)
    return url.href
  } catch {
    return ""
  }
}
function prune() {
  for (const [src, item] of covers) {
    if (covers.size <= MAX_CACHED) break
    if (!item.listeners.size && ["loaded", "failed"].includes(item.state)) covers.delete(src)
  }
}
function pump() {
  while (loading < MAX_LOADING && waiting.length) {
    const item = waiting.shift()
    if (covers.get(item.src) !== item || !item.listeners.size) continue
    loading += 1
    item.state = "loading"
    const image = new Image()
    item.image = image
    image.decoding = "async"
    const finish = (state) => {
      if (item.state !== "loading") return
      image.onload = image.onerror = null
      item.state = state
      loading -= 1
      for (const listener of item.listeners) listener({ src: item.src, state })
      prune()
      pump()
    }
    image.onload = () =>
      Promise.resolve(image.decode?.())
        .catch(() => {})
        .then(() => finish("loaded"))
    image.onerror = () => finish("failed")
    image.src = item.src
  }
}
function subscribeCover(src, listener) {
  let item = covers.get(src)
  if (!item) {
    item = { src, state: "queued", listeners: new Set(), image: null }
    covers.set(src, item)
    waiting.push(item)
  } else {
    covers.delete(src)
    covers.set(src, item)
  }
  item.listeners.add(listener)
  listener({ src, state: item.state })
  pump()
  return () => {
    item.listeners.delete(listener)
    if (!item.listeners.size && ["queued", "loading"].includes(item.state)) {
      if (item.state === "loading") {
        item.image.onload = item.image.onerror = null
        item.image.src = ""
        loading -= 1
      }
      item.state = "cancelled"
      covers.delete(src)
      waiting = waiting.filter((entry) => entry !== item)
      pump()
    }
    prune()
  }
}

export const AlbumCover = memo(
  function AlbumCover({ song, className, size = 160 }) {
    const { t } = useI18n()
    const src = thumbnailUrl(song?.artwork_url, size)
    const [result, setResult] = useState(null)
    useEffect(() => {
      if (!src) return undefined
      return subscribeCover(src, setResult)
    }, [src])
    const state = result?.src === src ? result.state : covers.get(src)?.state
    if (!src || state !== "loaded") {
      const failed = src && state === "failed"
      const Icon = failed ? ImageOff : Disc3
      return (
        <div
          aria-hidden={!failed}
          className={cn("flex-center flex-col gap-1 bg-muted/15 overflow-hidden", className)}
          title={failed ? t("coverLoadFailed") : undefined}
        >
          <Icon strokeWidth={1.25} className="size-5 text-muted" />
        </div>
      )
    }
    return (
      <img
        src={src}
        alt={song?.album_title || song?.title || ""}
        width={size}
        height={size}
        decoding="async"
        loading="lazy"
        className={cn("object-cover", className)}
        draggable="false"
      />
    )
  },
  (a, b) =>
    a.song?.artwork_url === b.song?.artwork_url &&
    a.song?.album_title === b.song?.album_title &&
    a.song?.title === b.song?.title &&
    a.className === b.className &&
    a.size === b.size,
)
