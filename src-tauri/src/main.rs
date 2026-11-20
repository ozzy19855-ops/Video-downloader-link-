#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};
use tauri::{
    AppHandle, Emitter, Manager, State, Url, WebviewUrl, WebviewWindow, WebviewWindowBuilder,
};
use tauri_plugin_deep_link::DeepLinkExt;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Shown when a TikTok request fails and this yt-dlp build cannot impersonate a browser.
const IMPERSONATION_HINT: &str = "TikTok usually needs browser impersonation, which this yt-dlp build does not have. Use the official yt-dlp release binary (run `npm run setup`) or install curl_cffi.";

/// Sniffer tuning.
const SNIFF_TIMEOUT: Duration = Duration::from_secs(20);
/// After the first stream hit, keep listening briefly so a master playlist can win over a variant.
const SNIFF_GRACE: Duration = Duration::from_millis(1200);
/// If the top page shows no stream by now, navigate the hidden window into an embedded iframe.
const FRAME_DESCENT_AFTER: Duration = Duration::from_secs(7);
const SNIFF_SCHEME: &str = "siphonpull-sniff";
const SNIFFER_JS: &str = include_str!("sniffer.js");
static SNIFFER_SEQ: AtomicUsize = AtomicUsize::new(0);

/// Running yt-dlp processes plus the set of jobs the user cancelled.
#[derive(Default)]
struct Jobs {
    running: Mutex<HashMap<String, Arc<Mutex<Child>>>>,
    cancelled: Mutex<HashSet<String>>,
}

/// Deep links that arrived before the frontend finished loading.
#[derive(Default)]
struct DeepLinkQueue {
    ready: bool,
    pending: Vec<LinkPayload>,
}

#[derive(Default)]
struct DeepLinks(Mutex<DeepLinkQueue>);

#[derive(Serialize)]
struct Info {
    title: String,
    uploader: Option<String>,
    duration: Option<f64>,
    extractor: Option<String>,
    /// Set when the page had to be sniffed: the stream the queue should download instead.
    resolved: Option<Resolved>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Resolved {
    url: String,
    referer: String,
    user_agent: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DownloadRequest {
    id: String,
    url: String,
    mode: String,
    quality: String,
    dir: String,
    #[serde(default)]
    referer: Option<String>,
    #[serde(default)]
    user_agent: Option<String>,
    /// Optional file name (without extension), used for sniffed / deep-linked streams.
    #[serde(default)]
    title: Option<String>,
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

#[derive(Serialize, Clone)]
struct NotePayload {
    id: String,
    message: String,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct LinkPayload {
    url: String,
    referer: Option<String>,
    user_agent: Option<String>,
    title: Option<String>,
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

/// Header values come from web pages and deep links, so they are untrusted:
/// no control characters, bounded length.
fn clean_header_value(value: &str, max_len: usize) -> Option<String> {
    let v = value.trim();
    if v.is_empty() || v.len() > max_len || v.chars().any(|c| c.is_control()) {
        None
    } else {
        Some(v.to_string())
    }
}

fn clean_referer(value: &str) -> Option<String> {
    clean_header_value(value, 2048).filter(|r| valid_url(r))
}

/// Turns a page title into a safe file name stem (no extension).
fn clean_title(value: &str) -> Option<String> {
    let cleaned: String = value
        .chars()
        .filter(|c| !c.is_control())
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            other => other,
        })
        .collect();
    let trimmed: String = cleaned
        .trim()
        .chars()
        .take(150)
        .collect::<String>()
        .trim_matches(|c: char| c == '.' || c.is_whitespace())
        .to_string();
    (!trimmed.is_empty()).then_some(trimmed)
}

fn origin_of(url: &str) -> Option<String> {
    Url::parse(url)
        .ok()
        .map(|u| u.origin().ascii_serialization())
        .filter(|o| o != "null")
}

/// True when the path of a pasted link itself is an HLS / DASH manifest.
fn is_stream_url(url: &str) -> bool {
    match Url::parse(url) {
        Ok(u) => {
            let path = u.path().to_ascii_lowercase();
            path.ends_with(".m3u8") || path.ends_with(".mpd")
        }
        Err(_) => false,
    }
}

/// True when `.m3u8` / `.mpd` appears anywhere in a URL as a whole extension.
fn has_stream_marker(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    [".m3u8", ".mpd"].iter().any(|marker| {
        lower.match_indices(marker).any(|(i, _)| {
            lower[i + marker.len()..]
                .chars()
                .next()
                .map_or(true, |c| !c.is_ascii_alphanumeric())
        })
    })
}

/// yt-dlp command shared by metadata lookups and downloads: Chrome TLS/HTTP
/// impersonation when the bundled build supports it (silently skipped when it
/// does not), plus optional Referer / Origin / User-Agent headers.
fn ytdlp_command_for(referer: Option<&str>, user_agent: Option<&str>) -> Command {
    let mut cmd = ytdlp_command();
    if impersonation_available() {
        cmd.args(["--impersonate", "chrome"]);
    }
    if let Some(referer) = referer.and_then(clean_referer) {
        cmd.arg("--add-header").arg(format!("Referer:{referer}"));
        // Browsers also send Origin on player requests; some CDNs check it.
        if let Some(origin) = origin_of(&referer) {
            cmd.arg("--add-header").arg(format!("Origin:{origin}"));
        }
    }
    if let Some(ua) = user_agent.and_then(|u| clean_header_value(u, 512)) {
        cmd.arg("--user-agent").arg(ua);
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

/* ------------------------------------------------------------------ */
/* Metadata lookup                                                      */
/* ------------------------------------------------------------------ */

fn run_info(url: &str, referer: Option<&str>, user_agent: Option<&str>) -> Result<Info, String> {
    let mut cmd = ytdlp_command_for(referer, user_agent);
    cmd.args([
        "--dump-single-json",
        "--no-playlist",
        "--no-warnings",
        "--skip-download",
        "--",
        url,
    ]);
    let out = cmd
        .output()
        .map_err(|e| format!("Could not run yt-dlp: {e}"))?;
    if !out.status.success() {
        let message = last_error_line(&String::from_utf8_lossy(&out.stderr));
        return Err(with_tiktok_hint(url, message));
    }
    let v: Value = serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())?;
    Ok(Info {
        title: v["title"].as_str().unwrap_or("Untitled").to_string(),
        uploader: v["uploader"].as_str().map(|s| s.to_string()),
        duration: v["duration"].as_f64(),
        extractor: v["extractor_key"].as_str().map(|s| s.to_string()),
        resolved: None,
    })
}

/// Errors that mean "yt-dlp has no extractor for this page / could not even
/// talk to it", which is exactly when a real browser engine may still find the stream.
fn should_sniff(message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    [
        "unsupported url",
        "[generic]",
        "wrong_version_number",
        "ssl:",
        "sslerror",
        "certificate verify failed",
        "handshake",
        "no video formats found",
    ]
    .iter()
    .any(|needle| m.contains(needle))
}

#[tauri::command]
async fn fetch_info(
    app: AppHandle,
    id: String,
    url: String,
    referer: Option<String>,
    user_agent: Option<String>,
) -> Result<Info, String> {
    if !valid_url(&url) {
        return Err("Not a valid http(s) link".into());
    }
    let joined = tauri::async_runtime::spawn_blocking(move || -> Result<Info, String> {
        let referer = referer.as_deref().and_then(clean_referer);
        let user_agent = user_agent.as_deref().and_then(|u| clean_header_value(u, 512));

        let first = run_info(&url, referer.as_deref(), user_agent.as_deref());
        let message = match first {
            Ok(info) => return Ok(info),
            Err(message) => message,
        };

        // Only plain web pages are worth sniffing: not stream links, not TikTok,
        // and not links that arrived with an explicit Referer (deep link / extension).
        let sniffable = referer.is_none()
            && !is_stream_url(&url)
            && !is_tiktok(&url)
            && should_sniff(&message);
        if !sniffable {
            return Err(message);
        }

        let _ = app.emit(
            "item-note",
            NotePayload {
                id,
                message: "Looking for a stream in the page…".into(),
            },
        );
        let hit = sniff_stream(&app, &url)
            .map_err(|e| format!("{message} (stream sniffer: {e})"))?;
        let mut info = run_info(&hit.stream_url, Some(&hit.referer), Some(&hit.user_agent))
            .map_err(|e| format!("Found a stream but yt-dlp could not open it: {e}"))?;
        if let Some(title) = hit.title {
            info.title = title;
        }
        info.resolved = Some(Resolved {
            url: hit.stream_url,
            referer: hit.referer,
            user_agent: hit.user_agent,
        });
        Ok(info)
    })
    .await;
    match joined {
        Ok(result) => result,
        Err(e) => Err(e.to_string()),
    }
}

/* ------------------------------------------------------------------ */
/* Hidden WebView stream sniffer                                        */
/* ------------------------------------------------------------------ */

struct Sniffed {
    stream_url: String,
    referer: String,
    user_agent: String,
    title: Option<String>,
}

enum SniffMsg {
    Hit(Sniffed),
    Frame(String),
}

/// Decodes a `siphonpull-sniff://hit?...` / `...://frame?...` navigation that
/// the injected script triggered. The page controls these values, so each one is validated.
fn parse_sniff_nav(url: &Url, page_url: &str) -> Option<SniffMsg> {
    let q: HashMap<String, String> = url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    match url.host_str()? {
        "hit" => {
            let stream_url = q
                .get("u")
                .filter(|u| valid_url(u) && has_stream_marker(u))?
                .clone();
            let referer = q
                .get("r")
                .and_then(|r| clean_referer(r))
                .unwrap_or_else(|| page_url.to_string());
            let user_agent = q.get("ua").and_then(|u| clean_header_value(u, 512))?;
            let title = q.get("t").and_then(|t| clean_title(t));
            Some(SniffMsg::Hit(Sniffed {
                stream_url,
                referer,
                user_agent,
                title,
            }))
        }
        "frame" => {
            let u = q.get("u").filter(|u| valid_url(u))?.clone();
            Some(SniffMsg::Frame(u))
        }
        _ => None,
    }
}

fn is_ad_frame(url: &str) -> bool {
    let u = url.to_ascii_lowercase();
    [
        "doubleclick",
        "googlesyndication",
        "adservice",
        "facebook.com",
        "twitter.com",
        "disqus",
    ]
    .iter()
    .any(|needle| u.contains(needle))
}

/// Prefers a master playlist over a single variant; otherwise the first hit wins.
fn pick_best(mut hits: Vec<Sniffed>) -> Option<Sniffed> {
    hits.sort_by_key(|h| {
        if h.stream_url.to_ascii_lowercase().contains("master") {
            0
        } else {
            1
        }
    });
    hits.into_iter().next()
}

fn collect_hits(
    window: &WebviewWindow,
    rx: &mpsc::Receiver<SniffMsg>,
) -> Result<Sniffed, String> {
    let started = Instant::now();
    let deadline = started + SNIFF_TIMEOUT;
    let mut hits: Vec<Sniffed> = Vec::new();
    let mut frames: Vec<String> = Vec::new();
    let mut grace_end: Option<Instant> = None;
    let mut descended = false;

    loop {
        let now = Instant::now();
        let limit = grace_end.map_or(deadline, |g| g.min(deadline));
        if now >= limit {
            break;
        }
        match rx.recv_timeout((limit - now).min(Duration::from_millis(250))) {
            Ok(SniffMsg::Hit(hit)) => {
                if grace_end.is_none() {
                    grace_end = Some(Instant::now() + SNIFF_GRACE);
                }
                hits.push(hit);
            }
            Ok(SniffMsg::Frame(u)) => {
                if !frames.contains(&u) {
                    frames.push(u);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }

        // Nothing on the top page: step into the first non-ad iframe.
        if !descended && hits.is_empty() && started.elapsed() >= FRAME_DESCENT_AFTER {
            if let Some(frame) = frames.iter().find(|f| !is_ad_frame(f)) {
                descended = true;
                let quoted = serde_json::to_string(frame).unwrap_or_default();
                let js = format!("location.replace({quoted});");
                let _ = window.eval(&js);
            }
        }
    }

    pick_best(hits).ok_or_else(|| "the page never requested an .m3u8 / .mpd stream".to_string())
}

/// Opens `page_url` in an invisible WebView, watches its network calls for an
/// HLS / DASH manifest, and closes the WebView again. Blocking: call from a
/// worker thread only.
fn sniff_stream(app: &AppHandle, page_url: &str) -> Result<Sniffed, String> {
    let parsed: Url = page_url
        .parse()
        .map_err(|_| "invalid page URL".to_string())?;
    let (tx, rx) = mpsc::channel::<SniffMsg>();
    let tx = Mutex::new(tx);
    let page_for_nav = page_url.to_string();
    let label = format!("sniffer-{}", SNIFFER_SEQ.fetch_add(1, Ordering::Relaxed));

    // This window's label is deliberately absent from capabilities/default.json,
    // so the remote page gets no access to the Tauri IPC.
    let window = WebviewWindowBuilder::new(app, &label, WebviewUrl::External(parsed))
        .title("SiphonPull sniffer")
        .visible(false)
        .focused(false)
        .skip_taskbar(true)
        .decorations(false)
        .inner_size(1280.0, 720.0)
        .initialization_script(SNIFFER_JS)
        .on_navigation(move |url| {
            if url.scheme() != SNIFF_SCHEME {
                return true;
            }
            if let Some(msg) = parse_sniff_nav(url, &page_for_nav) {
                if let Ok(tx) = tx.lock() {
                    let _ = tx.send(msg);
                }
            }
            false // never actually navigate to the report URL
        })
        .build()
        .map_err(|e| format!("could not open the sniffer window: {e}"))?;

    let result = collect_hits(&window, &rx);
    let _ = window.destroy();
    result
}

/* ------------------------------------------------------------------ */
/* Downloads                                                            */
/* ------------------------------------------------------------------ */

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

    let referer = req.referer.as_deref().and_then(clean_referer);
    let user_agent = req
        .user_agent
        .as_deref()
        .and_then(|u| clean_header_value(u, 512));

    let mut cmd = ytdlp_command_for(referer.as_deref(), user_agent.as_deref());
    cmd.args([
        // Survive CDN rate limiting and dropped HLS/DASH segments.
        "--retries",
        "15",
        "--fragment-retries",
        "15",
        "--concurrent-fragments",
        "4",
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

    // A raw .m3u8 is usually called "index" or "master", so sniffed and
    // deep-linked streams carry their own title. `%` must be escaped for yt-dlp.
    let output_template = match req.title.as_deref().and_then(clean_title) {
        Some(title) => format!("{}.%(ext)s", title.replace('%', "%%")),
        None => "%(title).150B.%(ext)s".to_string(),
    };

    cmd.args([
        "-P",
        req.dir.as_str(),
        "-o",
        output_template.as_str(),
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

/* ------------------------------------------------------------------ */
/* siphonpull:// deep links                                             */
/* ------------------------------------------------------------------ */

/// `siphonpull://download?url=<stream>&referer=<page>&ua=<user agent>&title=<name>`
/// (every value percent-encoded). Anything that is not a well-formed http(s)
/// download request is ignored.
fn parse_deep_link(raw: &str) -> Option<LinkPayload> {
    let parsed = Url::parse(raw).ok()?;
    if parsed.scheme() != "siphonpull" {
        return None;
    }
    if !parsed
        .host_str()
        .map_or(false, |h| h.eq_ignore_ascii_case("download"))
    {
        return None;
    }
    let mut url = None;
    let mut referer = None;
    let mut user_agent = None;
    let mut title = None;
    for (key, value) in parsed.query_pairs() {
        match key.as_ref() {
            "url" => url = Some(value.into_owned()),
            "referer" => referer = Some(value.into_owned()),
            "ua" | "user_agent" | "userAgent" => user_agent = Some(value.into_owned()),
            "title" => title = Some(value.into_owned()),
            _ => {}
        }
    }
    let url = url.filter(|u| valid_url(u))?;
    Some(LinkPayload {
        url,
        referer: referer.as_deref().and_then(clean_referer),
        user_agent: user_agent
            .as_deref()
            .and_then(|u| clean_header_value(u, 512)),
        title: title.as_deref().and_then(clean_title),
    })
}

fn focus_main(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// Routes deep links to the frontend: queued until it asks for them on
/// startup, emitted live afterwards.
fn handle_deep_links(app: &AppHandle, raws: impl IntoIterator<Item = String>) {
    let links: Vec<LinkPayload> = raws
        .into_iter()
        .filter_map(|raw| parse_deep_link(&raw))
        .collect();
    if links.is_empty() {
        return;
    }
    focus_main(app);
    let state = app.state::<DeepLinks>();
    let mut queue = state.0.lock().unwrap();
    for link in links {
        if queue.ready {
            let _ = app.emit("deep-link-download", link);
        } else {
            queue.pending.push(link);
        }
    }
}

/// Called once by the frontend after it registered its listeners.
#[tauri::command]
fn take_pending_links(links: State<'_, DeepLinks>) -> Vec<LinkPayload> {
    let mut queue = links.0.lock().unwrap();
    queue.ready = true;
    std::mem::take(&mut queue.pending)
}

fn main() {
    tauri::Builder::default()
        // Must be registered first: a second launch (e.g. from a siphonpull:// link)
        // forwards its arguments to the running instance and exits.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            focus_main(app);
        }))
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_opener::init())
        .manage(Jobs::default())
        .manage(DeepLinks::default())
        .setup(|app| {
            // Warm the impersonation probe so the first download does not wait for it.
            thread::spawn(|| {
                impersonation_available();
            });

            // The installers register the scheme; AppImages and dev builds need it at runtime.
            #[cfg(any(target_os = "linux", all(debug_assertions, windows)))]
            {
                let _ = app.deep_link().register_all();
            }

            // Link that launched the app.
            if let Ok(Some(urls)) = app.deep_link().get_current() {
                handle_deep_links(app.handle(), urls.iter().map(|u| u.to_string()));
            }
            // Links that arrive while the app is running.
            let handle = app.handle().clone();
            app.deep_link().on_open_url(move |event| {
                handle_deep_links(&handle, event.urls().iter().map(|u| u.to_string()));
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            fetch_info,
            start_download,
            cancel_download,
            default_dir,
            ytdlp_version,
            take_pending_links
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
