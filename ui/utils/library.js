export function isInstrumental(song) {
  return song.instrumental || /instrumental|纯音乐|伴奏|inst\./i.test(song.title || "")
}

export function songArtists(song) {
  const value = song.artists || song.artist || ""
  return [
    ...new Set(
      String(value)
        .split(" / ")
        .map((artist) => artist.trim())
        .filter(Boolean),
    ),
  ]
}

export function albumKey(song) {
  return String(song.album_id || `title:${song.album_title || ""}`)
}

export function compareSongs(a, b) {
  return (
    (b.publish || "").localeCompare(a.publish || "") ||
    (b.album_title || "").localeCompare(a.album_title || "") ||
    (a.disc || 0) - (b.disc || 0) ||
    (a.track || 0) - (b.track || 0) ||
    String(a.id).localeCompare(String(b.id))
  )
}

export function groupAlbums(songs) {
  const albums = new Map()
  for (const song of songs) {
    const id = albumKey(song)
    if (!albums.has(id))
      albums.set(id, {
        id,
        title: song.album_title || "",
        song,
        songs: [],
        artists: new Set(),
        publish: song.publish || "",
      })
    const album = albums.get(id)
    album.songs.push(song)
    for (const artist of songArtists(song)) album.artists.add(artist)
    if ((song.publish || "") > album.publish) album.publish = song.publish
  }
  return [...albums.values()].map((album) => ({ ...album, artists: [...album.artists].join(" / ") }))
}

export function createSearchIndex(songs) {
  const artists = new Set()
  for (const song of songs) for (const artist of songArtists(song)) artists.add(artist)
  return [
    ...songs.map((song) => ({
      type: "song",
      id: String(song.id),
      title: song.title || "",
      subtitle: [song.album_title, ...songArtists(song)].filter(Boolean).join(" · "),
    })),
    ...groupAlbums(songs).map((album) => ({
      type: "album",
      id: album.id,
      title: album.title,
      subtitle: album.artists,
    })),
    ...[...artists].map((artist) => ({ type: "artist", id: artist, title: artist, subtitle: "" })),
  ].map((entry) => ({ ...entry, match: `${entry.title} ${entry.subtitle}`.toLocaleLowerCase() }))
}

export function searchLibrary(index, query, limitPerType = 5) {
  const words = query.trim().toLocaleLowerCase().split(/\s+/).filter(Boolean)
  if (!words.length) return []
  const groups = { song: [], album: [], artist: [] }
  for (const entry of index) {
    const group = groups[entry.type]
    if (group.length < limitPerType && words.every((word) => entry.match.includes(word))) group.push(entry)
    if (Object.values(groups).every((group) => group.length >= limitPerType)) break
  }
  return [...groups.song, ...groups.album, ...groups.artist]
}
