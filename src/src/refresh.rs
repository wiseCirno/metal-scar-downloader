use crate::api::fetch_all_songs;
use crate::config::{load_app_config, load_music_data_for, save_music_data_for};
use crate::models::SongDetail;
use anyhow::{Context, Result};
use std::collections::HashMap;

pub async fn refresh_music_data(app: &tauri::AppHandle) -> Result<Vec<SongDetail>> {
    let config = load_app_config(app).await?;
    let old: HashMap<String, SongDetail> = load_music_data_for(app, &config)
        .await?
        .into_iter()
        .map(|song| (song.id.clone(), song))
        .collect();

    // Fetch fully before writing so upstream errors never replace a good local cache.
    let remote = fetch_all_songs(app, &config.netease_artist_id).await?;
    anyhow::ensure!(!remote.is_empty(), "网易云未返回歌曲，保留现有曲库");
    let mut songs: Vec<SongDetail> = remote
        .into_iter()
        .map(|mut song| {
            if let Some(previous) = old.get(&song.id) {
                song.download = previous.download;
                song.downloaded_quality = previous.downloaded_quality.clone();
            }
            song
        })
        .collect();
    let mut album_dates = HashMap::new();
    for song in songs.iter().filter(|song| song.publish.is_empty()) {
        if album_dates.contains_key(&song.album_id) {
            continue;
        }
        let response = crate::node_api::call(
            app,
            "album",
            serde_json::json!({"id": song.album_id, "cookie": ""}),
        )
        .await?;
        anyhow::ensure!(
            response.body["code"] == 200,
            "获取专辑发行日期失败，请重新刷新目录"
        );
        let date = response
            .body
            .pointer("/album/publishTime")
            .and_then(serde_json::Value::as_i64)
            .filter(|value| *value > 0)
            .and_then(chrono::DateTime::from_timestamp_millis)
            .map(|date| {
                date.with_timezone(&chrono::FixedOffset::east_opt(8 * 3600).unwrap())
                    .format("%Y-%m-%d")
                    .to_string()
            });
        album_dates.insert(song.album_id.clone(), date);
    }
    for song in &mut songs {
        if song.publish.is_empty() {
            if let Some(Some(date)) = album_dates.get(&song.album_id) {
                song.publish = date.clone();
            }
        }
    }
    songs.sort_by(|a, b| {
        b.publish
            .cmp(&a.publish)
            .then(a.album_title.cmp(&b.album_title))
            .then(a.disc.cmp(&b.disc))
            .then(a.track.cmp(&b.track))
            .then(a.id.cmp(&b.id))
    });
    save_music_data_for(app, &config, &songs)
        .await
        .context("保存网易云曲库失败")?;
    Ok(songs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downloaded_state_is_keyed_by_netease_song_id() {
        let mut fresh = vec![
            SongDetail {
                id: "100".into(),
                download: false,
                ..Default::default()
            },
            SongDetail {
                id: "200".into(),
                download: false,
                ..Default::default()
            },
        ];
        let old = HashMap::from([(
            "100".to_string(),
            SongDetail {
                id: "100".into(),
                download: true,
                ..Default::default()
            },
        )]);
        for song in &mut fresh {
            if let Some(previous) = old.get(&song.id) {
                song.download = previous.download;
                song.downloaded_quality = previous.downloaded_quality.clone();
            }
        }
        assert!(fresh[0].download);
        assert!(!fresh[1].download);
    }
}
