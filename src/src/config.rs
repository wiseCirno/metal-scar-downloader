use crate::models::{AppConfig, SongDetail};
use anyhow::{Context, Result};
use serde_json::{from_str, from_value, to_string_pretty};
use std::fs;
use std::path::PathBuf;
use std::sync::LazyLock;
use tauri::{AppHandle, Manager};
use tauri_plugin_store::StoreExt;
use tokio::sync::Mutex;

pub static SESSION_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

pub async fn load_app_config(app: &AppHandle) -> Result<AppConfig> {
    let value = app
        .store("config.json")
        .context("无法读取配置文件")?
        .get("app_config");
    let config: AppConfig = value.map(from_value).transpose()?.unwrap_or_default();
    Ok(normalize_app_config(config))
}

fn normalize_app_config(mut config: AppConfig) -> AppConfig {
    if config.audio_quality == "higher" {
        config.audio_quality = "exhigh".into();
    }
    config.artist_separator = config.normalized_artist_separator().to_string();
    config
}

fn cache_file(app: &AppHandle, config: &AppConfig) -> Result<PathBuf> {
    let dir = app.path().app_data_dir().context("无法访问应用目录")?;
    let configured_artist = config.netease_artist_id.trim();
    let artist = if configured_artist.is_empty() {
        "122546004"
    } else {
        configured_artist
    };
    anyhow::ensure!(
        !artist.is_empty()
            && artist.len() <= 20
            && artist.bytes().all(|b| b.is_ascii_digit())
            && artist.parse::<u64>().is_ok_and(|id| id > 0),
        "网易云歌手 ID 无效"
    );
    Ok(dir.join(format!("music_netease_{artist}.json")))
}

pub async fn load_music_data_for(app: &AppHandle, config: &AppConfig) -> Result<Vec<SongDetail>> {
    let path = cache_file(app, config)?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let data = fs::read_to_string(path).context("无法读取歌曲列表")?;
    from_str(&data).context("歌曲列表解析失败")
}

pub async fn load_music_data(app: &AppHandle) -> Result<Vec<SongDetail>> {
    let config = load_app_config(app).await?;
    load_music_data_for(app, &config).await
}

pub async fn save_music_data_for(
    app: &AppHandle,
    config: &AppConfig,
    songs: &[SongDetail],
) -> Result<()> {
    let path = cache_file(app, config)?;
    let dir = path.parent().context("歌曲目录无效")?;
    fs::create_dir_all(dir).context("无法创建应用目录")?;
    let temp = path.with_extension("json.tmp");
    fs::write(&temp, to_string_pretty(songs)?).context("无法写入歌曲列表临时文件")?;
    // std::fs::rename replaces an existing destination on Windows via MOVEFILE_REPLACE_EXISTING.
    fs::rename(&temp, &path).context("无法原子替换歌曲列表")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_artist_separator_without_adding_or_removing_spaces() {
        for separator in [";", "/", ",", "、", " & "] {
            let config = normalize_app_config(AppConfig {
                artist_separator: separator.into(),
                ..Default::default()
            });
            assert_eq!(config.artist_separator, separator);
            let saved = serde_json::to_value(&config).unwrap();
            assert_eq!(saved["artist_separator"], separator);
        }
        for separator in ["", " ", " ; ", "; ", " / ", "&", "\\", "\n", "unknown"] {
            let config = normalize_app_config(AppConfig {
                artist_separator: separator.into(),
                ..Default::default()
            });
            assert_eq!(config.artist_separator, ";", "invalid: {separator:?}");
        }
    }

    #[test]
    fn legacy_config_migrates_quality_and_defaults_artist_separator() {
        let legacy: AppConfig = from_value(serde_json::json!({
            "download_path": "Music", "audio_quality": "higher"
        }))
        .unwrap();
        let config = normalize_app_config(legacy);
        assert_eq!(config.artist_separator, ";");
        assert_eq!(config.audio_quality, "exhigh");
        assert_eq!(config.download_path, "Music");
    }
}
