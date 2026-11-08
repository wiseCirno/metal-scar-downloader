use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub download_path: String,
    pub concurrent_downloads: usize,
    pub download_instrumental: bool,
    pub download_lyrics: bool,
    pub id3_date_format: String,
    pub custom_album: String,
    pub show_cover: bool,
    pub language: String,
    pub netease_artist_id: String,
    pub audio_quality: String,
    pub artist_separator: String,
}

impl AppConfig {
    pub(crate) fn normalized_artist_separator(&self) -> &str {
        match self.artist_separator.as_str() {
            ";" | "/" | "," | "、" | " & " => &self.artist_separator,
            _ => ";",
        }
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            download_path: String::new(),
            concurrent_downloads: 3,
            download_instrumental: false,
            download_lyrics: false,
            id3_date_format: "year".into(),
            custom_album: "none".into(),
            show_cover: true,
            language: "zh-Hans".into(),
            netease_artist_id: "122546004".into(),
            audio_quality: "exhigh".into(),
            artist_separator: ";".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SongDetail {
    pub id: String,
    pub title: String,
    pub album_id: String,
    pub album_title: String,
    #[serde(default)]
    pub artwork_url: String,
    #[serde(default)]
    pub music_url: String,
    #[serde(default)]
    pub artist: String,
    #[serde(default)]
    pub language: String,
    #[serde(default)]
    pub disc: u32,
    #[serde(default)]
    pub track: u32,
    #[serde(default)]
    pub duration_ms: u64,
    #[serde(default)]
    pub publish: String,
    #[serde(default)]
    pub instrumental: bool,
    #[serde(default)]
    pub download: bool,
    #[serde(default)]
    pub downloaded_quality: String,
    #[serde(default)]
    pub vip: bool,
}

fn numeric_id(value: &Value) -> Option<String> {
    let id = value
        .as_u64()
        .map(|n| n.to_string())
        .or_else(|| value.as_str().map(str::to_string))?;
    (!id.is_empty()
        && id.len() <= 20
        && id.bytes().all(|b| b.is_ascii_digit())
        && id.parse::<u64>().ok().is_some_and(|n| n > 0))
    .then_some(id)
}

fn number(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| value.as_str()?.parse().ok())
}

fn publish_date(value: &Value) -> Option<String> {
    let ms = number(value).and_then(|n| i64::try_from(n).ok())?;
    if ms <= 0 {
        return None;
    }
    let date =
        DateTime::from_timestamp_millis(ms)?.with_timezone(&FixedOffset::east_opt(8 * 3600)?);
    Some(date.format("%Y-%m-%d").to_string())
}

fn vip_status(item: &Value) -> Option<bool> {
    let fee = number(&item["fee"]).or_else(|| number(&item["privilege"]["fee"]))?;
    match fee {
        1 | 8 => Some(true),
        0 | 4 => Some(false),
        _ => None,
    }
}
fn instrumental_title(title: &str) -> bool {
    let lower = title.to_ascii_lowercase();
    lower.contains("instrumental")
        || title.contains("纯音乐")
        || title.contains("伴奏")
        || lower.contains("inst.")
}
impl SongDetail {
    pub fn is_instrumental(&self) -> bool {
        self.instrumental || instrumental_title(&self.title)
    }

    pub fn merge_netease_detail(&mut self, item: &Value) {
        if numeric_id(&item["id"]).as_deref() != Some(self.id.as_str()) {
            return;
        }
        if let Some(vip) = vip_status(item) {
            self.vip = vip;
        }
        if let Some(title) = item["name"]
            .as_str()
            .filter(|value| !value.trim().is_empty())
        {
            self.title = title.trim().to_string();
        }
        let album = item
            .get("al")
            .filter(|value| value.is_object())
            .or_else(|| item.get("album"));
        if let Some(album) = album {
            if let Some(id) = numeric_id(&album["id"]).filter(|id| id != "0") {
                self.album_id = id;
            }
            if let Some(title) = album["name"]
                .as_str()
                .filter(|value| !value.trim().is_empty())
            {
                self.album_title = title.to_string();
            }
            if let Some(artwork) = album["picUrl"].as_str().filter(|value| !value.is_empty()) {
                self.artwork_url = artwork.to_string();
            }
            if self.publish.is_empty() {
                self.publish = publish_date(&album["publishTime"]).unwrap_or_default();
            }
        }
        if let Some(artwork) = item["picUrl"].as_str().filter(|value| !value.is_empty()) {
            self.artwork_url = artwork.to_string();
        }
        if let Some(artists) = item
            .get("ar")
            .or_else(|| item.get("artists"))
            .and_then(Value::as_array)
        {
            let artist = artists
                .iter()
                .filter_map(|value| value["name"].as_str())
                .filter(|name| !name.is_empty())
                .collect::<Vec<_>>()
                .join(" / ");
            if !artist.is_empty() {
                self.artist = artist;
            }
        }
        if let Some(publish) = publish_date(&item["publishTime"]) {
            self.publish = publish;
        }
        if let Some(duration) = number(&item["dt"])
            .or_else(|| number(&item["duration"]))
            .filter(|value| *value > 0)
        {
            self.duration_ms = duration;
        }
        if let Some(disc) = number(&item["cd"])
            .and_then(|value| u32::try_from(value).ok())
            .filter(|value| *value > 0)
        {
            self.disc = disc;
        }
        if let Some(track) = number(&item["no"])
            .and_then(|value| u32::try_from(value).ok())
            .filter(|value| *value > 0)
        {
            self.track = track;
        }
    }

    pub fn from_netease_api(item: &Value) -> Option<Self> {
        let id = numeric_id(item.get("id")?)?;
        let title = item.get("name")?.as_str()?.trim().to_string();
        if title.is_empty() {
            return None;
        }
        let album = item
            .get("al")
            .filter(|v| v.is_object())
            .or_else(|| item.get("album"))
            .unwrap_or(&Value::Null);
        let album_id = numeric_id(&album["id"])?;
        let album_title = album["name"].as_str().unwrap_or("").to_string();
        let artwork_url = album["picUrl"]
            .as_str()
            .filter(|s| !s.is_empty())
            .or_else(|| item["picUrl"].as_str())
            .unwrap_or("")
            .to_string();
        let artist = item
            .get("ar")
            .or_else(|| item.get("artists"))
            .and_then(Value::as_array)
            .map(|artists| {
                artists
                    .iter()
                    .filter_map(|a| a["name"].as_str())
                    .filter(|name| !name.is_empty())
                    .collect::<Vec<_>>()
                    .join(" / ")
            })
            .unwrap_or_default();
        let publish = publish_date(&item["publishTime"])
            .or_else(|| publish_date(&album["publishTime"]))
            .unwrap_or_default();
        let disc = number(&item["cd"])
            .and_then(|n| u32::try_from(n).ok())
            .filter(|n| *n > 0)
            .unwrap_or(1);
        let track = number(&item["no"])
            .and_then(|n| u32::try_from(n).ok())
            .unwrap_or(0);
        let duration_ms = number(&item["dt"])
            .or_else(|| number(&item["duration"]))
            .unwrap_or(0);
        let instrumental = instrumental_title(&title);
        Some(Self {
            music_url: format!("https://music.163.com/#/song?id={id}"),
            id,
            title,
            album_id,
            album_title,
            artwork_url,
            artist,
            disc,
            track,
            duration_ms,
            publish,
            instrumental,
            language: item["language"].as_str().unwrap_or("").to_string(),
            download: false,
            downloaded_quality: String::new(),
            vip: vip_status(item).unwrap_or(false),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vip_flags_follow_membership_fees_and_keep_unknown_cache_compatible() {
        for (fee, vip) in [(0, false), (1, true), (4, false), (8, true)] {
            let item = serde_json::json!({"id":123,"name":"Song","al":{"id":456},"fee":fee});
            let mut song = SongDetail::from_netease_api(&item).unwrap();
            assert_eq!(song.vip, vip);
            song.merge_netease_detail(&serde_json::json!({"id":123}));
            assert_eq!(song.vip, vip);
            song.merge_netease_detail(&serde_json::json!({"id":123,"fee":0}));
            assert!(!song.vip);
        }
        let item =
            serde_json::json!({"id":123,"name":"Song","al":{"id":456},"privilege":{"fee":"8"}});
        assert!(SongDetail::from_netease_api(&item).unwrap().vip);
        let cached: SongDetail = serde_json::from_value(serde_json::json!({"id":"123","title":"Old (INSTRUMENTAL)","album_id":"456","album_title":"Album"})).unwrap();
        assert!(!cached.vip);
        assert!(cached.is_instrumental());
    }
    #[test]
    fn normalizes_netease_song_without_using_preview_urls() {
        let item = serde_json::json!({
            "id": 123456, "name": "Song (Instrumental)",
            "al": {"id": 789, "name": "Album", "picUrl": "https://p1.music.126.net/cover.jpg"},
            "ar": [{"id": 122546004, "name": "铁痕电台"}, {"name": "Guest"}],
            "dt": 245000, "cd": "2", "no": 3,
            "publishTime": 1704124800000_u64,
            "url": "https://example.invalid/preview.mp3"
        });
        let song = SongDetail::from_netease_api(&item).unwrap();
        assert_eq!(song.id, "123456");
        assert_eq!(song.album_id, "789");
        assert_eq!(song.artist, "铁痕电台 / Guest");
        assert_eq!(song.duration_ms, 245000);
        assert_eq!((song.disc, song.track), (2, 3));
        assert_eq!(song.publish, "2024-01-02");
        assert_eq!(song.music_url, "https://music.163.com/#/song?id=123456");
        assert!(song.instrumental);
        assert!(!song.download);
    }

    #[test]
    fn missing_metadata_is_not_invented_and_album_metadata_can_fill_it() {
        let item = serde_json::json!({"id": "1", "name": "Song", "cd": 3,
            "album": {"id": "2", "name": "Album", "publishTime": 1704124800000_u64}});
        let song = SongDetail::from_netease_api(&item).unwrap();
        assert_eq!(song.duration_ms, 0);
        assert_eq!(song.disc, 3);
        assert_eq!(song.track, 0);
        assert_eq!(song.language, "");
        assert_eq!(song.publish, "2024-01-02");
        assert!(
            SongDetail::from_netease_api(&serde_json::json!({"id": "../1", "name": "X"})).is_none()
        );
    }

    #[test]
    fn merges_netease_detail_and_preserves_cached_fallbacks() {
        let mut song = SongDetail {
            id: "123".into(),
            title: "Cached title".into(),
            album_id: "789".into(),
            album_title: "Cached album".into(),
            artwork_url: "https://cached/cover.jpg".into(),
            artist: "Cached artist".into(),
            disc: 2,
            track: 4,
            duration_ms: 90_000,
            publish: "2020-01-02".into(),
            download: true,
            ..SongDetail::default()
        };

        song.merge_netease_detail(&serde_json::json!({
            "id": "123",
            "name": "Fresh title",
            "al": {
                "id": 456,
                "name": "Fresh album",
                "picUrl": "https://netease/album.jpg",
                "publishTime": 1704124800000_u64
            },
            "picUrl": "https://netease/song.jpg",
            "ar": [{"name": "Fresh artist"}],
            "publishTime": 1704124800000_u64,
            "dt": 245000,
            "cd": 1,
            "no": 5
        }));
        assert_eq!(song.title, "Fresh title");
        assert_eq!(song.album_id, "456");
        assert_eq!(song.album_title, "Fresh album");
        assert_eq!(song.artwork_url, "https://netease/song.jpg");
        assert_eq!(song.artist, "Fresh artist");
        assert_eq!(song.duration_ms, 245000);
        assert_eq!((song.disc, song.track), (1, 5));
        assert_eq!(song.publish, "2024-01-02");
        assert!(song.download);

        song.merge_netease_detail(&serde_json::json!({
            "id": "123",
            "al": {"id": 0, "name": "", "picUrl": "", "publishTime": 0},
            "picUrl": "",
            "ar": [],
            "dt": 0,
            "cd": 0,
            "no": 0
        }));
        assert_eq!(song.album_id, "456");
        assert_eq!(song.album_title, "Fresh album");
        assert_eq!(song.artwork_url, "https://netease/song.jpg");
        assert_eq!(song.artist, "Fresh artist");
        assert_eq!(song.duration_ms, 245000);
        assert_eq!((song.disc, song.track), (1, 5));
        assert_eq!(song.publish, "2024-01-02");
        assert!(song.download);

        let before = song.clone();
        song.merge_netease_detail(&serde_json::json!({
            "id": "999", "name": "Wrong song", "dt": 1,
            "al": {"id": 888, "name": "Wrong album"}
        }));
        assert_eq!(song.id, before.id);
        assert_eq!(song.title, before.title);
        assert_eq!(song.album_id, before.album_id);
        assert_eq!(song.album_title, before.album_title);
        assert_eq!(song.duration_ms, before.duration_ms);
        assert!(song.download);
    }

    #[test]
    fn legacy_config_defaults_to_exhigh_without_serializing_secrets() {
        let config: AppConfig = serde_json::from_value(serde_json::json!({
            "download_path": "Music", "audio_format": "aac", "media_user_token": "old-secret"
        }))
        .unwrap();
        assert_eq!(config.audio_quality, "exhigh");
        assert_eq!(config.netease_artist_id, "122546004");
        assert_eq!(config.artist_separator, ";");
        assert_eq!(AppConfig::default().artist_separator, ";");
        let round_trip: AppConfig =
            serde_json::from_value(serde_json::to_value(&config).unwrap()).unwrap();
        assert_eq!(round_trip.artist_separator, ";");
        let saved = serde_json::to_string(&config).unwrap();
        assert!(!saved.contains("old-secret"));
        assert!(!saved.contains("media_user_token"));
    }
}
