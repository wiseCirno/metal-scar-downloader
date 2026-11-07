use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::HashSet;
use std::time::Duration;
use tauri::AppHandle;

const DEFAULT_ARTIST_ID: &str = "122546004";
const PAGE_SIZE: u64 = 100;
const MAX_PAGES: u64 = 100;

pub async fn fetch_latest_version() -> Result<String> {
    let response = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()?
        .get("https://api.github.com/repos/wiseCirno/metal-scar-downloader/releases/latest")
        .header("User-Agent", "metal-scar-downloader")
        .send()
        .await?
        .error_for_status()?;
    response.json::<Value>().await?["tag_name"]
        .as_str()
        .map(|v| v.trim_start_matches('v').to_string())
        .context("获取最新版本失败")
}

fn validate_artist_id(value: &str) -> Result<&str> {
    let value = value.trim();
    anyhow::ensure!(
        !value.is_empty()
            && value.len() <= 20
            && value.bytes().all(|b| b.is_ascii_digit())
            && value.parse::<u64>().is_ok_and(|id| id > 0),
        "网易云歌手 ID 无效"
    );
    Ok(value)
}

fn song_id(item: &Value) -> Option<String> {
    let id = item.get("id")?;
    let value = id
        .as_u64()
        .map(|n| n.to_string())
        .or_else(|| id.as_str().map(str::to_string))?;
    (!value.is_empty() && value.len() <= 20 && value.bytes().all(|b| b.is_ascii_digit()))
        .then_some(value)
}

pub async fn fetch_all_songs(
    app: &AppHandle,
    artist_id: &str,
) -> Result<Vec<crate::models::SongDetail>> {
    let artist_id = validate_artist_id(if artist_id.trim().is_empty() {
        DEFAULT_ARTIST_ID
    } else {
        artist_id
    })?;
    let mut songs = Vec::new();
    let mut seen = HashSet::new();

    for page_index in 0..MAX_PAGES {
        let offset = page_index
            .checked_mul(PAGE_SIZE)
            .context("网易云目录分页偏移溢出")?;
        let page = crate::netease::fetch_artist_songs(app, artist_id, offset, PAGE_SIZE)
            .await
            .with_context(|| format!("读取网易云歌手歌曲失败（页 {}）", page_index + 1))?;
        if page.is_empty() {
            break;
        }
        let page_len = page.len();
        for item in page {
            let Some(id) = song_id(&item) else { continue };
            if seen.insert(id) {
                if let Some(song) = crate::models::SongDetail::from_netease_api(&item) {
                    songs.push(song);
                }
            }
        }
        if page_len < PAGE_SIZE as usize {
            break;
        }
        anyhow::ensure!(
            page_index + 1 < MAX_PAGES,
            "网易云歌手歌曲超过安全分页上限；未更新本地缓存"
        );
    }
    anyhow::ensure!(
        !songs.is_empty(),
        "网易云未返回可识别的歌曲；未更新本地缓存"
    );
    Ok(songs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_artist_ids_before_using_them_in_cache_or_requests() {
        assert_eq!(validate_artist_id("122546004").unwrap(), "122546004");
        for invalid in ["", "0", "-1", "../1", "1/2", "999999999999999999999999"] {
            assert!(validate_artist_id(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn song_id_dedup_key_accepts_integer_and_decimal_string() {
        assert_eq!(
            song_id(&serde_json::json!({"id": 42})).as_deref(),
            Some("42")
        );
        assert_eq!(
            song_id(&serde_json::json!({"id": "42"})).as_deref(),
            Some("42")
        );
        assert_eq!(song_id(&serde_json::json!({"id": "../42"})), None);
    }
}
