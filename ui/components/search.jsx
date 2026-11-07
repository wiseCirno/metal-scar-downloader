import { Search, X } from "lucide-react"
import { useDeferredValue, useEffect, useId, useMemo, useRef, useState } from "react"
import { useLocation, useNavigate } from "react-router-dom"
import { cn } from "@/utils/cn"
import { CONFIG_CHANGED_EVENT, getConfig } from "@/utils/config"
import { useI18n } from "@/utils/i18n"
import { createSearchIndex, isInstrumental, searchLibrary } from "@/utils/library"
import { useAppStore } from "@/utils/store"

const typeKeys = { song: "searchSongs", album: "searchAlbums", artist: "searchArtists" }

export function LibrarySearch() {
  const { t } = useI18n()
  const songList = useAppStore((state) => state.songList)
  const [includeInstrumental, setIncludeInstrumental] = useState(false)
  const [query, setQuery] = useState("")
  const deferredQuery = useDeferredValue(query)
  const [open, setOpen] = useState(false)
  const [active, setActive] = useState(-1)
  const root = useRef(null)
  const input = useRef(null)
  const listId = useId()
  const navigate = useNavigate()
  const location = useLocation()
  useEffect(() => {
    let mounted = true
    let changed = false
    const onConfig = (event) => {
      changed = true
      setIncludeInstrumental(event.detail.download_instrumental === true)
    }
    window.addEventListener(CONFIG_CHANGED_EVENT, onConfig)
    getConfig()
      .then((config) => {
        if (mounted && !changed) setIncludeInstrumental(config.download_instrumental === true)
      })
      .catch(() => {})
    return () => {
      mounted = false
      window.removeEventListener(CONFIG_CHANGED_EVENT, onConfig)
    }
  }, [])
  useEffect(() => {
    const close = (event) => {
      if (!root.current?.contains(event.target)) setOpen(false)
    }
    document.addEventListener("pointerdown", close)
    return () => document.removeEventListener("pointerdown", close)
  }, [])
  useEffect(() => {
    if (location.key) setOpen(false)
  }, [location.key])
  const index = useMemo(
    () => createSearchIndex((songList || []).filter((song) => includeInstrumental || !isInstrumental(song))),
    [songList, includeInstrumental],
  )
  const results = useMemo(() => searchLibrary(index, deferredQuery), [index, deferredQuery])
  const showResults = open && query.trim().length > 0
  const choose = (result) => {
    navigate(`/?${new URLSearchParams({ [result.type]: result.id })}`)
    setOpen(false)
    input.current?.blur()
  }
  const handleKey = (event) => {
    if (event.nativeEvent.isComposing) return
    if (event.key === "Escape") {
      event.preventDefault()
      setOpen(false)
    } else if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault()
      setOpen(true)
      setActive((current) =>
        results.length
          ? current < 0
            ? event.key === "ArrowDown"
              ? 0
              : results.length - 1
            : (current + (event.key === "ArrowDown" ? 1 : -1) + results.length) % results.length
          : -1,
      )
    } else if (event.key === "Enter" && showResults && results.length) {
      event.preventDefault()
      choose(results[active < 0 ? 0 : active] || results[0])
    }
  }
  useEffect(() => {
    if (active >= 0 && showResults) document.getElementById(`${listId}-${active}`)?.scrollIntoView({ block: "nearest" })
  }, [active, showResults, listId])
  return (
    <div ref={root} className="relative flex-1 min-w-36 max-w-xl">
      <div className="flex items-center gap-2 border bg-background/60 px-3 focus-within:border-primary/60">
        <Search size={16} className="shrink-0 text-secondary" aria-hidden="true" />
        <input
          ref={input}
          role="combobox"
          aria-label={t("searchPlaceholder")}
          aria-expanded={showResults}
          aria-controls={listId}
          aria-autocomplete="list"
          aria-activedescendant={showResults && results[active] ? `${listId}-${active}` : undefined}
          value={query}
          placeholder={t("searchPlaceholder")}
          className="min-w-0 w-full py-2.5 bg-transparent text-sm"
          onChange={(event) => {
            setQuery(event.target.value)
            setActive(-1)
            setOpen(true)
          }}
          onFocus={() => setOpen(true)}
          onKeyDown={handleKey}
        />
        {query && (
          <button
            type="button"
            aria-label={t("searchClear")}
            title={t("searchClear")}
            className="text-secondary hover:text-primary"
            onClick={() => {
              setQuery("")
              setActive(-1)
              input.current?.focus()
            }}
          >
            <X size={15} />
          </button>
        )}
      </div>
      {showResults && (
        <div className="absolute top-full left-0 right-0 z-50 mt-2 border bg-background shadow-2xl overflow-hidden">
          <div id={listId} role="listbox" aria-label={t("searchResults")} className="max-h-80 overflow-y-auto p-1">
            {!results.length && (
              <p role="status" className="px-3 py-5 text-secondary">
                {t("searchNoResults")}
              </p>
            )}
            {results.map((result, index) => (
              <div key={`${result.type}:${result.id}`}>
                {(index === 0 || results[index - 1].type !== result.type) && (
                  <p className="px-3 pt-3 pb-1 text-xs text-secondary">{t(typeKeys[result.type])}</p>
                )}
                <button
                  id={`${listId}-${index}`}
                  type="button"
                  role="option"
                  aria-selected={index === active}
                  className={cn("w-full text-left px-3 py-2 hover:bg-primary/10", index === active && "bg-primary/10")}
                  onPointerMove={() => setActive(index)}
                  onClick={() => choose(result)}
                >
                  <span className="block truncate">{result.title || t("unknownAlbum")}</span>
                  {result.subtitle && (
                    <span className="block truncate text-xs text-secondary mt-0.5">{result.subtitle}</span>
                  )}
                </button>
              </div>
            ))}
          </div>
          {!includeInstrumental && (
            <p className="border-t px-3 py-2 text-xs text-secondary">{t("searchInstrumentalHidden")}</p>
          )}
        </div>
      )}
    </div>
  )
}
