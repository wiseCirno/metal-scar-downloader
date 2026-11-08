import { confirm, message } from "@tauri-apps/plugin-dialog"
import { openUrl } from "@tauri-apps/plugin-opener"
import { useCallback, useEffect, useMemo, useRef, useState } from "react"
import { useLocation, useSearchParams } from "react-router-dom"
import { VirtuosoGrid } from "react-virtuoso"
import { Button } from "@/components/button"
import { AlbumCover } from "@/components/cover"
import { DownloadLauncher } from "@/components/downloads"
import { MusicItem } from "@/components/music"
import { Tab } from "@/components/tab"
import { cn } from "@/utils/cn"
import { CONFIG_CHANGED_EVENT, getConfig } from "@/utils/config"
import { useI18n } from "@/utils/i18n"
import { albumKey, compareSongs, groupAlbums, isInstrumental, songArtists } from "@/utils/library"
import { useAppStore } from "@/utils/store"
import { extractYears } from "@/utils/year"

function blocksSelectAll(target) {
  return (
    target instanceof Element &&
    (target.closest("input, textarea, select, [role='textbox']") || target.isContentEditable)
  )
}

export function HomePage() {
  const { t } = useI18n()
  const hasUpdate = useAppStore((state) => state.hasUpdate)
  const songList = useAppStore((state) => state.songList)
  const loading = useAppStore((state) => state.loading)
  const isDownloading = useAppStore((state) => state.isDownloading)
  const neteaseLoggedIn = useAppStore((state) => state.neteaseLoggedIn)
  const refreshSongList = useAppStore((state) => state.refreshSongList)
  const downloadSongs = useAppStore((state) => state.downloadSongs)
  const [params, setParams] = useSearchParams()
  const location = useLocation()
  const [selectedYear, setSelectedYear] = useState("all")
  const [selectedFilter, setSelectedFilter] = useState("all")
  const [view, setView] = useState("songs")
  const albumId = params.get("album")
  const artist = params.get("artist")
  const targetId = params.get("song")
  const albumGrid = view === "albums" && !albumId
  const [showCover, setShowCover] = useState(true)
  const [includeInstrumental, setIncludeInstrumental] = useState(false)
  const [selectedSongIds, setSelectedSongIds] = useState([])
  const [isConfirmingDownload, setIsConfirmingDownload] = useState(false)
  const anchor = useRef(null)
  const handledLocation = useRef(null)
  useEffect(() => {
    let active = true
    let changed = false
    const applyConfig = (config) => {
      setShowCover(config.show_cover)
      setIncludeInstrumental(config.download_instrumental === true)
    }
    const onConfigChanged = (event) => {
      changed = true
      applyConfig(event.detail)
    }
    window.addEventListener(CONFIG_CHANGED_EVENT, onConfigChanged)
    getConfig()
      .then((config) => {
        if (active && !changed) applyConfig(config)
      })
      .catch(() => {})
    return () => {
      active = false
      window.removeEventListener(CONFIG_CHANGED_EVENT, onConfigChanged)
    }
  }, [])
  useEffect(() => {
    if (handledLocation.current === location.key) return
    handledLocation.current = location.key
    if (targetId || albumId || artist) {
      if (!location.state?.preserveLibraryFilters) {
        setSelectedYear("all")
        setSelectedFilter("all")
      }
      setView(albumId ? "albums" : "songs")
      setSelectedSongIds([])
      anchor.current = null
    }
  }, [location.key, location.state?.preserveLibraryFilters, targetId, albumId, artist])
  const visibleSongs = useMemo(
    () => (songList || []).filter((song) => includeInstrumental || !isInstrumental(song)),
    [songList, includeInstrumental],
  )
  const years = useMemo(() => extractYears(visibleSongs), [visibleSongs])
  useEffect(() => {
    if (selectedYear !== "all" && !years.includes(selectedYear)) setSelectedYear("all")
  }, [years, selectedYear])
  const filteredSongs = useMemo(
    () =>
      visibleSongs
        .filter(
          (song) =>
            (!albumId || albumKey(song) === albumId) &&
            (!artist || songArtists(song).includes(artist)) &&
            (selectedYear === "all" || song.publish?.startsWith(selectedYear)) &&
            (selectedFilter !== "pending" || !song.download) &&
            (selectedFilter !== "downloaded" || song.download),
        )
        .sort(compareSongs),
    [visibleSongs, albumId, artist, selectedYear, selectedFilter],
  )
  const albums = useMemo(() => groupAlbums(filteredSongs), [filteredSongs])
  const currentAlbum = useMemo(
    () => (albumId ? groupAlbums(visibleSongs.filter((song) => albumKey(song) === albumId))[0] : null),
    [visibleSongs, albumId],
  )
  useEffect(() => {
    const ids = new Set(albumGrid ? [] : filteredSongs.map((song) => song.id))
    setSelectedSongIds((current) => current.filter((id) => ids.has(id)))
    if (!ids.has(anchor.current)) anchor.current = null
  }, [filteredSongs, albumGrid])
  const selectedSet = useMemo(() => new Set(selectedSongIds), [selectedSongIds])
  const selectedSongs = useMemo(
    () => (albumGrid ? [] : filteredSongs.filter((song) => selectedSet.has(song.id))),
    [filteredSongs, selectedSet, albumGrid],
  )
  useEffect(() => {
    const selectAll = (event) => {
      if (
        !(event.ctrlKey || event.metaKey) ||
        event.key.toLowerCase() !== "a" ||
        event.altKey ||
        albumGrid ||
        loading ||
        isDownloading ||
        isConfirmingDownload ||
        blocksSelectAll(event.target) ||
        blocksSelectAll(document.activeElement) ||
        document.querySelector('[role="dialog"], [role="alertdialog"], dialog[open], [aria-modal="true"]')
      )
        return
      event.preventDefault()
      setSelectedSongIds(filteredSongs.map((song) => song.id))
      anchor.current = filteredSongs[0]?.id ?? null
    }
    window.addEventListener("keydown", selectAll)
    return () => window.removeEventListener("keydown", selectAll)
  }, [filteredSongs, albumGrid, loading, isDownloading, isConfirmingDownload])
  const targetIndex = targetId ? filteredSongs.findIndex((song) => String(song.id) === targetId) : -1
  const selectSong = useCallback(
    (songId, event) => {
      if (loading || isDownloading) return
      if (event.shiftKey) {
        const a = filteredSongs.findIndex((song) => song.id === anchor.current)
        const b = filteredSongs.findIndex((song) => song.id === songId)
        if (a !== -1 && b !== -1) {
          const range = filteredSongs.slice(Math.min(a, b), Math.max(a, b) + 1).map((song) => song.id)
          setSelectedSongIds((current) =>
            event.ctrlKey || event.metaKey ? [...new Set([...current, ...range])] : range,
          )
          return
        }
      }
      anchor.current = songId
      setSelectedSongIds((current) =>
        !event.ctrlKey && !event.metaKey
          ? [songId]
          : current.includes(songId)
            ? current.filter((id) => id !== songId)
            : [...current, songId],
      )
    },
    [filteredSongs, loading, isDownloading],
  )
  const handleDownload = async (targets) => {
    setIsConfirmingDownload(true)
    try {
      const songs = targets || filteredSongs.filter((song) => !song.download)
      if (!songs.length) return await message(t("nothingToDownload"), { title: t("download"), okLabel: t("ok") })
      if (
        (!targets || songs.length > 1) &&
        !(await confirm(t("confirmDownload").replace("{count}", songs.length), {
          title: t("download"),
          kind: "warning",
          okLabel: t("confirm"),
          cancelLabel: t("cancel"),
        }))
      )
        return
      const result = await downloadSongs(songs)
      if (result?.successSongIds)
        setSelectedSongIds((current) => current.filter((id) => !result.successSongIds.includes(id)))
    } finally {
      setIsConfirmingDownload(false)
    }
  }
  const changeView = (next) => {
    setView(next)
    setParams({})
  }
  if (!songList)
    return (
      <div className="flex-center flex-col gap-2 h-full">
        <h2 className="text-2xl">{t("noData")}</h2>
        <span className="text-secondary pb-6">{t("emptyStatus")}</span>
        <Button onClick={refreshSongList} disabled={loading}>
          {t("getList")}
        </Button>
      </div>
    )
  return (
    <div className="flex flex-col h-full overflow-hidden">
      <div className="flex flex-col gap-4 mx-10 px-5 py-4 border border-t-0 backdrop-blur-2xs">
        <div className="flex items-center justify-between gap-4 flex-wrap">
          <Tab
            items={[
              { value: "songs", label: t("allSongs") },
              { value: "albums", label: t("byAlbum") },
            ]}
            value={view}
            onChange={changeView}
          />
          <Tab
            items={[{ value: "all", label: t("allYears") }, ...years.map((year) => ({ value: year, label: year }))]}
            value={selectedYear}
            onChange={setSelectedYear}
          />
          <Tab
            items={[
              { value: "all", label: t("allStatuses") },
              { value: "pending", label: t("pending") },
              { value: "downloaded", label: t("downloaded") },
            ]}
            value={selectedFilter}
            onChange={setSelectedFilter}
          />
        </div>
        {albumId && (
          <div className="flex items-center gap-4 min-w-0">
            <Button onClick={() => changeView("albums")}>{t("backToAlbums")}</Button>
            <span className="truncate font-semibold">{currentAlbum?.title || t("unknownAlbum")}</span>
            <span className="text-secondary truncate">{currentAlbum?.artists}</span>
          </div>
        )}
        {artist && (
          <div className="flex items-center gap-3">
            <span className="truncate">{t("artistFilter").replace("{artist}", artist)}</span>
            <Button onClick={() => setParams({})}>{t("clearFilter")}</Button>
          </div>
        )}
        {targetId && (
          <p role="status" className="text-xs text-secondary">
            {t(
              targetIndex >= 0
                ? "searchLocated"
                : !includeInstrumental && songList.some((song) => String(song.id) === targetId && isInstrumental(song))
                  ? "searchInstrumentalHidden"
                  : "searchTargetMissing",
            )}
          </p>
        )}
      </div>
      {!filteredSongs.length ? (
        <div className="flex-1 flex-center text-secondary">{t("noMatchingSongs")}</div>
      ) : albumGrid ? (
        <VirtuosoGrid
          key="albums"
          data={albums}
          computeItemKey={(_, album) => album.id}
          listClassName="grid grid-cols-[repeat(auto-fill,minmax(220px,1fr))] gap-4 my-4 pl-10 pr-8"
          style={{ flex: 1, minHeight: 0, overflowY: "scroll" }}
          itemContent={(_, album) => (
            <button
              type="button"
              className="w-full text-left p-4 border transition-colors bg-background/35"
              onClick={() => setParams({ album: album.id }, { state: { preserveLibraryFilters: true } })}
            >
              {showCover && <AlbumCover song={album.song} size={640} className="aspect-square w-full mb-3" />}
              <span className="block truncate font-semibold" title={album.title}>
                {album.title || t("unknownAlbum")}
              </span>
              <span className="block truncate text-secondary mt-1" title={album.artists}>
                {album.artists}
              </span>
              <span className="flex justify-between gap-2 text-xs text-secondary mt-3">
                <span>{album.publish}</span>
                <span>{t("albumSongCount").replace("{count}", album.songs.length)}</span>
              </span>
            </button>
          )}
        />
      ) : (
        <VirtuosoGrid
          key={`songs:${albumId || artist || "all"}:${targetId ? `${location.key}:${targetIndex}` : ""}`}
          initialTopMostItemIndex={targetIndex >= 0 ? { index: targetIndex, align: "center" } : 0}
          data={filteredSongs}
          computeItemKey={(_, song) => song.id}
          listClassName="grid grid-cols-[repeat(auto-fit,minmax(420px,1fr))] gap-2 my-2 pl-10 pr-8"
          style={{ flex: 1, minHeight: 0, overflowY: "scroll" }}
          itemContent={(_, song) => (
            <div
              className={cn(
                "transition-colors",
                String(song.id) === targetId && "ring-2 ring-lime-400 ring-inset bg-lime-400/10",
              )}
            >
              <MusicItem song={song} showCover={showCover} selected={selectedSet.has(song.id)} onSelect={selectSong} />
            </div>
          )}
        />
      )}
      <div className="flex-center justify-end gap-4 mx-10 py-5 border-t">
        <DownloadLauncher />
        {hasUpdate && (
          <Button
            onClick={() => openUrl("https://github.com/wiseCirno/metal-scar-downloader/releases/latest")}
            className="text-amber-400 border-amber-400/80"
          >
            {t("update")}
          </Button>
        )}
        <Button onClick={refreshSongList} disabled={loading}>
          {t("refresh")}
        </Button>
        <Button
          onClick={() => {
            setSelectedSongIds([])
            anchor.current = null
          }}
          disabled={!selectedSongIds.length || loading || isDownloading || isConfirmingDownload}
        >
          {t("deselectAll")}
        </Button>
        <Button
          onClick={() => handleDownload(selectedSongs)}
          disabled={
            neteaseLoggedIn !== true || loading || isDownloading || isConfirmingDownload || !selectedSongs.length
          }
        >
          {t("downloadSelected")} {selectedSongs.length > 0 && `(${selectedSongs.length})`}
        </Button>
        <Button
          onClick={() => handleDownload()}
          disabled={
            neteaseLoggedIn !== true || loading || isDownloading || isConfirmingDownload || !filteredSongs.length
          }
        >
          {t("downloadAll")}
        </Button>
      </div>
    </div>
  )
}
