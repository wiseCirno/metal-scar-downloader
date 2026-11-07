use anyhow::{Context, Result, anyhow, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager};

const MAX_SESSION_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, Serialize)]
pub struct AuthStatus {
    pub logged_in: bool,
    pub nickname: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AudioSource {
    pub url: String,
    pub quality: String,
    pub format: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadApi {
    V1,
    Legacy,
}
impl DownloadApi {
    pub const ALL: [Self; 2] = [Self::V1, Self::Legacy];
    pub fn method(self) -> &'static str {
        match self {
            Self::V1 => "song_download_url_v1",
            Self::Legacy => "song_download_url",
        }
    }
    fn query(self, song_id: &str, level: &str, cookie: &str) -> Result<Value> {
        ensure!(
            matches!(level, "standard" | "exhigh" | "lossless" | "hires"),
            "NETEASE_QUALITY_UNSUPPORTED"
        );
        Ok(match self {
            Self::V1 => json!({"id":song_id,"level":level,"cookie":cookie}),
            Self::Legacy => {
                let bitrate = match level {
                    "standard" => 128000,
                    "exhigh" => 320000,
                    _ => 999000,
                };
                json!({"id":song_id,"br":bitrate,"cookie":cookie})
            }
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct Session {
    cookie: String,
    #[serde(default)]
    nickname: String,
    #[serde(default)]
    phone: String,
}

fn storage_dir(app: &AppHandle) -> Result<PathBuf> {
    let app_data = app
        .path()
        .app_data_dir()
        .map_err(|_| anyhow!("NETEASE_STORAGE"))?;
    fs::create_dir_all(&app_data).map_err(|_| anyhow!("NETEASE_STORAGE"))?;
    let app_data_meta = fs::symlink_metadata(&app_data).map_err(|_| anyhow!("NETEASE_STORAGE"))?;
    ensure!(
        !is_link_or_reparse(&app_data_meta) && app_data_meta.is_dir(),
        "NETEASE_STORAGE"
    );
    let canonical_base = app_data
        .canonicalize()
        .map_err(|_| anyhow!("NETEASE_STORAGE"))?;
    let dir = app_data.join("netease");
    match fs::create_dir(&dir) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => bail!("NETEASE_STORAGE"),
    }
    let metadata = fs::symlink_metadata(&dir).map_err(|_| anyhow!("NETEASE_STORAGE"))?;
    ensure!(
        !is_link_or_reparse(&metadata) && metadata.is_dir(),
        "NETEASE_STORAGE"
    );
    let canonical = dir.canonicalize().map_err(|_| anyhow!("NETEASE_STORAGE"))?;
    ensure!(canonical.starts_with(&canonical_base), "NETEASE_STORAGE");
    Ok(canonical)
}

#[cfg(unix)]
fn is_link_or_reparse(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

#[cfg(windows)]
fn is_link_or_reparse(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.file_type().is_symlink() || metadata.file_attributes() & 0x400 != 0
}

#[cfg(not(any(unix, windows)))]
fn is_link_or_reparse(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

fn session_path(app: &AppHandle) -> Result<PathBuf> {
    Ok(storage_dir(app)?.join("session.json"))
}

fn pending_path(app: &AppHandle) -> Result<PathBuf> {
    Ok(storage_dir(app)?.join("pending.json"))
}

fn read_private_session(path: &Path) -> Result<Option<Session>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => bail!("NETEASE_STORAGE"),
    };
    ensure!(
        metadata.is_file() && !is_link_or_reparse(&metadata),
        "NETEASE_STORAGE"
    );
    ensure!(metadata.len() <= MAX_SESSION_BYTES, "NETEASE_STORAGE");
    let text = fs::read_to_string(path).map_err(|_| anyhow!("NETEASE_STORAGE"))?;
    let session: Session = serde_json::from_str(&text).map_err(|_| anyhow!("NETEASE_STORAGE"))?;
    if session.cookie.trim().is_empty() {
        return Ok(None);
    }
    Ok(Some(session))
}

fn read_session(app: &AppHandle) -> Result<Option<Session>> {
    let dir = storage_dir(app)?;
    secure_directory(&dir)?;
    read_private_session(&dir.join("session.json"))
}

fn write_private_session(path: &Path, session: &Session) -> Result<()> {
    let dir = path.parent().context("NETEASE_STORAGE")?;
    fs::create_dir_all(dir).map_err(|_| anyhow!("NETEASE_STORAGE"))?;
    secure_directory(dir)?;
    if let Ok(metadata) = fs::symlink_metadata(path) {
        ensure!(
            metadata.is_file() && !is_link_or_reparse(&metadata),
            "NETEASE_STORAGE"
        );
    }
    let mut nonce = [0_u8; 16];
    getrandom::getrandom(&mut nonce).map_err(|_| anyhow!("NETEASE_STORAGE"))?;
    let temp = dir.join(format!("session-{}.tmp", hex::encode(nonce)));
    let _cleanup = TempCleanup(temp.clone());
    let bytes = serde_json::to_vec(session).map_err(|_| anyhow!("NETEASE_STORAGE"))?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temp)
        .map_err(|_| anyhow!("NETEASE_STORAGE"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|_| anyhow!("NETEASE_STORAGE"))?;
    }
    file.write_all(&bytes)
        .map_err(|_| anyhow!("NETEASE_STORAGE"))?;
    file.sync_all().map_err(|_| anyhow!("NETEASE_STORAGE"))?;
    drop(file);
    #[cfg(windows)]
    secure_directory(dir)?;
    fs::rename(&temp, path).map_err(|_| anyhow!("NETEASE_STORAGE"))?;
    Ok(())
}

struct TempCleanup(PathBuf);

impl Drop for TempCleanup {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn save_session(app: &AppHandle, session: &Session) -> Result<()> {
    write_private_session(&session_path(app)?, session)
}

fn refresh_session_cookies(
    app: &AppHandle,
    session: &Session,
    values: impl Iterator<Item = String>,
) -> Result<String> {
    let mut cookie = merge_set_cookies(&session.cookie, values);
    if !cookie_map(&cookie).contains_key("deviceId") {
        cookie = merge_set_cookies(
            &cookie,
            std::iter::once(format!("deviceId={}", persistent_device_id()?)),
        );
    }
    if cookie != session.cookie {
        let mut updated = session.clone();
        updated.cookie = cookie.clone();
        save_session(app, &updated)?;
    }
    Ok(cookie)
}

fn save_pending_session(app: &AppHandle, phone: &str, cookie: &str) -> Result<()> {
    write_private_session(
        &pending_path(app)?,
        &Session {
            cookie: cookie.to_string(),
            nickname: String::new(),
            phone: phone.to_string(),
        },
    )
}

fn take_pending_session(app: &AppHandle, phone: &str) -> Result<String> {
    let path = pending_path(app)?;
    secure_directory(path.parent().context("NETEASE_STORAGE")?)?;
    let session = read_private_session(&path)?;
    if let Some(session) = session {
        let fresh = fs::metadata(&path)
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age <= std::time::Duration::from_secs(600));
        fs::remove_file(path).map_err(|_| anyhow!("NETEASE_STORAGE"))?;
        if fresh && session.phone == phone {
            return Ok(session.cookie);
        }
    }
    Ok(String::new())
}

fn remove_session(app: &AppHandle) -> Result<()> {
    let dir = storage_dir(app)?;
    secure_directory(&dir)?;
    for path in [dir.join("session.json"), dir.join("pending.json")] {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => bail!("NETEASE_STORAGE"),
        }
    }
    Ok(())
}

#[cfg(unix)]
fn secure_directory(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|_| anyhow!("NETEASE_PERMISSION"))
}

#[cfg(windows)]
fn secure_directory(path: &Path) -> Result<()> {
    let script = r#"$ErrorActionPreference='Stop'; $p=$env:MSR_NETEASE_ACL_PATH; $sid=[System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value; $allowed=@($sid,'S-1-5-18','S-1-5-32-544','S-1-3-0'); $acl=Get-Acl -LiteralPath $p; foreach($r in $acl.Access){ $entry=$r.IdentityReference.Translate([System.Security.Principal.SecurityIdentifier]).Value; if($entry -notin $allowed){ throw 'Unexpected ACL' } }"#;
    let output = std::process::Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            script,
        ])
        .env("MSR_NETEASE_ACL_PATH", path)
        .creation_flags(0x08000000)
        .output()
        .map_err(|_error| {
            #[cfg(test)]
            eprintln!("Windows ACL helper failed to start: {_error}");
            anyhow!("NETEASE_PERMISSION")
        })?;
    #[cfg(test)]
    if !output.status.success() {
        eprintln!(
            "Windows ACL helper failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    ensure!(output.status.success(), "NETEASE_PERMISSION");
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn secure_directory(_path: &Path) -> Result<()> {
    bail!("NETEASE_PERMISSION")
}

fn cookie_map(cookie: &str) -> HashMap<String, String> {
    cookie
        .split(';')
        .filter_map(|pair| pair.trim().split_once('='))
        .map(|(key, value)| (key.trim().to_string(), value.trim().to_string()))
        .filter(|(key, value)| !key.is_empty() && !value.is_empty())
        .collect()
}

fn merge_set_cookies(cookie: &str, values: impl Iterator<Item = String>) -> String {
    let mut all = cookie_map(cookie);
    for value in values {
        let pair = value.split(';').next().unwrap_or(&value);
        if let Some((key, value)) = pair.trim().split_once('=') {
            if !key.trim().is_empty() && !value.trim().is_empty() {
                all.insert(key.trim().to_string(), value.trim().to_string());
            }
        }
    }
    let mut entries: Vec<_> = all.into_iter().collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    entries
        .into_iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("; ")
}

fn persistent_device_id() -> Result<&'static str> {
    static DEVICE_ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    if let Some(value) = DEVICE_ID.get() {
        return Ok(value);
    }
    let mut bytes = [0_u8; 16];
    getrandom::getrandom(&mut bytes).map_err(|_| anyhow!("NETEASE_REQUEST"))?;
    let _ = DEVICE_ID.set(hex::encode(bytes));
    DEVICE_ID
        .get()
        .map(String::as_str)
        .context("NETEASE_REQUEST")
}

fn api_code(body: &Value) -> Result<i64> {
    body.get("code")
        .and_then(|value| value.as_i64().or_else(|| value.as_str()?.parse().ok()))
        .ok_or_else(|| anyhow!("NETEASE_RESPONSE"))
}

fn parse_source_data<'a>(
    data: &'a Value,
    song_id: &str,
    requested: &str,
) -> Result<(&'a str, &'a str, &'a str)> {
    if data.get("code").is_some() {
        let code = api_code(data)?;
        match code {
            -103 => bail!("NETEASE_DOWNLOAD_UNAVAILABLE"),
            -105 => bail!("NETEASE_DOWNLOAD_NOT_PERMITTED"),
            _ => ensure!(code == 200, "NETEASE_SOURCE_REJECTED"),
        }
    }
    if let Some(id) = data.get("id") {
        let returned_id = id
            .as_u64()
            .map(|value| value.to_string())
            .or_else(|| id.as_str().map(str::to_string));
        ensure!(
            returned_id.as_deref() == Some(song_id),
            "NETEASE_SOURCE_INVALID"
        );
    } else {
        bail!("NETEASE_SOURCE_INVALID");
    }
    ensure!(
        data.get("freeTrialInfo").is_none_or(Value::is_null),
        "NETEASE_SOURCE_PREVIEW"
    );
    for flag in ["freeTrial", "preview", "isPreview", "isFreeTrial"] {
        if let Some(value) = data.get(flag) {
            ensure!(value.as_bool() == Some(false), "NETEASE_SOURCE_PREVIEW");
        }
    }
    let url = data
        .get("url")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("NETEASE_SOURCE_MISSING"))?;
    let actual_level = data
        .get("level")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("NETEASE_SOURCE_QUALITY_UNVERIFIED"))?;
    ensure!(actual_level == requested, "NETEASE_QUALITY_UNAVAILABLE");
    let format = data
        .get("type")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            data.get("encodeType")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
        })
        .ok_or_else(|| anyhow!("NETEASE_SOURCE_MISSING"))?;
    Ok((url, actual_level, format))
}

fn normalize_source_url(value: &str) -> Result<reqwest::Url> {
    let mut parsed = reqwest::Url::parse(value).map_err(|_| anyhow!("NETEASE_SOURCE_INVALID"))?;
    ensure!(
        parsed.host_str().is_some()
            && parsed.username().is_empty()
            && parsed.password().is_none()
            && parsed.fragment().is_none(),
        "NETEASE_SOURCE_INVALID"
    );
    if parsed.scheme() == "http" {
        let host = parsed.host_str().unwrap_or_default();
        let known_cdn = ["music.126.net", "music.163.com", "music.163.net"]
            .iter()
            .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")));
        ensure!(known_cdn, "NETEASE_SOURCE_INVALID");
        parsed
            .set_scheme("https")
            .map_err(|_| anyhow!("NETEASE_SOURCE_INVALID"))?;
    }
    ensure!(parsed.scheme() == "https", "NETEASE_SOURCE_INVALID");
    Ok(parsed)
}

fn nickname(body: &Value) -> String {
    [
        body.pointer("/profile/nickname"),
        body.pointer("/data/profile/nickname"),
        body.pointer("/data/account/userName"),
        body.pointer("/account/userName"),
    ]
    .into_iter()
    .flatten()
    .find_map(Value::as_str)
    .unwrap_or_default()
    .to_string()
}

fn call_query(cookie: &str, values: Value) -> Value {
    let mut values = values;
    if let Some(object) = values.as_object_mut() {
        object.insert("cookie".into(), json!(cookie));
    }
    values
}

fn refresh_cookies(app: &AppHandle, session: &Session, cookies: Vec<String>) -> Result<()> {
    refresh_session_cookies(app, session, cookies.into_iter()).map(|_| ())
}

pub async fn send_captcha(app: &AppHandle, phone: &str, countrycode: &str) -> Result<()> {
    validate_phone(phone, countrycode)?;
    let response = crate::node_api::call(
        app,
        "captcha_sent",
        json!({"phone":phone,"ctcode":countrycode,"cookie":""}),
    )
    .await?;
    let code = api_code(&response.body)?;
    if code != 200 {
        bail!(captcha_send_error(code));
    }
    let cookie = merge_set_cookies("", response.cookies.into_iter());
    if !cookie.is_empty() {
        save_pending_session(app, phone, &cookie)?;
    }
    Ok(())
}

pub async fn login(
    app: &AppHandle,
    phone: &str,
    countrycode: &str,
    captcha: &str,
) -> Result<AuthStatus> {
    let _guard = session_request_lock().lock().await;
    validate_phone(phone, countrycode)?;
    ensure!(
        !captcha.is_empty() && captcha.len() <= 32 && captcha.bytes().all(|b| b.is_ascii_digit()),
        "NETEASE_CAPTCHA_INVALID"
    );
    let pending = take_pending_session(app, phone)?;
    let response = crate::node_api::call(app, "login_cellphone", json!({"phone":phone,"countrycode":countrycode,"captcha":captcha,"sca":"","cookie":pending})).await?;
    let body = response.body;
    let code = api_code(&body)?;
    if code != 200 {
        bail!(login_error(code));
    }
    let mut cookie = merge_set_cookies(&pending, response.cookies.into_iter());
    if let Some(value) = body.get("cookie").and_then(Value::as_str) {
        cookie = merge_set_cookies(&cookie, value.split(';').map(str::to_string));
    }
    ensure!(
        cookie_map(&cookie).contains_key("MUSIC_U"),
        "NETEASE_SESSION"
    );
    let status = AuthStatus {
        logged_in: true,
        nickname: nickname(&body),
    };
    save_session(
        app,
        &Session {
            cookie,
            nickname: status.nickname.clone(),
            phone: String::new(),
        },
    )?;
    Ok(status)
}

pub async fn auth_status(app: &AppHandle) -> Result<AuthStatus> {
    let _guard = session_request_lock().lock().await;
    let Some(session) = read_session(app)? else {
        return Ok(AuthStatus {
            logged_in: false,
            nickname: String::new(),
        });
    };
    let response =
        crate::node_api::call(app, "login_status", json!({"cookie":session.cookie})).await?;
    refresh_cookies(app, &session, response.cookies)?;
    let body = response.body;
    let body = body
        .get("data")
        .filter(|data| data.get("code").is_some())
        .unwrap_or(&body);
    if api_code(body)? != 200
        || !(body.get("account").is_some_and(|v| !v.is_null())
            || body.get("profile").is_some_and(|v| !v.is_null()))
    {
        return Ok(AuthStatus {
            logged_in: false,
            nickname: String::new(),
        });
    }
    let name = nickname(body);
    Ok(AuthStatus {
        logged_in: true,
        nickname: if name.is_empty() {
            session.nickname
        } else {
            name
        },
    })
}

pub async fn logout(app: &AppHandle) -> Result<()> {
    let _guard = session_request_lock().lock().await;
    let session = read_session(app)?;
    remove_session(app)?;
    if let Some(session) = session {
        let _ = crate::node_api::call(app, "logout", json!({"cookie":session.cookie})).await;
    }
    Ok(())
}

pub async fn fetch_artist_songs(
    app: &AppHandle,
    artist_id: &str,
    offset: u64,
    limit: u64,
) -> Result<Vec<Value>> {
    ensure!(
        !artist_id.is_empty() && artist_id.bytes().all(|b| b.is_ascii_digit()),
        "NETEASE_REQUEST"
    );
    ensure!(limit > 0 && limit <= 100, "NETEASE_REQUEST");
    let response = crate::node_api::call(
        app,
        "artist_songs",
        json!({"id":artist_id,"offset":offset,"limit":limit,"order":"hot"}),
    )
    .await?;
    ensure!(api_code(&response.body)? == 200, "NETEASE_CATALOG_REJECTED");
    response
        .body
        .pointer("/songs")
        .or_else(|| response.body.pointer("/data/songs"))
        .or_else(|| response.body.pointer("/data"))
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| anyhow!("NETEASE_RESPONSE"))
}

pub async fn fetch_song_detail(
    app: &AppHandle,
    song: &crate::models::SongDetail,
) -> Result<crate::models::SongDetail> {
    ensure!(
        !song.id.is_empty() && song.id.bytes().all(|b| b.is_ascii_digit()),
        "NETEASE_REQUEST"
    );
    let _guard = session_request_lock().lock().await;
    let session = read_session(app)?;
    let cookie = session.as_ref().map(|s| s.cookie.as_str()).unwrap_or("");
    let response = crate::node_api::call(
        app,
        "song_detail",
        call_query(cookie, json!({"ids":song.id})),
    )
    .await?;
    if let Some(s) = &session {
        refresh_cookies(app, s, response.cookies)?;
    }
    let body = response.body;
    ensure!(api_code(&body)? == 200, "NETEASE_RESPONSE");
    let detail = body
        .get("songs")
        .and_then(Value::as_array)
        .and_then(|songs| {
            songs.iter().find(|d| {
                d["id"].as_u64().is_some_and(|id| song.id == id.to_string())
                    || d["id"].as_str() == Some(song.id.as_str())
            })
        })
        .ok_or_else(|| anyhow!("NETEASE_RESPONSE"))?;
    let mut enriched = song.clone();
    enriched.merge_netease_detail(detail);
    Ok(enriched)
}

fn session_request_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

pub async fn fetch_song_source(
    app: &AppHandle,
    song_id: &str,
    level: &str,
    api: DownloadApi,
) -> Result<AudioSource> {
    ensure!(
        !song_id.is_empty() && song_id.bytes().all(|b| b.is_ascii_digit()),
        "NETEASE_REQUEST"
    );
    ensure!(
        matches!(level, "standard" | "exhigh" | "lossless" | "hires"),
        "NETEASE_QUALITY_UNSUPPORTED"
    );
    let _guard = session_request_lock().lock().await;
    let session = read_session(app)?.ok_or_else(|| anyhow!("NETEASE_AUTH_REQUIRED"))?;
    let response = crate::node_api::call(
        app,
        api.method(),
        api.query(song_id, level, &session.cookie)?,
    )
    .await?;
    refresh_cookies(app, &session, response.cookies)?;
    if matches!(api_code(&response.body)?, 301 | 401) {
        bail!("NETEASE_AUTH_REQUIRED");
    }
    ensure!(api_code(&response.body)? == 200, "NETEASE_SOURCE_REJECTED");
    let data = response
        .body
        .pointer("/data/0")
        .or_else(|| response.body.get("data"))
        .filter(|v| v.is_object())
        .ok_or_else(|| anyhow!("NETEASE_SOURCE_MISSING"))?;
    ensure!(
        !data.get("code").is_some_and(|v| v
            .as_i64()
            .or_else(|| v.as_str()?.parse().ok())
            .is_some_and(|c| c == 301 || c == 401)),
        "NETEASE_AUTH_REQUIRED"
    );
    let (url, quality, format) = parse_source_data(data, song_id, level)?;
    let parsed = normalize_source_url(url)?;
    Ok(AudioSource {
        url: parsed.to_string(),
        quality: quality.to_string(),
        format: format.to_ascii_lowercase(),
    })
}

pub async fn fetch_lyric(app: &AppHandle, id: &str) -> Result<Option<String>> {
    ensure!(
        !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()),
        "NETEASE_REQUEST"
    );
    let _guard = session_request_lock().lock().await;
    let session = read_session(app)?.ok_or_else(|| anyhow!("NETEASE_AUTH_REQUIRED"))?;
    let response =
        crate::node_api::call(app, "lyric", json!({"id":id,"cookie":session.cookie})).await?;
    refresh_cookies(app, &session, response.cookies)?;
    ensure!(api_code(&response.body)? == 200, "NETEASE_LYRIC_REJECTED");
    Ok(response
        .body
        .pointer("/lrc/lyric")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string))
}

fn captcha_send_error(code: i64) -> &'static str {
    if matches!(code, 429 | 503 | 509) {
        "NETEASE_RISK_CONTROL"
    } else {
        "NETEASE_CAPTCHA_SEND_FAILED"
    }
}

fn login_error(code: i64) -> &'static str {
    match code {
        400 | 502 => "NETEASE_CAPTCHA_INVALID",
        429 | 503 | 509 => "NETEASE_RISK_CONTROL",
        _ => "NETEASE_AUTH_FAILED",
    }
}

fn validate_phone(phone: &str, countrycode: &str) -> Result<()> {
    ensure!(
        (6..=18).contains(&phone.len()) && phone.bytes().all(|b| b.is_ascii_digit()),
        "NETEASE_PHONE_INVALID"
    );
    ensure!(
        (1..=4).contains(&countrycode.len()) && countrycode.bytes().all(|b| b.is_ascii_digit()),
        "NETEASE_PHONE_INVALID"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn download_api_queries_use_separate_endpoints_and_verified_levels() {
        assert_eq!(DownloadApi::V1.method(), "song_download_url_v1");
        assert_eq!(DownloadApi::Legacy.method(), "song_download_url");
        for (level, bitrate) in [
            ("hires", 999000),
            ("lossless", 999000),
            ("exhigh", 320000),
            ("standard", 128000),
        ] {
            let v1 = DownloadApi::V1.query("123", level, "").unwrap();
            let old = DownloadApi::Legacy.query("123", level, "").unwrap();
            assert_eq!(v1["level"], level);
            assert_eq!(old["br"], bitrate);
            assert!(old.get("level").is_none());
            assert_eq!(old["id"], "123");
        }
        assert!(DownloadApi::Legacy.query("123", "higher", "").is_err());
        // A maximum-bitrate legacy request can return ordinary lossless, never label it Hi-Res.
        let legacy = json!({"id":123,"code":200,"url":"https://example.invalid/a.flac","type":"flac","encodeType":"mp3","level":"lossless","br":1121663,"freeTrialInfo":null});
        assert_eq!(
            parse_source_data(&legacy, "123", "lossless").unwrap().2,
            "flac"
        );
        assert_eq!(
            parse_source_data(&legacy, "123", "hires")
                .unwrap_err()
                .to_string(),
            "NETEASE_QUALITY_UNAVAILABLE"
        );
        let unverified = json!({"id":123,"code":200,"url":"https://example.invalid/a.flac","type":"flac","br":999000});
        assert_eq!(
            parse_source_data(&unverified, "123", "lossless")
                .unwrap_err()
                .to_string(),
            "NETEASE_SOURCE_QUALITY_UNVERIFIED"
        );
    }
    #[test]
    fn source_parser_checks_item_identity_quality_and_preview_entitlement() {
        let valid = json!({"id":123,"code":200,"url":"https://example.invalid/a.flac","type":"flac","level":"lossless"});
        assert_eq!(
            parse_source_data(&valid, "123", "lossless").unwrap().1,
            "lossless"
        );
        assert_eq!(
            parse_source_data(&valid, "456", "lossless")
                .unwrap_err()
                .to_string(),
            "NETEASE_SOURCE_INVALID"
        );
        let preview = json!({"id":123,"url":"https://example.invalid/a.mp3","type":"mp3","level":"standard","freeTrialInfo":{"start":0}});
        assert_eq!(
            parse_source_data(&preview, "123", "standard")
                .unwrap_err()
                .to_string(),
            "NETEASE_SOURCE_PREVIEW"
        );
        let downgrade =
            json!({"id":123,"url":"https://example.invalid/a.mp3","type":"mp3","level":"exhigh"});
        assert_eq!(
            parse_source_data(&downgrade, "123", "lossless")
                .unwrap_err()
                .to_string(),
            "NETEASE_QUALITY_UNAVAILABLE"
        );
        for (code, expected) in [
            (-103, "NETEASE_DOWNLOAD_UNAVAILABLE"),
            (-105, "NETEASE_DOWNLOAD_NOT_PERMITTED"),
        ] {
            let response = json!({"id":123,"code":code,"url":null,"level":null,"type":null});
            assert_eq!(
                parse_source_data(&response, "123", "exhigh")
                    .unwrap_err()
                    .to_string(),
                expected
            );
        }
        let item_error = json!({"id":123,"code":404,"url":null});
        assert_eq!(
            parse_source_data(&item_error, "123", "lossless")
                .unwrap_err()
                .to_string(),
            "NETEASE_SOURCE_REJECTED"
        );
    }

    #[test]
    fn source_url_promotes_only_known_cdn_hosts_and_preserves_signed_components() {
        let promoted =
            normalize_source_url("http://m10.music.126.net/path/audio.flac?sign=a%2Fb&expires=123")
                .unwrap();
        assert_eq!(promoted.scheme(), "https");
        assert_eq!(promoted.host_str(), Some("m10.music.126.net"));
        assert_eq!(promoted.path(), "/path/audio.flac");
        assert_eq!(promoted.query(), Some("sign=a%2Fb&expires=123"));
        assert_eq!(
            normalize_source_url("https://cdn.example.invalid/audio.flac")
                .unwrap()
                .scheme(),
            "https"
        );
        assert_eq!(
            normalize_source_url("http://evil-music.126.net/audio.flac")
                .unwrap_err()
                .to_string(),
            "NETEASE_SOURCE_INVALID"
        );
        assert_eq!(
            normalize_source_url("http://music.126.net.evil.invalid/audio.flac")
                .unwrap_err()
                .to_string(),
            "NETEASE_SOURCE_INVALID"
        );
        assert_eq!(
            normalize_source_url("http://user@music.163.com/audio.flac")
                .unwrap_err()
                .to_string(),
            "NETEASE_SOURCE_INVALID"
        );
    }

    #[test]
    fn wrapped_login_status_requires_an_account_or_profile() {
        let response = json!({"data":{"code":200,"account":{"id":7},"profile":{"nickname":"A"}}});
        let body = response
            .get("data")
            .filter(|d| d.get("code").is_some())
            .unwrap_or(&response);
        assert_eq!(api_code(body).unwrap(), 200);
        assert!(
            body.get("account").is_some_and(|v| !v.is_null())
                || body.get("profile").is_some_and(|v| !v.is_null())
        );
        assert!(
            cookie_map(&merge_set_cookies(
                "MUSIC_U=old",
                ["MUSIC_U=new; Path=/".into()].into_iter()
            ))
            .contains_key("MUSIC_U")
        );
    }

    #[tokio::test]
    #[ignore = "explicit opt-in: reads the local session and contacts NetEase"]
    async fn node_api_session_schema_diagnostic() {
        assert_eq!(
            std::env::var("MSR_SESSION_DIAGNOSTIC").as_deref(),
            Ok("1"),
            "explicit diagnostic opt-in required"
        );
        let app_data = PathBuf::from(std::env::var_os("APPDATA").expect("APPDATA is required"))
            .join("com.wisecirno.metal-scar-downloader");
        let session = read_private_session(&app_data.join("netease/session.json"))
            .unwrap()
            .unwrap();
        let runtime = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/netease-api");
        let cache = app_data.join("netease/node-cache");

        let status = crate::node_api::call_at_paths(
            &runtime,
            &cache,
            "login_status",
            json!({"cookie":session.cookie}),
        )
        .await
        .unwrap();
        let status_body = status
            .body
            .get("data")
            .filter(|v| v.get("code").is_some())
            .unwrap_or(&status.body);
        let account_present = status_body.get("account").is_some_and(|v| !v.is_null());
        println!(
            "login_status code={:?} account_present={} vipType={:?}",
            status_body.get("code"),
            account_present,
            status_body
                .pointer("/profile/vipType")
                .or_else(|| status_body.pointer("/account/vipType"))
        );

        let catalog = crate::node_api::call_at_paths(
            &runtime,
            &cache,
            "artist_songs",
            json!({"id":"122546004","offset":0,"limit":1,"order":"hot","cookie":""}),
        )
        .await
        .unwrap();
        let songs = catalog
            .body
            .pointer("/songs")
            .and_then(Value::as_array)
            .expect("catalog songs missing");
        let id = songs
            .first()
            .and_then(|s| s.get("id"))
            .and_then(|v| {
                v.as_u64()
                    .map(|n| n.to_string())
                    .or_else(|| v.as_str().map(str::to_string))
            })
            .expect("catalog song id missing");
        println!("artist_songs code={:?}", catalog.body.get("code"));

        let detail_response = crate::node_api::call_at_paths(
            &runtime,
            &cache,
            "song_detail",
            json!({"ids":id,"cookie":session.cookie}),
        )
        .await
        .unwrap();
        let detail = detail_response
            .body
            .pointer("/songs/0")
            .unwrap_or(&Value::Null);
        println!(
            "song_detail code={:?} fee={:?} dlLevel={:?} maxBrLevel={:?}",
            detail_response.body.get("code"),
            detail.get("fee"),
            detail_response.body.pointer("/privileges/0/dlLevel"),
            detail_response.body.pointer("/privileges/0/maxBrLevel")
        );

        let cover = detail
            .pointer("/al/picUrl")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty());
        let cover_scheme = cover
            .and_then(|url| reqwest::Url::parse(url).ok())
            .map(|url| url.scheme().to_string());
        println!(
            "cover_available={} cover_scheme={:?}",
            cover.is_some(),
            cover_scheme
        );
        if std::env::var("MSR_AUDIO_DIAGNOSTIC").as_deref() == Ok("1") {
            let cover_url = cover
                .and_then(|url| reqwest::Url::parse(url).ok())
                .filter(|url| {
                    url.scheme() == "https"
                        && url.host_str().is_some()
                        && url.username().is_empty()
                        && url.password().is_none()
                        && url.fragment().is_none()
                })
                .unwrap_or_else(|| panic!("cover URL is not a valid HTTPS URL"));
            let client = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("cover client initialization failed");
            let response = client
                .get(cover_url)
                .send()
                .await
                .unwrap_or_else(|_| panic!("cover request failed"));
            assert!(
                response.status().is_success(),
                "cover request returned non-success status"
            );
            let content_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_ascii_lowercase();
            assert!(
                content_type.starts_with("image/"),
                "cover response was not an image"
            );
            let bytes = response
                .bytes()
                .await
                .unwrap_or_else(|_| panic!("cover response body failed"));
            let cover_bytes = bytes.len();
            let cover_fetch_ok = cover_bytes > 0;
            println!(
                "cover_bytes={} cover_fetch_ok={}",
                cover_bytes, cover_fetch_ok
            );
            assert!(cover_fetch_ok, "cover response was empty");
        }

        let lyric_response = crate::node_api::call_at_paths(
            &runtime,
            &cache,
            "lyric",
            json!({"id":id,"cookie":session.cookie}),
        )
        .await
        .unwrap();
        let has_lrc = lyric_response
            .body
            .pointer("/lrc/lyric")
            .and_then(Value::as_str)
            .is_some_and(|text| !text.is_empty());
        println!(
            "lyric code={:?} has_lrc={}",
            lyric_response.body.get("code"),
            has_lrc
        );

        for (api, level) in [
            (DownloadApi::V1, "lossless"),
            (DownloadApi::Legacy, "lossless"),
            (DownloadApi::V1, "hires"),
            (DownloadApi::Legacy, "hires"),
        ] {
            let response = crate::node_api::call_at_paths(
                &runtime,
                &cache,
                api.method(),
                api.query(&id, level, &session.cookie).unwrap(),
            )
            .await
            .unwrap();
            let item = response
                .body
                .pointer("/data/0")
                .or_else(|| response.body.get("data"))
                .unwrap_or(&Value::Null);
            let parsed_source = parse_source_data(item, &id, level).and_then(|(url, _, _)| {
                normalize_source_url(url).map(|normalized| (url, normalized))
            });
            let url_scheme = item
                .get("url")
                .and_then(Value::as_str)
                .and_then(|url| reqwest::Url::parse(url).ok())
                .map(|url| url.scheme().to_string());
            println!(
                "{} requested={} code={:?} item_code={:?} level={:?} format={:?} has_url={} url_scheme={:?} parse_ok={}",
                api.method(),
                level,
                response.body.get("code"),
                item.get("code"),
                item.get("level"),
                item.get("type").or_else(|| item.get("encodeType")),
                item.get("url")
                    .and_then(Value::as_str)
                    .is_some_and(|u| !u.is_empty()),
                url_scheme,
                parsed_source.is_ok()
            );
            if level == "lossless" && std::env::var("MSR_AUDIO_DIAGNOSTIC").as_deref() == Ok("1") {
                use tokio::io::AsyncWriteExt;

                struct AudioTemp {
                    path: PathBuf,
                    canonical: PathBuf,
                }
                impl Drop for AudioTemp {
                    fn drop(&mut self) {
                        if self.path.canonicalize().ok().as_ref() == Some(&self.canonical) {
                            let _ = fs::remove_file(&self.path);
                        }
                    }
                }

                let (_, source_url) =
                    parsed_source.unwrap_or_else(|_| panic!("source parsing failed"));
                let mut nonce = [0_u8; 16];
                getrandom::getrandom(&mut nonce).expect("temporary name generation failed");
                let temp_dir = std::env::temp_dir()
                    .canonicalize()
                    .expect("temporary directory resolution failed");
                let audio_path = temp_dir.join(format!("msr-audio-{}.tmp", hex::encode(nonce)));
                let client = reqwest::Client::builder()
                    .redirect(reqwest::redirect::Policy::none())
                    .timeout(std::time::Duration::from_secs(120))
                    .build()
                    .expect("audio client initialization failed");
                let mut download = client
                    .get(source_url)
                    .send()
                    .await
                    .unwrap_or_else(|_| panic!("audio request failed"));
                assert!(
                    download.status().is_success(),
                    "audio request returned non-success status"
                );
                let mut file = tokio::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&audio_path)
                    .await
                    .expect("temporary audio file creation failed");
                let _cleanup = AudioTemp {
                    path: audio_path.clone(),
                    canonical: audio_path.clone(),
                };
                let mut audio_bytes = 0_u64;
                while let Some(chunk) = download
                    .chunk()
                    .await
                    .unwrap_or_else(|_| panic!("audio stream failed"))
                {
                    audio_bytes = audio_bytes
                        .checked_add(chunk.len() as u64)
                        .expect("audio byte count overflow");
                    file.write_all(&chunk)
                        .await
                        .expect("temporary audio write failed");
                }
                file.flush().await.expect("temporary audio flush failed");
                drop(file);
                let expected_size = item
                    .get("size")
                    .and_then(|size| size.as_u64().or_else(|| size.as_str()?.parse::<u64>().ok()))
                    .filter(|size| *size > 0);
                if let Some(expected_size) = expected_size {
                    assert_eq!(
                        audio_bytes, expected_size,
                        "downloaded size did not match source metadata"
                    );
                }
                let decode = tokio::time::timeout(
                    std::time::Duration::from_secs(120),
                    tokio::process::Command::new("ffmpeg")
                        .args(["-v", "error", "-xerror", "-i"])
                        .arg(&audio_path)
                        .args(["-f", "null", "-"])
                        .stdin(std::process::Stdio::null())
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::piped())
                        .kill_on_drop(true)
                        .output(),
                )
                .await
                .expect("FFmpeg decode timed out")
                .unwrap_or_else(|_| panic!("FFmpeg could not be started"));
                let decode_ok = decode.status.success();
                println!("audio_bytes={} decode_ok={}", audio_bytes, decode_ok);
                assert!(decode_ok, "FFmpeg decode failed");
            }
        }
    }
}
