use mime_guess::MimeGuess;
use reqwest::blocking::Client;
use rfd::{FileDialog, MessageButtons, MessageDialog, MessageLevel};
use serde::{Deserialize, Serialize};
use serde_json::json;
use serde_json::Value;
use std::collections::HashMap;
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Cursor, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};
use tauri::{
    AppHandle,
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Emitter, Manager, WindowEvent, Wry,
};
use tauri_plugin_updater::{Update, UpdaterExt};
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};
use url::Url;
use uuid::Uuid;

const AGENT_HOST: &str = "127.0.0.1";
const AGENT_PORT: u16 = 17777;
const CLEANUP_DELAY_SECONDS: u64 = 300;
const DEFAULT_CHUNK_TIMEOUT_SECONDS: u64 = 120;
const DEFAULT_RETRY_COUNT: usize = 2;
const DEFAULT_PARALLEL_UPLOADS: usize = 4;
const UPDATE_ENDPOINT: Option<&str> = option_env!("CHUANGCUT_AGENT_UPDATER_ENDPOINT");
const UPDATE_PUBKEY: Option<&str> = option_env!("CHUANGCUT_AGENT_UPDATER_PUBKEY");

#[derive(Clone)]
struct GcloudCommandSpec {
    program: String,
    prefix_args: Vec<String>,
    display_path: String,
}

fn append_startup_log(message: impl AsRef<str>) {
    let path = startup_log_path();
    let timestamp = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|duration| duration.as_secs().to_string())
        .unwrap_or_else(|_| "0".to_string());
    let line = format!("[{timestamp}] {}\n", message.as_ref());

    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = std::io::Write::write_all(&mut file, line.as_bytes());
    }
}

fn startup_log_path() -> PathBuf {
    env::temp_dir().join("chuangcut-local-upload-agent-startup.log")
}

fn install_startup_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let location = panic_info
            .location()
            .map(|location| format!("{}:{}", location.file(), location.line()))
            .unwrap_or_else(|| "unknown".to_string());
        let payload = panic_info
            .payload()
            .downcast_ref::<&str>()
            .map(|message| (*message).to_string())
            .or_else(|| {
                panic_info
                    .payload()
                    .downcast_ref::<String>()
                    .map(|message| message.clone())
            })
            .unwrap_or_else(|| "unknown panic payload".to_string());

        append_startup_log(format!("捕获到 panic（{location}）：{payload}"));
        default_hook(panic_info);
    }));
}

fn show_startup_error_dialog(message: impl AsRef<str>) {
    let detail = format!(
        "{}\n\n请把这个日志文件发给开发者：{}",
        message.as_ref(),
        startup_log_path().display()
    );

    let _ = MessageDialog::new()
        .set_title("创剪本地上传助手启动失败")
        .set_level(MessageLevel::Error)
        .set_description(&detail)
        .set_buttons(MessageButtons::Ok)
        .show();
}

#[derive(Clone)]
struct AgentHttpState {
    version: String,
    app: AppHandle<Wry>,
    tasks: Arc<Mutex<HashMap<String, TaskHandle>>>,
    updater: Arc<Mutex<UpdaterRuntimeState>>,
    http: Client,
}

#[derive(Clone)]
struct TaskHandle {
    snapshot: Arc<Mutex<AgentTaskSnapshot>>,
    cancel_requested: Arc<AtomicBool>,
    active_pid: Arc<Mutex<Option<u32>>>,
    logs: Arc<Mutex<Vec<TaskLogEntry>>>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum AgentTaskStatus {
    Queued,
    Uploading,
    Finalizing,
    Completed,
    Cancelled,
    Failed,
}

#[derive(Clone, Serialize, Deserialize)]
struct UploadTaskResult {
    url: String,
    filename: String,
    #[serde(rename = "localPath", skip_serializing_if = "Option::is_none")]
    local_path: Option<String>,
    #[serde(rename = "gcsGsUri", skip_serializing_if = "Option::is_none")]
    gcs_gs_uri: Option<String>,
    #[serde(rename = "assetId", skip_serializing_if = "Option::is_none")]
    asset_id: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
struct AgentTaskSnapshot {
    #[serde(rename = "taskId")]
    task_id: String,
    status: AgentTaskStatus,
    #[serde(rename = "fileName")]
    file_name: String,
    #[serde(rename = "fileSize")]
    file_size: u64,
    #[serde(rename = "uploadedBytes")]
    uploaded_bytes: u64,
    #[serde(rename = "totalBytes")]
    total_bytes: u64,
    progress: f64,
    #[serde(rename = "speedBytesPerSecond")]
    speed_bytes_per_second: f64,
    #[serde(rename = "uploadId", skip_serializing_if = "Option::is_none")]
    upload_id: Option<String>,
    #[serde(rename = "gsUri", skip_serializing_if = "Option::is_none")]
    gs_uri: Option<String>,
    #[serde(rename = "objectName", skip_serializing_if = "Option::is_none")]
    object_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<UploadTaskResult>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TaskLogEntry {
    timestamp: String,
    level: String,
    message: String,
    chunk_index: Option<u64>,
    detail: Option<String>,
}

#[derive(Clone)]
struct AgentUploadTask {
    task_id: String,
    base_url: String,
    file_name: String,
    file_size: u64,
    mime_type: String,
    local_file_path: String,
    api_token: String,
    bucket_name: Option<String>,
    object_prefix: Option<String>,
}

struct UpdaterRuntimeState {
    pending_update: Option<Update>,
    snapshot: UpdaterSnapshot,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct UpdaterSnapshot {
    configured: bool,
    checking: bool,
    installing: bool,
    available: bool,
    current_version: String,
    latest_version: Option<String>,
    progress: f64,
    downloaded_bytes: u64,
    download_total_bytes: Option<u64>,
    notes: Option<String>,
    endpoint: Option<String>,
    last_error: Option<String>,
    last_checked_at: Option<String>,
}

#[derive(Deserialize)]
struct CreateTaskBody {
    #[serde(rename = "baseUrl")]
    base_url: String,
    #[serde(rename = "fileName")]
    file_name: String,
    #[serde(rename = "fileSize")]
    file_size: u64,
    #[serde(rename = "mimeType")]
    mime_type: String,
    #[serde(rename = "localFilePath")]
    local_file_path: String,
    #[serde(rename = "apiToken")]
    api_token: String,
}

#[derive(Deserialize)]
struct CreateGcloudImportBody {
    #[serde(rename = "baseUrl")]
    base_url: String,
    #[serde(rename = "fileName")]
    file_name: String,
    #[serde(rename = "fileSize")]
    file_size: u64,
    #[serde(rename = "mimeType")]
    mime_type: String,
    #[serde(rename = "localFilePath")]
    local_file_path: String,
    #[serde(rename = "apiToken")]
    api_token: String,
    #[serde(rename = "bucketName")]
    bucket_name: String,
    #[serde(rename = "objectPrefix")]
    object_prefix: String,
}

#[derive(Serialize)]
struct FileSelectionData {
    #[serde(rename = "fileName")]
    file_name: String,
    #[serde(rename = "fileSize")]
    file_size: u64,
    #[serde(rename = "mimeType")]
    mime_type: String,
    #[serde(rename = "localFilePath")]
    local_file_path: String,
}

#[derive(Deserialize)]
struct InitResponse {
    #[serde(rename = "uploadId")]
    upload_id: String,
    #[serde(rename = "chunkSize")]
    chunk_size: u64,
    #[serde(rename = "totalChunks")]
    total_chunks: Option<u64>,
}

#[derive(Deserialize)]
struct StatusResponse {
    #[serde(rename = "completedChunks")]
    completed_chunks: Vec<u64>,
}

#[derive(Deserialize)]
struct CompleteResponse {
    url: String,
    filename: String,
    #[serde(rename = "localPath")]
    local_path: Option<String>,
}

#[derive(Deserialize)]
struct ImportGcsAssetResponse {
    success: bool,
    data: ImportGcsAssetData,
    error: Option<String>,
}

#[derive(Deserialize)]
struct ImportGcsAssetData {
    #[serde(rename = "assetId")]
    asset_id: String,
    asset: ImportGcsAsset,
}

#[derive(Deserialize)]
struct ImportGcsAsset {
    #[serde(rename = "gsUri")]
    gs_uri: String,
}

fn updater_configured() -> bool {
    UPDATE_ENDPOINT.is_some_and(|value| !value.trim().is_empty())
        && UPDATE_PUBKEY.is_some_and(|value| !value.trim().is_empty())
}

fn default_updater_snapshot(current_version: String) -> UpdaterSnapshot {
    UpdaterSnapshot {
        configured: updater_configured(),
        checking: false,
        installing: false,
        available: false,
        current_version,
        latest_version: None,
        progress: 0.0,
        downloaded_bytes: 0,
        download_total_bytes: None,
        notes: None,
        endpoint: UPDATE_ENDPOINT.map(|value| value.to_string()),
        last_error: None,
        last_checked_at: None,
    }
}

fn json_header() -> Header {
    Header::from_bytes(
        b"Content-Type".to_vec(),
        b"application/json; charset=utf-8".to_vec(),
    )
    .expect("json header")
}

fn cors_headers() -> Vec<Header> {
    vec![
        Header::from_bytes(b"Access-Control-Allow-Origin".to_vec(), b"*".to_vec())
            .expect("cors allow origin"),
        Header::from_bytes(
            b"Access-Control-Allow-Methods".to_vec(),
            b"GET,POST,OPTIONS".to_vec(),
        )
        .expect("cors allow methods"),
        Header::from_bytes(
            b"Access-Control-Allow-Headers".to_vec(),
            b"Content-Type,Authorization".to_vec(),
        )
        .expect("cors allow headers"),
        Header::from_bytes(
            b"Access-Control-Allow-Private-Network".to_vec(),
            b"true".to_vec(),
        )
        .expect("cors private network"),
    ]
}

fn build_json_response(status: u16, body: serde_json::Value) -> Response<Cursor<Vec<u8>>> {
    let bytes = serde_json::to_vec(&body).expect("serialize response");
    let mut response = Response::from_data(bytes).with_status_code(StatusCode(status));
    response.add_header(json_header());
    for header in cors_headers() {
        response.add_header(header);
    }
    response
}

fn respond(request: Request, status: u16, body: serde_json::Value) {
    let response = build_json_response(status, body);
    let _ = request.respond(response);
}

fn read_json_body<T: for<'de> Deserialize<'de>>(request: &mut Request) -> Result<T, String> {
    let mut body = String::new();
    request
        .as_reader()
        .read_to_string(&mut body)
        .map_err(|error| format!("读取请求体失败：{error}"))?;
    serde_json::from_str(&body).map_err(|error| format!("解析 JSON 失败：{error}"))
}

fn sanitize_base_url(base_url: &str) -> String {
    base_url.trim_end_matches('/').to_string()
}

fn guess_mime_type(path: &Path) -> String {
    MimeGuess::from_path(path)
        .first_raw()
        .unwrap_or("video/mp4")
        .to_string()
}

fn serialize_snapshot(snapshot: &AgentTaskSnapshot) -> serde_json::Value {
    json!({
        "success": true,
        "data": snapshot,
    })
}

fn serialize_snapshots(snapshots: &[AgentTaskSnapshot]) -> serde_json::Value {
    json!({
        "success": true,
        "data": snapshots,
    })
}

fn update_snapshot(handle: &TaskHandle, mutate: impl FnOnce(&mut AgentTaskSnapshot)) {
    if let Ok(mut snapshot) = handle.snapshot.lock() {
        mutate(&mut snapshot);
    }
}

fn now_unix_seconds_string() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs().to_string())
        .unwrap_or_else(|_| "0".to_string())
}

fn update_updater_snapshot(
    updater: &Arc<Mutex<UpdaterRuntimeState>>,
    mutate: impl FnOnce(&mut UpdaterRuntimeState),
) {
    if let Ok(mut state) = updater.lock() {
        mutate(&mut state);
    }
}

fn get_updater_snapshot(updater: &Arc<Mutex<UpdaterRuntimeState>>) -> Option<UpdaterSnapshot> {
    updater
        .lock()
        .ok()
        .map(|state| state.snapshot.clone())
}

fn build_updater(app: &AppHandle<Wry>) -> Result<tauri_plugin_updater::Updater, String> {
    let endpoint = UPDATE_ENDPOINT
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "当前构建未配置自动更新端点".to_string())?;
    let pubkey = UPDATE_PUBKEY
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "当前构建未配置自动更新公钥".to_string())?;
    let endpoint =
        Url::parse(endpoint).map_err(|error| format!("解析自动更新端点失败：{error}"))?;

    app.updater_builder()
        .pubkey(pubkey.to_string())
        .endpoints(vec![endpoint])
        .map_err(|error| format!("配置自动更新端点失败：{error}"))?
        .build()
        .map_err(|error| format!("初始化自动更新器失败：{error}"))
}

fn check_for_updates(
    app: &AppHandle<Wry>,
    updater: &Arc<Mutex<UpdaterRuntimeState>>,
) -> Result<(), String> {
    update_updater_snapshot(updater, |state| {
        state.snapshot.checking = true;
        state.snapshot.last_error = None;
        state.snapshot.progress = 0.0;
        state.snapshot.downloaded_bytes = 0;
        state.snapshot.download_total_bytes = None;
    });

    let result = tauri::async_runtime::block_on(async {
        let updater_client = build_updater(app)?;
        updater_client
            .check()
            .await
            .map_err(|error| format!("检查更新失败：{error}"))
    });

    match result {
        Ok(Some(update)) => {
            update_updater_snapshot(updater, |state| {
                state.pending_update = Some(update.clone());
                state.snapshot.checking = false;
                state.snapshot.available = true;
                state.snapshot.latest_version = Some(update.version.clone());
                state.snapshot.notes = update.body.clone();
                state.snapshot.last_error = None;
                state.snapshot.last_checked_at = Some(now_unix_seconds_string());
            });
            Ok(())
        }
        Ok(None) => {
            update_updater_snapshot(updater, |state| {
                state.pending_update = None;
                state.snapshot.checking = false;
                state.snapshot.available = false;
                state.snapshot.latest_version = None;
                state.snapshot.notes = None;
                state.snapshot.last_error = None;
                state.snapshot.last_checked_at = Some(now_unix_seconds_string());
            });
            Ok(())
        }
        Err(error) => {
            update_updater_snapshot(updater, |state| {
                state.pending_update = None;
                state.snapshot.checking = false;
                state.snapshot.available = false;
                state.snapshot.latest_version = None;
                state.snapshot.last_error = Some(error.clone());
                state.snapshot.last_checked_at = Some(now_unix_seconds_string());
            });
            Err(error)
        }
    }
}

fn spawn_update_install(app: AppHandle<Wry>, updater: Arc<Mutex<UpdaterRuntimeState>>) {
    thread::spawn(move || {
        let pending = updater
            .lock()
            .ok()
            .and_then(|state| state.pending_update.clone());

        let Some(update) = pending else {
            update_updater_snapshot(&updater, |state| {
                state.snapshot.installing = false;
                state.snapshot.last_error = Some("当前没有待安装更新，请先检查更新".to_string());
            });
            return;
        };

        let total_downloaded = Arc::new(AtomicU64::new(0));
        let total_hint = Arc::new(AtomicU64::new(0));
        let progress_updater = Arc::clone(&updater);
        let progress_downloaded = Arc::clone(&total_downloaded);
        let progress_total = Arc::clone(&total_hint);

        let result = tauri::async_runtime::block_on(async move {
            update
                .download_and_install(
                    move |chunk_len, total| {
                        let downloaded = progress_downloaded
                            .fetch_add(chunk_len as u64, Ordering::Relaxed)
                            + chunk_len as u64;
                        let total_bytes = total.unwrap_or(0);
                        progress_total.store(total_bytes, Ordering::Relaxed);
                        let progress = if total_bytes > 0 {
                            ((downloaded as f64 / total_bytes as f64) * 100.0).min(100.0)
                        } else {
                            0.0
                        };

                        update_updater_snapshot(&progress_updater, |state| {
                            state.snapshot.downloaded_bytes = downloaded;
                            state.snapshot.download_total_bytes =
                                if total_bytes > 0 { Some(total_bytes) } else { None };
                            state.snapshot.progress = progress;
                            state.snapshot.last_error = None;
                        });
                    },
                    || {},
                )
                .await
                .map_err(|error| format!("安装更新失败：{error}"))
        });

        match result {
            Ok(()) => {
                update_updater_snapshot(&updater, |state| {
                    state.pending_update = None;
                    state.snapshot.installing = false;
                    state.snapshot.available = false;
                    state.snapshot.progress = 100.0;
                    state.snapshot.last_error = None;
                });
                let _ = app.emit("local-agent:update-installed", ());
            }
            Err(error) => {
                update_updater_snapshot(&updater, |state| {
                    state.snapshot.installing = false;
                    state.snapshot.last_error = Some(error);
                });
            }
        }
    });
}

fn append_log(
    handle: &TaskHandle,
    level: &str,
    message: impl Into<String>,
    chunk_index: Option<u64>,
    detail: Option<String>,
) {
    if let Ok(mut logs) = handle.logs.lock() {
        logs.push(TaskLogEntry {
            timestamp: now_unix_seconds_string(),
            level: level.to_string(),
            message: message.into(),
            chunk_index,
            detail,
        });

        if logs.len() > 200 {
            let extra = logs.len() - 200;
            logs.drain(0..extra);
        }
    }
}

fn get_snapshot(handle: &TaskHandle) -> Option<AgentTaskSnapshot> {
    handle.snapshot.lock().ok().map(|snapshot| snapshot.clone())
}

fn get_logs(handle: &TaskHandle) -> Vec<TaskLogEntry> {
    handle.logs.lock().map(|logs| logs.clone()).unwrap_or_default()
}

fn set_active_pid(handle: &TaskHandle, pid: Option<u32>) {
    if let Ok(mut active_pid) = handle.active_pid.lock() {
        *active_pid = pid;
    }
}

fn terminate_process(pid: u32) {
    #[cfg(target_os = "windows")]
    {
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

fn append_output_tail(output_tail: &Arc<Mutex<Vec<String>>>, line: &str) {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return;
    }

    if let Ok(mut tail) = output_tail.lock() {
        tail.push(trimmed.to_string());
        if tail.len() > 20 {
            let extra = tail.len() - 20;
            tail.drain(0..extra);
        }
    }
}

fn read_child_output<R: Read + Send + 'static>(
    reader: R,
    handle: TaskHandle,
    level: &'static str,
    output_tail: Arc<Mutex<Vec<String>>>,
) {
    thread::spawn(move || {
        let buffered = BufReader::new(reader);
        for line in buffered.lines().map_while(Result::ok) {
            append_output_tail(&output_tail, &line);
            append_log(&handle, level, line, None, None);
        }
    });
}

fn build_gcloud_env_vars() -> Vec<(String, String)> {
    let mut vars = Vec::new();
    let disable_parallel_composite_upload = env::var("LOCAL_UPLOAD_AGENT_GCLOUD_PARALLEL_COMPOSITE_UPLOAD")
        .ok()
        .map(|value| value.trim().eq_ignore_ascii_case("false"))
        .unwrap_or(false);

    if disable_parallel_composite_upload {
        vars.push((
            "CLOUDSDK_STORAGE_PARALLEL_COMPOSITE_UPLOAD_ENABLED".to_string(),
            "False".to_string(),
        ));
        vars.push(("CLOUDSDK_STORAGE_PROCESS_COUNT".to_string(), "1".to_string()));
        vars.push(("CLOUDSDK_STORAGE_THREAD_COUNT".to_string(), "1".to_string()));
    } else {
        vars.push((
            "CLOUDSDK_STORAGE_PARALLEL_COMPOSITE_UPLOAD_ENABLED".to_string(),
            "True".to_string(),
        ));
    }

    vars
}

fn build_gcloud_command(spec: &GcloudCommandSpec, args: &[&str]) -> Command {
    let mut command = Command::new(&spec.program);
    command.args(&spec.prefix_args);
    command.args(args);
    command
}

#[cfg(target_os = "windows")]
fn windows_gcloud_install_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();

    for env_name in ["LOCALAPPDATA", "ProgramW6432", "ProgramFiles", "ProgramFiles(x86)"] {
        if let Some(base) = env::var_os(env_name) {
            let base = PathBuf::from(base);
            candidates.push(
                base.join("Google")
                    .join("Cloud SDK")
                    .join("google-cloud-sdk")
                    .join("bin")
                    .join("gcloud.cmd"),
            );
            candidates.push(
                base.join("Google")
                    .join("Cloud SDK")
                    .join("google-cloud-sdk")
                    .join("bin")
                    .join("gcloud.exe"),
            );
        }
    }

    candidates
}

#[cfg(target_os = "windows")]
fn resolve_gcloud_command() -> Result<GcloudCommandSpec, String> {
    for candidate in ["gcloud.cmd", "gcloud.exe", "gcloud"] {
        let output = Command::new("where.exe").arg(candidate).output();
        if let Ok(output) = output {
            if output.status.success() {
                if let Some(line) = String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .map(str::trim)
                    .find(|line| !line.is_empty())
                {
                    let resolved = PathBuf::from(line);
                    let resolved_string = resolved.to_string_lossy().to_string();
                    let extension = resolved
                        .extension()
                        .and_then(|value| value.to_str())
                        .map(|value| value.to_ascii_lowercase());

                    if matches!(extension.as_deref(), Some("cmd") | Some("bat")) {
                        return Ok(GcloudCommandSpec {
                            program: "cmd.exe".to_string(),
                            prefix_args: vec!["/C".to_string(), resolved_string.clone()],
                            display_path: resolved_string,
                        });
                    }

                    return Ok(GcloudCommandSpec {
                        program: resolved_string.clone(),
                        prefix_args: Vec::new(),
                        display_path: resolved_string,
                    });
                }
            }
        }
    }

    for path in windows_gcloud_install_candidates() {
        if path.is_file() {
            let resolved_string = path.to_string_lossy().to_string();
            let extension = path
                .extension()
                .and_then(|value| value.to_str())
                .map(|value| value.to_ascii_lowercase());

            if matches!(extension.as_deref(), Some("cmd") | Some("bat")) {
                return Ok(GcloudCommandSpec {
                    program: "cmd.exe".to_string(),
                    prefix_args: vec!["/C".to_string(), resolved_string.clone()],
                    display_path: resolved_string,
                });
            }

            return Ok(GcloudCommandSpec {
                program: resolved_string.clone(),
                prefix_args: Vec::new(),
                display_path: resolved_string,
            });
        }
    }

    Err("未检测到 gcloud CLI，请先安装并完成 gcloud auth login".to_string())
}

#[cfg(not(target_os = "windows"))]
fn resolve_gcloud_command() -> Result<GcloudCommandSpec, String> {
    Ok(GcloudCommandSpec {
        program: "gcloud".to_string(),
        prefix_args: Vec::new(),
        display_path: "gcloud".to_string(),
    })
}

fn ensure_gcloud_installed() -> Result<GcloudCommandSpec, String> {
    let spec = resolve_gcloud_command()?;
    let status = build_gcloud_command(&spec, &["version"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    match status {
        Ok(result) if result.success() => Ok(spec),
        Ok(_) => Err("gcloud CLI 不可用，请检查本机安装和登录状态".to_string()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Err("未检测到 gcloud CLI，请先安装并完成 gcloud auth login".to_string())
        }
        Err(error) => Err(format!("gcloud CLI 不可用：{error}")),
    }
}

fn run_gcloud_command(task: &TaskHandle, args: &[&str]) -> Result<(), String> {
    let spec = ensure_gcloud_installed()?;
    append_log(
        task,
        "debug",
        format!("已解析 gcloud CLI 路径：{}", spec.display_path),
        None,
        None,
    );

    let mut command = build_gcloud_command(&spec, args);
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    command.envs(build_gcloud_env_vars());

    let mut child = command
        .spawn()
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => {
                "未检测到 gcloud CLI，请先安装并完成 gcloud auth login".to_string()
            }
            _ => format!("启动 gcloud 命令失败：{error}"),
        })?;

    let pid = child.id();
    set_active_pid(task, Some(pid));

    let output_tail = Arc::new(Mutex::new(Vec::<String>::new()));

    if let Some(stdout) = child.stdout.take() {
        read_child_output(stdout, task.clone(), "debug", Arc::clone(&output_tail));
    }

    if let Some(stderr) = child.stderr.take() {
        read_child_output(stderr, task.clone(), "warn", Arc::clone(&output_tail));
    }

    let status = child.wait().map_err(|error| format!("等待 gcloud 命令失败：{error}"))?;
    set_active_pid(task, None);

    if status.success() {
        return Ok(());
    }

    let last_output = output_tail
        .lock()
        .ok()
        .and_then(|tail| tail.last().cloned())
        .unwrap_or_else(|| format!("gcloud 命令执行失败，退出码 {}", status.code().unwrap_or(1)));

    Err(last_output)
}

fn build_auth(request: reqwest::blocking::RequestBuilder, api_token: &str) -> reqwest::blocking::RequestBuilder {
    request.bearer_auth(api_token)
}

fn get_chunk_length(file_size: u64, chunk_size: u64, chunk_index: u64) -> usize {
    let start = chunk_index.saturating_mul(chunk_size);
    if start >= file_size {
        return 0;
    }
    std::cmp::min(chunk_size, file_size - start) as usize
}

fn sanitize_filename(filename: &str) -> String {
    let path = Path::new(filename);
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("video");
    let ext = path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| format!(".{}", value.to_lowercase()))
        .unwrap_or_else(|| ".mp4".to_string());

    let mut sanitized = String::new();
    let mut previous_dash = false;

    for ch in stem.chars().flat_map(|value| value.to_lowercase()) {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            sanitized.push(ch);
            previous_dash = false;
        } else if !previous_dash {
            sanitized.push('-');
            previous_dash = true;
        }
    }

    let sanitized = sanitized.trim_matches('-');
    format!("{}{}", if sanitized.is_empty() { "video" } else { sanitized }, ext)
}

fn build_gcloud_object_name(object_prefix: &str, task_id: &str, filename: &str) -> String {
    format!(
        "{}/{}/{}",
        object_prefix.trim_end_matches('/'),
        task_id,
        sanitize_filename(filename)
    )
}

fn gcloud_config_dir() -> PathBuf {
    if let Ok(value) = env::var("CLOUDSDK_CONFIG") {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }

    #[cfg(target_os = "windows")]
    {
        if let Ok(value) = env::var("APPDATA") {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return PathBuf::from(trimmed).join("gcloud");
            }
        }
    }

    if let Ok(value) = env::var("HOME") {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            #[cfg(target_os = "windows")]
            {
                return PathBuf::from(trimmed).join("AppData").join("Roaming").join("gcloud");
            }

            #[cfg(not(target_os = "windows"))]
            {
                return PathBuf::from(trimmed).join(".config").join("gcloud");
            }
        }
    }

    PathBuf::from(".")
}

fn gcloud_tracker_dir() -> PathBuf {
    gcloud_config_dir()
        .join("surface_data")
        .join("storage")
        .join("tracker_files")
}

fn parse_gcloud_uploaded_bytes(range_header: Option<&str>) -> Option<u64> {
    match range_header.map(str::trim).filter(|value| !value.is_empty()) {
        None => Some(0),
        Some(value) => value
            .strip_prefix("bytes=0-")
            .and_then(|suffix| suffix.parse::<u64>().ok())
            .map(|end_offset| end_offset + 1),
    }
}

fn tracker_file_mtime_ok(path: &Path, started_at: SystemTime) -> Option<SystemTime> {
    let metadata = fs::metadata(path).ok()?;
    let modified = metadata.modified().ok()?;
    let lower_bound = started_at.checked_sub(Duration::from_secs(60)).unwrap_or(started_at);
    if modified >= lower_bound {
        Some(modified)
    } else {
        None
    }
}

fn query_tracker_progress(
    client: &Client,
    tracker_file_path: &Path,
) -> Option<(u64, u64)> {
    let tracker = fs::read_to_string(tracker_file_path).ok()?;
    let tracker = serde_json::from_str::<Value>(&tracker).ok()?;
    let serialization_data = tracker.get("serialization_data")?;
    let total_bytes = serialization_data.get("total_size")?.as_u64()?;
    let upload_url = serialization_data.get("url")?.as_str()?;
    if total_bytes == 0 || upload_url.trim().is_empty() {
        return None;
    }

    let response = client
        .put(upload_url)
        .timeout(Duration::from_secs(5))
        .header("Content-Length", "0")
        .header("Content-Range", format!("bytes */{total_bytes}"))
        .send()
        .ok()?;

    if response.status().as_u16() != 308 {
        return None;
    }

    let uploaded_bytes = parse_gcloud_uploaded_bytes(
        response
            .headers()
            .get("range")
            .and_then(|value| value.to_str().ok()),
    )?;

    Some((uploaded_bytes.min(total_bytes), total_bytes))
}

fn find_gcloud_tracker_file(task: &AgentUploadTask, started_at: SystemTime) -> Option<PathBuf> {
    let tracker_dir = gcloud_tracker_dir();
    let normalized_file_name = Path::new(&task.local_file_path)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_lowercase();

    fs::read_dir(tracker_dir)
        .ok()?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            let file_name = path.file_name().and_then(|value| value.to_str()).unwrap_or("");
            file_name.starts_with("upload_TRACKER_")
                && file_name.ends_with("__gs.url")
                && file_name.to_lowercase().contains(&normalized_file_name)
        })
        .filter_map(|path| tracker_file_mtime_ok(&path, started_at).map(|mtime| (path, mtime)))
        .max_by_key(|(_, mtime)| *mtime)
        .map(|(path, _)| path)
}

fn find_gcloud_parallel_tracker_files(
    task: &AgentUploadTask,
    started_at: SystemTime,
) -> Option<Vec<PathBuf>> {
    let tracker_dir = gcloud_tracker_dir();
    let normalized_file_name = Path::new(&task.local_file_path)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_lowercase();

    let entries = fs::read_dir(tracker_dir).ok()?;
    let mut manifest_anchor_time =
        started_at.checked_sub(Duration::from_secs(60)).unwrap_or(started_at);
    let mut grouped = HashMap::<String, Vec<(PathBuf, SystemTime)>>::new();

    for entry in entries.filter_map(|value| value.ok()) {
        let path = entry.path();
        let file_name = path.file_name().and_then(|value| value.to_str()).unwrap_or("");
        let file_name_lower = file_name.to_lowercase();

        if file_name.starts_with("parallel_upload_TRACKER_")
            && file_name.ends_with("__gs.url")
            && file_name_lower.contains(&normalized_file_name)
        {
            if let Some(mtime) = tracker_file_mtime_ok(&path, started_at) {
                if mtime > manifest_anchor_time {
                    manifest_anchor_time = mtime;
                }
            }
        }
    }

    let entries = fs::read_dir(gcloud_tracker_dir()).ok()?;
    for entry in entries.filter_map(|value| value.ok()) {
        let path = entry.path();
        let file_name = path.file_name().and_then(|value| value.to_str()).unwrap_or("");
        let is_parallel_tracker =
            file_name.starts_with("upload_TRACKER_") && file_name.contains("__gs.url_");
        if !is_parallel_tracker {
            continue;
        }

        let Some((group_key, _)) = file_name.split_once("__gs.url_") else {
            continue;
        };

        if let Some(mtime) = tracker_file_mtime_ok(&path, started_at) {
            if mtime < manifest_anchor_time {
                continue;
            }
            grouped
                .entry(group_key.to_string())
                .or_default()
                .push((path, mtime));
        }
    }

    grouped
        .into_values()
        .filter(|group| !group.is_empty())
        .max_by_key(|group| group.iter().map(|(_, mtime)| *mtime).max())
        .map(|mut group| {
            group.sort_by(|left, right| left.0.cmp(&right.0));
            group.into_iter().map(|(path, _)| path).collect::<Vec<_>>()
        })
}

fn get_gcloud_resumable_progress(
    client: &Client,
    task: &AgentUploadTask,
    started_at: SystemTime,
) -> Option<(u64, u64)> {
    if let Some(tracker_file_path) = find_gcloud_tracker_file(task, started_at) {
        return query_tracker_progress(client, &tracker_file_path)
            .map(|(uploaded_bytes, total_bytes)| (uploaded_bytes.min(task.file_size), total_bytes));
    }

    let tracker_files = find_gcloud_parallel_tracker_files(task, started_at)?;
    if tracker_files.is_empty() {
        return None;
    }

    let mut uploaded_bytes = 0_u64;
    let mut found = false;
    for tracker_file_path in tracker_files {
        if let Some((partial_uploaded_bytes, _)) = query_tracker_progress(client, &tracker_file_path) {
            uploaded_bytes = uploaded_bytes.saturating_add(partial_uploaded_bytes);
            found = true;
        }
    }

    if found {
        Some((uploaded_bytes.min(task.file_size), task.file_size))
    } else {
        None
    }
}

fn read_chunk(file: &mut File, offset: u64, chunk_size: usize) -> Result<Vec<u8>, String> {
    file.seek(SeekFrom::Start(offset))
        .map_err(|error| format!("定位分片失败：{error}"))?;

    let mut buffer = vec![0; chunk_size];
    let read_bytes = file
        .read(&mut buffer)
        .map_err(|error| format!("读取分片失败：{error}"))?;

    buffer.truncate(read_bytes);
    Ok(buffer)
}

fn get_remote_status(
    client: &Client,
    base_url: &str,
    upload_id: &str,
    api_token: &str,
) -> Result<StatusResponse, String> {
    let response = build_auth(
        client.get(format!(
            "{base_url}/api/upload/video/status?uploadId={upload_id}"
        )),
        api_token,
    )
    .send()
    .map_err(|error| format!("查询上传状态失败：{error}"))?;

    if !response.status().is_success() {
        return Err(format!("查询上传状态失败：HTTP {}", response.status()));
    }

    response
        .json::<StatusResponse>()
        .map_err(|error| format!("解析上传状态失败：{error}"))
}

fn abort_remote_upload(client: &Client, base_url: &str, upload_id: &str, api_token: &str) {
    let _ = build_auth(
        client.post(format!("{base_url}/api/upload/video/abort")),
        api_token,
    )
    .json(&json!({ "uploadId": upload_id }))
    .send();
}

fn revoke_agent_token(client: &Client, base_url: &str, api_token: &str) {
    let _ = build_auth(
        client.delete(format!("{base_url}/api/upload/video/agent-token")),
        api_token,
    )
    .send();
}

fn schedule_task_cleanup(tasks: Arc<Mutex<HashMap<String, TaskHandle>>>, task_id: String) {
    thread::spawn(move || {
        thread::sleep(Duration::from_secs(CLEANUP_DELAY_SECONDS));
        if let Ok(mut tasks) = tasks.lock() {
            let removable = tasks
                .get(&task_id)
                .and_then(|handle| get_snapshot(handle))
                .map(|snapshot| {
                    matches!(
                        snapshot.status,
                        AgentTaskStatus::Completed | AgentTaskStatus::Cancelled | AgentTaskStatus::Failed
                    )
                })
                .unwrap_or(false);

            if removable {
                tasks.remove(&task_id);
            }
        }
    });
}

fn update_progress_snapshot(
    handle: &TaskHandle,
    upload_id: &str,
    total_uploaded_bytes: u64,
    file_size: u64,
    started_at: Instant,
) {
    let elapsed = started_at.elapsed().as_secs_f64().max(0.001);
    let progress = ((total_uploaded_bytes as f64 / file_size as f64) * 100.0).min(99.0);

    update_snapshot(handle, |snapshot| {
        snapshot.status = AgentTaskStatus::Uploading;
        snapshot.upload_id = Some(upload_id.to_string());
        snapshot.uploaded_bytes = total_uploaded_bytes;
        snapshot.total_bytes = file_size;
        snapshot.progress = progress;
        snapshot.speed_bytes_per_second = total_uploaded_bytes as f64 / elapsed;
    });
}

fn upload_chunk_with_retry(
    client: &Client,
    handle: &TaskHandle,
    task: &AgentUploadTask,
    upload_id: &str,
    chunk_index: u64,
    chunk_size: u64,
    cancel_requested: &AtomicBool,
) -> Result<usize, String> {
    append_log(
        handle,
        "debug",
        "分片开始上传",
        Some(chunk_index),
        Some(format!("chunkSize={chunk_size}")),
    );
    let mut file =
        File::open(&task.local_file_path).map_err(|error| format!("打开本地文件失败：{error}"))?;
    let chunk_len = get_chunk_length(task.file_size, chunk_size, chunk_index);
    let chunk = read_chunk(&mut file, chunk_index.saturating_mul(chunk_size), chunk_len)?;
    let mut last_error = String::new();

    for attempt in 0..=DEFAULT_RETRY_COUNT {
        if cancel_requested.load(Ordering::Relaxed) {
            return Err("__cancelled__".to_string());
        }

        let response = build_auth(
            client.put(format!(
                "{}/api/upload/video/chunk?uploadId={}&chunkIndex={}",
                task.base_url, upload_id, chunk_index
            )),
            &task.api_token,
        )
        .header("Content-Type", "application/octet-stream")
        .header("x-upload-id", upload_id.to_string())
        .header("x-chunk-index", chunk_index.to_string())
        .body(chunk.clone())
        .send();

        match response {
            Ok(response) if response.status().is_success() => return Ok(chunk.len()),
            Ok(response) => {
                let status = response.status();
                let body = response.text().unwrap_or_default();
                last_error = format!("HTTP {status} {body}");
            }
            Err(error) => {
                last_error = error.to_string();
            }
        }

        if let Ok(status) = get_remote_status(client, &task.base_url, upload_id, &task.api_token) {
            if status.completed_chunks.iter().any(|value| *value == chunk_index) {
                append_log(
                    handle,
                    "debug",
                    "分片已由远端状态确认成功",
                    Some(chunk_index),
                    None,
                );
                return Ok(chunk.len());
            }
        }

        if attempt < DEFAULT_RETRY_COUNT {
            append_log(
                handle,
                "warn",
                "分片上传失败，准备重试",
                Some(chunk_index),
                Some(last_error.clone()),
            );
            thread::sleep(Duration::from_millis(800));
        }
    }

    Err(format!("上传分片失败：chunk #{chunk_index} {last_error}"))
}

fn run_parallel_chunk_upload(
    task: &AgentUploadTask,
    handle: &TaskHandle,
    client: &Client,
    upload_id: &str,
    chunk_size: u64,
    total_chunks: u64,
    started_at: Instant,
) -> Result<(), String> {
    let next_chunk_index = Arc::new(AtomicU64::new(0));
    let completed_bytes = Arc::new(AtomicU64::new(0));
    let failure = Arc::new(Mutex::new(None::<String>));

    thread::scope(|scope| {
        for _ in 0..DEFAULT_PARALLEL_UPLOADS {
            let worker_task = task.clone();
            let worker_handle = handle.clone();
            let worker_client = client.clone();
            let worker_upload_id = upload_id.to_string();
            let worker_next_chunk_index = Arc::clone(&next_chunk_index);
            let worker_completed_bytes = Arc::clone(&completed_bytes);
            let worker_failure = Arc::clone(&failure);
            let worker_cancel_requested = Arc::clone(&handle.cancel_requested);

            scope.spawn(move || loop {
                if worker_cancel_requested.load(Ordering::Relaxed) {
                    break;
                }

                let chunk_index = worker_next_chunk_index.fetch_add(1, Ordering::Relaxed);
                if chunk_index >= total_chunks {
                    break;
                }

                let result = upload_chunk_with_retry(
                    &worker_client,
                    &worker_handle,
                    &worker_task,
                    &worker_upload_id,
                    chunk_index,
                    chunk_size,
                    worker_cancel_requested.as_ref(),
                );

                match result {
                    Ok(uploaded_len) => {
                        append_log(
                            &worker_handle,
                            "debug",
                            "分片上传成功",
                            Some(chunk_index),
                            Some(format!("uploadedBytes={uploaded_len}")),
                        );
                        let total_uploaded_bytes =
                            worker_completed_bytes.fetch_add(uploaded_len as u64, Ordering::Relaxed)
                                + uploaded_len as u64;
                        update_progress_snapshot(
                            &worker_handle,
                            &worker_upload_id,
                            total_uploaded_bytes,
                            worker_task.file_size,
                            started_at,
                        );
                    }
                    Err(error) if error == "__cancelled__" => {
                        append_log(
                            &worker_handle,
                            "warn",
                            "分片上传被取消",
                            Some(chunk_index),
                            None,
                        );
                        worker_cancel_requested.store(true, Ordering::Relaxed);
                        break;
                    }
                    Err(error) => {
                        append_log(
                            &worker_handle,
                            "warn",
                            "分片上传失败",
                            Some(chunk_index),
                            Some(error.clone()),
                        );
                        worker_cancel_requested.store(true, Ordering::Relaxed);
                        if let Ok(mut failure) = worker_failure.lock() {
                            if failure.is_none() {
                                *failure = Some(error);
                            }
                        }
                        break;
                    }
                }
            });
        }
    });

    if handle.cancel_requested.load(Ordering::Relaxed) {
        return Err("__cancelled__".to_string());
    }

    if let Ok(failure) = failure.lock() {
        if let Some(error) = failure.clone() {
            return Err(error);
        }
    }

    update_progress_snapshot(handle, upload_id, task.file_size, task.file_size, started_at);
    Ok(())
}

fn run_upload_task(
    task: AgentUploadTask,
    handle: TaskHandle,
    client: Client,
    tasks: Arc<Mutex<HashMap<String, TaskHandle>>>,
) {
    let started_at = Instant::now();
    let mut upload_id: Option<String> = None;

    let result = (|| -> Result<UploadTaskResult, String> {
        append_log(&handle, "debug", "上传任务已创建", None, None);
        update_snapshot(&handle, |snapshot| {
            snapshot.status = AgentTaskStatus::Queued;
            snapshot.progress = 0.0;
            snapshot.uploaded_bytes = 0;
            snapshot.total_bytes = task.file_size;
            snapshot.speed_bytes_per_second = 0.0;
            snapshot.error = None;
        });

        let init_response = build_auth(
            client.post(format!("{}/api/upload/video/init", task.base_url)),
            &task.api_token,
        )
        .json(&json!({
            "filename": task.file_name,
            "fileSize": task.file_size,
            "mimeType": task.mime_type,
        }))
        .send()
        .map_err(|error| format!("初始化上传失败：{error}"))?;

        if !init_response.status().is_success() {
            let status = init_response.status();
            let body = init_response.text().unwrap_or_default();
            return Err(format!("初始化上传失败：HTTP {status} {body}"));
        }

        let init = init_response
            .json::<InitResponse>()
            .map_err(|error| format!("解析上传初始化响应失败：{error}"))?;

        let upload_id_value = init.upload_id;
        let chunk_size = init.chunk_size.max(1);
        let total_chunks = init
            .total_chunks
            .unwrap_or_else(|| (task.file_size + chunk_size - 1) / chunk_size)
            .max(1);

        upload_id = Some(upload_id_value.clone());
        append_log(
            &handle,
            "debug",
            "远端上传会话初始化成功",
            None,
            Some(format!(
                "uploadId={upload_id_value}, totalChunks={total_chunks}, chunkSize={chunk_size}"
            )),
        );

        update_snapshot(&handle, |snapshot| {
            snapshot.status = AgentTaskStatus::Uploading;
            snapshot.upload_id = Some(upload_id_value.clone());
            snapshot.total_bytes = task.file_size;
        });

        run_parallel_chunk_upload(
            &task,
            &handle,
            &client,
            &upload_id_value,
            chunk_size,
            total_chunks,
            started_at,
        )?;

        if handle.cancel_requested.load(Ordering::Relaxed) {
            return Err("__cancelled__".to_string());
        }

        append_log(&handle, "debug", "分片上传完成，开始 finalize", None, None);
        update_snapshot(&handle, |snapshot| {
            snapshot.status = AgentTaskStatus::Finalizing;
            snapshot.progress = 99.0;
            snapshot.uploaded_bytes = task.file_size;
            snapshot.total_bytes = task.file_size;
            snapshot.speed_bytes_per_second = 0.0;
        });

        let complete_response = build_auth(
            client.post(format!("{}/api/upload/video/complete", task.base_url)),
            &task.api_token,
        )
        .json(&json!({
            "uploadId": upload_id_value,
        }))
        .send()
        .map_err(|error| format!("完成上传失败：{error}"))?;

        if !complete_response.status().is_success() {
            let status = complete_response.status();
            let body = complete_response.text().unwrap_or_default();
            return Err(format!("完成上传失败：HTTP {status} {body}"));
        }

        let completed = complete_response
            .json::<CompleteResponse>()
            .map_err(|error| format!("解析完成上传响应失败：{error}"))?;

        Ok(UploadTaskResult {
            url: completed.url,
            filename: completed.filename,
            local_path: completed.local_path,
            gcs_gs_uri: None,
            asset_id: None,
        })
    })();

    match result {
        Ok(task_result) => {
            append_log(
                &handle,
                "debug",
                "上传任务完成",
                None,
                Some(format!("filename={}", task_result.filename)),
            );
            update_snapshot(&handle, |snapshot| {
                snapshot.status = AgentTaskStatus::Completed;
                snapshot.progress = 100.0;
                snapshot.uploaded_bytes = task.file_size;
                snapshot.total_bytes = task.file_size;
                snapshot.speed_bytes_per_second = 0.0;
                snapshot.result = Some(task_result);
                snapshot.error = None;
            });
        }
        Err(error) if error == "__cancelled__" => {
            if let Some(upload_id_value) = upload_id.as_deref() {
                abort_remote_upload(&client, &task.base_url, upload_id_value, &task.api_token);
            }

            append_log(&handle, "warn", "上传任务已取消", None, None);
            update_snapshot(&handle, |snapshot| {
                snapshot.status = AgentTaskStatus::Cancelled;
                snapshot.progress = 0.0;
                snapshot.speed_bytes_per_second = 0.0;
                snapshot.error = None;
            });
        }
        Err(error) => {
            if let Some(upload_id_value) = upload_id.as_deref() {
                abort_remote_upload(&client, &task.base_url, upload_id_value, &task.api_token);
            }

            append_log(&handle, "warn", "上传任务失败", None, Some(error.clone()));
            update_snapshot(&handle, |snapshot| {
                snapshot.status = AgentTaskStatus::Failed;
                snapshot.speed_bytes_per_second = 0.0;
                snapshot.error = Some(error);
            });
        }
    }

    revoke_agent_token(&client, &task.base_url, &task.api_token);
    schedule_task_cleanup(tasks, task.task_id);
}

fn import_gcs_asset(
    client: &Client,
    task: &AgentUploadTask,
    gs_uri: &str,
) -> Result<ImportGcsAssetResponse, String> {
    let response = build_auth(
        client.post(format!("{}/api/media/import-gcs", task.base_url)),
        &task.api_token,
    )
    .json(&json!({
        "gsUri": gs_uri,
        "originalFilename": task.file_name,
    }))
    .send()
    .map_err(|error| format!("导入 GCS 素材失败：{error}"))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().unwrap_or_default();
        return Err(format!("导入 GCS 素材失败：HTTP {status} {body}"));
    }

    let imported = response
        .json::<ImportGcsAssetResponse>()
        .map_err(|error| format!("解析 GCS 素材导入响应失败：{error}"))?;

    if !imported.success {
        return Err(
            imported
                .error
                .unwrap_or_else(|| "导入 GCS 素材失败".to_string()),
        );
    }

    Ok(imported)
}

fn run_gcloud_import_task(
    task: AgentUploadTask,
    handle: TaskHandle,
    client: Client,
    tasks: Arc<Mutex<HashMap<String, TaskHandle>>>,
) {
    let started_at_system = SystemTime::now();

    let result = (|| -> Result<UploadTaskResult, String> {
        let bucket_name = task
            .bucket_name
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| "GCloud 导入任务缺少 bucketName".to_string())?;
        let object_prefix = task
            .object_prefix
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| "GCloud 导入任务缺少 objectPrefix".to_string())?;
        let object_name = build_gcloud_object_name(object_prefix, &task.task_id, &task.file_name);
        let gs_uri = format!("gs://{bucket_name}/{object_name}");

        append_log(&handle, "debug", "GCloud 导入任务已创建", None, None);
        update_snapshot(&handle, |snapshot| {
            snapshot.status = AgentTaskStatus::Uploading;
            snapshot.progress = 5.0;
            snapshot.uploaded_bytes = 0;
            snapshot.total_bytes = task.file_size;
            snapshot.speed_bytes_per_second = 0.0;
            snapshot.error = None;
            snapshot.gs_uri = Some(gs_uri.clone());
            snapshot.object_name = Some(object_name.clone());
        });

        let gcloud_spec = ensure_gcloud_installed()?;
        append_log(
            &handle,
            "debug",
            format!("已检测到 gcloud CLI：{}，开始上传到 GCS", gcloud_spec.display_path),
            None,
            Some(gs_uri.clone()),
        );

        let mut command =
            build_gcloud_command(&gcloud_spec, &["storage", "cp", "--quiet", &task.local_file_path, &gs_uri]);
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());
        command.envs(build_gcloud_env_vars());

        let mut child = command.spawn().map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => {
                "未检测到 gcloud CLI，请先安装并完成 gcloud auth login".to_string()
            }
            _ => format!("启动 gcloud 上传失败：{error}"),
        })?;

        let pid = child.id();
        set_active_pid(&handle, Some(pid));

        let output_tail = Arc::new(Mutex::new(Vec::<String>::new()));
        if let Some(stdout) = child.stdout.take() {
            read_child_output(stdout, handle.clone(), "debug", Arc::clone(&output_tail));
        }
        if let Some(stderr) = child.stderr.take() {
            read_child_output(stderr, handle.clone(), "warn", Arc::clone(&output_tail));
        }

        let mut last_uploaded_bytes = 0_u64;
        let mut last_sampled_at: Option<Instant> = None;
        let status = loop {
            if handle.cancel_requested.load(Ordering::Relaxed) {
                terminate_process(pid);
                let _ = child.wait();
                set_active_pid(&handle, None);
                return Err("__cancelled__".to_string());
            }

            match child.try_wait() {
                Ok(Some(status)) => {
                    set_active_pid(&handle, None);
                    break status;
                }
                Ok(None) => {
                    if let Some((uploaded_bytes, total_bytes)) =
                        get_gcloud_resumable_progress(&client, &task, started_at_system)
                    {
                        let sampled_at = Instant::now();
                        let delta_bytes = uploaded_bytes.saturating_sub(last_uploaded_bytes);
                        let speed = if let Some(previous_sampled_at) = last_sampled_at {
                            let elapsed_seconds =
                                sampled_at.duration_since(previous_sampled_at).as_secs_f64().max(0.001);
                            delta_bytes as f64 / elapsed_seconds
                        } else {
                            0.0
                        };

                        last_uploaded_bytes = uploaded_bytes;
                        last_sampled_at = Some(sampled_at);

                        update_snapshot(&handle, |snapshot| {
                            snapshot.status = AgentTaskStatus::Uploading;
                            snapshot.uploaded_bytes = uploaded_bytes;
                            snapshot.total_bytes = total_bytes.max(task.file_size);
                            snapshot.progress =
                                ((uploaded_bytes as f64 / task.file_size as f64) * 100.0).clamp(5.0, 88.0);
                            if speed > 0.0 {
                                snapshot.speed_bytes_per_second = speed;
                            }
                        });
                    }

                    thread::sleep(Duration::from_secs(3));
                }
                Err(error) => {
                    set_active_pid(&handle, None);
                    return Err(format!("等待 gcloud 上传进程失败：{error}"));
                }
            }
        };

        if !status.success() {
            let last_output = output_tail
                .lock()
                .ok()
                .and_then(|tail| tail.last().cloned())
                .unwrap_or_else(|| format!("gcloud 命令执行失败，退出码 {}", status.code().unwrap_or(1)));
            return Err(last_output);
        }

        update_snapshot(&handle, |snapshot| {
            snapshot.uploaded_bytes = task.file_size;
            snapshot.total_bytes = task.file_size;
            snapshot.progress = 88.0;
            snapshot.speed_bytes_per_second = 0.0;
        });

        run_gcloud_command(&handle, &["storage", "objects", "describe", &gs_uri, "--format=json"])?;

        append_log(&handle, "debug", "GCS 对象已上传完成，开始导入站内素材", None, Some(gs_uri.clone()));
        update_snapshot(&handle, |snapshot| {
            snapshot.status = AgentTaskStatus::Finalizing;
            snapshot.progress = 95.0;
            snapshot.speed_bytes_per_second = 0.0;
        });

        let imported = import_gcs_asset(&client, &task, &gs_uri)?;
        append_log(
            &handle,
            "debug",
            "GCS 素材已导入并回填 asset",
            None,
            Some(format!("assetId={}", imported.data.asset_id)),
        );

        Ok(UploadTaskResult {
            url: imported.data.asset.gs_uri.clone(),
            filename: task.file_name.clone(),
            local_path: None,
            gcs_gs_uri: Some(imported.data.asset.gs_uri),
            asset_id: Some(imported.data.asset_id),
        })
    })();

    match result {
        Ok(task_result) => {
            update_snapshot(&handle, |snapshot| {
                snapshot.status = AgentTaskStatus::Completed;
                snapshot.progress = 100.0;
                snapshot.uploaded_bytes = task.file_size;
                snapshot.total_bytes = task.file_size;
                snapshot.speed_bytes_per_second = 0.0;
                snapshot.result = Some(task_result);
                snapshot.error = None;
            });
        }
        Err(error) if error == "__cancelled__" => {
            append_log(&handle, "warn", "GCloud 导入任务已取消", None, None);
            update_snapshot(&handle, |snapshot| {
                snapshot.status = AgentTaskStatus::Cancelled;
                snapshot.progress = 0.0;
                snapshot.speed_bytes_per_second = 0.0;
                snapshot.error = None;
            });
        }
        Err(error) => {
            append_log(&handle, "warn", "GCloud 导入任务失败", None, Some(error.clone()));
            update_snapshot(&handle, |snapshot| {
                snapshot.status = AgentTaskStatus::Failed;
                snapshot.speed_bytes_per_second = 0.0;
                snapshot.error = Some(error);
            });
        }
    }

    revoke_agent_token(&client, &task.base_url, &task.api_token);
    schedule_task_cleanup(tasks, task.task_id);
}

fn handle_pick_file(request: Request) {
    let picked = FileDialog::new()
        .add_filter("视频文件", &["mp4"])
        .pick_file();

    let Some(path) = picked else {
        respond(
            request,
            400,
            json!({
                "success": false,
                "error": "已取消选择文件"
            }),
        );
        return;
    };

    let metadata = match std::fs::metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) => {
            respond(
                request,
                500,
                json!({
                    "success": false,
                    "error": format!("读取文件信息失败：{error}")
                }),
            );
            return;
        }
    };

    let body = FileSelectionData {
        file_name: path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("video.mp4")
            .to_string(),
        file_size: metadata.len(),
        mime_type: guess_mime_type(&path),
        local_file_path: path.to_string_lossy().to_string(),
    };

    respond(
        request,
        200,
        json!({
            "success": true,
            "data": body
        }),
    );
}

fn handle_create_upload(mut request: Request, state: &AgentHttpState) {
    let body = match read_json_body::<CreateTaskBody>(&mut request) {
        Ok(body) => body,
        Err(error) => {
            respond(
                request,
                400,
                json!({
                    "success": false,
                    "error": error
                }),
            );
            return;
        }
    };

    if body.base_url.trim().is_empty()
        || body.file_name.trim().is_empty()
        || body.mime_type.trim().is_empty()
        || body.local_file_path.trim().is_empty()
        || body.api_token.trim().is_empty()
        || body.file_size == 0
    {
        respond(
            request,
            400,
            json!({
                "success": false,
                "error": "baseUrl、localFilePath、fileName、fileSize、apiToken 必填"
            }),
        );
        return;
    }

    let file_path = PathBuf::from(body.local_file_path.trim());
    let metadata = match std::fs::metadata(&file_path) {
        Ok(metadata) => metadata,
        Err(error) => {
            respond(
                request,
                400,
                json!({
                    "success": false,
                    "error": format!("读取本地文件失败：{error}")
                }),
            );
            return;
        }
    };

    if metadata.len() != body.file_size {
        respond(
            request,
            400,
            json!({
                "success": false,
                "error": "所选文件大小与页面记录不一致，请重新选择"
            }),
        );
        return;
    }

    let task_id = Uuid::new_v4().to_string();
    let handle = TaskHandle {
        snapshot: Arc::new(Mutex::new(AgentTaskSnapshot {
            task_id: task_id.clone(),
            status: AgentTaskStatus::Queued,
            file_name: body.file_name.trim().to_string(),
            file_size: body.file_size,
            uploaded_bytes: 0,
            total_bytes: body.file_size,
            progress: 0.0,
            speed_bytes_per_second: 0.0,
            upload_id: None,
            gs_uri: None,
            object_name: None,
            error: None,
            result: None,
        })),
        cancel_requested: Arc::new(AtomicBool::new(false)),
        active_pid: Arc::new(Mutex::new(None)),
        logs: Arc::new(Mutex::new(Vec::new())),
    };

    if let Ok(mut tasks) = state.tasks.lock() {
        tasks.insert(task_id.clone(), handle.clone());
    }

    let upload_task = AgentUploadTask {
        task_id: task_id.clone(),
        base_url: sanitize_base_url(&body.base_url),
        file_name: body.file_name.trim().to_string(),
        file_size: body.file_size,
        mime_type: body.mime_type.trim().to_string(),
        local_file_path: body.local_file_path.trim().to_string(),
        api_token: body.api_token.trim().to_string(),
        bucket_name: None,
        object_prefix: None,
    };

    let background_handle = handle.clone();
    let background_client = state.http.clone();
    let background_tasks = Arc::clone(&state.tasks);
    thread::spawn(move || {
        run_upload_task(upload_task, background_handle, background_client, background_tasks);
    });

    let snapshot = get_snapshot(&handle).unwrap();
    respond(request, 200, serialize_snapshot(&snapshot));
}

fn handle_create_gcloud_import(mut request: Request, state: &AgentHttpState) {
    let body = match read_json_body::<CreateGcloudImportBody>(&mut request) {
        Ok(body) => body,
        Err(error) => {
            respond(
                request,
                400,
                json!({
                    "success": false,
                    "error": error
                }),
            );
            return;
        }
    };

    if body.base_url.trim().is_empty()
        || body.file_name.trim().is_empty()
        || body.mime_type.trim().is_empty()
        || body.local_file_path.trim().is_empty()
        || body.api_token.trim().is_empty()
        || body.bucket_name.trim().is_empty()
        || body.object_prefix.trim().is_empty()
        || body.file_size == 0
    {
        respond(
            request,
            400,
            json!({
                "success": false,
                "error": "baseUrl、localFilePath、fileName、fileSize、apiToken、bucketName、objectPrefix 必填"
            }),
        );
        return;
    }

    let file_path = PathBuf::from(body.local_file_path.trim());
    let metadata = match std::fs::metadata(&file_path) {
        Ok(metadata) => metadata,
        Err(error) => {
            respond(
                request,
                400,
                json!({
                    "success": false,
                    "error": format!("读取本地文件失败：{error}")
                }),
            );
            return;
        }
    };

    if metadata.len() != body.file_size {
        respond(
            request,
            400,
            json!({
                "success": false,
                "error": "所选文件大小与页面记录不一致，请重新选择"
            }),
        );
        return;
    }

    let task_id = Uuid::new_v4().to_string();
    let handle = TaskHandle {
        snapshot: Arc::new(Mutex::new(AgentTaskSnapshot {
            task_id: task_id.clone(),
            status: AgentTaskStatus::Queued,
            file_name: body.file_name.trim().to_string(),
            file_size: body.file_size,
            uploaded_bytes: 0,
            total_bytes: body.file_size,
            progress: 0.0,
            speed_bytes_per_second: 0.0,
            upload_id: None,
            gs_uri: None,
            object_name: None,
            error: None,
            result: None,
        })),
        cancel_requested: Arc::new(AtomicBool::new(false)),
        active_pid: Arc::new(Mutex::new(None)),
        logs: Arc::new(Mutex::new(Vec::new())),
    };

    if let Ok(mut tasks) = state.tasks.lock() {
        tasks.insert(task_id.clone(), handle.clone());
    }

    let upload_task = AgentUploadTask {
        task_id: task_id.clone(),
        base_url: sanitize_base_url(&body.base_url),
        file_name: body.file_name.trim().to_string(),
        file_size: body.file_size,
        mime_type: body.mime_type.trim().to_string(),
        local_file_path: body.local_file_path.trim().to_string(),
        api_token: body.api_token.trim().to_string(),
        bucket_name: Some(body.bucket_name.trim().to_string()),
        object_prefix: Some(body.object_prefix.trim().to_string()),
    };

    let background_handle = handle.clone();
    let background_client = state.http.clone();
    let background_tasks = Arc::clone(&state.tasks);
    thread::spawn(move || {
        run_gcloud_import_task(upload_task, background_handle, background_client, background_tasks);
    });

    let snapshot = get_snapshot(&handle).unwrap();
    respond(request, 200, serialize_snapshot(&snapshot));
}

fn handle_list_tasks(request: Request, state: &AgentHttpState) {
    let mut snapshots = state
        .tasks
        .lock()
        .ok()
        .map(|tasks| {
            tasks.values()
                .filter_map(get_snapshot)
                .collect::<Vec<AgentTaskSnapshot>>()
        })
        .unwrap_or_default();

    snapshots.sort_by(|left, right| right.task_id.cmp(&left.task_id));
    respond(request, 200, serialize_snapshots(&snapshots));
}

fn handle_get_task(request: Request, state: &AgentHttpState, task_id: &str) {
    let snapshot = state
        .tasks
        .lock()
        .ok()
        .and_then(|tasks| tasks.get(task_id).cloned())
        .and_then(|handle| get_snapshot(&handle));

    let Some(snapshot) = snapshot else {
        respond(
            request,
            404,
            json!({
                "success": false,
                "error": "任务不存在"
            }),
        );
        return;
    };

    respond(request, 200, serialize_snapshot(&snapshot));
}

fn handle_get_task_logs(request: Request, state: &AgentHttpState, task_id: &str) {
    let logs = state
        .tasks
        .lock()
        .ok()
        .and_then(|tasks| tasks.get(task_id).cloned())
        .map(|handle| get_logs(&handle));

    let Some(logs) = logs else {
        respond(
            request,
            404,
            json!({
                "success": false,
                "error": "任务不存在"
            }),
        );
        return;
    };

    respond(
        request,
        200,
        json!({
            "success": true,
            "data": logs,
        }),
    );
}

fn handle_cancel_task(request: Request, state: &AgentHttpState, task_id: &str) {
    let handle = state
        .tasks
        .lock()
        .ok()
        .and_then(|tasks| tasks.get(task_id).cloned());

    let Some(handle) = handle else {
        respond(
            request,
            404,
            json!({
                "success": false,
                "error": "任务不存在"
            }),
        );
        return;
    };

    handle.cancel_requested.store(true, Ordering::Relaxed);
    if let Ok(active_pid) = handle.active_pid.lock() {
        if let Some(pid) = *active_pid {
            terminate_process(pid);
        }
    }
    update_snapshot(&handle, |snapshot| {
        snapshot.status = AgentTaskStatus::Cancelled;
        snapshot.progress = 0.0;
        snapshot.speed_bytes_per_second = 0.0;
    });

    let snapshot = get_snapshot(&handle).unwrap();
    respond(request, 200, serialize_snapshot(&snapshot));
}

fn handle_get_update(request: Request, state: &AgentHttpState) {
    let snapshot =
        get_updater_snapshot(&state.updater).unwrap_or_else(|| default_updater_snapshot(state.version.clone()));

    respond(
        request,
        200,
        json!({
            "success": true,
            "data": snapshot,
        }),
    );
}

fn handle_check_update(request: Request, state: &AgentHttpState) {
    if !updater_configured() {
        handle_get_update(request, state);
        return;
    }

    match check_for_updates(&state.app, &state.updater) {
        Ok(()) => handle_get_update(request, state),
        Err(error) => {
            let snapshot =
                get_updater_snapshot(&state.updater).unwrap_or_else(|| default_updater_snapshot(state.version.clone()));
            respond(
                request,
                500,
                json!({
                    "success": false,
                    "error": error,
                    "data": snapshot,
                }),
            );
        }
    }
}

fn handle_install_update(request: Request, state: &AgentHttpState) {
    if !updater_configured() {
        respond(
            request,
            400,
            json!({
                "success": false,
                "error": "当前构建未启用自动更新"
            }),
        );
        return;
    }

    update_updater_snapshot(&state.updater, |runtime| {
        runtime.snapshot.installing = true;
        runtime.snapshot.checking = false;
        runtime.snapshot.last_error = None;
        runtime.snapshot.progress = 0.0;
        runtime.snapshot.downloaded_bytes = 0;
        runtime.snapshot.download_total_bytes = None;
    });

    spawn_update_install(state.app.clone(), Arc::clone(&state.updater));
    handle_get_update(request, state);
}

fn handle_request(request: Request, state: &AgentHttpState) {
    let method = request.method().clone();
    let path = request.url().to_string();

    if method == Method::Options {
        respond(request, 204, json!({}));
        return;
    }

    match (method, path.as_str()) {
        (Method::Get, "/v1/health") => {
            respond(
                request,
                200,
                json!({
                    "success": true,
                    "data": {
                        "version": state.version,
                        "platform": std::env::consts::OS,
                        "capabilities": [
                            "health",
                            "desktop-shell",
                            "localhost-http",
                            "auto-update",
                            "pick-file",
                            "upload-task",
                            "gcloud-import",
                            "gcloud-cli-orchestrated",
                            "cancel-task",
                            "concurrent-upload",
                            "tray-resident"
                        ]
                    }
                }),
            );
        }
        (Method::Get, "/v1/system/info") => {
            respond(
                request,
                200,
                json!({
                    "success": true,
                    "data": {
                        "host": AGENT_HOST,
                        "port": AGENT_PORT,
                        "mode": "desktop-v1",
                        "transport": "localhost-http",
                        "updaterConfigured": updater_configured()
                    }
                }),
            );
        }
        (Method::Get, "/v1/system/update") => {
            handle_get_update(request, state);
        }
        (Method::Post, "/v1/system/update/check") => {
            handle_check_update(request, state);
        }
        (Method::Post, "/v1/system/update/install") => {
            handle_install_update(request, state);
        }
        (Method::Post, "/v1/files/pick") => {
            handle_pick_file(request);
        }
        (Method::Get, "/v1/uploads") => {
            handle_list_tasks(request, state);
        }
        (Method::Post, "/v1/uploads") => {
            handle_create_upload(request, state);
        }
        (Method::Get, "/v1/gcloud/imports") => {
            handle_list_tasks(request, state);
        }
        (Method::Post, "/v1/gcloud/imports") => {
            handle_create_gcloud_import(request, state);
        }
        _ => {
            if request.method() == &Method::Get
                && path.starts_with("/v1/uploads/")
                && path.ends_with("/logs")
            {
                let task_id = path
                    .trim_start_matches("/v1/uploads/")
                    .trim_end_matches("/logs")
                    .trim_end_matches('/');
                handle_get_task_logs(request, state, task_id);
                return;
            }

            if request.method() == &Method::Get
                && path.starts_with("/v1/gcloud/imports/")
                && path.ends_with("/logs")
            {
                let task_id = path
                    .trim_start_matches("/v1/gcloud/imports/")
                    .trim_end_matches("/logs")
                    .trim_end_matches('/');
                handle_get_task_logs(request, state, task_id);
                return;
            }

            if request.method() == &Method::Get && path.starts_with("/v1/uploads/") {
                let task_id = path.trim_start_matches("/v1/uploads/");
                handle_get_task(request, state, task_id);
                return;
            }

            if request.method() == &Method::Get && path.starts_with("/v1/gcloud/imports/") {
                let task_id = path.trim_start_matches("/v1/gcloud/imports/");
                handle_get_task(request, state, task_id);
                return;
            }

            if request.method() == &Method::Post && path.starts_with("/v1/uploads/") && path.ends_with("/cancel") {
                let task_id = path
                    .trim_start_matches("/v1/uploads/")
                    .trim_end_matches("/cancel")
                    .trim_end_matches('/');
                handle_cancel_task(request, state, task_id);
                return;
            }

            if request.method() == &Method::Post
                && path.starts_with("/v1/gcloud/imports/")
                && path.ends_with("/cancel")
            {
                let task_id = path
                    .trim_start_matches("/v1/gcloud/imports/")
                    .trim_end_matches("/cancel")
                    .trim_end_matches('/');
                handle_cancel_task(request, state, task_id);
                return;
            }

            respond(
                request,
                404,
                json!({
                    "success": false,
                    "error": "未找到接口"
                }),
            );
        }
    }
}

fn spawn_local_http_server(state: Arc<AgentHttpState>) {
    thread::spawn(move || {
        let address = format!("{AGENT_HOST}:{AGENT_PORT}");
        let server = match Server::http(&address) {
            Ok(server) => server,
            Err(error) => {
                append_startup_log(format!("启动本地 HTTP 服务失败（{address}）：{error}"));
                return;
            }
        };

        for request in server.incoming_requests() {
            handle_request(request, state.as_ref());
        }
    });
}

fn show_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn hide_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.hide();
    }
}

fn main() {
    append_startup_log("main() entered");
    install_startup_panic_hook();
    append_startup_log("startup panic hook installed");

    append_startup_log("即将进入 tauri 启动流程");
    let mut builder = tauri::Builder::default();
    if updater_configured() {
        append_startup_log("检测到有效的 updater 配置，注册 updater 插件");
        let updater_plugin = if let Some(pubkey) = UPDATE_PUBKEY.filter(|value| !value.trim().is_empty()) {
            tauri_plugin_updater::Builder::new().pubkey(pubkey.to_string())
        } else {
            tauri_plugin_updater::Builder::new()
        };
        builder = builder.plugin(updater_plugin.build());
    } else {
        append_startup_log("未检测到有效的 updater 配置，跳过 updater 插件注册");
    }

    let run_result = builder
        .on_window_event(|window, event| {
            if window.label() != "main" {
                return;
            }

            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .setup(|app| -> Result<(), Box<dyn std::error::Error>> {
            append_startup_log("开始执行桌面助手启动流程");
            let client = match Client::builder()
                .connect_timeout(Duration::from_secs(15))
                .timeout(Duration::from_secs(DEFAULT_CHUNK_TIMEOUT_SECONDS))
                .build()
            {
                Ok(client) => client,
                Err(error) => {
                    append_startup_log(format!("创建 HTTP 客户端失败：{error}"));
                    show_startup_error_dialog(format!("初始化网络模块失败：{error}"));
                    return Err(Box::new(error));
                }
            };

            let state = Arc::new(AgentHttpState {
                version: app.package_info().version.to_string(),
                app: app.handle().clone(),
                tasks: Arc::new(Mutex::new(HashMap::new())),
                updater: Arc::new(Mutex::new(UpdaterRuntimeState {
                    pending_update: None,
                    snapshot: default_updater_snapshot(app.package_info().version.to_string()),
                })),
                http: client,
            });

            spawn_local_http_server(state);
            append_startup_log("本地 HTTP 服务线程已启动");

            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                append_startup_log("主窗口已请求显示");
            } else {
                append_startup_log("未找到主窗口");
            }

            match (|| -> tauri::Result<_> {
                let show_item = MenuItem::with_id(app, "show", "显示窗口", true, None::<&str>)?;
                let hide_item = MenuItem::with_id(app, "hide", "隐藏窗口", true, None::<&str>)?;
                let quit_item = MenuItem::with_id(app, "quit", "退出助手", true, None::<&str>)?;
                let menu = Menu::with_items(app, &[&show_item, &hide_item, &quit_item])?;

                let mut tray_builder = TrayIconBuilder::with_id("main-tray")
                    .menu(&menu)
                    .show_menu_on_left_click(true)
                    .tooltip("创剪本地上传助手")
                    .on_menu_event(|app, event| match event.id.as_ref() {
                        "show" => show_main_window(app),
                        "hide" => hide_main_window(app),
                        "quit" => app.exit(0),
                        _ => {}
                    })
                    .on_tray_icon_event(|tray, event| {
                        if let TrayIconEvent::Click {
                            button: MouseButton::Left,
                            button_state: MouseButtonState::Up,
                            ..
                        } = event
                        {
                            show_main_window(&tray.app_handle());
                        }
                    });

                if let Some(icon) = app.default_window_icon() {
                    tray_builder = tray_builder.icon(icon.clone());
                }

                tray_builder.build(app)
            })() {
                Ok(tray) => {
                    let _ = Box::leak(Box::new(tray));
                    append_startup_log("系统托盘初始化成功");
                }
                Err(error) => {
                    append_startup_log(format!("系统托盘初始化失败：{error}"));
                }
            }

            append_startup_log("桌面助手启动流程完成");

            Ok(())
        })
        .run(tauri::generate_context!());

    match run_result {
        Ok(()) => append_startup_log("tauri 事件循环已正常退出"),
        Err(error) => {
            append_startup_log(format!("tauri 启动失败：{error}"));
            show_startup_error_dialog(format!("桌面助手启动失败：{error}"));
        }
    }
}
