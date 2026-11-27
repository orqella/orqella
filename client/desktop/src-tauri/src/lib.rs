use anyhow::{Context, Result};
use orqella_desktop_gateway::{
    AppOriginConfig, FullscreenSetter, Gateway, GatewayConfig, SettingsOpener, Upstream,
};
use orqella_desktop_transport::{
    ClientIdentity, LoginChallenge, RemoteTunnel, begin_login, connect_remote, list_devices,
    poll_login_status, register_client,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::net::IpAddr;
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use tauri::menu::{MenuBuilder, MenuItemBuilder, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::webview::{DownloadEvent, PageLoadEvent, PermissionKind, PermissionResponse};
use tauri::{Emitter, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use tokio::sync::Mutex;
use tracing_subscriber::prelude::*;

const SHELL_PORT: u16 = 19477;
const LOCAL_HUB: &str = "http://127.0.0.1:9477";
const KEYCHAIN_SERVICE: &str = "io.orqella.desktop";

const RELOAD_SHORTCUT_SCRIPT: &str = r#"
(() => {
  if (window.top !== window) return;
  window.addEventListener('keydown', (event) => {
    const reload = event.key === 'F5'
      || ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 'r');
    if (!reload || event.repeat) return;
    event.preventDefault();
    event.stopPropagation();
    location.reload();
  }, true);
})();
"#;

/// Mark pages as running inside the native desktop shell.
///
/// The Linux client forces WebKitGTK's shared-memory transport on Wayland.
/// That path repaints the whole window for every animation frame, so a single
/// ambient CSS animation can consume a full CPU core on a large window. Themes
/// use the `data-orqella-motion="reduce"` marker to keep their static styling
/// without the continuous motion. It runs in every frame so module pages get
/// the same treatment as the shell.
const SHELL_MARKER_SCRIPT: &str = r#"
(() => {
  const reduceMotion = __ORQELLA_REDUCE_MOTION__;
  const mark = () => {
    const root = document.documentElement;
    if (!root) return false;
    root.dataset.orqellaShell = 'desktop';
    if (reduceMotion) root.dataset.orqellaMotion = 'reduce';
    return true;
  };
  if (mark()) return;
  const observer = new MutationObserver(() => {
    if (mark()) observer.disconnect();
  });
  observer.observe(document, { childList: true });
})();
"#;

fn shell_marker_script(reduce_motion: bool) -> String {
    SHELL_MARKER_SCRIPT.replace(
        "__ORQELLA_REDUCE_MOTION__",
        if reduce_motion { "true" } else { "false" },
    )
}

/// Browser-style page zoom for the whole webview.
///
/// Module pages and app surfaces live in frames, so the listener runs in every
/// frame. A frame that sees the gesture relays it to the top document, which
/// owns the Tauri IPC bridge; the Rust side keeps the level and applies it to
/// the native webview (which scales every frame together).
const ZOOM_SHORTCUT_SCRIPT: &str = r#"
(() => {
  const IS_MACOS = __ORQELLA_IS_MACOS__;
  const isTop = window.top === window;
  const WHEEL_THRESHOLD = 100;
  const ZOOM_IN = 1;
  const ZOOM_OUT = -1;
  const ZOOM_RESET = 0;
  let wheelDelta = 0;

  const invoke = (command, args) => {
    const bridge = window.__TAURI_INTERNALS__;
    if (!bridge || typeof bridge.invoke !== 'function') return;
    bridge.invoke(command, args).catch(() => {});
  };

  const requestZoom = (steps) => {
    if (isTop) {
      invoke('zoom_page', { steps });
      return;
    }
    try {
      window.top.postMessage({ type: 'orqella:zoom', steps }, '*');
    } catch (_) {}
  };

  const zoomModified = (event) => IS_MACOS
    ? (event.metaKey || event.ctrlKey)
    : event.ctrlKey;

  const onWheel = (event) => {
    if (!zoomModified(event)) {
      wheelDelta = 0;
      return;
    }
    event.preventDefault();
    event.stopPropagation();
    const unit = event.deltaMode === 1 ? 40 : event.deltaMode === 2 ? 100 : 1;
    wheelDelta += event.deltaY * unit;
    if (Math.abs(wheelDelta) < WHEEL_THRESHOLD) return;
    const steps = Math.trunc(wheelDelta / WHEEL_THRESHOLD);
    wheelDelta -= steps * WHEEL_THRESHOLD;
    if (steps !== 0) requestZoom(-steps);
  };

  const onKey = (event) => {
    if (!zoomModified(event) || event.altKey) return;
    let steps;
    if (event.key === '0' || event.code === 'Digit0' || event.code === 'Numpad0') {
      steps = ZOOM_RESET;
    } else if (event.key === '=' || event.key === '+'
      || event.code === 'Equal' || event.code === 'NumpadAdd') {
      steps = ZOOM_IN;
    } else if (event.key === '-' || event.key === '_'
      || event.code === 'Minus' || event.code === 'NumpadSubtract') {
      steps = ZOOM_OUT;
    } else {
      return;
    }
    event.preventDefault();
    event.stopPropagation();
    requestZoom(steps);
  };

  if (isTop) {
    // Frames can only ask for page zoom. The request is cosmetic, and the
    // Rust command clamps it to a fixed level table.
    window.addEventListener('message', (event) => {
      const data = event.data;
      if (!data || data.type !== 'orqella:zoom') return;
      const steps = data.steps;
      if (typeof steps !== 'number' || !Number.isFinite(steps)) return;
      invoke('zoom_page', { steps: Math.max(-100, Math.min(100, Math.trunc(steps))) });
    });
  }

  window.addEventListener('wheel', onWheel, { capture: true, passive: false });
  window.addEventListener('keydown', onKey, true);
})();
"#;

fn zoom_shortcut_script() -> String {
    ZOOM_SHORTCUT_SCRIPT.replace(
        "__ORQELLA_IS_MACOS__",
        if cfg!(target_os = "macos") {
            "true"
        } else {
            "false"
        },
    )
}

/// Discrete page-zoom stops, matching the levels browsers use.
const ZOOM_LEVELS: [f64; 17] = [
    0.25, 0.33, 0.5, 0.67, 0.75, 0.8, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0, 4.0, 5.0,
];
const DEFAULT_ZOOM_INDEX: usize = 7;
const MAX_ZOOM_MESSAGE_STEPS: i32 = 100;

fn zoom_index_after(current: usize, steps: i32) -> usize {
    if steps == 0 {
        return DEFAULT_ZOOM_INDEX;
    }
    let last = (ZOOM_LEVELS.len() - 1) as i64;
    (current as i64 + i64::from(steps)).clamp(0, last) as usize
}

fn local_hub() -> String {
    // Debug-only override for exercising the disconnected state.
    #[cfg(debug_assertions)]
    if let Ok(value) = std::env::var("ORQELLA_LOCAL_HUB")
        && !value.trim().is_empty()
    {
        return value;
    }
    LOCAL_HUB.to_string()
}
const LOG_FILE_NAME: &str = "orqella.log";
const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;

/// Line-oriented writer that removes credentials before they reach disk.
///
/// The formatter may write an event in several chunks, so complete lines are
/// buffered and redacted as a unit. Any trailing partial line is redacted on
/// flush.
struct RedactingWriter<W: Write> {
    inner: W,
    buffer: Vec<u8>,
    in_private_key: bool,
}

impl<W: Write> RedactingWriter<W> {
    fn new(inner: W) -> Self {
        Self {
            inner,
            buffer: Vec::new(),
            in_private_key: false,
        }
    }

    fn write_redacted_line(&mut self, text: &str) -> std::io::Result<()> {
        if self.in_private_key {
            if text.contains("-----END") {
                self.in_private_key = false;
            }
            return Ok(());
        }
        if text.contains("-----BEGIN") && text.contains("PRIVATE KEY") && !text.contains("-----END")
        {
            self.in_private_key = true;
            self.inner.write_all(b"[REDACTED PRIVATE KEY]\n")?;
            return Ok(());
        }
        self.inner.write_all(redact_text(text).as_bytes())?;
        self.inner.write_all(b"\n")
    }

    fn drain_complete_lines(&mut self) -> std::io::Result<()> {
        while let Some(position) = self.buffer.iter().position(|byte| *byte == b'\n') {
            let line = self.buffer.drain(..=position).collect::<Vec<_>>();
            let text = String::from_utf8_lossy(&line);
            let text = text.trim_end_matches(['\r', '\n']);
            self.write_redacted_line(text)?;
        }
        Ok(())
    }
}

impl<W: Write> Write for RedactingWriter<W> {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.buffer.extend_from_slice(buffer);
        self.drain_complete_lines()?;
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if !self.buffer.is_empty() {
            let text = String::from_utf8_lossy(&self.buffer).into_owned();
            self.buffer.clear();
            let text = text.trim_end_matches(['\r', '\n']);
            if self.in_private_key {
                self.in_private_key = false;
                self.inner.write_all(b"[REDACTED PRIVATE KEY]\n")?;
            } else {
                self.inner.write_all(redact_text(text).as_bytes())?;
                self.inner.write_all(b"\n")?;
            }
        }
        self.inner.flush()
    }
}

fn redaction_patterns() -> &'static [(regex::Regex, &'static str)] {
    static PATTERNS: OnceLock<Vec<(regex::Regex, &'static str)>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        vec![
            // Cookie headers can contain several values; redact the remainder
            // of the line rather than only the first value.
            (
                regex::Regex::new(r#"(?i)\b(cookie|set-cookie)\b("?\s*[:=]\s*"?).*"#)
                    .expect("cookie redaction pattern"),
                "$1$2[REDACTED]",
            ),
            (
                regex::Regex::new(
                    r#"(?i)\b(authorization|token|access_token|refresh_token|ws_token|device_code|user_code|confirmation_code|ticket|nonce|password|secret|api[_-]?key)\b("?\s*[:=]\s*"?)(?:bearer\s+)?([^\s,;"'&}]+)"#,
                )
                .expect("credential redaction pattern"),
                "$1$2[REDACTED]",
            ),
            (
                regex::Regex::new(r"(?i)\bbearer\s+[A-Za-z0-9._~+/=-]+")
                    .expect("bearer redaction pattern"),
                "[REDACTED]",
            ),
            (
                regex::Regex::new(
                    r"(?s)-----BEGIN [A-Z ]*PRIVATE KEY-----.*?-----END [A-Z ]*PRIVATE KEY-----",
                )
                .expect("private key redaction pattern"),
                "[REDACTED PRIVATE KEY]",
            ),
        ]
    })
}

fn redact_text(value: &str) -> String {
    let mut redacted = value.to_string();
    for (pattern, replacement) in redaction_patterns() {
        redacted = pattern.replace_all(&redacted, *replacement).into_owned();
    }
    redacted
}

fn log_dir() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        return std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("io.orqella.desktop")
            .join("logs");
    }
    #[cfg(target_os = "macos")]
    {
        return std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("Library")
            .join("Logs")
            .join("io.orqella.desktop");
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let base = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .map(|home| home.join(".local").join("state"))
            })
            .unwrap_or_else(std::env::temp_dir);
        base.join("io.orqella.desktop").join("logs")
    }
}

fn create_private_dir(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)?;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(path)
    }
}

fn set_private_file(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

fn open_log_file() -> std::io::Result<std::fs::File> {
    let directory = log_dir();
    create_private_dir(&directory)?;
    let path = directory.join(LOG_FILE_NAME);
    if std::fs::metadata(&path).is_ok_and(|metadata| metadata.len() > MAX_LOG_BYTES) {
        let rotated = directory.join(format!("{LOG_FILE_NAME}.1"));
        let _ = std::fs::remove_file(&rotated);
        let _ = std::fs::rename(&path, rotated);
        let _ = set_private_file(&directory.join(format!("{LOG_FILE_NAME}.1")));
    }
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    options.mode(0o600);
    let file = options.open(&path)?;
    set_private_file(&path)?;
    Ok(file)
}

fn init_logging() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        "orqella_desktop=info,orqella_desktop_gateway=info,orqella_desktop_transport=info".into()
    });
    let registry = tracing_subscriber::registry().with(filter);
    let console = tracing_subscriber::fmt::layer().with_writer(std::sync::Mutex::new(
        RedactingWriter::new(std::io::stdout()),
    ));
    match open_log_file() {
        Ok(file) => {
            let file = std::sync::Mutex::new(RedactingWriter::new(file));
            registry
                .with(console)
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_ansi(false)
                        .with_writer(file),
                )
                .init();
        }
        Err(error) => {
            registry.with(console).init();
            eprintln!("could not open the Orqella log file: {error}");
        }
    }
}

fn read_recent_logs(max_lines: usize) -> String {
    let path = log_dir().join(LOG_FILE_NAME);
    let Ok(value) = std::fs::read_to_string(path) else {
        return String::new();
    };
    let mut lines = value.lines().collect::<Vec<_>>();
    if lines.len() > max_lines {
        lines = lines.split_off(lines.len() - max_lines);
    }
    redact_text(&lines.join("\n"))
}

fn diagnostics_export_path(app: &tauri::AppHandle) -> Result<PathBuf> {
    #[cfg(debug_assertions)]
    if let Ok(path) = std::env::var("ORQELLA_DIAGNOSTICS_EXPORT_PATH")
        && !path.trim().is_empty()
    {
        return Ok(PathBuf::from(path));
    }
    let directory = app.path().download_dir()?;
    std::fs::create_dir_all(&directory)?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    Ok(unique_download_path(
        &directory,
        &format!("orqella-diagnostics-{stamp}.txt"),
    ))
}

fn diagnostics_bundle(diagnostics: &Diagnostics, logs: &str) -> String {
    let summary = serde_json::to_string_pretty(diagnostics).unwrap_or_else(|_| "{}".to_string());
    format!(
        "Orqella desktop diagnostics\n\n## Summary\n\n{summary}\n\n## Recent logs (redacted)\n\n{}\n",
        redact_text(logs)
    )
}

fn write_private_file(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(path)?;
    file.write_all(contents)?;
    set_private_file(path)
}

fn write_diagnostics_bundle(path: &Path, diagnostics: &Diagnostics, logs: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    write_private_file(path, diagnostics_bundle(diagnostics, logs).as_bytes())?;
    Ok(())
}

#[derive(Default)]
struct AppState {
    gateway: Mutex<Option<Gateway>>,
    tunnel: Mutex<Option<RemoteTunnel>>,
    identity: Mutex<Option<ClientIdentity>>,
    account_token: Mutex<Option<String>>,
    challenge: Mutex<Option<LoginChallenge>>,
    local_challenge: Mutex<Option<LoginChallenge>>,
    mode: Mutex<String>,
    profile: Mutex<Profile>,
    /// Exact origin of the running desktop gateway, used by the sync
    /// permission and navigation callbacks.
    gateway_origin: std::sync::Mutex<Option<String>>,
    /// Exact app origins registered with the running gateway.
    navigation_origins: std::sync::Mutex<BTreeSet<String>>,
    /// Whether the bundled local settings/onboarding page is showing in the
    /// main window. The local page has privileged commands; the remote
    /// dashboard does not.
    settings_open: AtomicBool,
    /// Dashboard URL to return to when the settings page closes.
    settings_return_url: std::sync::Mutex<Option<String>>,
    /// Base URL of the bundled local pages, captured from the startup window.
    local_page_base: std::sync::Mutex<Option<url::Url>>,
    session: AtomicU64,
    downloads: std::sync::Mutex<Vec<DownloadRecord>>,
    /// Index into [`ZOOM_LEVELS`]. Native webviews keep their zoom across
    /// navigations, so this must live for the whole app session.
    zoom_index: AtomicUsize,
}

impl AppState {
    fn new() -> Self {
        Self {
            mode: Mutex::new("starting".into()),
            profile: Mutex::new(Profile::default()),
            zoom_index: AtomicUsize::new(DEFAULT_ZOOM_INDEX),
            ..Self::default()
        }
    }
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LinuxWebKitTransport {
    /// Keep DMA-BUF and turn off NVIDIA explicit sync. This avoids Hyprland's
    /// strict acquire-point check without the shared-memory performance cost.
    DmaBufWithoutExplicitSync,
    /// Fall back to WebKitGTK's shared-memory renderer.
    ForceSharedMemory,
    /// Respect an explicit user choice; change nothing.
    Unchanged,
}

#[cfg(target_os = "linux")]
fn linux_webkit_transport(
    wayland: bool,
    using_x11: bool,
    nvidia: bool,
    hyprland: bool,
    force_shm_set: bool,
    dmabuf_disabled: bool,
    explicit_sync_set: bool,
) -> LinuxWebKitTransport {
    if !wayland || force_shm_set || dmabuf_disabled || explicit_sync_set {
        return LinuxWebKitTransport::Unchanged;
    }
    if using_x11 {
        return LinuxWebKitTransport::ForceSharedMemory;
    }
    if nvidia && hyprland {
        LinuxWebKitTransport::DmaBufWithoutExplicitSync
    } else {
        LinuxWebKitTransport::ForceSharedMemory
    }
}

#[cfg(target_os = "linux")]
fn configure_linux_wayland() {
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some()
        || std::env::var("XDG_SESSION_TYPE").is_ok_and(|value| value == "wayland");
    if !wayland {
        return;
    }

    // GDK_BACKEND=x11 runs WebKitGTK through XWayland. Keep the shared-memory
    // fallback there: the X11 DMA-BUF path cannot always allocate GBM buffers.
    let using_x11 = std::env::var("GDK_BACKEND").is_ok_and(|value| {
        value
            .split(',')
            .next()
            .is_some_and(|backend| backend.trim().eq_ignore_ascii_case("x11"))
    });
    let nvidia = Path::new("/proc/driver/nvidia/version").exists();
    let hyprland = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some()
        || ["XDG_CURRENT_DESKTOP", "XDG_SESSION_DESKTOP"]
            .iter()
            .any(|key| {
                std::env::var(key).is_ok_and(|value| {
                    value
                        .split(':')
                        .any(|desktop| desktop.eq_ignore_ascii_case("Hyprland"))
                })
            });

    // WebKitGTK's DMA-BUF renderer enables Wayland explicit sync but commits
    // without an acquire point. Hyprland rejects that with a protocol error
    // (Error 71) and closes the connection. Disabling NVIDIA explicit sync
    // keeps the fast DMA-BUF path; other Wayland setups keep the shared-memory
    // renderer. Do not use WEBKIT_DISABLE_DMABUF_RENDERER=1 for this: on
    // WebKitGTK 2.54 it leaves the renderer transport mode empty, so the web
    // process never initializes its shared PlatformDisplay. The first WebCodecs
    // H.264 decoder (for example noVNC's VM console capability probe) then hits
    // RELEASE_ASSERT(s_sharedDisplay) and aborts the web process.
    let transport = linux_webkit_transport(
        wayland,
        using_x11,
        nvidia,
        hyprland,
        std::env::var_os("WEBKIT_DMABUF_RENDERER_FORCE_SHM").is_some(),
        std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_some(),
        std::env::var_os("__NV_DISABLE_EXPLICIT_SYNC").is_some(),
    );
    match transport {
        LinuxWebKitTransport::DmaBufWithoutExplicitSync => unsafe {
            std::env::set_var("__NV_DISABLE_EXPLICIT_SYNC", "1");
        },
        LinuxWebKitTransport::ForceSharedMemory => unsafe {
            std::env::set_var("WEBKIT_DMABUF_RENDERER_FORCE_SHM", "1");
        },
        LinuxWebKitTransport::Unchanged => {}
    }
}

fn same_http_origin(url: &url::Url, origin: &str) -> bool {
    let Ok(expected) = url::Url::parse(origin) else {
        return false;
    };
    expected.scheme() == url.scheme()
        && expected.host_str() == url.host_str()
        && expected.port_or_known_default() == url.port_or_known_default()
}

fn desktop_navigation_allowed(state: &AppState, url: &url::Url) -> bool {
    if matches!(url.scheme(), "tauri" | "about") {
        return true;
    }
    if url.scheme() != "http" {
        return false;
    }
    if let Ok(origin) = state.gateway_origin.lock()
        && let Some(origin) = origin.as_deref()
        && same_http_origin(url, origin)
    {
        return true;
    }
    state
        .navigation_origins
        .lock()
        .is_ok_and(|origins| origins.contains(&url.origin().ascii_serialization()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NewWindowAction {
    Navigate,
    External,
    Ignore,
}

/// Route a `window.open`/`target="_blank"` request. Desktop links to an
/// allowed loopback origin navigate the existing window; every other HTTP(S)
/// link goes to the system browser. Nothing opens a second Tauri webview.
fn new_window_action(state: &AppState, url: &url::Url) -> NewWindowAction {
    if !matches!(url.scheme(), "http" | "https") {
        return NewWindowAction::Ignore;
    }
    if desktop_navigation_allowed(state, url) {
        NewWindowAction::Navigate
    } else {
        NewWindowAction::External
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    #[cfg(target_os = "linux")]
    configure_linux_wayland();
    init_logging();

    let state = Arc::new(AppState::new());
    let permission_state = Arc::clone(&state);
    let navigation_state = Arc::clone(&state);
    tauri::Builder::default()
        .manage(state)
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            handle_deep_links(app, &argv);
        }))
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(navigation_plugin(navigation_state))
        .on_permission_request(move |webview, kind| match kind {
            PermissionKind::Microphone
            | PermissionKind::Camera
            | PermissionKind::Geolocation
            | PermissionKind::Notifications
            | PermissionKind::ClipboardRead
            | PermissionKind::DisplayCapture => {
                let allowed = webview.url().ok().is_some_and(|url| {
                    permission_state
                        .gateway_origin
                        .lock()
                        .ok()
                        .and_then(|origin| origin.clone())
                        .is_some_and(|origin| same_http_origin(&url, &origin))
                });
                if allowed {
                    PermissionResponse::Allow
                } else {
                    PermissionResponse::Default
                }
            }
            _ => PermissionResponse::Default,
        })
        .invoke_handler(tauri::generate_handler![
            desktop_status,
            local_link_status,
            begin_local_link_login,
            poll_local_link_login,
            sign_out_local_link,
            connect_local,
            begin_link_login,
            poll_link_login,
            list_machines,
            connect_link_machine,
            disconnect_session,
            test_notification,
            diagnostics,
            export_diagnostics,
            sign_out,
            updater_configured,
            check_updates,
            install_update,
            saved_hubs,
            list_downloads,
            zoom_page,
            close_settings,
        ])
        .setup(|app| {
            create_main_window(app.handle())?;
            install_menu(app.handle())?;
            install_tray(app.handle())?;
            let startup_args = std::env::args().collect::<Vec<_>>();
            handle_deep_links(app.handle(), &startup_args);
            let state = app.state::<Arc<AppState>>().inner().clone();
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let settings_requested = state.settings_open.load(Ordering::SeqCst);
                if let Err(error) = restore_saved_account(state.clone()).await {
                    tracing::debug!(%error, "could not restore saved Link account");
                }
                if let Ok(profile) = load_profile(&handle) {
                    tracing::info!(path = ?profile_path(&handle), "loaded desktop profile");
                    *state.profile.lock().await = profile;
                }
                let profile = state.profile.lock().await.clone();
                let mut connected = false;
                if profile.auto_connect
                    && profile.mode == "remote"
                    && let Some(target) = profile.last_target.clone()
                    && let Err(error) = connect_link_inner(
                        &handle,
                        state.clone(),
                        target,
                        !settings_requested,
                    )
                    .await
                {
                    tracing::warn!(error = %format!("{error:#}"), "could not restore remote Orqella session");
                } else if profile.auto_connect && profile.mode == "remote" {
                    connected = true;
                }
                if !connected
                    && let Err(error) =
                        start_local_session(handle.clone(), state, !settings_requested).await
                {
                    tracing::warn!(%error, "local Orqella session did not start");
                }
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Orqella desktop");
}

fn navigation_plugin<R: tauri::Runtime>(state: Arc<AppState>) -> tauri::plugin::TauriPlugin<R> {
    tauri::plugin::Builder::new("orqella-navigation")
        .on_navigation(move |_webview, url| {
            if desktop_navigation_allowed(&state, url) {
                return true;
            }
            if matches!(url.scheme(), "http" | "https") {
                open_external(url.as_str());
            }
            false
        })
        .build()
}

fn create_main_window(app: &tauri::AppHandle) -> Result<()> {
    let download_handle = app.clone();
    let navigation_state = app.state::<Arc<AppState>>().inner().clone();
    let page_state = navigation_state.clone();
    let navigation_app = app.clone();
    let reduce_motion =
        cfg!(target_os = "linux") && std::env::var_os("WEBKIT_DMABUF_RENDERER_FORCE_SHM").is_some();
    let window = WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
        .title("Orqella")
        .inner_size(1280.0, 820.0)
        .min_inner_size(880.0, 600.0)
        .initialization_script(RELOAD_SHORTCUT_SCRIPT)
        .initialization_script_for_all_frames(shell_marker_script(reduce_motion))
        .initialization_script_for_all_frames(zoom_shortcut_script())
        .on_page_load(|window, payload| {
            // Native webviews preserve zoom across navigations, but reapply it
            // so the JS-visible gesture state and the displayed scale stay in
            // step if a platform ever resets the webview.
            if matches!(payload.event(), PageLoadEvent::Started) {
                let state = window.state::<Arc<AppState>>();
                let index = state.zoom_index.load(Ordering::SeqCst);
                if index != DEFAULT_ZOOM_INDEX {
                    let _ = window.set_zoom(ZOOM_LEVELS[index]);
                }
            }
        })
        .on_download(move |_webview, event| handle_download_event(&download_handle, event))
        .on_new_window(move |url, _features| {
            handle_new_window(&navigation_app, navigation_state.as_ref(), "main", &url)
        })
        .build()?;
    if let Ok(url) = window.url()
        && url.scheme() != "about"
    {
        *page_state
            .local_page_base
            .lock()
            .expect("local page base lock") = Some(url);
    }
    apply_test_theme(&window);
    Ok(())
}

/// Resolve a bundled local page without relying on the remote dashboard URL.
fn local_page_url(state: &AppState, path: &str) -> Option<url::Url> {
    if let Some(base) = state
        .local_page_base
        .lock()
        .expect("local page base lock")
        .clone()
        && let Ok(url) = base.join(path)
    {
        return Some(url);
    }
    let base = if cfg!(windows) {
        "http://tauri.localhost/"
    } else {
        "tauri://localhost/"
    };
    url::Url::parse(base).ok()?.join(path).ok()
}

/// Test-only system theme override for automated light/dark screenshots.
fn apply_test_theme(window: &WebviewWindow) {
    #[cfg(debug_assertions)]
    {
        let Ok(theme) = std::env::var("ORQELLA_TEST_THEME") else {
            return;
        };
        let theme = match theme.as_str() {
            "light" => Some(tauri::Theme::Light),
            "dark" => Some(tauri::Theme::Dark),
            _ => None,
        };
        if let Some(theme) = theme {
            let _ = window.set_theme(Some(theme));
        }
    }
    #[cfg(not(debug_assertions))]
    let _ = window;
}

fn download_filename(url: &url::Url) -> String {
    let raw = url
        .path_segments()
        .and_then(|mut segments| segments.next_back())
        .filter(|name| !name.is_empty())
        .unwrap_or("download");
    let decoded = percent_encoding::percent_decode_str(raw).decode_utf8_lossy();
    let cleaned = decoded
        .chars()
        .map(|character| {
            if character.is_control()
                || matches!(
                    character,
                    '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'
                )
            {
                '_'
            } else {
                character
            }
        })
        .collect::<String>();
    let cleaned = cleaned.trim_end_matches(['.', ' ']).to_string();
    if cleaned.trim_matches('.').is_empty() {
        return "download".into();
    }
    let stem = cleaned
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    let reserved = matches!(
        stem.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    );
    if reserved {
        format!("_{cleaned}")
    } else {
        cleaned
    }
}

fn unique_download_path(dir: &std::path::Path, filename: &str) -> PathBuf {
    let initial = dir.join(filename);
    if !initial.exists() {
        return initial;
    }
    let path = std::path::Path::new(filename);
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("download");
    let extension = path.extension().and_then(|value| value.to_str());
    for index in 1..1000 {
        let name = match extension {
            Some(extension) => format!("{stem}-{index}.{extension}"),
            None => format!("{stem}-{index}"),
        };
        let candidate = dir.join(name);
        if !candidate.exists() {
            return candidate;
        }
    }
    dir.join(format!("download-{}", std::process::id()))
}

fn update_download(app: &tauri::AppHandle, path: &std::path::Path, status: &str, bytes: u64) {
    let state = app.state::<Arc<AppState>>();
    let Ok(mut downloads) = state.downloads.lock() else {
        return;
    };
    let Some(record) = downloads
        .iter_mut()
        .find(|record| record.path == path.to_string_lossy())
    else {
        return;
    };
    record.status = status.into();
    record.bytes = bytes;
    let record = record.clone();
    drop(downloads);
    let _ = app.emit("orqella-download", record);
}

fn handle_download_event(app: &tauri::AppHandle, event: DownloadEvent<'_>) -> bool {
    match event {
        DownloadEvent::Requested { url, destination } => {
            let Ok(directory) = app.path().download_dir() else {
                tracing::warn!("could not resolve downloads directory");
                return false;
            };
            if let Err(error) = std::fs::create_dir_all(&directory) {
                tracing::warn!(%error, "could not create downloads directory");
                return false;
            }
            let filename = download_filename(&url);
            let path = unique_download_path(&directory, &filename);
            *destination = path.clone();
            let record = DownloadRecord {
                url: url.to_string(),
                path: path.to_string_lossy().into_owned(),
                status: "downloading".into(),
                bytes: 0,
            };
            let state = app.state::<Arc<AppState>>();
            if let Ok(mut downloads) = state.downloads.lock() {
                downloads.push(record.clone());
                if downloads.len() > 100 {
                    downloads.remove(0);
                }
            }
            let _ = app.emit("orqella-download", record);
            let poll_app = app.clone();
            let poll_path = path.clone();
            tauri::async_runtime::spawn(async move {
                let mut last_bytes = 0;
                let mut stable_ticks = 0;
                for _ in 0..1200 {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    let bytes = std::fs::metadata(&poll_path)
                        .map(|metadata| metadata.len())
                        .unwrap_or(0);
                    if bytes != last_bytes {
                        last_bytes = bytes;
                        stable_ticks = 0;
                        update_download(&poll_app, &poll_path, "downloading", bytes);
                    } else if bytes > 0 {
                        stable_ticks += 1;
                    }
                    // WebKitGTK does not always deliver a Finished event for a
                    // completed attachment. Treat a stable, non-empty file as
                    // complete after two seconds.
                    if stable_ticks >= 8 {
                        update_download(&poll_app, &poll_path, "finished", bytes);
                        return;
                    }
                    let finished = poll_app
                        .state::<Arc<AppState>>()
                        .downloads
                        .lock()
                        .map(|downloads| {
                            downloads.iter().any(|record| {
                                record.path == poll_path.to_string_lossy()
                                    && matches!(record.status.as_str(), "finished" | "failed")
                            })
                        })
                        .unwrap_or(true);
                    if finished {
                        return;
                    }
                }
            });
            true
        }
        DownloadEvent::Finished { url, path, success } => {
            let path = path.unwrap_or_else(|| app.path().download_dir().unwrap_or_default());
            let bytes = std::fs::metadata(&path)
                .map(|metadata| metadata.len())
                .unwrap_or(0);
            let status = if success { "finished" } else { "failed" };
            update_download(app, &path, status, bytes);
            let _ = url;
            true
        }
        _ => true,
    }
}

/// Move the main webview through the browser-style [`ZOOM_LEVELS`] stops.
///
/// `steps` is positive to zoom in and negative to zoom out; zero restores
/// 100%. Input is handled in the page because module and app surfaces are
/// iframes, but zooming itself stays native so every frame scales together.
#[tauri::command]
fn zoom_page(
    webview: tauri::Webview,
    state: tauri::State<'_, Arc<AppState>>,
    steps: i32,
) -> Result<f64, String> {
    // The capability already scopes this command to the shell origin, but
    // re-check the navigation allowlist so an unexpected origin that matches a
    // URL pattern still cannot touch the webview.
    let url = webview.url().map_err(|error| error.to_string())?;
    if !desktop_navigation_allowed(state.inner(), &url) {
        return Err("page zoom is not available on this origin".into());
    }
    let steps = steps.clamp(-MAX_ZOOM_MESSAGE_STEPS, MAX_ZOOM_MESSAGE_STEPS);
    let current = state.zoom_index.load(Ordering::SeqCst);
    let next = zoom_index_after(current, steps);
    let level = ZOOM_LEVELS[next];
    webview.set_zoom(level).map_err(|error| error.to_string())?;
    state.zoom_index.store(next, Ordering::SeqCst);
    tracing::debug!(steps, level, "page zoom adjusted");
    Ok(level)
}

#[tauri::command]
fn list_downloads(state: tauri::State<'_, Arc<AppState>>) -> Result<Vec<DownloadRecord>, String> {
    let downloads = state.downloads.lock().map_err(|error| error.to_string())?;
    Ok(downloads.clone())
}

fn in_window_menu_enabled() -> bool {
    #[cfg(target_os = "linux")]
    {
        let desktop = std::env::var("XDG_CURRENT_DESKTOP")
            .unwrap_or_default()
            .to_ascii_lowercase();
        let tiling = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some()
            || std::env::var_os("SWAYSOCK").is_some()
            || std::env::var_os("NIRI_SOCKET").is_some()
            || desktop
                .split(':')
                .any(|value| matches!(value, "hyprland" | "sway" | "river" | "wayfire" | "niri"));
        !tiling
    }
    #[cfg(not(target_os = "linux"))]
    {
        true
    }
}

fn install_menu(app: &tauri::AppHandle) -> Result<()> {
    if !in_window_menu_enabled() {
        tracing::info!("tiling compositor detected; using Ctrl+, and the tray for Settings");
        return Ok(());
    }
    let settings = MenuItemBuilder::with_id("settings", "Settings…")
        .accelerator("CmdOrCtrl+,")
        .build(app)?;
    let quit = PredefinedMenuItem::quit(app, None)?;
    let menu = MenuBuilder::new(app).items(&[&settings, &quit]).build()?;
    app.set_menu(menu)?;
    app.on_menu_event(|app, event| {
        if event.id() == "settings" {
            show_settings(app);
        }
    });
    Ok(())
}

fn handle_deep_links(app: &tauri::AppHandle, args: &[String]) {
    for arg in args {
        if arg == "--settings" {
            show_settings(app);
            continue;
        }
        let Ok(url) = url::Url::parse(arg) else {
            continue;
        };
        if url.scheme() != "orqella" {
            continue;
        }
        if let Some(window) = app.get_webview_window("main") {
            let _ = window.show();
            let _ = window.set_focus();
        }
        match url.host_str() {
            Some("settings") => show_settings(app),
            Some("hub") => {
                let target = url.path().trim_matches('/').to_string();
                if !target.is_empty() {
                    let state = app.state::<Arc<AppState>>().inner().clone();
                    let app = app.clone();
                    tauri::async_runtime::spawn(async move {
                        if let Err(error) = connect_link_inner(&app, state, target, true).await {
                            tracing::warn!(error = %format!("{error:#}"), "deep link connect failed");
                        }
                    });
                }
            }
            Some("app") | Some("module") => {
                // The shell owns app/module routing. Deep links focus the
                // dashboard until the shell exposes a route event.
                tracing::info!("deep link received");
            }
            _ => {}
        }
    }
}

fn install_tray(app: &tauri::AppHandle) -> Result<()> {
    let menu = MenuBuilder::new(app)
        .text("tray-open", "Open Orqella")
        .text("tray-settings", "Settings…")
        .separator()
        .text("tray-quit", "Quit")
        .build()?;
    let mut builder = TrayIconBuilder::with_id("orqella-tray")
        .tooltip("Orqella")
        .menu(&menu)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "tray-open" => {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
            "tray-settings" => show_settings(app),
            "tray-quit" => app.exit(0),
            _ => {}
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

/// Show the bundled local settings/onboarding page in the main window.
///
/// The dashboard is external content and has no privileged Tauri commands. The
/// bundled local page is covered by a local-only capability, so this navigation
/// keeps the same security boundary as the old separate settings window.
fn show_settings(app: &tauri::AppHandle) {
    let state = app.state::<Arc<AppState>>().inner().clone();
    show_settings_for(app, &state);
}

fn show_settings_for(app: &tauri::AppHandle, state: &Arc<AppState>) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    if state.settings_open.swap(true, Ordering::SeqCst) {
        let showing = window
            .url()
            .ok()
            .is_some_and(|url| url.path().ends_with("/settings.html"));
        if showing {
            let _ = window.show();
            let _ = window.set_focus();
            return;
        }
    }
    if let Ok(current) = window.url()
        && current.scheme() != "tauri"
        && current.scheme() != "about"
        && desktop_navigation_allowed(state, &current)
    {
        *state
            .settings_return_url
            .lock()
            .expect("settings return lock") = Some(current.to_string());
    }
    let page = local_page_url(state, "settings.html");
    let Some(page) = page else {
        tracing::warn!("local page base is unavailable");
        state.settings_open.store(false, Ordering::SeqCst);
        return;
    };
    if let Err(error) = window.navigate(page) {
        tracing::warn!(%error, "could not show settings page");
        state.settings_open.store(false, Ordering::SeqCst);
    }
    let _ = window.show();
    let _ = window.set_focus();
}

#[tauri::command]
async fn close_settings(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<(), String> {
    close_settings_inner(&app, state.inner())
        .await
        .map_err(|error| error.to_string())
}

async fn close_settings_inner(app: &tauri::AppHandle, state: &Arc<AppState>) -> Result<()> {
    let Some(window) = app.get_webview_window("main") else {
        return Ok(());
    };
    let return_url = state
        .settings_return_url
        .lock()
        .expect("settings return lock")
        .take();
    if let Some(value) = return_url
        && let Ok(url) = value.parse::<url::Url>()
        && desktop_navigation_allowed(state, &url)
    {
        state.settings_open.store(false, Ordering::SeqCst);
        window.navigate(url)?;
        return Ok(());
    }
    if state.gateway.lock().await.is_some() {
        state.settings_open.store(false, Ordering::SeqCst);
        navigate_main(app, state).await?;
        return Ok(());
    }
    // No dashboard is available. Keep the local page visible; the close
    // control is hidden in this state.
    state.settings_open.store(true, Ordering::SeqCst);
    Ok(())
}

async fn restore_saved_account(state: Arc<AppState>) -> Result<()> {
    if let Ok(Some(identity)) = load_identity() {
        *state.identity.lock().await = Some(identity);
    }
    if let Ok(Some(token)) = load_account_token() {
        *state.account_token.lock().await = Some(token);
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
struct DownloadRecord {
    url: String,
    path: String,
    status: String,
    bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Profile {
    #[serde(default = "default_shell_port")]
    shell_port: u16,
    /// Stable loopback shell port per hub profile. A distinct origin per hub
    /// keeps cookies, localStorage, and service workers isolated when the user
    /// switches machines.
    #[serde(default)]
    shell_ports: BTreeMap<String, u16>,
    #[serde(default)]
    mode: String,
    #[serde(default)]
    last_target: Option<String>,
    #[serde(default)]
    last_target_name: Option<String>,
    #[serde(default)]
    auto_connect: bool,
    #[serde(default)]
    hubs: Vec<SavedHub>,
    /// Stable app-origin leases keyed by hub/profile, mount, and canonical host.
    #[serde(default)]
    app_origins: BTreeMap<String, AppOriginLease>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AppOriginLease {
    /// Last octet of the 127.0.0.x address (ignored on macOS, where the host
    /// carries the isolation and the port is persisted instead).
    ip: u8,
    /// Preferred local port on macOS.
    #[serde(default)]
    port: Option<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SavedHub {
    id: String,
    name: String,
    #[serde(default)]
    last_connected_at: String,
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            shell_port: default_shell_port(),
            shell_ports: BTreeMap::new(),
            mode: "local".into(),
            last_target: None,
            last_target_name: None,
            auto_connect: false,
            hubs: Vec::new(),
            app_origins: BTreeMap::new(),
        }
    }
}

fn chrono_like_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs().to_string())
        .unwrap_or_default()
}

fn default_shell_port() -> u16 {
    SHELL_PORT
}

fn profile_path(app: &tauri::AppHandle) -> Result<PathBuf> {
    let dir = app
        .path()
        .app_config_dir()
        .context("resolve desktop config directory")?;
    create_private_dir(&dir)?;
    Ok(dir.join("profile.json"))
}

fn load_profile(app: &tauri::AppHandle) -> Result<Profile> {
    let path = profile_path(app)?;
    if !path.exists() {
        return Ok(Profile::default());
    }
    let _ = set_private_file(&path);
    let value = std::fs::read_to_string(path)?;
    let profile: Profile = serde_json::from_str(&value)?;
    Ok(profile)
}

fn save_profile(app: &tauri::AppHandle, profile: &Profile) -> Result<()> {
    let path = profile_path(app)?;
    write_private_file(&path, &serde_json::to_vec_pretty(profile)?)?;
    Ok(())
}

#[derive(Debug, Serialize)]
struct StatusPayload {
    mode: String,
    shell_url: Option<String>,
    link_signed_in: bool,
}

#[tauri::command]
async fn desktop_status(state: tauri::State<'_, Arc<AppState>>) -> Result<StatusPayload, String> {
    let mode = state.mode.lock().await.clone();
    let shell_url = state
        .gateway
        .lock()
        .await
        .as_ref()
        .map(|gateway| gateway.base_url.clone());
    Ok(StatusPayload {
        mode,
        shell_url,
        link_signed_in: state.account_token.lock().await.is_some(),
    })
}

fn loopback_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("build loopback HTTP client")
}

async fn local_gateway_base(state: &Arc<AppState>) -> Result<String> {
    if *state.mode.lock().await != "local" {
        anyhow::bail!("the local daemon is not connected");
    }
    state
        .gateway
        .lock()
        .await
        .as_ref()
        .map(|gateway| gateway.base_url.clone())
        .context("the local daemon is not connected")
}

fn local_bootstrap_auth() -> Result<String, String> {
    local_bootstrap_token().ok_or_else(|| {
        "the local Orqella bootstrap token is unavailable; run `orqella open`".to_string()
    })
}

#[derive(Debug, Serialize)]
struct LocalLinkStatus {
    signed_in: bool,
    device_name: String,
    error: Option<String>,
}

#[tauri::command]
async fn local_link_status(
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<LocalLinkStatus, String> {
    let base = local_gateway_base(state.inner())
        .await
        .map_err(|error| error.to_string())?;
    let response = loopback_http_client()
        .get(format!("{base}api/v1/link"))
        .header("x-orqella-bootstrap", local_bootstrap_auth()?)
        .send()
        .await
        .map_err(|error| error.to_string())?;
    if !response.status().is_success() {
        return Err(format!("local daemon returned {}", response.status()));
    }
    let value: serde_json::Value = response.json().await.map_err(|error| error.to_string())?;
    let status = &value["status"];
    Ok(LocalLinkStatus {
        signed_in: status["enabled"].as_bool().unwrap_or(false),
        device_name: status["device_name"].as_str().unwrap_or("").to_string(),
        error: value["error"].as_str().map(str::to_string),
    })
}

#[tauri::command]
async fn begin_local_link_login(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<LoginChallenge, String> {
    let base = local_gateway_base(state.inner())
        .await
        .map_err(|error| error.to_string())?;
    let response = loopback_http_client()
        .post(format!("{base}api/v1/link/device/start"))
        .header("X-Orqella", "1")
        .header("x-orqella-bootstrap", local_bootstrap_auth()?)
        .json(&serde_json::json!({}))
        .send()
        .await
        .map_err(|error| error.to_string())?;
    if !response.status().is_success() {
        return Err(format!("local daemon returned {}", response.status()));
    }
    let challenge: LoginChallenge = response.json().await.map_err(|error| error.to_string())?;
    open_external(&challenge.verification_uri);
    *state.local_challenge.lock().await = Some(challenge.clone());
    let _ = app;
    Ok(challenge)
}

#[tauri::command]
async fn poll_local_link_login(
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<LoginPoll, String> {
    let challenge = state
        .local_challenge
        .lock()
        .await
        .clone()
        .ok_or_else(|| "no local Link login is in progress".to_string())?;
    let base = local_gateway_base(state.inner())
        .await
        .map_err(|error| error.to_string())?;
    let response = loopback_http_client()
        .post(format!("{base}api/v1/link/device/poll"))
        .header("X-Orqella", "1")
        .header("x-orqella-bootstrap", local_bootstrap_auth()?)
        .json(&serde_json::json!({
            "device_code": challenge.device_code,
        }))
        .send()
        .await
        .map_err(|error| error.to_string())?;
    if !response.status().is_success() {
        return Err(format!("local daemon returned {}", response.status()));
    }
    let value: serde_json::Value = response.json().await.map_err(|error| error.to_string())?;
    let status = value["status"].as_str().unwrap_or("pending").to_string();
    if status == "approved" {
        *state.local_challenge.lock().await = None;
    }
    Ok(LoginPoll {
        status,
        email: None,
        entitled: value["status_detail"]["link_entitled"].as_bool(),
    })
}

#[tauri::command]
async fn sign_out_local_link(state: tauri::State<'_, Arc<AppState>>) -> Result<(), String> {
    let base = local_gateway_base(state.inner())
        .await
        .map_err(|error| error.to_string())?;
    let response = loopback_http_client()
        .post(format!("{base}api/v1/link/logout"))
        .header("X-Orqella", "1")
        .header("x-orqella-bootstrap", local_bootstrap_auth()?)
        .send()
        .await
        .map_err(|error| error.to_string())?;
    if !response.status().is_success() {
        return Err(format!("local daemon returned {}", response.status()));
    }
    Ok(())
}

#[tauri::command]
async fn connect_local(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<StatusPayload, String> {
    connect_local_inner(&app, state.inner().clone(), true)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn begin_link_login(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<LoginChallenge, String> {
    let challenge = begin_login().await.map_err(|error| error.to_string())?;
    *state.challenge.lock().await = Some(challenge.clone());
    open_external(&challenge.verification_uri);
    let _ = app; // keep command signature stable for future UI events
    Ok(challenge)
}

#[derive(Debug, Serialize)]
struct LoginPoll {
    status: String,
    email: Option<String>,
    entitled: Option<bool>,
}

#[tauri::command]
async fn poll_link_login(state: tauri::State<'_, Arc<AppState>>) -> Result<LoginPoll, String> {
    let challenge = state
        .challenge
        .lock()
        .await
        .clone()
        .ok_or_else(|| "no Link login is in progress".to_string())?;
    let (status, session) = poll_login_status(&challenge)
        .await
        .map_err(|error| error.to_string())?;
    let Some(session) = session else {
        return Ok(LoginPoll {
            status,
            email: None,
            entitled: None,
        });
    };
    save_account_token(&session.token).map_err(|error| error.to_string())?;
    *state.account_token.lock().await = Some(session.token);
    *state.challenge.lock().await = None;
    Ok(LoginPoll {
        status: "approved".into(),
        email: Some(session.email),
        entitled: Some(session.entitled),
    })
}

#[derive(Debug, Serialize)]
struct MachinePayload {
    id: String,
    name: String,
    kind: String,
    last_seen_at: String,
    ssh_user: String,
    ssh_port: u16,
    peer_port: u16,
}

#[tauri::command]
async fn list_machines(
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<Vec<MachinePayload>, String> {
    let token = account_token(&state)
        .await
        .map_err(|error| error.to_string())?;
    let devices = list_devices(&token)
        .await
        .map_err(|error| error.to_string())?;
    Ok(devices
        .into_iter()
        .filter(|device| !device.ssh_user.trim().is_empty())
        .map(|device| MachinePayload {
            id: device.id,
            name: device.name,
            kind: device.kind,
            last_seen_at: device.last_seen_at,
            ssh_user: device.ssh_user,
            ssh_port: device.ssh_port,
            peer_port: device.peer_port,
        })
        .collect())
}

#[tauri::command]
async fn connect_link_machine(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<AppState>>,
    target_id: String,
) -> Result<StatusPayload, String> {
    connect_link_inner(&app, state.inner().clone(), target_id, true)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn disconnect_session(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<StatusPayload, String> {
    state.session.fetch_add(1, Ordering::SeqCst);
    stop_tunnel(&state).await;
    stop_gateway(&state).await;
    *state.mode.lock().await = "disconnected".into();
    {
        let mut profile = state.profile.lock().await;
        profile.mode = "disconnected".into();
        profile.auto_connect = false;
        if let Err(error) = save_profile(&app, &profile) {
            tracing::warn!(%error, "could not save disconnected profile");
        }
    }
    Ok(StatusPayload {
        mode: "disconnected".into(),
        shell_url: None,
        link_signed_in: state.account_token.lock().await.is_some(),
    })
}

#[derive(Debug, Serialize)]
struct Diagnostics {
    app_version: String,
    platform: String,
    mode: String,
    shell_url: Option<String>,
    has_identity: bool,
    has_account_token: bool,
    hub_version: Option<String>,
    webview: Option<String>,
    log_file: Option<String>,
}

#[derive(Debug, Serialize)]
struct SavedHubPayload {
    id: String,
    name: String,
    last_connected_at: String,
}

#[tauri::command]
async fn saved_hubs(
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<Vec<SavedHubPayload>, String> {
    let profile = state.profile.lock().await;
    Ok(profile
        .hubs
        .iter()
        .map(|hub| SavedHubPayload {
            id: hub.id.clone(),
            name: hub.name.clone(),
            last_connected_at: hub.last_connected_at.clone(),
        })
        .collect())
}

#[derive(Debug, Serialize)]
struct UpdateInfo {
    version: String,
    notes: Option<String>,
    published: Option<String>,
}

#[derive(Clone, Serialize)]
struct UpdateProgress {
    downloaded: u64,
    total: Option<u64>,
}

#[tauri::command]
fn updater_configured() -> bool {
    option_env!("ORQELLA_DESKTOP_UPDATER").is_some()
}

#[tauri::command]
async fn check_updates(app: tauri::AppHandle) -> Result<Option<UpdateInfo>, String> {
    if option_env!("ORQELLA_DESKTOP_UPDATER").is_none() {
        return Err(
            "Release updates are not configured in this preview build. See docs/UPDATES.md.".into(),
        );
    }
    use tauri_plugin_updater::UpdaterExt;
    let updater = app.updater().map_err(|error| error.to_string())?;
    let update = updater.check().await.map_err(|error| error.to_string())?;
    Ok(update.map(|update| UpdateInfo {
        version: update.version,
        notes: update.body,
        published: update.date.map(|date| date.to_string()),
    }))
}

#[tauri::command]
async fn install_update(app: tauri::AppHandle) -> Result<Option<String>, String> {
    if option_env!("ORQELLA_DESKTOP_UPDATER").is_none() {
        return Err(
            "Release updates are not configured in this preview build. See docs/UPDATES.md.".into(),
        );
    }
    use tauri_plugin_updater::UpdaterExt;
    let updater = app.updater().map_err(|error| error.to_string())?;
    let Some(update) = updater.check().await.map_err(|error| error.to_string())? else {
        return Ok(None);
    };
    let version = update.version.clone();
    let progress_handle = app.clone();
    let mut downloaded = 0u64;
    update
        .download_and_install(
            move |chunk, total| {
                downloaded += chunk as u64;
                let _ = progress_handle.emit(
                    "orqella-update-progress",
                    UpdateProgress { downloaded, total },
                );
            },
            || {},
        )
        .await
        .map_err(|error| format!("{error:#}"))?;
    let _ = app.emit("orqella-update-installed", &version);
    app.restart();
}

async fn hub_version(state: &AppState) -> Option<String> {
    let base_url = state
        .gateway
        .lock()
        .await
        .as_ref()
        .map(|gateway| gateway.base_url.clone())?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .no_proxy()
        .build()
        .ok()?;
    let value = client
        .get(format!("{base_url}api/v1/info"))
        .send()
        .await
        .ok()?
        .json::<serde_json::Value>()
        .await
        .ok()?;
    value
        .get("version")
        .and_then(|version| version.as_str())
        .map(str::to_string)
}

async fn collect_diagnostics(state: &AppState, webview: Option<String>) -> Diagnostics {
    let shell_url = state
        .gateway
        .lock()
        .await
        .as_ref()
        .map(|gateway| gateway.base_url.clone());
    Diagnostics {
        app_version: env!("CARGO_PKG_VERSION").into(),
        platform: format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
        mode: state.mode.lock().await.clone(),
        shell_url,
        has_identity: state.identity.lock().await.is_some()
            || load_identity().ok().flatten().is_some(),
        has_account_token: state.account_token.lock().await.is_some()
            || load_account_token().ok().flatten().is_some(),
        hub_version: hub_version(state).await,
        webview,
        log_file: Some(log_dir().join(LOG_FILE_NAME).to_string_lossy().into_owned()),
    }
}

#[tauri::command]
async fn diagnostics(
    state: tauri::State<'_, Arc<AppState>>,
    webview: Option<String>,
) -> Result<Diagnostics, String> {
    Ok(collect_diagnostics(&state, webview).await)
}

#[tauri::command]
async fn export_diagnostics(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<AppState>>,
    webview: Option<String>,
) -> Result<String, String> {
    let diagnostics = collect_diagnostics(&state, webview).await;
    let logs = read_recent_logs(5000);
    let path = diagnostics_export_path(&app).map_err(|error| format!("{error:#}"))?;
    write_diagnostics_bundle(&path, &diagnostics, &logs).map_err(|error| format!("{error:#}"))?;
    tracing::info!(path = %path.display(), bytes = diagnostics_bundle(&diagnostics, &logs).len(), "diagnostics exported");
    Ok(path.to_string_lossy().into_owned())
}

#[tauri::command]
async fn sign_out(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<StatusPayload, String> {
    state.session.fetch_add(1, Ordering::SeqCst);
    stop_tunnel(&state).await;
    stop_gateway(&state).await;
    clear_main_browsing_data(&app);
    *state.account_token.lock().await = None;
    *state.challenge.lock().await = None;
    *state.local_challenge.lock().await = None;
    let mut keychain_error = None;
    if let Err(error) = delete_keyring_entry("account-token") {
        tracing::warn!(%error, "could not delete Link token from keychain");
        keychain_error = Some(error);
    }
    {
        let mut profile = state.profile.lock().await;
        profile.mode = "disconnected".into();
        profile.auto_connect = false;
        profile.last_target = None;
        profile.last_target_name = None;
        profile.hubs.clear();
        profile.shell_ports.clear();
        if let Err(error) = save_profile(&app, &profile) {
            tracing::warn!(%error, "could not save signed-out profile");
        }
    }
    if let Some(error) = keychain_error {
        return Err(format!(
            "Local state was cleared, but the OS keychain could not be updated: {error}"
        ));
    }
    Ok(StatusPayload {
        mode: "disconnected".into(),
        shell_url: None,
        link_signed_in: state.account_token.lock().await.is_some(),
    })
}

#[tauri::command]
async fn test_notification(app: tauri::AppHandle) -> Result<(), String> {
    use tauri_plugin_notification::NotificationExt;
    app.notification()
        .builder()
        .title("Orqella")
        .body("Desktop notifications are working.")
        .show()
        .map_err(|error| error.to_string())
}

fn ensure_session_current(state: &AppState, session: u64) -> Result<()> {
    if state.session.load(Ordering::SeqCst) != session {
        anyhow::bail!("connection attempt was cancelled");
    }
    Ok(())
}

async fn connect_local_inner(
    app: &tauri::AppHandle,
    state: Arc<AppState>,
    navigate: bool,
) -> Result<StatusPayload> {
    let session = state.session.load(Ordering::SeqCst);
    tracing::info!("connect_local_inner: stopping tunnel and starting local");
    stop_tunnel(&state).await;
    let local_hub = local_hub();
    let hub = probe_hub(&local_hub).await?;
    ensure_session_current(&state, session)?;
    start_gateway(app, &state, "local", "127.0.0.1", 9477, &hub).await?;
    if ensure_session_current(&state, session).is_err() {
        stop_gateway(&state).await;
        anyhow::bail!("connection attempt was cancelled");
    }
    *state.mode.lock().await = "local".into();
    {
        let mut profile = state.profile.lock().await;
        profile.mode = "local".into();
        profile.auto_connect = true;
        if let Err(error) = save_profile(app, &profile) {
            tracing::warn!(%error, "could not save local profile");
        }
    }
    if navigate {
        navigate_main(app, &state).await?;
    }
    begin_health_monitor(app.clone(), state.clone(), "local".into(), None);
    Ok(StatusPayload {
        mode: "local".into(),
        shell_url: state
            .gateway
            .lock()
            .await
            .as_ref()
            .map(|gateway| gateway.base_url.clone()),
        link_signed_in: state.account_token.lock().await.is_some(),
    })
}

async fn connect_link_inner(
    app: &tauri::AppHandle,
    state: Arc<AppState>,
    target_id: String,
    navigate: bool,
) -> Result<StatusPayload> {
    let session = state.session.load(Ordering::SeqCst);
    let token = account_token(&state).await?;
    let devices = list_devices(&token).await?;
    ensure_session_current(&state, session)?;
    let target = devices
        .iter()
        .find(|device| device.id == target_id)
        .cloned()
        .context("selected machine is no longer available")?;
    if target.ssh_host_key_fingerprint.trim().is_empty() {
        anyhow::bail!(
            "machine {} did not advertise an SSH host-key fingerprint; refusing to connect",
            target.name
        );
    }

    let identity = ensure_identity(&state).await?;
    ensure_session_current(&state, session)?;
    let client = register_client(
        &token,
        format!(
            "Orqella Desktop ({})",
            hostname::get()
                .ok()
                .and_then(|name| name.into_string().ok())
                .unwrap_or_else(|| "desktop".into())
        ),
        &identity,
    )
    .await?;

    stop_tunnel(&state).await;
    let tunnel = connect_remote(
        &token,
        &client.device_id,
        &target.id,
        target.peer_port.max(1),
        &identity,
        0,
    )
    .await?;
    let local_url = tunnel.local_url.clone();
    let port = url::Url::parse(&local_url)?
        .port()
        .context("Link tunnel has no local port")?;
    if let Err(error) = ensure_session_current(&state, session) {
        tunnel.stop().await;
        return Err(error);
    }

    let hub = match probe_hub(&local_url).await {
        Ok(hub) => hub,
        Err(error) => {
            tunnel.stop().await;
            return Err(error);
        }
    };
    if let Err(error) = ensure_session_current(&state, session) {
        tunnel.stop().await;
        return Err(error);
    }
    if let Err(error) = start_gateway(app, &state, &target.id, "127.0.0.1", port, &hub).await {
        tunnel.stop().await;
        return Err(error);
    }
    if ensure_session_current(&state, session).is_err() {
        stop_gateway(&state).await;
        tunnel.stop().await;
        anyhow::bail!("connection attempt was cancelled");
    }
    *state.tunnel.lock().await = Some(tunnel);
    tracing::info!("connect_link_inner: remote tunnel stored");
    *state.mode.lock().await = "remote".into();
    {
        let mut profile = state.profile.lock().await;
        profile.mode = "remote".into();
        profile.last_target = Some(target.id.clone());
        profile.last_target_name = Some(target.name.clone());
        profile.auto_connect = true;
        if let Some(hub) = profile.hubs.iter_mut().find(|hub| hub.id == target.id) {
            hub.name = target.name.clone();
            hub.last_connected_at = chrono_like_now();
        } else {
            profile.hubs.push(SavedHub {
                id: target.id.clone(),
                name: target.name.clone(),
                last_connected_at: chrono_like_now(),
            });
        }
        if let Err(error) = save_profile(app, &profile) {
            tracing::warn!(%error, "could not save remote profile");
        }
    }
    if navigate {
        navigate_main(app, &state).await?;
    }
    begin_health_monitor(
        app.clone(),
        state.clone(),
        "remote".into(),
        Some(target.id.clone()),
    );
    Ok(StatusPayload {
        mode: "remote".into(),
        shell_url: state
            .gateway
            .lock()
            .await
            .as_ref()
            .map(|gateway| gateway.base_url.clone()),
        link_signed_in: state.account_token.lock().await.is_some(),
    })
}

fn valid_dns_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && !value.starts_with('-')
        && !value.ends_with('-')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

fn valid_host_name(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 253
        && !host.starts_with('.')
        && !host.ends_with('.')
        && host.split('.').all(valid_dns_label)
}

fn canonical_host_allowed(host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    valid_host_name(&host) && (host == "orqella.localhost" || host.ends_with(".orqella.localhost"))
}

fn proxy_host_allowed(host: &str, canonical: &str) -> bool {
    let host = host.to_ascii_lowercase();
    let canonical = canonical.to_ascii_lowercase();
    valid_host_name(&host) && (host == canonical || host.ends_with(&format!(".{canonical}")))
}

fn stable_hash(value: &str) -> u64 {
    // FNV-1a gives a short, stable label without adding a dependency.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in value.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn stable_hub_label(profile_key: &str) -> String {
    format!("hub-{:016x}", stable_hash(profile_key))
}

fn profile_origin_namespace(profile_key: &str) -> u8 {
    if profile_key == "local" {
        0
    } else {
        (stable_hash(profile_key) % 254 + 1) as u8
    }
}

fn profile_browser_label(profile_key: &str) -> Option<String> {
    (profile_key != "local").then(|| stable_hub_label(profile_key))
}

fn app_origin_key(profile_key: &str, mount: &str, canonical_host: &str) -> String {
    format!(
        "{profile_key}|{mount}|{}",
        canonical_host.to_ascii_lowercase()
    )
}

async fn allocate_app_origins(
    app: &tauri::AppHandle,
    state: &Arc<AppState>,
    profile_key: &str,
    hub: &HubInfo,
    canonical: &str,
) -> Vec<(String, AppOriginConfig)> {
    let mut profile = state.profile.lock().await;
    let mut used = profile
        .app_origins
        .values()
        .map(|lease| lease.ip)
        .collect::<BTreeSet<_>>();
    let mut changed = false;
    let mut result = Vec::new();
    let browser_label = profile_browser_label(profile_key);

    for proxy in &hub.config.proxy {
        let Some(host) = proxy.host.as_ref() else {
            continue;
        };
        if !proxy_host_allowed(host, canonical) {
            tracing::warn!(host = %host, canonical = %canonical, "ignoring app host outside the hub namespace");
            continue;
        }
        let key = app_origin_key(profile_key, &proxy.id, host);
        let lease = if let Some(lease) = profile.app_origins.get(&key) {
            lease.clone()
        } else {
            let Some(ip) = (2u8..=254).find(|ip| !used.contains(ip)) else {
                tracing::warn!(mount = %proxy.id, "app origin pool is exhausted");
                continue;
            };
            let lease = AppOriginLease { ip, port: None };
            used.insert(ip);
            profile.app_origins.insert(key.clone(), lease.clone());
            changed = true;
            lease
        };
        result.push((
            key,
            AppOriginConfig {
                ip: IpAddr::from([127, 0, 0, lease.ip]),
                mount: proxy.id.clone(),
                node: "local".into(),
                canonical_host: host.clone(),
                browser_label: browser_label.clone(),
                preferred_port: lease.port,
            },
        ));
    }

    if changed && let Err(error) = save_profile(app, &profile) {
        tracing::warn!(%error, "could not save app origin leases");
    }
    result
}

async fn remember_app_origin_ports(
    app: &tauri::AppHandle,
    state: &Arc<AppState>,
    gateway: &Gateway,
    origins: &[(String, AppOriginConfig)],
) {
    let mut profile = state.profile.lock().await;
    let mut navigation_origins = BTreeSet::new();
    let mut changed = false;
    for (key, origin_config) in origins {
        let Some(origin) = gateway.app_origin(origin_config.ip).await else {
            continue;
        };
        let Ok(url) = url::Url::parse(&origin) else {
            continue;
        };
        navigation_origins.insert(url.origin().ascii_serialization());
        let Some(port) = url.port() else {
            continue;
        };
        if let Some(lease) = profile.app_origins.get_mut(key)
            && lease.port != Some(port)
        {
            lease.port = Some(port);
            changed = true;
        }
    }
    *state
        .navigation_origins
        .lock()
        .expect("navigation origin lock") = navigation_origins;
    if changed && let Err(error) = save_profile(app, &profile) {
        tracing::warn!(%error, "could not save app origin ports");
    }
}

async fn start_gateway(
    app: &tauri::AppHandle,
    state: &Arc<AppState>,
    profile_key: &str,
    upstream_host: &str,
    upstream_port: u16,
    hub: &HubInfo,
) -> Result<()> {
    stop_gateway(state).await;
    let canonical = hub
        .config
        .canonical_host
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "orqella.localhost".into());
    let canonical = if canonical_host_allowed(&canonical) {
        canonical
    } else {
        tracing::warn!(host = %canonical, "hub canonical host is outside the supported namespace");
        "orqella.localhost".to_string()
    };
    let app_origin_specs = allocate_app_origins(app, state, profile_key, hub, &canonical).await;
    let app_origins = app_origin_specs
        .iter()
        .map(|(_, config)| config.clone())
        .collect::<Vec<_>>();

    let preferred = {
        let profile = state.profile.lock().await;
        profile
            .shell_ports
            .get(profile_key)
            .copied()
            .unwrap_or_else(|| {
                if profile_key == "local" {
                    profile.shell_port
                } else {
                    profile.shell_port.saturating_add(1000)
                }
            })
            .max(1024)
    };
    let mut last_error = None;
    for offset in 0..20u16 {
        let Some(port) = preferred.checked_add(offset) else {
            break;
        };
        match Gateway::start(GatewayConfig {
            bind: IpAddr::from([127, 0, 0, 1]),
            port,
            shell_host: canonical.clone(),
            upstream: Upstream::http(canonical.clone(), upstream_host, upstream_port)
                .with_canonical_port(hub.config.port.unwrap_or(9477)),
            app_origins: app_origins.clone(),
            origin_namespace: profile_origin_namespace(profile_key),
        })
        .await
        {
            Ok(gateway) => {
                remember_app_origin_ports(app, state, &gateway, &app_origin_specs).await;
                let settings_app = app.clone();
                let settings_opener: SettingsOpener = Arc::new(move || {
                    let app = settings_app.clone();
                    let callback_app = app.clone();
                    if let Err(error) = app.run_on_main_thread(move || show_settings(&callback_app))
                    {
                        tracing::warn!(%error, "could not schedule settings page");
                    }
                });
                gateway.set_settings_opener(settings_opener);
                let notification_app = app.clone();
                gateway.set_notification_sender(Arc::new(move |title, body| {
                    use tauri_plugin_notification::NotificationExt;
                    if let Err(error) = notification_app
                        .notification()
                        .builder()
                        .title(title)
                        .body(body)
                        .show()
                    {
                        tracing::warn!(%error, "could not show native notification");
                    }
                }));
                let fullscreen_app = app.clone();
                let fullscreen_setter: FullscreenSetter = Arc::new(move |enabled| {
                    let app = fullscreen_app.clone();
                    let callback_app = app.clone();
                    if let Err(error) = app.run_on_main_thread(move || {
                        if let Some(window) = callback_app.get_webview_window("main")
                            && let Err(error) = window.set_fullscreen(enabled)
                        {
                            tracing::warn!(%error, "could not set native fullscreen");
                        }
                    }) {
                        tracing::warn!(%error, "could not schedule native fullscreen");
                    }
                });
                gateway.set_fullscreen_setter(fullscreen_setter);
                *state.gateway_origin.lock().expect("gateway origin lock") =
                    Some(gateway.base_url.trim_end_matches('/').to_string());
                *state.gateway.lock().await = Some(gateway);
                if port != preferred {
                    tracing::warn!(preferred, port, "preferred shell port was unavailable");
                }
                let mut profile = state.profile.lock().await;
                profile.shell_ports.insert(profile_key.to_string(), port);
                if profile_key == "local" {
                    profile.shell_port = port;
                }
                if let Err(error) = save_profile(app, &profile) {
                    tracing::warn!(%error, "could not save shell port");
                }
                return Ok(());
            }
            Err(error) => {
                last_error = Some(error);
            }
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("no loopback shell port available")))
}

fn clear_main_browsing_data(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main")
        && let Err(error) = window.clear_all_browsing_data()
    {
        tracing::warn!(%error, "could not clear main webview browsing data");
    }
}

fn local_bootstrap_token() -> Option<String> {
    let data_dir = if let Some(home) = std::env::var_os("ORQELLA_HOME") {
        PathBuf::from(home).join("data")
    } else {
        #[cfg(target_os = "windows")]
        {
            std::env::var_os("LOCALAPPDATA")
                .map(PathBuf::from)?
                .join("orqella")
        }
        #[cfg(target_os = "macos")]
        {
            std::env::var_os("HOME")
                .map(PathBuf::from)?
                .join("Library")
                .join("Application Support")
                .join("orqella")
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            std::env::var_os("XDG_DATA_HOME")
                .map(PathBuf::from)
                .or_else(|| {
                    std::env::var_os("HOME")
                        .map(PathBuf::from)
                        .map(|home| home.join(".local").join("share"))
                })?
                .join("orqella")
        }
    };
    let token = std::fs::read_to_string(data_dir.join("bootstrap.token")).ok()?;
    let token = token.trim();
    (!token.is_empty()).then(|| token.to_string())
}

async fn navigate_main(app: &tauri::AppHandle, state: &Arc<AppState>) -> Result<()> {
    let mut url = state
        .gateway
        .lock()
        .await
        .as_ref()
        .map(|gateway| gateway.base_url.clone())
        .context("gateway is not running")?;
    let local = state.mode.lock().await.as_str() == "local";
    if local && let Some(token) = local_bootstrap_token() {
        url.push_str("#orqella_bootstrap=");
        url.push_str(&token);
    }
    let window = app
        .get_webview_window("main")
        .context("main webview window is missing")?;
    state.settings_open.store(false, Ordering::SeqCst);
    *state
        .settings_return_url
        .lock()
        .expect("settings return lock") = None;
    window.navigate(url.parse()?)?;
    Ok(())
}

async fn stop_gateway(state: &Arc<AppState>) {
    *state.gateway_origin.lock().expect("gateway origin lock") = None;
    state
        .navigation_origins
        .lock()
        .expect("navigation origin lock")
        .clear();
    let gateway = state.gateway.lock().await.take();
    if let Some(gateway) = gateway {
        gateway.stop().await;
    }
}

async fn stop_tunnel(state: &Arc<AppState>) {
    let tunnel = state.tunnel.lock().await.take();
    if let Some(tunnel) = tunnel {
        tunnel.stop().await;
    }
}

async fn account_token(state: &Arc<AppState>) -> Result<String> {
    if let Some(token) = state.account_token.lock().await.clone() {
        return Ok(token);
    }
    if let Some(token) = load_account_token()? {
        *state.account_token.lock().await = Some(token.clone());
        return Ok(token);
    }
    anyhow::bail!("sign in to Link first")
}

async fn ensure_identity(state: &Arc<AppState>) -> Result<ClientIdentity> {
    if let Some(identity) = state.identity.lock().await.clone() {
        return Ok(identity);
    }
    let identity = ClientIdentity::generate()?;
    save_identity(&identity)?;
    *state.identity.lock().await = Some(identity.clone());
    Ok(identity)
}

fn interface_signature() -> Vec<String> {
    let mut addresses = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .map(|interface| format!("{}:{}", interface.name, interface.addr.ip()))
        .collect::<Vec<_>>();
    addresses.sort();
    addresses
}

fn resumed_or_network_changed(elapsed: Duration, network_changed: bool) -> bool {
    network_changed || elapsed > Duration::from_secs(30)
}

fn begin_health_monitor(
    app: tauri::AppHandle,
    state: Arc<AppState>,
    mode: String,
    target: Option<String>,
) {
    let session = state.session.fetch_add(1, Ordering::SeqCst) + 1;
    tokio::spawn(async move {
        let mut failures = 0u8;
        let mut interfaces = interface_signature();
        let mut last_tick = Instant::now();
        loop {
            tokio::time::sleep(Duration::from_secs(10)).await;
            let elapsed = last_tick.elapsed();
            last_tick = Instant::now();
            let current_interfaces = interface_signature();
            let network_changed = current_interfaces != interfaces;
            if network_changed {
                tracing::info!(
                    previous = interfaces.len(),
                    current = current_interfaces.len(),
                    "network interfaces changed; probing hub"
                );
                interfaces = current_interfaces;
            }
            if resumed_or_network_changed(elapsed, network_changed) {
                if elapsed > Duration::from_secs(30) {
                    tracing::info!(
                        elapsed_secs = elapsed.as_secs(),
                        "detected resume from sleep; probing hub"
                    );
                }
                // Probe immediately after a wake or address change instead of
                // waiting for three missed health checks.
                failures = 2;
            }
            if state.session.load(Ordering::SeqCst) != session {
                return;
            }
            if *state.mode.lock().await != mode {
                return;
            }
            let Some(base_url) = state
                .gateway
                .lock()
                .await
                .as_ref()
                .map(|gateway| gateway.base_url.clone())
            else {
                return;
            };
            let healthy = loopback_http_client()
                .get(format!("{base_url}healthz"))
                .timeout(Duration::from_secs(4))
                .send()
                .await
                .is_ok_and(|response| response.status().is_success());
            if healthy {
                failures = 0;
                continue;
            }
            failures += 1;
            if failures < 3 {
                continue;
            }
            *state.mode.lock().await = "reconnecting".into();
            let reconnect_navigate = !state.settings_open.load(Ordering::SeqCst);
            for attempt in 0..3u32 {
                let result = match mode.as_str() {
                    "remote" => {
                        if let Some(target) = target.clone() {
                            connect_link_inner(&app, state.clone(), target, reconnect_navigate)
                                .await
                        } else {
                            connect_local_inner(&app, state.clone(), reconnect_navigate).await
                        }
                    }
                    _ => connect_local_inner(&app, state.clone(), reconnect_navigate).await,
                };
                if result.is_ok() {
                    return;
                }
                if let Err(error) = result {
                    tracing::warn!(
                        attempt,
                        error = %format!("{error:#}"),
                        "Orqella reconnect attempt failed"
                    );
                }
                tokio::time::sleep(Duration::from_secs(2u64 << attempt.min(3))).await;
                if state.session.load(Ordering::SeqCst) != session {
                    return;
                }
            }
            *state.mode.lock().await = "disconnected".into();
            return;
        }
    });
}

async fn start_local_session(
    handle: tauri::AppHandle,
    state: Arc<AppState>,
    navigate: bool,
) -> Result<()> {
    tracing::info!("start_local_session: starting local fallback");
    let local_hub = local_hub();
    let hub = match probe_hub(&local_hub).await {
        Ok(hub) => hub,
        Err(error) => {
            *state.mode.lock().await = "disconnected".into();
            if navigate {
                tracing::info!(%error, "no local daemon found; showing onboarding");
                show_settings(&handle);
            }
            return Ok(());
        }
    };
    start_gateway(&handle, &state, "local", "127.0.0.1", 9477, &hub).await?;
    *state.mode.lock().await = "local".into();
    {
        let mut profile = state.profile.lock().await;
        if profile.mode == "remote" {
            // Keep the saved remote preference for the next explicit Link
            // connection, but the current session is local.
            profile.auto_connect = true;
        } else {
            profile.mode = "local".into();
            profile.auto_connect = true;
            if let Err(error) = save_profile(&handle, &profile) {
                tracing::warn!(%error, "could not save local profile");
            }
        }
    }
    if navigate {
        navigate_main(&handle, &state).await?;
    }
    begin_health_monitor(handle.clone(), state.clone(), "local".into(), None);
    Ok(())
}

#[derive(Debug, Clone, Deserialize)]
struct HubInfo {
    api_version: String,
    #[serde(default)]
    config: HubConfig,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct HubConfig {
    #[serde(default)]
    canonical_host: Option<String>,
    #[serde(default)]
    port: Option<u16>,
    #[serde(default)]
    proxy: Vec<HubProxy>,
}

#[derive(Debug, Clone, Deserialize)]
struct HubProxy {
    id: String,
    #[serde(default)]
    host: Option<String>,
}

async fn probe_hub(base: &str) -> Result<HubInfo> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(4))
        .no_proxy()
        .build()?;
    let base = base.trim_end_matches('/');
    let info_url = format!("{base}/api/v1/info");

    // The daemon requires a browser session for /api. A remote Link tunnel
    // does not expose the remote admin token, so bootstrap the same way the
    // webview does: a document navigation to / mints a session cookie.
    let mut response = client.get(&info_url).send().await?;
    if response.status() == reqwest::StatusCode::UNAUTHORIZED
        && let Some(cookie) = mint_session_cookie(&client, base).await?
    {
        response = client
            .get(&info_url)
            .header(reqwest::header::COOKIE, cookie)
            .send()
            .await?;
    }
    if !response.status().is_success() {
        anyhow::bail!("hub returned {}", response.status());
    }
    let body: HubInfo = response.json().await?;
    if body.api_version != "1" {
        anyhow::bail!("unsupported hub API version {}", body.api_version);
    }
    Ok(body)
}

/// Ask the daemon for a browser session by requesting the shell document.
/// The request deliberately matches the daemon's document-navigation
/// heuristic: no Origin or X-Orqella header, a browser user agent, and an
/// HTML accept header.
async fn mint_session_cookie(client: &reqwest::Client, base: &str) -> Result<Option<String>> {
    let response = client
        .get(format!("{base}/"))
        .header(reqwest::header::ACCEPT, "text/html")
        .header(reqwest::header::USER_AGENT, "Mozilla/5.0 (Orqella Desktop)")
        .send()
        .await?;
    if !response.status().is_success() {
        return Ok(None);
    }
    Ok(session_cookie(response.headers()))
}

fn session_cookie(headers: &reqwest::header::HeaderMap) -> Option<String> {
    headers
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .filter_map(|value| value.split(';').next())
        .find_map(|pair| {
            pair.trim()
                .strip_prefix("orqella_session=")
                .filter(|token| !token.is_empty())
                .map(|token| format!("orqella_session={token}"))
        })
}

fn safe_external_url(value: &str) -> Option<String> {
    let url = url::Url::parse(value).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    if !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    Some(url.to_string())
}

fn open_external(url: &str) {
    let Some(url) = safe_external_url(url) else {
        tracing::warn!("refusing to open a non-http(s) external URL");
        return;
    };

    #[cfg(target_os = "macos")]
    let mut command = std::process::Command::new("open");
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    let mut command = std::process::Command::new("xdg-open");
    #[cfg(not(target_os = "windows"))]
    command.arg(&url);

    // Windows: never route through cmd.exe. `cmd /C start` re-parses the URL
    // and treats `&`, `|`, and `^` as command separators.
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::UI::Shell::ShellExecuteW;
        use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

        let wide = |value: &str| {
            std::ffi::OsStr::new(value)
                .encode_wide()
                .chain(std::iter::once(0))
                .collect::<Vec<_>>()
        };
        let operation = wide("open");
        let target = wide(&url);
        unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                operation.as_ptr(),
                target.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            );
        }
        return;
    }

    #[cfg(not(target_os = "windows"))]
    let _ = command
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

fn handle_new_window<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    target_label: &str,
    url: &url::Url,
) -> tauri::webview::NewWindowResponse<R> {
    match new_window_action(state, url) {
        NewWindowAction::Navigate => {
            if let Some(window) = app.get_webview_window(target_label)
                && let Err(error) = window.navigate(url.clone())
            {
                tracing::warn!(%error, "could not navigate new-window request");
            }
        }
        NewWindowAction::External => open_external(url.as_str()),
        NewWindowAction::Ignore => {}
    }
    tauri::webview::NewWindowResponse::Deny
}

fn keyring_entry(name: &str) -> Result<keyring::Entry> {
    keyring::Entry::new(KEYCHAIN_SERVICE, name).context("open OS keychain")
}

fn delete_keyring_entry(name: &str) -> Result<()> {
    match keyring_entry(name)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(anyhow::anyhow!(error)),
    }
}

fn load_identity() -> Result<Option<ClientIdentity>> {
    let entry = keyring_entry("identity")?;
    match entry.get_password() {
        Ok(value) => Ok(Some(
            serde_json::from_str(&value).context("decode stored identity")?,
        )),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(error).context("read identity from keychain"),
    }
}

fn save_identity(identity: &ClientIdentity) -> Result<()> {
    let value = serde_json::to_string(identity)?;
    keyring_entry("identity")?
        .set_password(&value)
        .context("store identity in keychain")
}

fn load_account_token() -> Result<Option<String>> {
    let entry = keyring_entry("account-token")?;
    match entry.get_password() {
        Ok(value) => Ok(Some(value)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(error).context("read Link token from keychain"),
    }
}

fn save_account_token(token: &str) -> Result<()> {
    keyring_entry("account-token")?
        .set_password(token)
        .context("store Link token in keychain")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_pages_use_the_bundled_app_base() {
        let state = AppState::new();
        let url = local_page_url(&state, "settings.html").expect("local settings URL");
        assert_eq!(url.path(), "/settings.html");
        #[cfg(windows)]
        assert_eq!(url.scheme(), "http");
        #[cfg(not(windows))]
        assert_eq!(url.scheme(), "tauri");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_webkit_transport_respects_overrides_and_fallbacks() {
        use LinuxWebKitTransport::{DmaBufWithoutExplicitSync, ForceSharedMemory, Unchanged};
        assert_eq!(
            linux_webkit_transport(false, false, true, true, false, false, false),
            Unchanged
        );
        assert_eq!(
            linux_webkit_transport(true, false, true, true, true, false, false),
            Unchanged
        );
        assert_eq!(
            linux_webkit_transport(true, false, true, true, false, true, false),
            Unchanged
        );
        assert_eq!(
            linux_webkit_transport(true, false, true, true, false, false, true),
            Unchanged
        );
        assert_eq!(
            linux_webkit_transport(true, true, true, true, false, false, false),
            ForceSharedMemory
        );
        assert_eq!(
            linux_webkit_transport(true, false, true, false, false, false, false),
            ForceSharedMemory
        );
        assert_eq!(
            linux_webkit_transport(true, false, false, true, false, false, false),
            ForceSharedMemory
        );
        assert_eq!(
            linux_webkit_transport(true, false, true, true, false, false, false),
            DmaBufWithoutExplicitSync
        );
    }

    #[test]
    fn shell_marker_script_marks_desktop_and_resolves_motion() {
        let reduced = shell_marker_script(true);
        assert!(reduced.contains("root.dataset.orqellaShell = 'desktop'"));
        assert!(reduced.contains("const reduceMotion = true;"));
        assert!(!reduced.contains("__ORQELLA_REDUCE_MOTION__"));

        let full = shell_marker_script(false);
        assert!(full.contains("const reduceMotion = false;"));
        assert!(!full.contains("__ORQELLA_REDUCE_MOTION__"));
    }

    #[test]
    fn downloads_use_safe_unique_names() {
        let url = url::Url::parse("http://127.0.0.1:19477/files/report%20one.txt?x=1").unwrap();
        assert_eq!(download_filename(&url), "report one.txt");
        let reserved = url::Url::parse("http://127.0.0.1:19477/files/CON").unwrap();
        assert_eq!(download_filename(&reserved), "_CON");
        let dir =
            std::env::temp_dir().join(format!("orqella-download-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = unique_download_path(&dir, "report.txt");
        std::fs::write(&first, b"one").unwrap();
        let second = unique_download_path(&dir, "report.txt");
        assert_ne!(first, second);
        assert_eq!(second.file_name().unwrap(), "report-1.txt");
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn detects_resume_and_network_changes() {
        assert!(!resumed_or_network_changed(Duration::from_secs(10), false));
        assert!(resumed_or_network_changed(Duration::from_secs(31), false));
        assert!(resumed_or_network_changed(Duration::from_secs(1), true));
        let signature = interface_signature();
        assert!(
            signature.windows(2).all(|pair| pair[0] <= pair[1]),
            "interface signature must be sorted: {signature:?}"
        );
    }

    #[test]
    fn redacts_credentials_in_logs() {
        let cases = [
            (
                "Authorization: Bearer orq_secret_value",
                "Authorization: [REDACTED]",
            ),
            ("token=abc123&next=1", "token=[REDACTED]&next=1"),
            ("password: hunter2", "password: [REDACTED]"),
            (
                r#"{"access_token":"abc.def","safe":true}"#,
                r#"{"access_token":"[REDACTED]","safe":true}"#,
            ),
            ("Set-Cookie: session=abc; Path=/", "Set-Cookie: [REDACTED]"),
            ("ws_token=term_secret&x=1", "ws_token=[REDACTED]&x=1"),
            ("device_code=dev_secret", "device_code=[REDACTED]"),
        ];
        for (input, expected) in cases {
            let redacted = redact_text(input);
            assert_eq!(redacted, expected, "input: {input}");
        }
        let private_key = "-----BEGIN OPENSSH PRIVATE KEY-----\nsecret-key-material\n-----END OPENSSH PRIVATE KEY-----";
        let redacted = redact_text(private_key);
        assert_eq!(redacted, "[REDACTED PRIVATE KEY]");
        assert!(!redacted.contains("secret-key-material"));
    }

    #[test]
    fn diagnostics_bundle_contains_redacted_logs() {
        let diagnostics = Diagnostics {
            app_version: "0.1.0".into(),
            platform: "linux x86_64".into(),
            mode: "local".into(),
            shell_url: Some("http://127.0.0.1:19477/".into()),
            has_identity: true,
            has_account_token: false,
            hub_version: Some("0.1.0".into()),
            webview: Some("WebKitGTK".into()),
            log_file: Some("/tmp/orqella.log".into()),
        };
        let bundle = diagnostics_bundle(
            &diagnostics,
            "connected with token=super-secret\nAuthorization: Bearer abc.def\nok",
        );
        assert!(bundle.contains("\"app_version\": \"0.1.0\""));
        assert!(bundle.contains("token=[REDACTED]"));
        assert!(bundle.contains("Authorization: [REDACTED]"));
        assert!(!bundle.contains("super-secret"));
        assert!(!bundle.contains("abc.def"));
        assert!(bundle.contains("\nok"));
    }

    #[test]
    fn writes_diagnostics_bundle_to_disk() {
        let directory =
            std::env::temp_dir().join(format!("orqella-diagnostics-test-{}", std::process::id()));
        let path = directory.join("bundle.txt");
        let diagnostics = Diagnostics {
            app_version: "0.1.0".into(),
            platform: "linux x86_64".into(),
            mode: "local".into(),
            shell_url: None,
            has_identity: false,
            has_account_token: false,
            hub_version: None,
            webview: None,
            log_file: None,
        };
        write_diagnostics_bundle(&path, &diagnostics, "secret=one").unwrap();
        let value = std::fs::read_to_string(&path).unwrap();
        assert!(value.contains("Orqella desktop diagnostics"));
        assert!(!value.contains("one"));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn redacting_writer_flushes_partial_lines() {
        let mut output = Vec::new();
        {
            let mut writer = RedactingWriter::new(&mut output);
            writer.write_all(b"token=abc").unwrap();
            writer.flush().unwrap();
        }
        assert_eq!(String::from_utf8(output).unwrap(), "token=[REDACTED]\n");
    }

    #[test]
    fn redacting_writer_hides_multiline_private_keys() {
        let mut output = Vec::new();
        {
            let mut writer = RedactingWriter::new(&mut output);
            writer
                .write_all(b"before\n-----BEGIN OPENSSH PRIVATE KEY-----\nsecret-material\n-----END OPENSSH PRIVATE KEY-----\nafter\n")
                .unwrap();
            writer.flush().unwrap();
        }
        let value = String::from_utf8(output).unwrap();
        assert!(value.contains("before"));
        assert!(value.contains("after"));
        assert!(value.contains("[REDACTED PRIVATE KEY]"));
        assert!(!value.contains("secret-material"));
    }

    #[test]
    fn safe_external_urls_are_http_or_https_only() {
        assert_eq!(
            safe_external_url("https://example.com/a?b=1&c=2").as_deref(),
            Some("https://example.com/a?b=1&c=2")
        );
        assert!(safe_external_url("javascript:alert(1)").is_none());
        assert!(safe_external_url("file:///tmp/x").is_none());
        assert!(safe_external_url("https://user:pass@example.com/").is_none());
    }

    #[test]
    fn app_origin_hosts_are_namespaced_and_stable() {
        assert!(canonical_host_allowed("orqella.localhost"));
        assert!(canonical_host_allowed("opencode.orqella.localhost"));
        assert!(!canonical_host_allowed("opencode.evil.example"));
        assert!(proxy_host_allowed(
            "opencode.orqella.localhost",
            "orqella.localhost"
        ));
        assert!(!proxy_host_allowed("evil.example", "orqella.localhost"));
        assert_eq!(stable_hub_label("target-a"), stable_hub_label("target-a"));
        assert_ne!(stable_hub_label("target-a"), stable_hub_label("target-b"));
        assert_eq!(
            profile_origin_namespace("target-a"),
            profile_origin_namespace("target-a")
        );
        assert_ne!(profile_origin_namespace("target-a"), 0);
        assert_eq!(profile_origin_namespace("local"), 0);
        assert!(valid_dns_label("opencode-1"));
        assert!(!valid_dns_label("bad_host"));
    }

    #[cfg(unix)]
    #[test]
    fn private_files_and_directories_use_restricted_modes() {
        use std::os::unix::fs::PermissionsExt;
        let root =
            std::env::temp_dir().join(format!("orqella-private-mode-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        create_private_dir(&root).unwrap();
        let file = root.join("profile.json");
        write_private_file(&file, b"{}").unwrap();
        assert_eq!(
            std::fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn host_names_are_validated_without_accepting_empty_labels() {
        assert!(valid_host_name("orqella.localhost"));
        assert!(valid_host_name("opencode.node-1.orqella.localhost"));
        assert!(!valid_host_name(""));
        assert!(!valid_host_name(".orqella.localhost"));
        assert!(!valid_host_name("orqella.localhost."));
        assert!(!valid_host_name("orqella..localhost"));
        assert!(!valid_host_name("bad_host.localhost"));
        assert!(!valid_host_name(&format!("{}.localhost", "a".repeat(64))));
        assert!(proxy_host_allowed(
            "opencode.node-1.orqella.localhost",
            "orqella.localhost"
        ));
        assert!(!proxy_host_allowed(
            "opencode.evil.example",
            "orqella.localhost"
        ));
    }

    #[test]
    fn profile_namespaces_and_labels_are_stable_and_distinct() {
        assert_eq!(profile_origin_namespace("local"), 0);
        assert_eq!(
            profile_origin_namespace("target-a"),
            profile_origin_namespace("target-a")
        );
        assert_ne!(profile_origin_namespace("target-a"), 0);
        assert_eq!(profile_browser_label("local"), None);
        assert_eq!(
            profile_browser_label("target-a"),
            Some(stable_hub_label("target-a"))
        );
        assert!(
            profile_browser_label("target-a")
                .unwrap()
                .starts_with("hub-")
        );
        assert_eq!(
            app_origin_key("target-a", "opencode", "OpenCode.Orqella.Localhost"),
            "target-a|opencode|opencode.orqella.localhost"
        );
    }

    #[test]
    fn finds_the_session_cookie_among_other_set_cookie_headers() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.append(
            reqwest::header::SET_COOKIE,
            reqwest::header::HeaderValue::from_static("theme=dark; Path=/"),
        );
        headers.append(
            reqwest::header::SET_COOKIE,
            reqwest::header::HeaderValue::from_static(
                "orqella_session=abc123; Path=/; HttpOnly; SameSite=Strict",
            ),
        );
        assert_eq!(
            session_cookie(&headers).as_deref(),
            Some("orqella_session=abc123")
        );
        assert_eq!(session_cookie(&reqwest::header::HeaderMap::new()), None);
    }

    #[test]
    fn desktop_csp_does_not_allow_eval() {
        let config = include_str!("../tauri.conf.json");
        assert!(!config.contains("unsafe-eval"));
        assert!(config.contains("connect-src ipc: http://ipc.localhost"));
    }

    #[test]
    fn zoom_steps_stop_at_browser_limits_and_reset() {
        assert_eq!(DEFAULT_ZOOM_INDEX, 7);
        assert_eq!(ZOOM_LEVELS[DEFAULT_ZOOM_INDEX], 1.0);
        assert_eq!(zoom_index_after(DEFAULT_ZOOM_INDEX, 1), 8);
        assert_eq!(zoom_index_after(DEFAULT_ZOOM_INDEX, -1), 6);
        assert_eq!(zoom_index_after(0, -3), 0);
        assert_eq!(
            zoom_index_after(ZOOM_LEVELS.len() - 1, 4),
            ZOOM_LEVELS.len() - 1
        );
        assert_eq!(zoom_index_after(3, 0), DEFAULT_ZOOM_INDEX);
        assert_eq!(zoom_index_after(0, i32::MIN), 0);
    }

    #[test]
    fn zoom_script_is_ready_for_every_frame() {
        let script = zoom_shortcut_script();
        assert!(!script.contains("__ORQELLA_IS_MACOS__"));
        assert!(script.contains("orqella:zoom"));
        assert!(script.contains("zoom_page"));
        assert!(script.contains("addEventListener('wheel'"));
        assert!(script.contains("addEventListener('keydown'"));
    }

    #[test]
    fn remote_zoom_capability_only_reaches_loopback_origins() {
        let capability: serde_json::Value =
            serde_json::from_str(include_str!("../capabilities/main-remote-zoom.json"))
                .expect("parse remote zoom capability");
        assert_eq!(capability["local"], serde_json::json!(false));
        assert_eq!(
            capability["permissions"],
            serde_json::json!(["allow-zoom-page"])
        );
        let patterns = capability["remote"]["urls"]
            .as_array()
            .expect("remote urls")
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .expect("url pattern")
                    .parse::<tauri::utils::acl::RemoteUrlPattern>()
                    .expect("valid url pattern")
            })
            .collect::<Vec<_>>();
        let covered = |value: &str| {
            let url = url::Url::parse(value).expect("url");
            patterns.iter().any(|pattern| pattern.test(&url))
        };
        for url in [
            "http://127.0.0.1:19477/",
            "http://localhost:19477/",
            "http://orqella.localhost:19477/",
            "http://opencode.orqella.localhost:8810/",
            "http://orqella.local:19477/",
            "http://opencode.hub-3f.orqella.local:54421/",
        ] {
            assert!(covered(url), "{url} must be covered by the zoom capability");
        }
        for url in [
            "https://127.0.0.1:19477/",
            "http://198.51.100.7:8810/",
            "http://example.com/",
            "http://127.0.0.1.example.com:19477/",
        ] {
            assert!(!covered(url), "{url} must not reach the zoom command");
        }
    }

    #[test]
    fn navigation_allows_only_the_gateway_and_app_origins() {
        let state = AppState::new();
        *state.gateway_origin.lock().unwrap() = Some("http://127.0.0.1:19477".to_string());
        let allowed =
            |value: &str| desktop_navigation_allowed(&state, &url::Url::parse(value).unwrap());
        assert!(allowed("http://127.0.0.1:19477/"));
        assert!(!allowed("http://127.0.0.1:9999/"));
        assert!(!allowed("http://127.0.0.2:8810/"));
        state
            .navigation_origins
            .lock()
            .unwrap()
            .insert("http://127.0.0.2:8810".to_string());
        state
            .navigation_origins
            .lock()
            .unwrap()
            .insert("http://home.orqella.localhost:8810".to_string());
        assert!(allowed("http://127.0.0.2:8810/"));
        assert!(allowed("http://home.orqella.localhost:8810/"));
        assert!(!allowed("https://example.com/"));
    }

    #[test]
    fn new_window_actions_route_links_without_opening_webviews() {
        let state = AppState::new();
        *state.gateway_origin.lock().unwrap() = Some("http://127.0.0.1:19477".to_string());
        let action = |value: &str| new_window_action(&state, &url::Url::parse(value).unwrap());
        assert_eq!(
            action("http://127.0.0.1:19477/settings"),
            NewWindowAction::Navigate
        );
        assert_eq!(
            action("https://github.com/orqella/orqella"),
            NewWindowAction::External
        );
        assert_eq!(action("http://example.com/"), NewWindowAction::External);
        assert_eq!(action("about:blank"), NewWindowAction::Ignore);
        assert_eq!(
            action("tauri://localhost/index.html"),
            NewWindowAction::Ignore
        );
        assert_eq!(action("file:///tmp/x"), NewWindowAction::Ignore);
        assert_eq!(action("mailto:dev@example.com"), NewWindowAction::Ignore);
    }

    #[test]
    fn permission_origins_match_exactly() {
        let origin = "http://127.0.0.1:19477";
        assert!(same_http_origin(
            &url::Url::parse("http://127.0.0.1:19477/app").unwrap(),
            origin,
        ));
        assert!(!same_http_origin(
            &url::Url::parse("http://127.0.0.1:8810/").unwrap(),
            origin,
        ));
        assert!(!same_http_origin(
            &url::Url::parse("http://127.0.0.2:19477/").unwrap(),
            origin,
        ));
    }

    #[test]
    fn download_filenames_handle_reserved_names_and_missing_segments() {
        let root = url::Url::parse("http://127.0.0.1:19477/").unwrap();
        assert_eq!(download_filename(&root), "download");
        let dotted = url::Url::parse("http://127.0.0.1:19477/file...").unwrap();
        assert_eq!(download_filename(&dotted), "file");
        let reserved = url::Url::parse("http://127.0.0.1:19477/LPT9.txt").unwrap();
        assert_eq!(download_filename(&reserved), "_LPT9.txt");
        let unicode = url::Url::parse("http://127.0.0.1:19477/r%C3%A9sum%C3%A9.pdf").unwrap();
        assert_eq!(download_filename(&unicode), "résumé.pdf");
        let slash = url::Url::parse("http://127.0.0.1:19477/one%2Ftwo.txt").unwrap();
        assert_eq!(download_filename(&slash), "one_two.txt");
    }
}
