use crate::config::{load_app_config, load_music_data_for, save_music_data_for};
use crate::download_control::{Batch, Cancellation, Cancelled};
use crate::models::{AppConfig, SongDetail};
use crate::netease::DownloadApi;
use anyhow::{Context, Result, bail};
use futures::stream::{self, StreamExt};
use serde_json::json;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager};
use tokio::process::Command;

static JOB_ID: AtomicU64 = AtomicU64::new(0);
struct Staging(PathBuf);
impl Drop for Staging {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn is_link(m: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        m.file_type().is_symlink() || m.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        m.file_type().is_symlink()
    }
}
fn release_date(s: &str) -> Option<chrono::NaiveDate> {
    chrono::NaiveDate::parse_from_str(s.get(..10)?, "%Y-%m-%d").ok()
}
fn metadata_date(mode: &str, publish: &str) -> Option<String> {
    Some(
        release_date(publish)?
            .format(if mode == "full" { "%Y-%m-%d" } else { "%Y" })
            .to_string(),
    )
}
fn album_folder_template(mode: &str, publish: &str) -> String {
    let Some(date) = release_date(publish) else {
        return "{AlbumName}".into();
    };
    let prefix = match mode {
        "year" => date.format("%Y").to_string(),
        "lite" => date.format("%y%m%d").to_string(),
        "full" => date.format("%Y-%m-%d").to_string(),
        _ => return "{AlbumName}".into(),
    };
    format!("[{prefix}] {{AlbumName}}")
}
fn safe_name(value: &str) -> String {
    let s: String = value
        .chars()
        .map(|c| {
            if c.is_control() || "<>:\"/\\|?*".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let s = s.trim().trim_matches('.');
    if s.is_empty() {
        "Unknown".into()
    } else {
        s.chars().take(160).collect()
    }
}
fn album_output_dir(root: &Path, config: &AppConfig, song: &SongDetail) -> Result<PathBuf> {
    let album = album_folder_template(&config.custom_album, &song.publish)
        .replace("{AlbumName}", &safe_name(&song.album_title));
    safe_subdir(root, &album)
}
fn track_file_stem(song: &SongDetail) -> String {
    let disc = if song.disc > 1 {
        format!("{}-", song.disc)
    } else {
        String::new()
    };
    format!("{disc}{:02}. {}", song.track, safe_name(&song.title))
}
fn safe_subdir(parent: &Path, name: &str) -> Result<PathBuf> {
    let path = parent.join(name);
    match std::fs::create_dir(&path) {
        Ok(()) => (),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(e) => return Err(e.into()),
    }
    let meta = std::fs::symlink_metadata(&path)?;
    anyhow::ensure!(meta.is_dir() && !is_link(&meta), "输出路径含有链接或非目录");
    let canonical = path.canonicalize()?;
    anyhow::ensure!(canonical.parent() == Some(parent), "输出目录越界");
    Ok(canonical)
}
fn staging(root: &Path) -> Result<Staging> {
    let workspace = safe_subdir(root, ".metal-scar")?;
    let jobs = safe_subdir(&workspace, "jobs")?;
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let path = jobs.join(format!(
        "job-{}-{stamp}-{}",
        std::process::id(),
        JOB_ID.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&path)?;
    Ok(Staging(path.canonicalize()?))
}
struct FailureLog {
    path: PathBuf,
    file: std::fs::File,
}
impl FailureLog {
    fn create(root: &Path) -> Result<Self> {
        anyhow::ensure!(
            !root.as_os_str().is_empty(),
            "未设置下载目录，无法保存错误日志"
        );
        std::fs::create_dir_all(root)?;
        let root = root.canonicalize()?;
        let workspace = safe_subdir(&root, ".metal-scar")?;
        let logs = safe_subdir(&workspace, "logs")?;
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S-%f");
        let path = logs.join(format!(
            "download-failures-{stamp}-{}-{}.log",
            std::process::id(),
            JOB_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        std::io::Write::write_all(&mut file, "\u{feff}铁痕电台下载失败记录\r\n每条记录包含失败歌曲及原因；成功曲目不计入本日志。\r\n".as_bytes())?;
        Ok(Self { path, file })
    }
    fn record(&mut self, song: &SongDetail, error: &str) -> Result<()> {
        let clean = |value: &str| {
            value
                .chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .collect::<String>()
        };
        let reason = safe_diagnostic(error);
        let hint = if reason.contains("NETEASE_DOWNLOAD_UNAVAILABLE") {
            "网易云本周期 VIP 歌曲下载额度已用尽（-103），请在官方客户端查看额度，等待下一周期刷新。"
        } else if reason.contains("NETEASE_DOWNLOAD_NOT_PERMITTED") {
            "网易云拒绝该歌曲的下载权限（-105），请在官方客户端检查账号及歌曲下载权益。"
        } else {
            "请根据下方错误检查歌曲权限、下载目录或网络后重试。"
        };
        let entry = format!(
            "\r\n[{}] 下载失败\r\n歌曲：{}\r\n歌曲 ID：{}\r\n艺人：{}\r\n专辑：{}\r\n原因：{}\r\n错误：{}\r\n",
            chrono::Local::now().to_rfc3339(),
            clean(&song.title),
            clean(&song.id),
            clean(&song.artist),
            clean(&song.album_title),
            hint,
            reason.replace('\n', "\r\n")
        );
        std::io::Write::write_all(&mut self.file, entry.as_bytes())?;
        self.file.sync_data()?;
        Ok(())
    }
}
fn lossless_format(f: &str) -> bool {
    matches!(
        f.to_ascii_lowercase().as_str(),
        "flac" | "alac" | "ape" | "wav"
    )
}
fn expected_codec(f: &str) -> &'static [&'static str] {
    match f.to_ascii_lowercase().as_str() {
        "flac" => &["flac"],
        "alac" => &["alac"],
        "ape" => &["ape"],
        "wav" => &[
            "pcm_s16le",
            "pcm_s24le",
            "pcm_s32le",
            "pcm_f32le",
            "pcm_f64le",
        ],
        "mp3" => &["mp3"],
        "aac" => &["aac"],
        "m4a" | "mp4" => &["aac", "alac"],
        "ogg" => &["vorbis"],
        "opus" => &["opus"],
        _ => &[],
    }
}
fn validate_source_quality(requested: &str, quality: &str, format: &str, url: &str) -> Result<()> {
    anyhow::ensure!(
        quality == requested,
        "网易云未提供所请求的音质（要求 {requested}，实际 {quality}）"
    );
    anyhow::ensure!(!url.trim().is_empty(), "网易云未返回音源地址");
    anyhow::ensure!(
        !expected_codec(format).is_empty(),
        "不支持的音频格式: {format}"
    );
    if matches!(requested, "lossless" | "hires") {
        anyhow::ensure!(lossless_format(format), "无损响应格式不是真正的无损音频");
    }
    Ok(())
}
fn validate_duration(progress: &[u8], expected: u64) -> Result<()> {
    anyhow::ensure!(expected > 0, "歌曲缺少目录时长，请刷新目录后重新下载");
    let decoded = String::from_utf8_lossy(progress)
        .lines()
        .filter_map(|l| {
            l.strip_prefix("out_time_us=")
                .and_then(|v| v.parse::<u64>().ok())
        })
        .max()
        .context("FFmpeg 未报告解码时长")?
        / 1000;
    anyhow::ensure!(
        decoded > 0 && decoded.abs_diff(expected) <= 2000_u64.max(expected / 50),
        "音频时长不符（{}ms / {}ms），可能为试听或不完整文件",
        decoded,
        expected
    );
    Ok(())
}
fn validate_input_format(log: &str, format: &str) -> Result<()> {
    let header = log.split("Stream mapping:").next().unwrap_or("");
    let container = header
        .lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("Input #0, ")
                .and_then(|rest| rest.split_once(", from ").map(|(name, _)| name))
        })
        .context("FFmpeg 未识别输入容器")?;
    let valid_container = match format.to_ascii_lowercase().as_str() {
        "flac" => container == "flac",
        "ape" => container == "ape",
        "wav" => container == "wav",
        "alac" | "m4a" | "mp4" => container
            .split(',')
            .any(|v| matches!(v, "mov" | "mp4" | "m4a")),
        "aac" => {
            container == "aac"
                || container
                    .split(',')
                    .any(|v| matches!(v, "mov" | "mp4" | "m4a"))
        }
        "mp3" => container == "mp3",
        "ogg" | "opus" => container == "ogg",
        _ => false,
    };
    anyhow::ensure!(valid_container, "实际输入容器与音源格式 {format} 不符");
    let codec = header
        .lines()
        .filter(|line| line.trim_start().starts_with("Stream #0:"))
        .find_map(|line| {
            line.split_once("Audio: ")
                .and_then(|(_, rest)| rest.split(|c: char| c == ',' || c.is_whitespace()).next())
        })
        .context("输入文件缺少音轨")?;
    anyhow::ensure!(
        expected_codec(format).contains(&codec),
        "实际输入编码与音源格式 {format} 不符"
    );
    Ok(())
}
fn ffmpeg_command(ffmpeg: &Path) -> Command {
    let mut command = Command::new(ffmpeg);
    command.kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    command
}
async fn process_output(
    command: &mut Command,
    cancel: &Cancellation,
    timeout: Duration,
) -> Result<std::process::Output> {
    use tokio::io::AsyncReadExt;
    cancel.check()?;
    let mut child = command.kill_on_drop(true).spawn()?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let read = async move {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let (a, b) = tokio::join!(
            async {
                if let Some(mut pipe) = stdout {
                    pipe.read_to_end(&mut out).await?;
                }
                Ok::<_, std::io::Error>(())
            },
            async {
                if let Some(mut pipe) = stderr {
                    pipe.read_to_end(&mut err).await?;
                }
                Ok::<_, std::io::Error>(())
            },
        );
        a?;
        b?;
        Ok::<_, std::io::Error>((out, err))
    };
    let wait = async {
        let result = tokio::select! {
            biased;
            status = child.wait() => status.map_err(anyhow::Error::from),
            _ = cancel.cancelled() => Err(Cancelled.into()),
            _ = tokio::time::sleep(timeout) => Err(anyhow::anyhow!("FFmpeg 执行超时")),
        };
        if result.is_err() {
            // A dropped Child only schedules killing. Explicit kill+wait releases
            // Windows file handles before the caller's staging guard is dropped.
            let _ = child.start_kill();
            let _ = child.wait().await;
        }
        result
    };
    let (status, pipes) = tokio::join!(wait, read);
    let status = status?;
    let (stdout, stderr) = pipes?;
    cancel.check()?;
    Ok(std::process::Output {
        status,
        stdout,
        stderr,
    })
}
async fn validate_audio(
    path: &Path,
    ffmpeg: &Path,
    format: &str,
    duration: u64,
    cancel: &Cancellation,
) -> Result<()> {
    let output = process_output(
        ffmpeg_command(ffmpeg)
            .args(["-nostdin", "-hide_banner", "-v", "info", "-xerror", "-i"])
            .arg(path)
            .args(["-map", "0:a:0", "-progress", "pipe:1", "-f", "null", "-"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()),
        cancel,
        Duration::from_secs(7200),
    )
    .await?;
    anyhow::ensure!(
        output.status.success(),
        "音频完整解码失败: {}",
        String::from_utf8_lossy(&output.stderr)
            .chars()
            .take(1500)
            .collect::<String>()
    );
    validate_input_format(&String::from_utf8_lossy(&output.stderr), format)?;
    validate_duration(&output.stdout, duration)
}
fn is_cover(path: &Path) -> bool {
    path.extension().is_some_and(|e| {
        ["jpg", "jpeg", "png", "webp"]
            .iter()
            .any(|x| e.eq_ignore_ascii_case(x))
    }) && std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file() && !is_link(&m) && m.len() > 0)
}
fn reusable_sidecar(path: &Path) -> bool {
    is_cover(path)
        || (path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("lrc"))
            && std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file() && !is_link(&m)))
}
struct PublishedFile {
    destination: PathBuf,
    backup: Option<PathBuf>,
}
struct PublicationRollback(Vec<PublishedFile>);
impl PublicationRollback {
    fn finish(mut self) {
        for published in self.0.drain(..) {
            if let Some(backup) = published.backup {
                let _ = std::fs::remove_file(backup);
            }
        }
    }
}
impl Drop for PublicationRollback {
    fn drop(&mut self) {
        for published in self.0.iter().rev() {
            let _ = std::fs::remove_file(&published.destination);
            if let Some(backup) = &published.backup {
                let _ = std::fs::rename(backup, &published.destination);
            }
        }
    }
}
fn sibling_path(target: &Path, suffix: &str) -> Result<PathBuf> {
    let parent = target.parent().context("目标文件缺少父目录")?;
    for _ in 0..100 {
        let id = JOB_ID.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!(".msr-{}-{id}-{suffix}", std::process::id()));
        match std::fs::symlink_metadata(&candidate) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(candidate),
            Ok(_) => continue,
            Err(error) => return Err(error.into()),
        }
    }
    bail!("无法为目标文件分配暂存路径")
}
fn stage_replacement(source: &Path, target: &Path) -> Result<PathBuf> {
    let parent = target.parent().context("目标文件缺少父目录")?;
    for _ in 0..100 {
        let id = JOB_ID.fetch_add(1, Ordering::Relaxed);
        let staged = parent.join(format!(".msr-{}-{id}-replacement", std::process::id()));
        let mut output = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staged)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        };
        let result = (|| {
            std::io::copy(&mut std::fs::File::open(source)?, &mut output)?;
            output.sync_all()?;
            Ok(())
        })();
        drop(output);
        if let Err(error) = result {
            let _ = std::fs::remove_file(&staged);
            return Err(error);
        }
        return Ok(staged);
    }
    bail!("无法为目标文件创建替换暂存文件")
}
fn replace_existing_file(
    source: &Path,
    target: &Path,
    rollback: &mut PublicationRollback,
) -> Result<()> {
    let staged = stage_replacement(source, target)?;
    let result = (|| {
        let metadata = std::fs::symlink_metadata(target)?;
        anyhow::ensure!(
            metadata.is_file() && !is_link(&metadata),
            "目标文件已存在，拒绝覆盖: {}",
            target.display()
        );
        let backup = sibling_path(target, "backup")?;
        std::fs::rename(target, &backup)?;
        if let Err(error) = std::fs::rename(&staged, target) {
            return match std::fs::rename(&backup, target) {
                Ok(()) => Err(error.into()),
                Err(restore_error) => Err(anyhow::anyhow!(
                    "替换失败，原文件备份保留在 {}: {}; 恢复失败: {}",
                    backup.display(),
                    error,
                    restore_error
                )),
            };
        }
        rollback.0.push(PublishedFile {
            destination: target.to_path_buf(),
            backup: Some(backup),
        });
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(staged);
    }
    result
}
fn publish_files(source: &Path, destination: &Path, files: &[PathBuf]) -> Result<()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = LOCK.lock().map_err(|_| anyhow::anyhow!("发布锁异常"))?;
    let mut pairs = Vec::new();
    for file in files {
        let relative = file.strip_prefix(source).context("暂存文件越界")?;
        anyhow::ensure!(
            relative
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_))),
            "目标相对路径无效"
        );
        let mut parent = destination.to_path_buf();
        for component in relative.parent().into_iter().flat_map(Path::components) {
            parent = safe_subdir(&parent, &component.as_os_str().to_string_lossy())?;
        }
        let target = parent.join(relative.file_name().context("缺少文件名")?);
        match std::fs::symlink_metadata(&target) {
            Ok(metadata) => anyhow::ensure!(
                metadata.is_file() && !is_link(&metadata),
                "目标文件已存在，拒绝覆盖: {}",
                target.display()
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.into()),
        }
        pairs.push((file, target));
    }
    let mut rollback = PublicationRollback(Vec::new());
    for (from, to) in pairs {
        if reusable_sidecar(&to) {
            continue;
        }
        let m = std::fs::symlink_metadata(from)?;
        anyhow::ensure!(m.is_file() && !is_link(&m), "暂存文件无效");
        if std::fs::symlink_metadata(&to).is_ok() {
            replace_existing_file(from, &to, &mut rollback)?;
            continue;
        }
        let mut input = std::fs::File::open(from)?;
        let mut output = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&to)
        {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && reusable_sidecar(&to) => {
                continue;
            }
            Err(e) => return Err(e).context("发布失败，目标可能已存在"),
        };
        rollback.0.push(PublishedFile {
            destination: to,
            backup: None,
        });
        let result = std::io::copy(&mut input, &mut output).and_then(|_| output.sync_all());
        drop(output);
        result?;
    }
    rollback.finish();
    Ok(())
}
fn extension(f: &str) -> &'static str {
    match f.to_ascii_lowercase().as_str() {
        "alac" | "aac" | "m4a" => "m4a",
        "mp3" => "mp3",
        "flac" => "flac",
        "ape" => "ape",
        "wav" => "wav",
        "opus" => "opus",
        "ogg" => "ogg",
        _ => "bin",
    }
}
fn absolute_ffmpeg(raw: &str) -> Result<PathBuf> {
    let p = Path::new(raw);
    if p.is_absolute() {
        anyhow::ensure!(p.is_file(), "FFmpeg 文件不存在");
        return Ok(p.canonicalize()?);
    }
    anyhow::ensure!(p.components().count() == 1, "FFmpeg 路径无效");
    for d in std::env::split_paths(&std::env::var_os("PATH").context("系统 PATH 中找不到 FFmpeg")?)
    {
        if !d.is_absolute() {
            continue;
        }
        let c = d.join(p);
        if c.is_file() {
            return Ok(c.canonicalize()?);
        }
        #[cfg(windows)]
        {
            let c = d.join(format!("{raw}.exe"));
            if c.is_file() {
                return Ok(c.canonicalize()?);
            }
        }
    }
    bail!("系统 PATH 中找不到 FFmpeg")
}
fn quality_candidates(maximum: &str) -> Result<&'static [&'static str]> {
    const LEVELS: &[&str] = &["hires", "lossless", "exhigh", "standard"];
    let index = LEVELS
        .iter()
        .position(|level| *level == maximum)
        .ok_or_else(|| anyhow::anyhow!("NETEASE_REQUEST"))?;
    Ok(&LEVELS[index..])
}

async fn first_available_quality<T, F, Fut>(maximum: &str, mut attempt: F) -> Result<(T, String)>
where
    F: FnMut(&'static str, DownloadApi) -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let mut last_rejection = None;
    for candidate in quality_candidates(maximum)? {
        for api in DownloadApi::ALL {
            match attempt(candidate, api).await {
                Ok(value) => return Ok((value, candidate.to_string())),
                Err(error) => {
                    if error.is::<Cancelled>() {
                        return Err(error);
                    }
                    last_rejection = Some(error.to_string());
                    let message = format!("{error:#}");
                    if [
                        "NETEASE_AUTH_REQUIRED",
                        "NETEASE_API_UNAVAILABLE",
                        "NETEASE_FILE_IO",
                        "NETEASE_REQUEST",
                        "NETEASE_STORAGE",
                    ]
                    .iter()
                    .any(|code| message.contains(code))
                    {
                        return Err(error);
                    }
                }
            }
        }
        if last_rejection.as_deref() == Some("NETEASE_DOWNLOAD_UNAVAILABLE") {
            bail!("NETEASE_DOWNLOAD_UNAVAILABLE");
        }
    }
    if let Some(code @ ("NETEASE_DOWNLOAD_UNAVAILABLE" | "NETEASE_DOWNLOAD_NOT_PERMITTED")) =
        last_rejection.as_deref()
    {
        bail!("{code}");
    }
    bail!("NETEASE_ALL_QUALITIES_FAILED")
}

// Song metadata uses only this explicit boundary; names such as AC/DC stay intact.
// Format tags without mutating the song used by the cache, search, or failure logs.
fn metadata_artist(config: &AppConfig, artist: &str) -> String {
    artist
        .split(" / ")
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .collect::<Vec<_>>()
        .join(config.normalized_artist_separator())
}

async fn write_metadata(
    path: &Path,
    ffmpeg: &Path,
    config: &AppConfig,
    song: &SongDetail,
    cover: Option<&Path>,
    quality: &str,
    cancel: &Cancellation,
) -> Result<()> {
    let date = metadata_date(&config.id3_date_format, &song.publish);
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("m4a");
    let temp = path.with_file_name(format!(
        ".tag-{}-{}.{}",
        std::process::id(),
        JOB_ID.fetch_add(1, Ordering::Relaxed),
        ext
    ));
    let mut cmd = ffmpeg_command(ffmpeg);
    cmd.args(["-nostdin", "-y", "-i"]).arg(path);
    if let Some(cover) = cover {
        cmd.arg("-i").arg(cover);
    }
    cmd.args(["-map", "0:a:0", "-c:a", "copy", "-map_metadata", "0"]);
    if cover.is_some() {
        cmd.args([
            "-map",
            "1:v:0",
            "-c:v",
            "mjpeg",
            "-disposition:v:0",
            "attached_pic",
            "-metadata:s:v:0",
            "title=Album cover",
            "-metadata:s:v:0",
            "comment=Cover (front)",
        ]);
    }
    if ext.eq_ignore_ascii_case("mp3") {
        cmd.args(["-id3v2_version", "3"]);
    }
    cmd.arg("-metadata")
        .arg(format!("comment=NetEase quality: {quality}"));
    for (key, value) in [
        ("title", song.title.clone()),
        ("artist", metadata_artist(config, &song.artist)),
        // FFmpeg maps album_artist to Vorbis ALBUMARTIST, ID3 TPE2 and MP4 aART.
        ("album_artist", "铁痕电台-MSR".to_string()),
        ("album", song.album_title.clone()),
        ("track", song.track.to_string()),
        ("disc", song.disc.to_string()),
    ] {
        cmd.arg("-metadata").arg(format!("{key}={value}"));
    }
    if let Some(date) = date {
        cmd.arg("-metadata").arg(format!("date={date}"));
    }
    let status = process_output(
        cmd.args(["-loglevel", "error"])
            .arg(&temp)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null()),
        cancel,
        Duration::from_secs(600),
    )
    .await?
    .status;
    anyhow::ensure!(
        status.success() && std::fs::metadata(&temp).is_ok_and(|m| m.len() > 0),
        "NETEASE_METADATA_FAILED"
    );
    // Both paths are private files inside this job; never delete a published target.
    let staged = path.canonicalize()?;
    let replacement = temp.canonicalize()?;
    anyhow::ensure!(
        staged.parent() == replacement.parent(),
        "元数据暂存路径越界"
    );
    std::fs::remove_file(&staged).context("移除原始暂存音频失败")?;
    std::fs::rename(replacement, staged).context("替换元数据文件失败")?;
    Ok(())
}
async fn download_one(
    app: &AppHandle,
    config: &AppConfig,
    song: &SongDetail,
    ffmpeg: &Path,
    batch_id: &str,
    cancel: &Cancellation,
) -> Result<String> {
    let mut enriched = match cancel
        .run(crate::netease::fetch_song_detail(app, song))
        .await
    {
        Ok(detail) => detail,
        Err(error) => {
            if error.is::<Cancelled>() {
                return Err(error);
            }
            emit_optional_warning(app, batch_id, song, "detail", &error);
            song.clone()
        }
    };
    enriched.download = song.download;
    let song = &enriched;
    anyhow::ensure!(
        song.duration_ms > 0,
        "歌曲缺少目录时长，请刷新目录后重新下载"
    );
    anyhow::ensure!(!config.download_path.trim().is_empty(), "请先设置下载目录");
    quality_candidates(config.audio_quality.trim())?;
    // Keep preparation in the application cache. Do not touch the user's output
    // root or album directory until a complete, tagged file is ready to publish.
    let cache = app.path().app_cache_dir().context("NETEASE_STORAGE")?;
    tokio::fs::create_dir_all(&cache)
        .await
        .context("NETEASE_STORAGE")?;
    let cache = cache.canonicalize().context("NETEASE_STORAGE")?;
    let stem = track_file_stem(song);
    let ((job, audio, format), quality) = first_available_quality(config.audio_quality.trim(), |candidate, api| {
        let cache = &cache;
        let stem = &stem;
        async move {
            let job = staging(cache).context("NETEASE_STORAGE")?;
            let _ = app.emit("download-progress", json!({"batch_id":batch_id,"song_id":song.id,"song_title":song.title,"stage":"quality","quality":candidate}));
            let (audio, format) = download_candidate(app, song, ffmpeg, candidate, api, stem, &job.0, batch_id, cancel).await?;
            Ok((job, audio, format))
        }
    }).await?;
    // Artwork belongs to the file even when artwork display is disabled in the UI.
    let cover: Result<Option<PathBuf>> = async {
        if song.artwork_url.is_empty() {
            return Ok(None);
        }
        let original = cancel
            .run(crate::cover::get_cover_path(
                app,
                &song.album_id,
                &song.artwork_url,
            ))
            .await?;
        let target = job.0.join("cover.jpg");
        let status = process_output(
            ffmpeg_command(ffmpeg)
                .args(["-nostdin", "-y", "-i"])
                .arg(original)
                .args([
                    "-vf",
                    "scale=1200:1200:force_original_aspect_ratio=decrease",
                    "-frames:v",
                    "1",
                    "-update",
                    "1",
                    "-q:v",
                    "2",
                    "-loglevel",
                    "error",
                ])
                .arg(&target)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null()),
            cancel,
            Duration::from_secs(600),
        )
        .await?
        .status;
        anyhow::ensure!(status.success() && is_cover(&target), "封面转换失败");
        Ok(Some(target))
    }
    .await;
    let cover = match cover {
        Ok(cover) => cover,
        Err(error) => {
            if error.is::<Cancelled>() {
                return Err(error);
            }
            emit_optional_warning(app, batch_id, song, "cover", &error);
            None
        }
    };
    let supports_cover = matches!(extension(&format), "flac" | "mp3" | "m4a");
    write_metadata(
        &audio,
        ffmpeg,
        config,
        song,
        cover.as_deref().filter(|_| supports_cover),
        &quality,
        cancel,
    )
    .await
    .context("NETEASE_METADATA_FAILED")?;
    validate_audio(&audio, ffmpeg, &format, song.duration_ms, cancel)
        .await
        .context("NETEASE_AUDIO_INVALID")?;
    let mut files = vec![audio];
    if config.show_cover {
        if let Some(cover) = cover {
            files.push(cover);
        }
    }
    if config.download_lyrics {
        let result: Result<Option<PathBuf>> = async {
            if let Some(lyrics) = cancel
                .run(crate::netease::fetch_lyric(app, &song.id))
                .await?
            {
                if !lyrics.trim().is_empty() {
                    let lrc = job.0.join(format!("{stem}.lrc"));
                    std::fs::write(&lrc, lyrics)?;
                    return Ok(Some(lrc));
                }
            }
            Ok(None)
        }
        .await;
        match result {
            Ok(Some(lrc)) => files.push(lrc),
            Ok(None) => (),
            Err(error) => {
                if error.is::<Cancelled>() {
                    return Err(error);
                }
                emit_optional_warning(app, batch_id, song, "lyrics", &error);
            }
        }
    }
    commit_download(cancel, || {
        publish_completed_download(
            Path::new(&config.download_path),
            config,
            song,
            &job.0,
            &files,
        )
    })?;
    Ok(quality)
}

fn publish_completed_download(
    root_path: &Path,
    config: &AppConfig,
    song: &SongDetail,
    staging: &Path,
    files: &[PathBuf],
) -> Result<()> {
    anyhow::ensure!(
        !files.is_empty() && files.iter().all(|file| file.is_file()),
        "NETEASE_FILE_IO"
    );
    std::fs::create_dir_all(root_path).context("NETEASE_FILE_IO")?;
    let root = root_path.canonicalize().context("NETEASE_FILE_IO")?;
    let dest = album_output_dir(&root, config, song).context("NETEASE_FILE_IO")?;
    publish_files(staging, &dest, files).map_err(|error| {
        if error.to_string().contains("已存在") {
            anyhow::anyhow!("NETEASE_OUTPUT_EXISTS")
        } else {
            error.context("NETEASE_FILE_IO")
        }
    })
}

async fn download_candidate(
    app: &AppHandle,
    song: &SongDetail,
    ffmpeg: &Path,
    quality: &str,
    api: DownloadApi,
    stem: &str,
    staging: &Path,
    batch_id: &str,
    cancel: &Cancellation,
) -> Result<(PathBuf, String)> {
    let source = cancel
        .run(crate::netease::fetch_song_source(
            app, &song.id, quality, api,
        ))
        .await?;
    validate_source_quality(quality, &source.quality, &source.format, &source.url)?;
    let audio = staging.join(format!("{stem}.{}", extension(&source.format)));
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(300))
        .build()?;
    let response = cancel
        .run(async {
            client
                .get(&source.url)
                .send()
                .await
                .map_err(|_| anyhow::anyhow!("NETEASE_NETWORK"))
        })
        .await?;
    anyhow::ensure!(response.status().is_success(), "NETEASE_SOURCE_REJECTED");
    let mut last_progress = std::time::Instant::now();
    receive_audio(response, &audio, cancel, |received, size| {
        if last_progress.elapsed() >= Duration::from_millis(250) {
            let progress = size
                .filter(|n| *n > 0)
                .map(|n| (received.saturating_mul(85) / n).min(85))
                .unwrap_or(1);
            let _ = app.emit(
                "download-single-progress",
                json!({"batch_id":batch_id,"song_id":song.id,"progress":progress,"stage":"download","quality":quality}),
            );
            last_progress = std::time::Instant::now();
        }
    }).await?;
    let _ = app.emit("download-progress", json!({"batch_id":batch_id,"song_id":song.id,"song_title":song.title,"stage":"processing","quality":quality}));
    let _ = app.emit("download-single-progress", json!({"batch_id":batch_id,"song_id":song.id,"progress":85,"stage":"processing","quality":quality}));
    validate_audio(&audio, ffmpeg, &source.format, song.duration_ms, cancel)
        .await
        .context("NETEASE_AUDIO_INVALID")?;
    Ok((audio, source.format))
}
async fn receive_audio(
    response: reqwest::Response,
    audio: &Path,
    cancel: &Cancellation,
    mut progress: impl FnMut(u64, Option<u64>),
) -> Result<()> {
    let size = response.content_length();
    let mut received = 0_u64;
    let mut chunks = response.bytes_stream();
    // These short writes are synchronous so no detached Tokio filesystem task
    // can retain a Windows handle after cancellation drops the staging guard.
    let mut out = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(audio)
        .context("NETEASE_FILE_IO")?;
    while let Some(chunk) = cancel.run(async { Ok(chunks.next().await) }).await? {
        let chunk = chunk.map_err(|_| anyhow::anyhow!("NETEASE_NETWORK"))?;
        std::io::Write::write_all(&mut out, &chunk).context("NETEASE_FILE_IO")?;
        received = received.saturating_add(chunk.len() as u64);
        progress(received, size);
    }
    out.sync_all().context("NETEASE_FILE_IO")?;
    drop(out);
    cancel.check()?;
    anyhow::ensure!(
        received > 0 && size.is_none_or(|size| size == received),
        "NETEASE_AUDIO_INVALID"
    );
    Ok(())
}
fn commit_download<T>(cancel: &Cancellation, commit: impl FnOnce() -> Result<T>) -> Result<T> {
    cancel.check()?;
    let result = commit()?;
    // The synchronous commit cannot be interrupted, even if another thread
    // requests cancellation during the copy. A published file is successful.
    cancel.finish();
    Ok(result)
}
fn emit_optional_warning(
    app: &AppHandle,
    batch_id: &str,
    song: &SongDetail,
    artifact: &str,
    error: &anyhow::Error,
) {
    let _ = app.emit(
        "download-progress",
        json!({
            "batch_id": batch_id,
            "song_id": song.id,
            "song_title": song.title,
            "stage": "warning",
            "artifact": artifact,
            "warning": safe_diagnostic(&format!("可选文件未下载（{artifact}）: {error:#}"))
        }),
    );
}
fn safe_diagnostic(text: &str) -> String {
    text.lines()
        .map(|line| {
            let lower = line.to_ascii_lowercase();
            if [
                "https://",
                "http://",
                "cookie",
                "token",
                "authorization",
                "password",
                "secret",
            ]
            .iter()
            .any(|needle| lower.contains(needle))
            {
                "[音源地址或凭据已隐藏]".to_string()
            } else {
                line.chars().take(1000).collect()
            }
        })
        .take(8)
        .collect::<Vec<_>>()
        .join("\n")
}
// Expose completions as soon as each job finishes, independently of queue order.
fn stream_download_results<S>(jobs: S, concurrency: usize) -> futures::stream::BufferUnordered<S>
where
    S: futures::Stream,
    S::Item: std::future::Future,
{
    jobs.buffer_unordered(concurrency)
}

pub(crate) fn emit_cancelled(app: &AppHandle, batch_id: &str, id: &str, title: Option<&str>) {
    let _ = app.emit("download-progress", json!({"batch_id":batch_id,"song_id":id,"song_title":title,"success":false,"stage":"cancelled"}));
    let _ = app.emit(
        "download-single-progress",
        json!({"batch_id":batch_id,"song_id":id,"progress":0,"stage":"cancelled"}),
    );
}

fn select_download_queue(
    config: &AppConfig,
    songs: &[SongDetail],
    song_ids: Option<&[String]>,
) -> (Vec<SongDetail>, Vec<(String, String)>) {
    match song_ids {
        Some(ids) => {
            let mut seen = HashSet::new();
            let mut queue = Vec::new();
            let mut excluded = Vec::new();
            for id in ids.iter().filter(|id| seen.insert(id.as_str())) {
                match songs.iter().find(|song| song.id == *id) {
                    Some(song) if config.download_instrumental || !song.is_instrumental() => {
                        queue.push(song.clone())
                    }
                    song => excluded.push((
                        id.clone(),
                        song.map(|song| song.title.clone())
                            .unwrap_or_else(|| id.clone()),
                    )),
                }
            }
            (queue, excluded)
        }
        None => (
            songs
                .iter()
                .filter(|song| {
                    !song.download && (config.download_instrumental || !song.is_instrumental())
                })
                .cloned()
                .collect(),
            Vec::new(),
        ),
    }
}

pub async fn download_music(
    app: &AppHandle,
    batch_id: &str,
    batch: &Batch,
    song_ids: Option<&[String]>,
) -> Result<(usize, usize, usize)> {
    let config = load_app_config(app).await?;
    let mut songs = load_music_data_for(app, &config).await?;
    let (queue, excluded) = select_download_queue(&config, &songs, song_ids);
    // Every explicit ID already exists in the frontend queue. Cache changes or
    // settings changes must settle excluded entries rather than leave them queued.
    let excluded_count = excluded.len();
    for (id, title) in excluded {
        let token = batch.song(&id);
        token.cancel();
        token.finish();
        emit_cancelled(app, batch_id, &id, Some(&title));
    }
    let total = queue.len();
    if total == 0 {
        return Ok((0, 0, excluded_count));
    }
    // Register the entire queue before any authentication/setup await.
    for song in &queue {
        batch.song(&song.id);
    }
    let all_cancelled = queue
        .iter()
        .all(|song| batch.song(&song.id).check().is_err());
    let setup: Result<PathBuf> = async {
        if all_cancelled {
            return Ok(PathBuf::new());
        }
        let tokens: Vec<_> = queue.iter().map(|song| batch.song(&song.id)).collect();
        let auth = tokio::select! {
            biased;
            _ = futures::future::join_all(tokens.iter().map(|token| token.cancelled())) => None,
            auth = crate::netease::auth_status(app) => Some(auth),
        };
        if let Some(auth) = auth {
            anyhow::ensure!(auth?.logged_in, "请先登录网易云音乐账号");
            absolute_ffmpeg(&crate::ffmpeg::get_ffmpeg_path(app)?)
        } else {
            Ok(PathBuf::new())
        }
    }
    .await;
    let ffmpeg = match setup {
        Ok(ffmpeg) => ffmpeg,
        Err(error) => {
            let ids: Vec<_> = queue.iter().map(|song| song.id.clone()).collect();
            let cancelled = batch.finish_initialization_failure(&ids);
            for id in &cancelled {
                let title = queue
                    .iter()
                    .find(|song| song.id == *id)
                    .map(|song| song.title.as_str());
                emit_cancelled(app, batch_id, id, title);
            }
            if cancelled.len() == total {
                return Ok((0, 0, total + excluded_count));
            }
            return Err(error);
        }
    };
    let concurrency = config.concurrent_downloads.clamp(1, 10);
    let slots = std::sync::Arc::new(tokio::sync::Semaphore::new(concurrency));
    let jobs = stream::iter(queue.into_iter().enumerate()).map(|(i, song)| {
        let app = app.clone();
        let config = config.clone();
        let ffmpeg = ffmpeg.clone();
        let cancel = batch.song(&song.id);
        let slots = slots.clone();
        async move {
            let result: Result<String> = async {
                let _permit = cancel.run(async { Ok(slots.acquire().await?) }).await?;
                cancel.check()?;
                let _ = app.emit("download-progress", json!({"batch_id":batch_id,"current":i+1,"total":total,"song_id":song.id,"song_title":song.title,"stage":"start"}));
                let _ = app.emit("download-single-progress", json!({"batch_id":batch_id,"song_id":song.id,"progress":0,"stage":"start"}));
                download_one(&app, &config, &song, &ffmpeg, batch_id, &cancel).await
            }.await;
            let result = cancel.finish_result(result);
            let result = result.map_err(|error| {
                if error.is::<Cancelled>() { error }
                else { anyhow::anyhow!(safe_diagnostic(&format!("{error:#}"))) }
            });
            (song.id, song.title, result)
        }
    });
    // Poll queue waiters as well as active work, so cancellation is reported
    // immediately even when every execution slot is occupied by slow songs.
    let mut results = stream_download_results(jobs, total);
    let (mut success, mut failed, mut cancelled) = (0, 0, excluded_count);
    let mut failure_log: Option<FailureLog> = None;
    let mut persistence_error = None;
    while let Some((id, title, result)) = results.next().await {
        match result {
            Ok(quality) => {
                if let Some(song) = songs.iter_mut().find(|song| song.id == id) {
                    song.download = true;
                    song.downloaded_quality = quality.clone();
                }
                // Persist and notify this song now, without waiting for slower jobs.
                if let Err(error) = save_music_data_for(app, &config, &songs).await {
                    if persistence_error.is_none() {
                        persistence_error = Some(error);
                    }
                    // Drain cooperative cancellations rather than dropping active
                    // process futures while they still own staging file handles.
                    batch.cancel_all();
                }
                success += 1;
                let _ = app.emit("download-progress", json!({"batch_id":batch_id,"song_id":id,"song_title":title,"success":true,"stage":"complete","quality":quality}));
                let _ = app.emit(
                    "download-single-progress",
                    json!({"batch_id":batch_id,"song_id":id,"progress":100,"stage":"complete"}),
                );
            }
            Err(error) if error.is::<Cancelled>() => {
                cancelled += 1;
                emit_cancelled(app, batch_id, &id, Some(&title));
            }
            Err(error) => {
                failed += 1;
                let log_result: Result<PathBuf> = (|| {
                    if failure_log.is_none() {
                        failure_log = Some(FailureLog::create(Path::new(&config.download_path))?);
                    }
                    let log = failure_log.as_mut().context("无法创建错误日志")?;
                    let song = songs
                        .iter()
                        .find(|song| song.id == id)
                        .context("歌曲记录不存在")?;
                    log.record(song, &error.to_string())?;
                    Ok(log.path.clone())
                })();
                let (log_path, log_error) = match log_result {
                    Ok(path) => (Some(path.to_string_lossy().into_owned()), None),
                    Err(error) => (
                        None,
                        Some(safe_diagnostic(&format!("无法保存错误日志：{error:#}"))),
                    ),
                };
                let _ = app.emit("download-progress", json!({"batch_id":batch_id,"song_id":id,"song_title":title,"success":false,"stage":"error","error":error.to_string(),"log_path":log_path,"log_error":log_error}));
                let _ = app.emit(
                    "download-single-progress",
                    json!({"batch_id":batch_id,"song_id":id,"progress":100,"stage":"error"}),
                );
            }
        }
    }
    if let Some(error) = persistence_error {
        return Err(error);
    }
    Ok((success, failed, cancelled))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn test_root(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "msr-{label}-{}-{}",
            std::process::id(),
            JOB_ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        root.canonicalize().unwrap()
    }

    #[test]
    fn explicit_queue_accounts_for_missing_filtered_and_duplicate_ids() {
        let songs = vec![
            SongDetail {
                id: "valid".into(),
                title: "Valid song".into(),
                ..Default::default()
            },
            SongDetail {
                id: "filtered".into(),
                title: "Instrumental".into(),
                instrumental: true,
                ..Default::default()
            },
        ];
        let ids = vec![
            "valid".into(),
            "missing".into(),
            "filtered".into(),
            "missing".into(),
            "valid".into(),
        ];
        let (queue, excluded) = select_download_queue(&AppConfig::default(), &songs, Some(&ids));
        assert_eq!(
            queue
                .iter()
                .map(|song| song.id.as_str())
                .collect::<Vec<_>>(),
            vec!["valid"]
        );
        assert_eq!(
            excluded,
            vec![
                ("missing".into(), "missing".into()),
                ("filtered".into(), "Instrumental".into())
            ]
        );
        assert_eq!(queue.len() + excluded.len(), 3);
        let ids = vec!["missing".into(), "filtered".into(), "missing".into()];
        let (queue, excluded) = select_download_queue(&AppConfig::default(), &songs, Some(&ids));
        assert!(queue.is_empty());
        assert_eq!(excluded.len(), 2);
    }

    #[tokio::test]
    async fn queued_cancel_is_consumed_before_unrelated_slow_download() {
        use std::{future::Future, pin::Pin};
        let slots = tokio::sync::Semaphore::new(1);
        let _occupied = slots.acquire().await.unwrap();
        let queued = Cancellation::default();
        let (release, slow) = tokio::sync::oneshot::channel::<()>();
        let pending = Box::pin(async {
            slow.await.unwrap();
            "slow"
        }) as Pin<Box<dyn Future<Output = &str>>>;
        let cancelled = Box::pin(async {
            let error = queued
                .run(async {
                    let _permit = slots.acquire().await?;
                    Ok(())
                })
                .await
                .unwrap_err();
            assert!(error.is::<Cancelled>());
            "cancelled"
        }) as Pin<Box<dyn Future<Output = &str>>>;
        let mut results = stream_download_results(stream::iter([pending, cancelled]), 2);
        let cancel = async {
            tokio::task::yield_now().await;
            queued.cancel();
        };
        let (first, _) = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(results.next(), cancel)
        })
        .await
        .unwrap();
        assert_eq!(first, Some("cancelled"));
        release.send(()).unwrap();
        assert_eq!(results.next().await, Some("slow"));
    }

    #[tokio::test]
    async fn cancelling_real_network_stream_closes_connection_and_removes_staging() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let root = test_root("cancel-network");
        let job = staging(&root).unwrap();
        let staged = job.0.clone();
        let audio = staged.join("partial.flac");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (closed_tx, closed_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            loop {
                let byte = socket.read_u8().await.unwrap();
                request.push(byte);
                if request.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 999999\r\n\r\npartial audio")
                .await
                .unwrap();
            let mut byte = [0];
            let disconnected = socket.read(&mut byte).await;
            assert!(matches!(disconnected, Ok(0) | Err(_)));
            closed_tx.send(()).unwrap();
        });
        let response = reqwest::get(format!("http://{address}/audio"))
            .await
            .unwrap();
        let token = Cancellation::default();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let mut ready_tx = Some(ready_tx);
        let transfer = async {
            let result = receive_audio(response, &audio, &token, |_, _| {
                if let Some(tx) = ready_tx.take() {
                    tx.send(()).unwrap();
                }
            })
            .await;
            drop(job);
            result
        };
        let cancel = async {
            ready_rx.await.unwrap();
            token.cancel();
        };
        let (result, _) = tokio::time::timeout(Duration::from_secs(3), async {
            tokio::join!(transfer, cancel)
        })
        .await
        .unwrap();
        assert!(result.unwrap_err().is::<Cancelled>());
        assert!(!staged.exists());
        assert!(!root.join(".metal-scar/logs").exists());
        tokio::time::timeout(Duration::from_secs(3), closed_rx)
            .await
            .unwrap()
            .unwrap();
        server.await.unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn cancelling_child_waits_for_exit_before_staging_cleanup() {
        let root = test_root("cancel-child");
        let job = staging(&root).unwrap();
        let staged = job.0.clone();
        let ready = staged.join("ready");
        let locked = staged.join("locked");
        let completed = root.join("completed.flac");
        std::fs::write(&completed, b"complete").unwrap();
        #[cfg(windows)]
        let mut command = {
            let executable = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
                .join("System32/WindowsPowerShell/v1.0/powershell.exe");
            let mut command = ffmpeg_command(&executable);
            let quote = |p: &Path| p.to_string_lossy().replace('\'', "''");
            command.args(["-NoProfile", "-NonInteractive", "-Command", &format!("$f=[System.IO.File]::Open('{}','Create','ReadWrite','None'); [System.IO.File]::WriteAllText('{}','ready'); Start-Sleep -Seconds 120", quote(&locked), quote(&ready))]);
            command
        };
        #[cfg(not(windows))]
        let mut command = {
            let mut command = ffmpeg_command(Path::new("/bin/sh"));
            command
                .arg("-c")
                .arg("echo ready > \"$1\"; exec 3>\"$2\"; while :; do :; done")
                .arg("test")
                .arg(&ready)
                .arg(&locked);
            command
        };
        command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let token = Cancellation::default();
        let work = async {
            let result = process_output(&mut command, &token, Duration::from_secs(120)).await;
            drop(job);
            result
        };
        let cancel = async {
            while !ready.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            token.cancel();
        };
        let (result, _) = tokio::time::timeout(Duration::from_secs(15), async {
            tokio::join!(work, cancel)
        })
        .await
        .unwrap();
        assert!(result.unwrap_err().is::<Cancelled>());
        assert!(
            !staged.exists(),
            "child must release file handles before Staging is dropped"
        );
        assert_eq!(std::fs::read(&completed).unwrap(), b"complete");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn completed_synchronous_commit_wins_over_cancellation() {
        let root = test_root("commit-cancel");
        let job = staging(&root).unwrap();
        let source = job.0.join("audio.flac");
        std::fs::write(&source, b"valid audio").unwrap();
        let destination = root.join("album");
        std::fs::create_dir(&destination).unwrap();
        let token = Cancellation::default();
        commit_download(&token, || {
            assert!(token.cancel()); // Simulate a request arriving during commit.
            publish_files(&job.0, &destination, &[source])
        })
        .unwrap();
        assert!(!token.cancel());
        assert!(token.check().is_ok());
        assert_eq!(
            std::fs::read(destination.join("audio.flac")).unwrap(),
            b"valid audio"
        );
        drop(job);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn cancellation_bypasses_quality_fallback() {
        let mut attempts = 0;
        let result = first_available_quality::<(), _, _>("hires", |_, _| {
            attempts += 1;
            async { Err(Cancelled.into()) }
        })
        .await;
        assert!(result.unwrap_err().is::<Cancelled>());
        assert_eq!(attempts, 1);
    }

    #[test]
    fn failure_log_lists_each_failed_song_without_credentials_or_album_folders() {
        let base = std::env::temp_dir().join(format!(
            "msr-failure-log-{}-{}",
            std::process::id(),
            JOB_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let mut log = FailureLog::create(&base).unwrap();
        let song = SongDetail {
            id: "123".into(),
            title: "Failed Song".into(),
            artist: "Artist".into(),
            album_title: "Album".into(),
            ..Default::default()
        };
        log.record(&song, "NETEASE_DOWNLOAD_UNAVAILABLE").unwrap();
        log.record(
            &SongDetail {
                id: "456".into(),
                title: "Another Song".into(),
                ..Default::default()
            },
            "request failed https://host/audio?token=private",
        )
        .unwrap();
        let text = std::fs::read_to_string(&log.path).unwrap();
        assert!(
            text.contains("Failed Song")
                && text.contains("123")
                && text.contains("Artist")
                && text.contains("Album")
        );
        assert!(text.contains("Another Song") && text.contains("456") && text.contains("-103"));
        assert!(!text.contains("private"));
        assert!(
            log.path.starts_with(
                base.canonicalize()
                    .unwrap()
                    .join(".metal-scar")
                    .join("logs")
            )
        );
        assert!(!base.join("Album").exists());
        drop(log);
        let cache = base.canonicalize().unwrap();
        let job = staging(&cache).unwrap();
        assert_eq!(
            job.0.parent().unwrap(),
            cache.join(".metal-scar").join("jobs")
        );
        assert!(!base.join(".msr-jobs").exists());
        drop(job);
        std::fs::remove_dir_all(&base).unwrap();
    }
    #[tokio::test]
    async fn finished_download_is_reported_while_an_earlier_job_is_still_running() {
        use std::future::Future;
        use std::pin::Pin;
        let (release, blocked) = tokio::sync::oneshot::channel::<()>();
        let slow = Box::pin(async move {
            blocked.await.unwrap();
            "slow"
        }) as Pin<Box<dyn Future<Output = &str>>>;
        let fast = Box::pin(async { "fast" }) as Pin<Box<dyn Future<Output = &str>>>;
        let mut results = stream_download_results(stream::iter([slow, fast]), 2);
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), results.next())
                .await
                .unwrap(),
            Some("fast")
        );
        release.send(()).unwrap();
        assert_eq!(results.next().await, Some("slow"));
        assert_eq!(results.next().await, None);
    }
    #[tokio::test]
    async fn fallback_tries_both_apis_before_downgrading_and_stops_on_success() {
        use DownloadApi::{Legacy, V1};
        let mut attempted = Vec::new();
        let ((), quality) = first_available_quality("hires", |level, api| {
            attempted.push((level, api));
            std::future::ready(if level == "lossless" && api == Legacy {
                Ok(())
            } else {
                Err(anyhow::anyhow!("NETEASE_SOURCE_REJECTED"))
            })
        })
        .await
        .unwrap();
        assert_eq!(quality, "lossless");
        assert_eq!(
            attempted,
            [
                ("hires", V1),
                ("hires", Legacy),
                ("lossless", V1),
                ("lossless", Legacy)
            ]
        );
        attempted.clear();
        let result: Result<((), String)> = first_available_quality("exhigh", |level, api| {
            attempted.push((level, api));
            std::future::ready(Err(anyhow::anyhow!("NETEASE_AUDIO_INVALID")))
        })
        .await;
        assert_eq!(
            result.unwrap_err().to_string(),
            "NETEASE_ALL_QUALITIES_FAILED"
        );
        assert_eq!(
            attempted,
            [
                ("exhigh", V1),
                ("exhigh", Legacy),
                ("standard", V1),
                ("standard", Legacy)
            ]
        );
        attempted.clear();
        let result: Result<((), String)> = first_available_quality("lossless", |level, api| {
            attempted.push((level, api));
            std::future::ready(Err(anyhow::anyhow!("NETEASE_DOWNLOAD_UNAVAILABLE")))
        })
        .await;
        assert_eq!(
            result.unwrap_err().to_string(),
            "NETEASE_DOWNLOAD_UNAVAILABLE"
        );
        assert_eq!(attempted, [("lossless", V1), ("lossless", Legacy)]);
        for code in [
            "NETEASE_AUTH_REQUIRED",
            "NETEASE_FILE_IO",
            "NETEASE_API_UNAVAILABLE",
            "NETEASE_STORAGE",
        ] {
            attempted.clear();
            let result: Result<((), String)> = first_available_quality("hires", |level, api| {
                attempted.push((level, api));
                std::future::ready(Err(anyhow::anyhow!(code)))
            })
            .await;
            assert_eq!(result.unwrap_err().to_string(), code);
            assert_eq!(attempted, [("hires", V1)]);
        }
    }
    #[test]
    fn output_root_is_created_only_when_completed_files_are_published() {
        let base = std::env::temp_dir().join(format!(
            "msr-publish-order-{}-{}",
            std::process::id(),
            JOB_ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&base).unwrap();
        let base = base.canonicalize().unwrap();
        let output = base.join("not-created-yet");
        let config = AppConfig::default();
        let song = SongDetail {
            album_title: "测试专辑".into(),
            ..Default::default()
        };
        let job = staging(&base).unwrap();
        let audio = job.0.join("01. test.mp3");
        assert!(!output.exists());
        assert!(
            publish_completed_download(&output, &config, &song, &job.0, &[audio.clone()]).is_err()
        );
        assert!(!output.exists());
        std::fs::write(&audio, b"completed file").unwrap();
        publish_completed_download(&output, &config, &song, &job.0, &[audio]).unwrap();
        assert!(output.join("测试专辑/01. test.mp3").is_file());
        drop(job);
        std::fs::remove_dir_all(base).unwrap();
    }
    #[test]
    fn artist_tags_use_exact_separator_without_changing_song_metadata() {
        let song = SongDetail {
            artist: "  AC/DC /  / Guest / 艺人  ".into(),
            ..Default::default()
        };
        for (separator, expected) in [
            (";", "AC/DC;Guest;艺人"),
            ("/", "AC/DC/Guest/艺人"),
            (",", "AC/DC,Guest,艺人"),
            ("、", "AC/DC、Guest、艺人"),
            (" & ", "AC/DC & Guest & 艺人"),
            ("invalid", "AC/DC;Guest;艺人"),
        ] {
            let config = AppConfig {
                artist_separator: separator.into(),
                ..Default::default()
            };
            assert_eq!(metadata_artist(&config, &song.artist), expected);
            assert_eq!(metadata_artist(&config, " /  / "), "");
            assert_eq!(
                metadata_artist(&config, " A/B,C;D、E & F "),
                "A/B,C;D、E & F"
            );
        }
        assert_eq!(song.artist, "  AC/DC /  / Guest / 艺人  ");
        assert_eq!(metadata_artist(&AppConfig::default(), "A / B"), "A;B");
    }

    #[test]
    fn quality_ceiling_never_tries_higher_levels() {
        assert_eq!(
            quality_candidates("hires").unwrap(),
            &["hires", "lossless", "exhigh", "standard"]
        );
        assert_eq!(
            quality_candidates("lossless").unwrap(),
            &["lossless", "exhigh", "standard"]
        );
        assert_eq!(
            quality_candidates("exhigh").unwrap(),
            &["exhigh", "standard"]
        );
        assert_eq!(quality_candidates("standard").unwrap(), &["standard"]);
        assert!(quality_candidates("unknown").is_err());
    }
    #[tokio::test]
    #[ignore = "requires system FFmpeg and FFprobe"]
    async fn embeds_front_cover_and_preserves_audio_and_tags() {
        let root = std::env::temp_dir().join(format!(
            "msr-artwork-{}-{}",
            std::process::id(),
            JOB_ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        let cover = root.join("cover.jpg");
        let ffmpeg = absolute_ffmpeg("ffmpeg").unwrap();
        let generated = ffmpeg_command(&ffmpeg)
            .args([
                "-nostdin",
                "-y",
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=c=red:s=32x32",
                "-frames:v",
                "1",
                "-update",
                "1",
            ])
            .arg(&cover)
            .output()
            .await
            .unwrap();
        assert!(generated.status.success());
        for (extension, codec, format, quality) in [
            ("flac", "flac", "flac", "lossless"),
            ("mp3", "libmp3lame", "mp3", "exhigh"),
            ("m4a", "aac", "aac", "standard"),
        ] {
            let audio = root.join(format!("test.{extension}"));
            let generated = ffmpeg_command(&ffmpeg)
                .args([
                    "-nostdin",
                    "-y",
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=frequency=440:duration=3",
                    "-c:a",
                    codec,
                ])
                .arg(&audio)
                .output()
                .await
                .unwrap();
            assert!(generated.status.success());
            let song = SongDetail {
                title: "知山水".into(),
                album_title: "专辑".into(),
                artist: " AC/DC / 铁痕电台 / Guest ".into(),
                track: 1,
                disc: 1,
                duration_ms: 3000,
                publish: "2026-01-02".into(),
                ..Default::default()
            };
            let config = AppConfig::default();
            write_metadata(
                &audio,
                &ffmpeg,
                &config,
                &song,
                Some(&cover),
                quality,
                &Cancellation::default(),
            )
            .await
            .unwrap();
            validate_audio(
                &audio,
                &ffmpeg,
                format,
                song.duration_ms,
                &Cancellation::default(),
            )
            .await
            .unwrap();
            let probe = ffmpeg_command(Path::new("ffprobe"))
                .args([
                    "-v",
                    "error",
                    "-show_streams",
                    "-show_format",
                    "-of",
                    "json",
                ])
                .arg(&audio)
                .output()
                .await
                .unwrap();
            assert!(probe.status.success());
            let metadata: serde_json::Value = serde_json::from_slice(&probe.stdout).unwrap();
            assert!(
                metadata["streams"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|stream| stream["disposition"]["attached_pic"] == 1)
            );
            let tags = &metadata["format"]["tags"];
            let tag = |name: &str| {
                tags.as_object().unwrap().iter().find_map(|(key, value)| {
                    key.eq_ignore_ascii_case(name)
                        .then(|| value.as_str())
                        .flatten()
                })
            };
            assert_eq!(
                tag("artist"),
                Some("AC/DC;铁痕电台;Guest"),
                "{extension}: {tags}"
            );
            assert_eq!(
                tag("album_artist"),
                Some("铁痕电台-MSR"),
                "{extension}: {tags}"
            );
            println!(
                "{extension}: artist={:?}, album_artist={:?}",
                tag("artist"),
                tag("album_artist")
            );
            assert_eq!(
                tags["title"].as_str().or_else(|| tags["TITLE"].as_str()),
                Some("知山水")
            );
            assert_eq!(
                tags["comment"]
                    .as_str()
                    .or_else(|| tags["COMMENT"].as_str()),
                Some(format!("NetEase quality: {quality}").as_str())
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn quality_fallback_and_lossless_codec_are_rejected() {
        assert!(validate_source_quality("lossless", "standard", "mp3", "https://host/a").is_err());
        assert!(validate_source_quality("lossless", "lossless", "mp3", "https://host/a").is_err());
        assert!(validate_source_quality("lossless", "lossless", "flac", "").is_err());
        assert!(validate_source_quality("lossless", "lossless", "flac", "https://host/a").is_ok());
    }
    #[test]
    fn duration_checks_reject_unknown_catalog_length_and_preview() {
        assert!(validate_duration(b"out_time_us=30000000\n", 180000).is_err());
        assert!(validate_duration(b"out_time_us=180000000\n", 0).is_err());
        assert!(validate_duration(b"out_time_us=179000000\n", 180000).is_ok());
    }
    #[test]
    fn publication_overwrites_audio_and_skips_existing_cover() {
        let root = std::env::temp_dir().join(format!(
            "msr-publish-{}-{}",
            std::process::id(),
            JOB_ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("dst")).unwrap();
        let source = root.join("src").canonicalize().unwrap();
        let dest = root.join("dst").canonicalize().unwrap();
        std::fs::write(source.join("a.mp3"), b"new").unwrap();
        std::fs::write(source.join("cover.jpg"), b"cover").unwrap();
        std::fs::write(dest.join("a.mp3"), b"old").unwrap();
        std::fs::write(dest.join("cover.jpg"), b"old-cover").unwrap();
        publish_files(
            &source,
            &dest,
            &[source.join("cover.jpg"), source.join("a.mp3")],
        )
        .unwrap();
        assert_eq!(std::fs::read(dest.join("a.mp3")).unwrap(), b"new");
        assert_eq!(std::fs::read(dest.join("cover.jpg")).unwrap(), b"old-cover");
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn input_codec_cannot_be_spoofed_by_metadata_or_decode_output() {
        let spoof = "Input #0, wav, from 'audio':\n  Metadata:\n    title: flac\n  Stream #0:0: Audio: aac (LC), 44100 Hz\nStream mapping:\n  Stream #0:0 -> #0:0\nOutput #0, null, to '-':\n  Stream #0:0: Audio: pcm_s16le, 44100 Hz\n";
        assert!(validate_input_format(spoof, "wav").is_err());
        assert!(validate_input_format(spoof, "flac").is_err());
        assert!(validate_input_format("Input #0, flac, from 'audio':\n  Stream #0:0: Audio: flac, 44100 Hz\nStream mapping:\n", "flac").is_ok());
        assert!(validate_source_quality("hires", "hires", "mp3", "https://host/a").is_err());
        assert!(
            !safe_diagnostic("request failed https://host/audio?token=private").contains("private")
        );
    }
    #[test]
    fn successful_multifile_publish_and_late_failure_rollback() {
        let root = std::env::temp_dir().join(format!(
            "msr-multi-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("dst")).unwrap();
        let source = root.join("src").canonicalize().unwrap();
        let dest = root.join("dst").canonicalize().unwrap();
        std::fs::write(source.join("audio.flac"), b"audio").unwrap();
        std::fs::write(source.join("audio.lrc"), b"lyrics").unwrap();
        std::fs::write(source.join("cover.jpg"), b"new-cover").unwrap();
        std::fs::write(dest.join("cover.jpg"), b"existing-cover").unwrap();
        publish_files(
            &source,
            &dest,
            &[
                source.join("audio.flac"),
                source.join("audio.lrc"),
                source.join("cover.jpg"),
            ],
        )
        .unwrap();
        assert_eq!(std::fs::read(dest.join("audio.flac")).unwrap(), b"audio");
        assert_eq!(std::fs::read(dest.join("audio.lrc")).unwrap(), b"lyrics");
        assert_eq!(
            std::fs::read(dest.join("cover.jpg")).unwrap(),
            b"existing-cover"
        );
        std::fs::write(source.join("other.flac"), b"other").unwrap();
        std::fs::write(source.join("audio.flac"), b"replacement audio").unwrap();
        assert!(
            publish_files(
                &source,
                &dest,
                &[
                    source.join("audio.flac"),
                    source.join("other.flac"),
                    source.join("missing.lrc"),
                ]
            )
            .is_err()
        );
        assert_eq!(std::fs::read(dest.join("audio.flac")).unwrap(), b"audio");
        assert!(!dest.join("other.flac").exists());
        std::fs::write(source.join("audio.lrc"), b"replacement lyrics").unwrap();
        publish_files(
            &source,
            &dest,
            &[source.join("other.flac"), source.join("audio.lrc")],
        )
        .unwrap();
        assert_eq!(std::fs::read(dest.join("other.flac")).unwrap(), b"other");
        assert_eq!(std::fs::read(dest.join("audio.lrc")).unwrap(), b"lyrics");
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn album_output_uses_root_directly_for_every_date_mode() {
        let root = std::env::temp_dir().join(format!(
            "msr-album-layout-{}-{}",
            std::process::id(),
            JOB_ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let song = SongDetail {
            title: "知山水".into(),
            album_title: "零号委托OST（下）".into(),
            artist: "塞壬唱片-MSR".into(),
            publish: "2026-03-31".into(),
            disc: 1,
            track: 1,
            ..Default::default()
        };
        for (mode, folder) in [
            ("none", "零号委托OST（下）"),
            ("year", "[2026] 零号委托OST（下）"),
            ("lite", "[260331] 零号委托OST（下）"),
            ("full", "[2026-03-31] 零号委托OST（下）"),
        ] {
            let config = AppConfig {
                custom_album: mode.into(),
                ..Default::default()
            };
            let dest = album_output_dir(&root, &config, &song).unwrap();
            assert_eq!(dest, root.join(folder));
            assert_eq!(
                dest.join(format!("{}.flac", track_file_stem(&song))),
                root.join(folder).join("01. 知山水.flac")
            );
        }
        assert!(!root.join(&song.artist).exists());
        let config = AppConfig {
            custom_album: "full".into(),
            ..Default::default()
        };
        let unknown_date = SongDetail {
            publish: "invalid".into(),
            ..song
        };
        assert_eq!(
            album_output_dir(&root, &config, &unknown_date).unwrap(),
            root.join("零号委托OST（下）")
        );
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn audio_and_lyrics_share_numbered_sanitized_stem_across_discs() {
        for (disc, title, expected) in [
            (0, "知山水", "01. 知山水"),
            (1, "知山水", "01. 知山水"),
            (2, "知山水", "2-01. 知山水"),
            (3, "山/水?", "3-01. 山_水_"),
            (1, "山水.live", "01. 山水.live"),
        ] {
            let song = SongDetail {
                title: title.into(),
                disc,
                track: 1,
                ..Default::default()
            };
            let stem = track_file_stem(&song);
            assert_eq!(stem, expected);
            let lyrics = PathBuf::from(format!("{stem}.lrc"));
            for format in ["flac", "alac", "mp3", "wav", "opus"] {
                let audio = PathBuf::from(format!("{stem}.{}", extension(format)));
                assert_eq!(audio.file_stem(), lyrics.file_stem());
            }
        }
    }
    #[test]
    fn date_and_album_folder_behavior_is_retained() {
        assert_eq!(
            metadata_date("full", "2026-03-31"),
            Some("2026-03-31".into())
        );
        assert_eq!(
            album_folder_template("full", "2026-03-31"),
            "[2026-03-31] {AlbumName}"
        );
    }
}
