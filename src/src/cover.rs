use anyhow::{Context, Result};
use reqwest::Client;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::LazyLock;
use tauri::{AppHandle, Manager};
use tokio::fs;
use tokio::sync::Mutex;

static DOWNLOADING_COVERS: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

pub async fn get_cover_path(app: &AppHandle, album_id: &str, cover_url: &str) -> Result<PathBuf> {
    anyhow::ensure!(
        album_id.bytes().all(|b| b.is_ascii_alphanumeric()),
        "无效专辑 ID"
    );
    anyhow::ensure!(cover_url.starts_with("https://"), "封面地址必须使用 HTTPS");
    let cover_dir = app
        .path()
        .app_data_dir()
        .context("无法访问应用目录")?
        .join("covers");
    let cover_path = cover_dir.join(format!("{}.jpg", album_id));
    if cover_path.exists() {
        return Ok(cover_path);
    }
    let key = album_id.to_string();
    let mut guard = DOWNLOADING_COVERS.lock().await;
    if guard.contains(&key) {
        drop(guard);
        for _ in 0..600 {
            if cover_path.exists() {
                return Ok(cover_path);
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            if !DOWNLOADING_COVERS.lock().await.contains(&key) {
                break;
            }
        }
        if cover_path.exists() {
            return Ok(cover_path);
        }
        guard = DOWNLOADING_COVERS.lock().await;
    }
    guard.insert(key.clone());
    drop(guard);
    let result = async {
        fs::create_dir_all(&cover_dir)
            .await
            .context("无法创建封面目录")?;
        let bytes = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()?
            .get(cover_url)
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?;
        let temp = cover_path.with_extension("jpg.tmp");
        fs::write(&temp, &bytes).await.context("保存封面失败")?;
        fs::rename(&temp, &cover_path)
            .await
            .context("原子保存封面失败")?;
        Ok::<_, anyhow::Error>(cover_path.clone())
    }
    .await;
    DOWNLOADING_COVERS.lock().await.remove(&key);
    result
}
