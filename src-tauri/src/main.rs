#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use tauri::{AppHandle, Emitter, Manager, State};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Shown when a TikTok request fails and this yt-dlp build cannot impersonate a browser.
const IMPERSONATION_HINT: &str = "TikTok usually needs browser impersonation, which this yt-dlp build does not have. Use the official yt-dlp release binary (run `npm run setup`) or install curl_cffi.";

/// Running yt-dlp processes plus the set of jobs the user cancelled.
#[derive(Default)]
struct Jobs {
    running: Mutex<HashMap<String, Arc<Mutex<Child>>>>,
    cancelled: Mutex<HashSet<String>>,
}

#[derive(Serialize)]
struct Info {
    title: String,
    uploader: Option<String>,
    duration: Option<f64>,
    extractor: Option<String>,
}

#[derive(Deserialize)]
struct DownloadRequest {
    id: String,
    url: String,
    mode: String,
    quality: String,
    dir: String,
}

#[derive(Serialize, Clone)]
struct ProgressPayload {
    id: String,
    percent: f64,
    speed: Option<f64>,
    eta: Option<f64>,
}

#[derive(Serialize, Clone)]
struct DonePayload {
    id: String,
    path: Option<String>,
}

#[derive(Serialize, Clone)]
struct ErrorPayload {
    id: String,
    message: String,
}

#[derive(Serialize, Clone)]
struct IdPayload {
    id: String,
}

/// Directory holding the executable. Tauri places bundled sidecars here
/// (the target-triple suffix is stripped at build/bundle time).
fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
}

fn sidecar(name: &str) -> Option<PathBuf> {
    let file = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    let path = exe_dir()?.join(file);
    path.exists().then_some(path)
}

/// Builds a base yt-dlp command: bundled sidecar first, `yt-dlp` on PATH as a fallback.
fn ytdlp_command() -> Command {
    let mut cmd = match sidecar("yt-dlp") {
        Some(path) => Command::new(path),
        None => Command::new("yt-dlp"),
    };
    if let Some(ffmpeg) = sidecar("ffmpeg") {
        cmd.arg("--ffmpeg-location").arg(ffmpeg);
    }
    cmd.env("PYTHONIOENCODING", "utf-8");
    cmd.env("PYTHONUTF8", "1");
    cmd.stdin(Stdio::null());
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

/// True for tiktok.com and any of its subdomains (www, vm, vt, m, ...).
fn is_tiktok(url: &str) -> bool {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let authority = rest
        .split(|c: char| c == '/' || c == '?' || c == '#')
        .next()
        .unwrap_or("");
    let host_port = authority.rsplit('@').next().unwrap_or(authority);
    let host = host_port
        .split(':')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    host == "tiktok.com" || host.ends_with(".tiktok.com")
}

static IMPERSONATION: OnceLock<bool> = OnceLock::new();

/// Checks once whether this yt-dlp build can impersonate Chrome. Passing
/// `--impersonate` to a build without curl_cffi makes yt-dlp abort, so it is
/// only added when the target is actually listed as available.
fn impersonation_available() -> bool {
    *IMPERSONATION.get_or_init(|| {
        let mut cmd = ytdlp_command();
        cmd.arg("--list-impersonate-targets");
        match cmd.output() {
            Ok(out) => {
                let text = format!(
                    "{}\n{}",
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr)
                );
                text.lines()
                    .map(|l| l.trim().to_ascii_lowercase())
                    .any(|l| l.starts_with("chrome") && !l.contains("unavailable"))
            }
            Err(_) => false,
        }
    })
}

/// yt-dlp command for a specific URL. TikTok URLs get `--impersonate chrome`
/// (browser TLS/HTTP fingerprint) so TikTok does not block the requests.
fn ytdlp_command_for(url: &str) -> Command {
    let mut cmd = ytdlp_command();
    if is_tiktok(url) && impersonation_available() {
        cmd.args(["--impersonate", "chrome"]);
    }
    cmd
}

/// Appends the impersonation hint to a TikTok error when impersonation is missing.
fn with_tiktok_hint(url: &str, message: String) -> String {
    if is_tiktok(url) && !impersonation_available() {
        format!("{message} ({IMPERSONATION_HINT})")
    } else {
        message
    }
}

fn valid_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    (lower.starts_with("http://") || lower.starts_with("https://"))
        && !url.contains(char::is_whitespace)
}

fn last_error_line(stderr: &str) -> String {
    let lines: Vec<&str> = stderr
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect();
    let picked = lines
        .iter()
        .rev()
        .find(|l| l.starts_with("ERROR"))
        .or_else(|| lines.last());
    match picked {
        Some(line) => line.trim_start_matches("ERROR:").trim().to_string(),
        None => "yt-dlp failed without a message".to_string(),
    }
}

/// Terminates a process and its children. yt-dlp's single-file builds spawn a
/// child process, so killing only the parent would leave the download running.
fn kill_tree(pid: u32) {
    let pid_text = pid.to_string();
    #[cfg(windows)]
    {
        let mut cmd = Command::new("taskkill");
        cmd.args(["/PID", pid_text.as_str(), "/T", "/F"]);
        cmd.creation_flags(CREATE_NO_WINDOW);
        let _ = cmd.status();
    }
    #[cfg(not(windows))]
    {
        let _ = Command::new("kill")
            .args(["-TERM", pid_text.as_str()])
            .status();
    }
}

fn parse_num(s: &str) -> Option<f64> {
    s.trim().parse::<f64>().ok()
}

#[tauri::command]
async fn fetch_info(url: String) -> Result<Info, String> {
    if !valid_url(&url) {
        return Err("Not a valid http(s) link".into());
    }
    let joined = tauri::async_runtime::spawn_blocking(move || -> Result<Info, String> {
        let mut cmd = ytdlp_command_for(&url);
        cmd.args([
            "--dump-single-json",
            "--no-playlist",
            "--no-warnings",
            "--skip-download",
            "--",
            url.as_str(),
        ]);
        let out = cmd
            .output()
            .map_err(|e| format!("Could not run yt-dlp: {e}"))?;
        if !out.status.success() {
            let message = last_error_line(&String::from_utf8_lossy(&out.stderr));
            return Err(with_tiktok_hint(&url, message));
        }
        let v: Value = serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())?;
        Ok(Info {
            title: v["title"].as_str().unwrap_or("Untitled").to_string(),
            uploader: v["uploader"].as_str().map(|s| s.to_string()),
            duration: v["duration"].as_f64(),
            extractor: v["extractor_key"].as_str().map(|s| s.to_string()),
        })
    })
    .await;
    match joined {
        Ok(result) => result,
        Err(e) => Err(e.to_string()),
    }
}

#[tauri::command]
fn start_download(
    app: AppHandle,
    jobs: State<'_, Jobs>,
    req: DownloadRequest,
) -> Result<(), String> {
    if !valid_url(&req.url) {
        return Err("Not a valid http(s) link".into());
    }
    if req.dir.trim().is_empty() {
        return Err("No output folder selected".into());
    }

    let tiktok = is_tiktok(&req.url);
    let audio = req.mode == "audio";
    // For TikTok video downloads, refuse to save a watermarked stream.
    let guard_watermark = tiktok && !audio;

    let mut cmd = ytdlp_command_for(&req.url);
    cmd.args([
        "--no-playlist",
        "--no-warnings",
        "--newline",
        "--progress",
        "--color",
        "never",
        "--windows-filenames",
        "--progress-template",
        "download:SPPROG|%(progress.downloaded_bytes)s|%(progress.total_bytes)s|%(progress.total_bytes_estimate)s|%(progress.speed)s|%(progress.eta)s",
        "--print",
        "after_move:SPFILE|%(filepath)s",
    ]);

    if audio {
        // Audio only: converted to MP3 by ffmpeg. For TikTok the audio comes
        // from a combined stream, and the watermarked variant is never chosen.
        let selector = if tiktok {
            "ba/b[format_id!=download]/b"
        } else {
            "bestaudio/best"
        };
        cmd.args([
            "-f",
            selector,
            "-x",
            "--audio-format",
            "mp3",
            "--audio-quality",
            "0",
        ]);
    } else if tiktok {
        // TikTok serves single combined streams, so plain `b` (best single
        // file) is correct. yt-dlp tags the burnt-in-watermark variant with
        // the format id "download"; it is excluded explicitly here.
        //
        // No `-S` override on purpose: a user-supplied sort such as `res`
        // outranks yt-dlp's own preference ordering (which ranks the
        // watermarked stream last) and can pull the watermarked file.
        cmd.args([
            "-f",
            "b[format_id!=download]/b",
            "--print",
            "before_dl:SPFMT|%(format_id)s",
        ]);
    } else {
        let height: Option<u32> = match req.quality.as_str() {
            "4k" => Some(2160),
            "1080p" => Some(1080),
            _ => None,
        };
        let format = match height {
            Some(h) => format!("bv*[height<={h}]+ba/b[height<={h}]/bv*+ba/b"),
            None => "bv*+ba/b".to_string(),
        };
        cmd.args([
            "-f",
            format.as_str(),
            "-S",
            "res,vcodec:h264,acodec:m4a",
            "--merge-output-format",
            "mp4",
        ]);
    }

    cmd.args([
        "-P",
        req.dir.as_str(),
        "-o",
        "%(title).150B.%(ext)s",
        "--",
        req.url.as_str(),
    ]);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Could not start yt-dlp: {e}"))?;
    let stdout = child.stdout.take().ok_or("Could not read yt-dlp output")?;
    let stderr = child.stderr.take().ok_or("Could not read yt-dlp errors")?;

    let child = Arc::new(Mutex::new(child));
    jobs.running
        .lock()
        .unwrap()
        .insert(req.id.clone(), child.clone());

    let id = req.id.clone();
    let url = req.url.clone();
    let app_handle = app.clone();
    thread::spawn(move || {
        let pid = child.lock().unwrap().id();

        // Collect the last ERROR line from stderr on a side thread.
        let last_error = Arc::new(Mutex::new(String::new()));
        let last_error_writer = last_error.clone();
        let stderr_thread = thread::spawn(move || {
            for chunk in BufReader::new(stderr).split(b'\n').map_while(Result::ok) {
                let line = String::from_utf8_lossy(&chunk).trim().to_string();
                if line.starts_with("ERROR") {
                    *last_error_writer.lock().unwrap() =
                        line.trim_start_matches("ERROR:").trim().to_string();
                }
            }
        });

        let mut final_path: Option<String> = None;
        let mut watermark_blocked = false;
        for chunk in BufReader::new(stdout).split(b'\n').map_while(Result::ok) {
            let line = String::from_utf8_lossy(&chunk).trim().to_string();
            if let Some(rest) = line.strip_prefix("SPPROG|") {
                let parts: Vec<&str> = rest.split('|').collect();
                if parts.len() >= 5 {
                    let downloaded = parse_num(parts[0]);
                    let total = parse_num(parts[1]).or_else(|| parse_num(parts[2]));
                    if let (Some(done), Some(total)) = (downloaded, total) {
                        if total > 0.0 {
                            let _ = app_handle.emit(
                                "download-progress",
                                ProgressPayload {
                                    id: id.clone(),
                                    percent: (done / total * 100.0).clamp(0.0, 100.0),
                                    speed: parse_num(parts[3]),
                                    eta: parse_num(parts[4]),
                                },
                            );
                        }
                    }
                }
            } else if let Some(format_id) = line.strip_prefix("SPFMT|") {
                // Reported by yt-dlp right before the download starts. If the
                // only stream left is the watermarked one, stop before any
                // bytes are written.
                if guard_watermark && format_id.trim() == "download" && !watermark_blocked {
                    watermark_blocked = true;
                    kill_tree(pid);
                }
            } else if let Some(path) = line.strip_prefix("SPFILE|") {
                final_path = Some(path.to_string());
            }
        }

        let _ = stderr_thread.join();
        let status = child.lock().unwrap().wait();

        let state = app_handle.state::<Jobs>();
        state.running.lock().unwrap().remove(&id);
        let was_cancelled = state.cancelled.lock().unwrap().remove(&id);

        if was_cancelled {
            let _ = app_handle.emit("download-cancelled", IdPayload { id });
            return;
        }

        if watermark_blocked {
            let _ = app_handle.emit(
                "download-error",
                ErrorPayload {
                    id,
                    message: with_tiktok_hint(
                        &url,
                        "TikTok only offered a watermarked stream for this video, so nothing was saved".to_string(),
                    ),
                },
            );
            return;
        }

        match status {
            Ok(s) if s.success() => {
                let _ = app_handle.emit(
                    "download-done",
                    DonePayload {
                        id,
                        path: final_path,
                    },
                );
            }
            Ok(s) => {
                let message = {
                    let err = last_error.lock().unwrap().clone();
                    if err.is_empty() {
                        format!("yt-dlp exited with code {}", s.code().unwrap_or(-1))
                    } else {
                        err
                    }
                };
                let _ = app_handle.emit(
                    "download-error",
                    ErrorPayload {
                        id,
                        message: with_tiktok_hint(&url, message),
                    },
                );
            }
            Err(e) => {
                let _ = app_handle.emit(
                    "download-error",
                    ErrorPayload {
                        id,
                        message: e.to_string(),
                    },
                );
            }
        }
    });

    Ok(())
}

#[tauri::command]
fn cancel_download(jobs: State<'_, Jobs>, id: String) -> Result<(), String> {
    let child = jobs.running.lock().unwrap().get(&id).cloned();
    if let Some(child) = child {
        let pid = child.lock().unwrap().id();
        jobs.cancelled.lock().unwrap().insert(id);
        kill_tree(pid);
    }
    Ok(())
}

#[tauri::command]
fn default_dir(app: AppHandle) -> String {
    app.path()
        .download_dir()
        .map(|p| p.join("SiphonPull").to_string_lossy().to_string())
        .unwrap_or_default()
}

#[tauri::command]
async fn ytdlp_version() -> Result<String, String> {
    let joined = tauri::async_runtime::spawn_blocking(|| -> Result<String, String> {
        let mut cmd = ytdlp_command();
        cmd.arg("--version");
        let out = cmd
            .output()
            .map_err(|e| format!("yt-dlp not found: {e}"))?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
        } else {
            Err(last_error_line(&String::from_utf8_lossy(&out.stderr)))
        }
    })
    .await;
    match joined {
        Ok(result) => result,
        Err(e) => Err(e.to_string()),
    }
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_opener::init())
        .manage(Jobs::default())
        .invoke_handler(tauri::generate_handler![
            fetch_info,
            start_download,
            cancel_download,
            default_dir,
            ytdlp_version
        ])
        .build(tauri::generate_context!())
        .expect("error while building SiphonPull")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                // Don't leave orphaned downloads behind when the window closes.
                let jobs = app.state::<Jobs>();
                let running = jobs.running.lock().unwrap();
                for child in running.values() {
                    if let Ok(child) = child.try_lock() {
                        kill_tree(child.id());
                    }
                }
            }
        });
}
