use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::TrayIconBuilder,
    AppHandle, Emitter, Manager, State, WebviewUrl,
    WebviewWindow, WebviewWindowBuilder,
};

#[derive(Debug, Serialize, Deserialize, Clone)]
struct UrlItem {
    url: String,
    name: Option<String>,
    enabled: Option<bool>,
    interval: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
struct MonitorSettings {
    urls: Vec<UrlItem>,
    interval: Option<u64>,
    theme: Option<String>,
    layout: Option<String>,
}

// content window 정보
#[derive(Debug, Clone)]
struct ContentWindow {
    label: String,
    url: String,
}

struct AppState {
    settings: Mutex<HashMap<String, MonitorSettings>>,
    settings_path: PathBuf,
    content_windows: Mutex<HashMap<String, Vec<ContentWindow>>>,
    generation: Mutex<u32>,
}

impl AppState {
    fn new() -> Self {
        let path = data_path();
        let settings = load_settings_from_disk(&path);
        AppState {
            settings: Mutex::new(settings),
            settings_path: path,
            content_windows: Mutex::new(HashMap::new()),
            generation: Mutex::new(0),
        }
    }

    fn save(&self) {
        let settings = self.settings.lock().unwrap();
        let json = serde_json::to_string_pretty(&*settings).unwrap_or_default();
        let _ = fs::write(&self.settings_path, json);
    }
}

fn data_path() -> PathBuf {
    let mut p = dirs::data_local_dir().unwrap_or_else(|| PathBuf::from("."));
    p.push("sentinel");
    let _ = fs::create_dir_all(&p);
    p.push("settings.json");
    p
}

fn load_settings_from_disk(path: &PathBuf) -> HashMap<String, MonitorSettings> {
    fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

// ── IPC Commands ──

#[tauri::command]
fn load_settings(monitor_id: String, state: State<AppState>) -> Option<MonitorSettings> {
    state.settings.lock().unwrap().get(&monitor_id).cloned()
}

#[tauri::command]
fn save_settings(monitor_id: String, settings: MonitorSettings, state: State<AppState>) {
    state.settings.lock().unwrap().insert(monitor_id, settings);
    state.save();
}

#[tauri::command]
fn get_layout_mode(state: State<AppState>) -> serde_json::Value {
    let settings = state.settings.lock().unwrap();
    let mode = settings
        .get("layout")
        .and_then(|s| s.layout.clone())
        .unwrap_or_else(|| "single".to_string());
    let display_count = 1usize; // simplified
    serde_json::json!({ "mode": mode, "displayCount": display_count })
}

#[tauri::command]
fn set_layout_mode(mode: String, state: State<AppState>, app: AppHandle) {
    {
        let mut map = state.settings.lock().unwrap();
        let entry = map.entry("layout".to_string()).or_default();
        entry.layout = Some(mode.clone());
    }
    state.save();
    launch_windows(&app, &mode);
}

// ── Content window management ──

#[derive(Debug, Serialize, Deserialize, Clone)]
struct UrlEntry {
    url: String,
    enabled: bool,
}

#[tauri::command]
fn sync_content_windows(
    monitor_id: String,
    urls: Vec<UrlEntry>,
    content_x: f64,
    content_y: f64,
    content_w: f64,
    content_h: f64,
    app: AppHandle,
    state: State<AppState>,
) -> Result<(), String> {
    // 기존 content windows 닫기
    let existing: Vec<ContentWindow> = state
        .content_windows
        .lock()
        .unwrap()
        .remove(&monitor_id)
        .unwrap_or_default();

    for win in &existing {
        if let Some(w) = app.get_webview_window(&win.label) {
            let _ = w.close();
        }
    }

    // 새 generation 번호로 라벨 충돌 방지
    let gen = {
        let mut g = state.generation.lock().unwrap();
        *g += 1;
        *g
    };

    // 새 content windows 동기 생성 (스레드 없음 — 순서 보장)
    let mut new_windows = Vec::new();
    for (i, entry) in urls.iter().enumerate() {
        if !entry.enabled || !entry.url.starts_with("http") {
            continue;
        }
        let label = format!("content-{}-{}-{}", monitor_id, gen, i);
        let parsed_url = entry.url.parse::<url::Url>().map_err(|e| e.to_string())?;

        WebviewWindowBuilder::new(&app, &label, WebviewUrl::External(parsed_url))
            .position(content_x, content_y)
            .inner_size(content_w, content_h)
            .decorations(false)
            .skip_taskbar(true)
            .visible(false)
            .build()
            .map_err(|e| e.to_string())?;

        new_windows.push(ContentWindow { label, url: entry.url.clone() });
    }

    state
        .content_windows
        .lock()
        .unwrap()
        .insert(monitor_id, new_windows);

    Ok(())
}

#[tauri::command]
fn show_content_window(monitor_id: String, index: usize, app: AppHandle, state: State<AppState>) {
    let windows = state
        .content_windows
        .lock()
        .unwrap()
        .get(&monitor_id)
        .cloned()
        .unwrap_or_default();

    for (i, win) in windows.iter().enumerate() {
        if let Some(w) = app.get_webview_window(&win.label) {
            if i == index {
                let _ = w.show();
            } else {
                let _ = w.hide();
            }
        }
    }
}

#[tauri::command]
fn destroy_content_windows(monitor_id: String, app: AppHandle, state: State<AppState>) {
    let windows = state
        .content_windows
        .lock()
        .unwrap()
        .remove(&monitor_id)
        .unwrap_or_default();

    for win in windows {
        if let Some(w) = app.get_webview_window(&win.label) {
            let _ = w.close();
        }
    }
}

// ── Credentials (Windows Credential Manager) ──

#[tauri::command]
fn save_credentials(key: String, username: String, password: String) -> serde_json::Value {
    match cred_save(&key, &username, &password) {
        Ok(_) => serde_json::json!({ "ok": true }),
        Err(e) => serde_json::json!({ "ok": false, "error": e }),
    }
}

#[tauri::command]
fn load_credentials(key: String) -> Option<serde_json::Value> {
    cred_load(&key)
        .ok()
        .flatten()
        .map(|(u, p)| serde_json::json!({ "username": u, "password": p }))
}

#[tauri::command]
fn has_credentials(key: String) -> bool {
    cred_load(&key).ok().and_then(|x| x).is_some()
}

#[tauri::command]
fn delete_credentials(key: String) -> serde_json::Value {
    match cred_delete(&key) {
        Ok(_) => serde_json::json!({ "ok": true }),
        Err(e) => serde_json::json!({ "ok": false, "error": e }),
    }
}

// ── Window management ──

#[tauri::command]
fn get_fullscreen(window: WebviewWindow) -> bool {
    window.is_fullscreen().unwrap_or(false)
}

#[tauri::command]
fn toggle_fullscreen(window: WebviewWindow) {
    let next = !window.is_fullscreen().unwrap_or(false);
    let _ = window.set_fullscreen(next);
    let _ = window.emit("fullscreen-changed", next);
}

#[tauri::command]
fn minimize_window(window: WebviewWindow) {
    let _ = window.minimize();
}

#[tauri::command]
fn quit_app(app: AppHandle) {
    app.exit(0);
}

// ── Launch windows ──

fn get_layout_mode_str(state: &AppState) -> String {
    state
        .settings
        .lock()
        .unwrap()
        .get("layout")
        .and_then(|s| s.layout.clone())
        .unwrap_or_else(|| "single".to_string())
}

fn launch_windows(app: &AppHandle, mode: &str) {
    for (label, win) in app.webview_windows() {
        if label.starts_with("monitor-") || label.starts_with("content-") {
            let _ = win.close();
        }
    }

    let monitors: Vec<tauri::Monitor> = app.available_monitors().unwrap_or_default();

    if mode == "dual" && monitors.len() >= 2 {
        for (i, monitor) in monitors.iter().take(2).enumerate() {
            create_control_window(app, i + 1, Some(monitor));
        }
    } else {
        let primary = app.primary_monitor().unwrap_or(None);
        create_control_window(app, 1, primary.as_ref());
    }
}

fn create_control_window(app: &AppHandle, id: usize, monitor: Option<&tauri::Monitor>) {
    let label = format!("monitor-{}", id);

    let mut builder = WebviewWindowBuilder::new(app, &label, WebviewUrl::App("index.html".into()))
        .title(format!("Sentinel - MON-{}", id))
        .decorations(false)
        .fullscreen(true)
        .always_on_top(true)
        .transparent(true)
        .initialization_script(&format!("window.__MONITOR_ID__ = '{}';", id));

    if let Some(m) = monitor {
        let pos = m.position();
        let size = m.size();
        builder = builder
            .position(pos.x as f64, pos.y as f64)
            .inner_size(size.width as f64, size.height as f64);
    }

    let _ = builder.build();
}

// ── Windows Credential Manager ──

#[cfg(windows)]
fn cred_save(key: &str, username: &str, password: &str) -> Result<(), String> {
    use windows::core::PWSTR;
    use windows::Win32::Security::Credentials::{
        CredWriteW, CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW,
    };

    let mut target: Vec<u16> = format!("sentinel:{}", key)
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let mut user: Vec<u16> = username.encode_utf16().chain(std::iter::once(0)).collect();
    let pass_bytes = password.as_bytes();

    let mut cred = CREDENTIALW {
        Type: CRED_TYPE_GENERIC,
        TargetName: PWSTR(target.as_mut_ptr()),
        UserName: PWSTR(user.as_mut_ptr()),
        CredentialBlob: pass_bytes.as_ptr() as *mut u8,
        CredentialBlobSize: pass_bytes.len() as u32,
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        ..Default::default()
    };

    unsafe { CredWriteW(&mut cred, 0).map_err(|e| e.to_string()) }
}

#[cfg(windows)]
fn cred_load(key: &str) -> Result<Option<(String, String)>, String> {
    use windows::core::PCWSTR;
    use windows::Win32::Security::Credentials::{CredFree, CredReadW, CRED_TYPE_GENERIC, CREDENTIALW};

    let target: Vec<u16> = format!("sentinel:{}", key)
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    unsafe {
        let mut pcred: *mut CREDENTIALW = std::ptr::null_mut();
        if CredReadW(PCWSTR(target.as_ptr()), CRED_TYPE_GENERIC, None, &mut pcred).is_err() {
            return Ok(None);
        }
        let cred = &*pcred;
        let username = cred.UserName.to_string().unwrap_or_default();
        let password = String::from_utf8_lossy(std::slice::from_raw_parts(
            cred.CredentialBlob,
            cred.CredentialBlobSize as usize,
        ))
        .to_string();
        CredFree(pcred as *mut _);
        Ok(Some((username, password)))
    }
}

#[cfg(windows)]
fn cred_delete(key: &str) -> Result<(), String> {
    use windows::core::PCWSTR;
    use windows::Win32::Security::Credentials::{CredDeleteW, CRED_TYPE_GENERIC};

    let target: Vec<u16> = format!("sentinel:{}", key)
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    unsafe {
        CredDeleteW(PCWSTR(target.as_ptr()), CRED_TYPE_GENERIC, None).map_err(|e| e.to_string())
    }
}

#[cfg(not(windows))]
fn cred_save(_: &str, _: &str, _: &str) -> Result<(), String> {
    Err("Windows only".to_string())
}

#[cfg(not(windows))]
fn cred_load(_: &str) -> Result<Option<(String, String)>, String> {
    Ok(None)
}

#[cfg(not(windows))]
fn cred_delete(_: &str) -> Result<(), String> {
    Ok(())
}

// ── Entry point ──

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(AppState::new())
        .setup(|app| {
            let quit = MenuItem::with_id(app, "quit", "종료", true, None::<&str>)?;
            let single = MenuItem::with_id(app, "single", "싱글 모니터", true, None::<&str>)?;
            let dual = MenuItem::with_id(app, "dual", "듀얼 모니터", true, None::<&str>)?;
            let sep = PredefinedMenuItem::separator(app)?;
            let menu = Menu::with_items(app, &[&single, &dual, &sep, &quit])?;

            TrayIconBuilder::new()
                .tooltip("Sentinel")
                .menu(&menu)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "quit" => app.exit(0),
                    "single" => {
                        let state = app.state::<AppState>();
                        set_layout_mode("single".to_string(), state, app.clone());
                    }
                    "dual" => {
                        let state = app.state::<AppState>();
                        set_layout_mode("dual".to_string(), state, app.clone());
                    }
                    _ => {}
                })
                .build(app)?;

            let state = app.state::<AppState>();
            let mode = get_layout_mode_str(&state);
            launch_windows(app.handle(), &mode);

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            load_settings,
            save_settings,
            get_layout_mode,
            set_layout_mode,
            sync_content_windows,
            show_content_window,
            destroy_content_windows,
            save_credentials,
            load_credentials,
            has_credentials,
            delete_credentials,
            get_fullscreen,
            toggle_fullscreen,
            minimize_window,
            quit_app,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
