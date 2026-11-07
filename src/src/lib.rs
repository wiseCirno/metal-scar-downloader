mod api;
mod config;
mod cover;
mod download;
mod download_control;
mod ffmpeg;
#[cfg(target_os = "macos")]
mod menu;
mod models;
mod netease;
mod node_api;
mod refresh;

use models::SongDetail;

// 检查更新
#[tauri::command]
async fn load_latest_version() -> Result<String, String> {
    api::fetch_latest_version().await.map_err(|e| e.to_string())
}

// 网易云短信验证码登录状态
#[tauri::command]
async fn netease_auth_status(app: tauri::AppHandle) -> Result<netease::AuthStatus, String> {
    netease::auth_status(&app).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn netease_send_captcha(
    app: tauri::AppHandle,
    phone: String,
    countrycode: String,
) -> Result<(), String> {
    let _guard = config::SESSION_LOCK
        .try_lock()
        .map_err(|_| "NETEASE_BUSY".to_string())?;
    netease::send_captcha(&app, &phone, &countrycode)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn netease_login(
    app: tauri::AppHandle,
    phone: String,
    countrycode: String,
    captcha: String,
) -> Result<netease::AuthStatus, String> {
    let _guard = config::SESSION_LOCK
        .try_lock()
        .map_err(|_| "NETEASE_BUSY".to_string())?;
    netease::login(&app, &phone, &countrycode, &captcha)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn netease_logout(app: tauri::AppHandle) -> Result<(), String> {
    let _guard = config::SESSION_LOCK
        .try_lock()
        .map_err(|_| "NETEASE_BUSY".to_string())?;
    netease::logout(&app).await.map_err(|e| e.to_string())
}

// 检查 FFmpeg 版本
#[tauri::command]
async fn get_ffmpeg_version(app: tauri::AppHandle) -> Result<String, String> {
    let ffmpeg_path = ffmpeg::get_ffmpeg_path(&app).map_err(|e| e.to_string())?;
    ffmpeg::check_ffmpeg_version(&app, &ffmpeg_path)
        .await
        .map_err(|e| e.to_string())
}

// 下载 FFmpeg
#[tauri::command]
async fn download_ffmpeg(
    app: tauri::AppHandle,
    on_progress: tauri::ipc::Channel<ffmpeg::DownloadProgress>,
) -> Result<(), String> {
    ffmpeg::download_ffmpeg(&app, on_progress)
        .await
        .map_err(|e| e.to_string())
}

// 删除 FFmpeg
#[tauri::command]
async fn delete_ffmpeg(app: tauri::AppHandle) -> Result<(), String> {
    ffmpeg::delete_ffmpeg(&app).await.map_err(|e| e.to_string())
}

// 加载本地歌曲列表
#[tauri::command]
async fn load_local_music(app: tauri::AppHandle) -> Result<Vec<SongDetail>, String> {
    config::load_music_data(&app)
        .await
        .map_err(|e| e.to_string())
}

// 更新歌曲列表并保存完整信息到本地
#[tauri::command]
async fn refresh_music_list(app: tauri::AppHandle) -> Result<Vec<SongDetail>, String> {
    let _guard = config::SESSION_LOCK.lock().await;
    refresh::refresh_music_data(&app)
        .await
        .map_err(|e| e.to_string())
}

// 获取封面路径（本地存在则直接返回，不存在则下载）
#[tauri::command]
async fn get_cover(
    app: tauri::AppHandle,
    album_id: String,
    cover_url: String,
) -> Result<String, String> {
    let path = cover::get_cover_path(&app, &album_id, &cover_url)
        .await
        .map_err(|e| e.to_string())?;
    Ok(path.to_string_lossy().to_string())
}

// 下载歌曲
#[tauri::command]
async fn download_music(
    app: tauri::AppHandle,
    batch_id: String,
    song_ids: Option<Vec<String>>,
) -> Result<(usize, usize, usize), String> {
    use tauri::Manager;
    // Register before waiting for SESSION_LOCK: queued batches are cancellable.
    let batch = app
        .state::<download_control::Downloads>()
        .begin(&batch_id, song_ids.as_deref())
        .map_err(|e| e.to_string())?;
    let _guard = if let Some(ids) = song_ids.as_deref() {
        match batch
            .0
            .wait_before_start(ids, config::SESSION_LOCK.lock())
            .await
        {
            Some(guard) => guard,
            None => {
                let cancelled = batch.0.finish_initialization_failure(ids);
                for id in &cancelled {
                    download::emit_cancelled(&app, &batch_id, id, None);
                }
                return Ok((0, 0, cancelled.len()));
            }
        }
    } else {
        config::SESSION_LOCK.lock().await
    };
    download::download_music(&app, &batch_id, &batch.0, song_ids.as_deref())
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn cancel_downloads(
    app: tauri::AppHandle,
    batch_id: String,
    song_ids: Option<Vec<String>>,
) -> Result<usize, String> {
    use tauri::Manager;
    app.state::<download_control::Downloads>()
        .cancel(&batch_id, song_ids.as_deref())
        .map_err(|e| e.to_string())
}

// 更新歌曲下载状态
#[tauri::command]
async fn set_download_status(
    app: tauri::AppHandle,
    song_id: String,
    downloaded: bool,
) -> Result<(), String> {
    let _guard = config::SESSION_LOCK.lock().await;
    let config = config::load_app_config(&app)
        .await
        .map_err(|e| e.to_string())?;
    let mut songs = config::load_music_data_for(&app, &config)
        .await
        .map_err(|e| e.to_string())?;

    if let Some(song) = songs.iter_mut().find(|s| s.id == song_id) {
        song.download = downloaded;
        config::save_music_data_for(&app, &config, &songs)
            .await
            .map_err(|e| e.to_string())?;
    }

    Ok(())
}

// 批量更新所有歌曲下载状态
#[tauri::command]
async fn set_all_download_status(app: tauri::AppHandle, downloaded: bool) -> Result<usize, String> {
    let _guard = config::SESSION_LOCK.lock().await;
    let config = config::load_app_config(&app)
        .await
        .map_err(|e| e.to_string())?;
    let mut songs = config::load_music_data_for(&app, &config)
        .await
        .map_err(|e| e.to_string())?;

    if songs.is_empty() {
        return Ok(0);
    }

    let mut changed = 0;
    for song in &mut songs {
        if song.download != downloaded {
            song.download = downloaded;
            changed += 1;
        }
    }

    if changed > 0 {
        config::save_music_data_for(&app, &config, &songs)
            .await
            .map_err(|e| e.to_string())?;
    }

    Ok(changed)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(download_control::Downloads::default())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_store::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_os::init())
        .setup(|_app| {
            #[cfg(target_os = "macos")]
            {
                let menu = menu::create_menu(_app)?;
                _app.set_menu(menu)?;
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            load_latest_version,
            netease_auth_status,
            netease_send_captcha,
            netease_login,
            netease_logout,
            load_local_music,
            refresh_music_list,
            get_cover,
            download_music,
            cancel_downloads,
            set_download_status,
            set_all_download_status,
            get_ffmpeg_version,
            download_ffmpeg,
            delete_ffmpeg,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|_app, _event| {});
}
