use anyhow::{Result, anyhow, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::Duration;
use tauri::{AppHandle, Manager};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

const MAX_RESPONSE: u64 = 8 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

pub struct NodeResponse {
    pub body: Value,
    pub cookies: Vec<String>,
}

#[derive(Deserialize)]
struct Envelope {
    id: u64,
    body: Option<Value>,
    #[serde(default)]
    cookies: Vec<String>,
    error: Option<String>,
}

struct Worker {
    _child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    runtime: PathBuf,
    cache: PathBuf,
    sequence: u64,
}

impl Worker {
    fn spawn(runtime: &Path, cache: &Path) -> Result<Self> {
        let executable = runtime.join(if cfg!(windows) {
            "node.exe"
        } else {
            "bin/node"
        });
        let bridge = runtime.join("bridge.cjs");
        ensure!(
            executable.is_file() && bridge.is_file(),
            "NETEASE_API_UNAVAILABLE"
        );
        std::fs::create_dir_all(cache).map_err(|_| anyhow!("NETEASE_STORAGE"))?;
        let metadata = std::fs::symlink_metadata(cache).map_err(|_| anyhow!("NETEASE_STORAGE"))?;
        ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "NETEASE_STORAGE"
        );
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            ensure!(metadata.file_attributes() & 0x400 == 0, "NETEASE_STORAGE");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(cache, std::fs::Permissions::from_mode(0o700))
                .map_err(|_| anyhow!("NETEASE_STORAGE"))?;
        }
        let mut command = Command::new(executable);
        command
            .arg(bridge)
            .current_dir(cache)
            .env("TMP", cache)
            .env("TEMP", cache)
            .env("TMPDIR", cache)
            .env("DOTENV_CONFIG_QUIET", "true")
            .env_remove("NODE_OPTIONS")
            .env_remove("NODE_PATH")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x08000000);
        let mut child = command
            .spawn()
            .map_err(|_| anyhow!("NETEASE_API_UNAVAILABLE"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("NETEASE_API_UNAVAILABLE"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("NETEASE_API_UNAVAILABLE"))?;
        Ok(Self {
            _child: child,
            stdin,
            stdout: BufReader::new(stdout),
            runtime: runtime.into(),
            cache: cache.into(),
            sequence: 0,
        })
    }

    async fn request(&mut self, method: &str, query: Value) -> Result<NodeResponse> {
        self.sequence += 1;
        let mut message =
            serde_json::to_vec(&json!({ "id": self.sequence, "method": method, "query": query }))
                .map_err(|_| anyhow!("NETEASE_REQUEST"))?;
        ensure!(message.len() <= 1024 * 1024, "NETEASE_REQUEST");
        message.push(b'\n');
        self.stdin
            .write_all(&message)
            .await
            .map_err(|_| anyhow!("NETEASE_API_UNAVAILABLE"))?;
        self.stdin
            .flush()
            .await
            .map_err(|_| anyhow!("NETEASE_API_UNAVAILABLE"))?;
        let mut line = Vec::new();
        (&mut self.stdout)
            .take(MAX_RESPONSE + 1)
            .read_until(b'\n', &mut line)
            .await
            .map_err(|_| anyhow!("NETEASE_API_UNAVAILABLE"))?;
        ensure!(
            !line.is_empty() && line.len() as u64 <= MAX_RESPONSE && line.last() == Some(&b'\n'),
            "NETEASE_RESPONSE"
        );
        let envelope: Envelope =
            serde_json::from_slice(&line).map_err(|_| anyhow!("NETEASE_RESPONSE"))?;
        ensure!(envelope.id == self.sequence, "NETEASE_RESPONSE");
        if let Some(code) = envelope.error {
            return Err(anyhow!(match code.as_str() {
                "NETEASE_NETWORK" => "NETEASE_NETWORK",
                "NETEASE_REQUEST" => "NETEASE_REQUEST",
                "NETEASE_RESPONSE" => "NETEASE_RESPONSE",
                _ => "NETEASE_API_UNAVAILABLE",
            }));
        }
        let body = envelope
            .body
            .filter(Value::is_object)
            .ok_or_else(|| anyhow!("NETEASE_RESPONSE"))?;
        Ok(NodeResponse {
            body,
            cookies: envelope.cookies,
        })
    }
}

fn unpack_runtime(base: &Path) -> Result<PathBuf> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    static PAYLOAD: &[u8] = include_bytes!("../resources/netease-runtime.bin");
    static DIGEST: OnceLock<String> = OnceLock::new();
    let digest = DIGEST.get_or_init(|| hex::encode(Sha256::digest(PAYLOAD)));
    let runtime = base.join(format!("runtime-{digest}"));
    std::fs::create_dir_all(base)?;
    let valid = |path: &Path| {
        std::fs::read_to_string(path.join(".ready")).is_ok_and(|value| value == *digest)
            && path
                .join("node_modules/@neteasecloudmusicapienhanced/api/main.js")
                .is_file()
            && path
                .join(if cfg!(windows) {
                    "node.exe"
                } else {
                    "bin/node"
                })
                .is_file()
            && path.join("bridge.cjs").is_file()
    };
    if valid(&runtime) {
        return Ok(runtime);
    }
    let staging = base.join(format!(
        ".extract-{}-{}",
        std::process::id(),
        getrandom_nonce()?
    ));
    std::fs::create_dir(&staging)?;
    let result: Result<()> = (|| {
        let mut reader = flate2::read::ZlibDecoder::new(PAYLOAD);
        let mut header = [0_u8; 12];
        reader.read_exact(&mut header)?;
        ensure!(&header[..8] == b"MSRAPI01", "NETEASE_API_UNAVAILABLE");
        let count = u32::from_le_bytes(header[8..12].try_into()?);
        ensure!(count <= 10000, "NETEASE_API_UNAVAILABLE");
        let mut total = 0_u64;
        for _ in 0..count {
            let mut lengths = [0_u8; 12];
            reader.read_exact(&mut lengths)?;
            let name_len = u32::from_le_bytes(lengths[..4].try_into()?) as usize;
            let size = u64::from_le_bytes(lengths[4..].try_into()?);
            total = total
                .checked_add(size)
                .ok_or_else(|| anyhow!("NETEASE_API_UNAVAILABLE"))?;
            ensure!(
                name_len > 0 && name_len < 4096 && total <= 400 * 1024 * 1024,
                "NETEASE_API_UNAVAILABLE"
            );
            let mut bytes = vec![0; name_len];
            reader.read_exact(&mut bytes)?;
            let name = String::from_utf8(bytes)?;
            let relative = Path::new(&name);
            ensure!(
                !name.contains('\\')
                    && !name.contains(':')
                    && relative
                        .components()
                        .all(|c| matches!(c, std::path::Component::Normal(_))),
                "NETEASE_API_UNAVAILABLE"
            );
            let target = staging.join(relative);
            std::fs::create_dir_all(
                target
                    .parent()
                    .ok_or_else(|| anyhow!("NETEASE_API_UNAVAILABLE"))?,
            )?;
            let mut output = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)?;
            ensure!(
                std::io::copy(&mut reader.by_ref().take(size), &mut output)? == size,
                "NETEASE_API_UNAVAILABLE"
            );
            #[cfg(unix)]
            if name == "bin/node" {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o700))?;
            }
        }
        let mut extra = [0_u8; 1];
        ensure!(reader.read(&mut extra)? == 0, "NETEASE_API_UNAVAILABLE");
        std::fs::write(staging.join(".ready"), &digest)?;
        ensure!(valid(&staging), "NETEASE_API_UNAVAILABLE");
        match std::fs::rename(&staging, &runtime) {
            Ok(()) => (),
            Err(_) if valid(&runtime) => (),
            Err(error) => return Err(error.into()),
        }
        Ok(())
    })();
    // Only this invocation's freshly created staging directory can be removed.
    if staging.is_dir() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    result.map_err(|_| anyhow!("NETEASE_API_UNAVAILABLE"))?;
    Ok(runtime)
}

fn getrandom_nonce() -> Result<String> {
    let mut bytes = [0_u8; 12];
    getrandom::getrandom(&mut bytes).map_err(|_| anyhow!("NETEASE_STORAGE"))?;
    Ok(hex::encode(bytes))
}

pub async fn call(app: &AppHandle, method: &str, query: Value) -> Result<NodeResponse> {
    let data = app
        .path()
        .app_data_dir()
        .map_err(|_| anyhow!("NETEASE_STORAGE"))?;
    static EXTRACTION: Mutex<()> = Mutex::const_new(());
    let runtime = {
        let _guard = EXTRACTION.lock().await;
        let base = data.join("netease");
        tokio::task::spawn_blocking(move || unpack_runtime(&base))
            .await
            .map_err(|_| anyhow!("NETEASE_API_UNAVAILABLE"))??
    };
    let cache = data.join("netease/node-cache");
    call_at_paths(&runtime, &cache, method, query).await
}

pub(crate) async fn call_at_paths(
    runtime: &Path,
    cache: &Path,
    method: &str,
    query: Value,
) -> Result<NodeResponse> {
    static WORKER: OnceLock<Mutex<Option<Worker>>> = OnceLock::new();
    let mut slot = WORKER.get_or_init(|| Mutex::new(None)).lock().await;
    // Taking ownership ensures timeout/cancellation kills the child rather than desynchronizing IPC.
    let existing = slot.take();
    let mut worker = match existing {
        Some(worker) if worker.runtime == runtime && worker.cache == cache => worker,
        _ => Worker::spawn(runtime, cache)?,
    };
    let response = tokio::time::timeout(REQUEST_TIMEOUT, worker.request(method, query))
        .await
        .map_err(|_| anyhow!("NETEASE_NETWORK"))??;
    *slot = Some(worker);
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "requires the prepared bundled Node runtime"]
    async fn bundled_worker_health_and_anonymous_catalog() {
        let base =
            std::env::temp_dir().join(format!("msr-embedded-{}", getrandom_nonce().unwrap()));
        let runtime = unpack_runtime(&base).unwrap();
        assert_eq!(runtime, unpack_runtime(&base).unwrap());
        let cache = base.join("cache");
        let health = call_at_paths(&runtime, &cache, "health", json!({}))
            .await
            .unwrap();
        assert_eq!(health.body["code"], 200);
        assert!(health.body["version"].is_string());
        let catalog = call_at_paths(
            &runtime,
            &cache,
            "artist_songs",
            json!({"id": "122546004", "offset": 0, "limit": 1, "order": "hot", "cookie": ""}),
        )
        .await
        .unwrap();
        assert_eq!(catalog.body["code"], 200);
        assert!(
            catalog.body["songs"]
                .as_array()
                .is_some_and(|songs| !songs.is_empty())
        );
    }
}
