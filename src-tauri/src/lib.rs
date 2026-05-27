use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::TrayIconBuilder,
    AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder,
};

#[derive(Debug, Serialize, Deserialize, Clone)]
struct UrlItem {
    url: String,
    label: Option<String>,
    enabled: Option<bool>,
    interval: Option<u64>,
    #[serde(rename = "userSelector")]
    user_selector: Option<String>,
    #[serde(rename = "passSelector")]
    pass_selector: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
struct MonitorSettings {
    urls: Vec<UrlItem>,
    interval: Option<u64>,
    layout: Option<String>,
}

struct AppState {
    settings: Mutex<HashMap<String, MonitorSettings>>,
    settings_path: PathBuf,
}

impl AppState {
    fn new() -> Self {
        let path = dirs_path();
        let settings = load_settings_from_disk(&path);
        AppState {
            settings: Mutex::new(settings),
            settings_path: path,
        }
    }

    fn save(&self) {
        let settings = self.settings.lock().unwrap();
        let json = serde_json::to_string_pretty(&*settings).unwrap_or_default();
        let _ = fs::write(&self.settings_path, json);
    }
}

fn dirs_path() -> PathBuf {
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
fn load_settings(
    monitor_id: String,
    state: State<AppState>,
) -> Option<MonitorSettings> {
    let settings = state.settings.lock().unwrap();
    settings.get(&monitor_id).cloned()
}

#[tauri::command]
fn save_settings(
    monitor_id: String,
    settings: MonitorSettings,
    state: State<AppState>,
) {
    {
        let mut map = state.settings.lock().unwrap();
        map.insert(monitor_id, settings);
    }
    state.save();
}

#[tauri::command]
fn get_layout_mode(state: State<AppState>) -> serde_json::Value {
    let settings = state.settings.lock().unwrap();
    let mode = settings
        .get("layout")
        .and_then(|s| s.layout.clone())
        .unwrap_or_else(|| "single".to_string());
    let display_count = get_display_count();
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

// ── Credentials (Windows Credential Manager) ──

#[tauri::command]
fn save_credentials(key: String, username: String, password: String) -> serde_json::Value {
    match save_cred_windows(&key, &username, &password) {
        Ok(_) => serde_json::json!({ "ok": true }),
        Err(e) => serde_json::json!({ "ok": false, "error": e }),
    }
}

#[tauri::command]
fn load_credentials(key: String) -> Option<serde_json::Value> {
    load_cred_windows(&key)
        .ok()
        .flatten()
        .map(|(u, p)| serde_json::json!({ "username": u, "password": p }))
}

#[tauri::command]
fn has_credentials(key: String) -> bool {
    load_cred_windows(&key)
        .ok()
        .and_then(|x| x)
        .is_some()
}

#[tauri::command]
fn delete_credentials(key: String) -> serde_json::Value {
    match delete_cred_windows(&key) {
        Ok(_) => serde_json::json!({ "ok": true }),
        Err(e) => serde_json::json!({ "ok": false, "error": e }),
    }
}

// ── Window management ──

#[tauri::command]
fn get_fullscreen(window: tauri::WebviewWindow) -> bool {
    window.is_fullscreen().unwrap_or(false)
}

#[tauri::command]
fn toggle_fullscreen(window: tauri::WebviewWindow) {
    let next = !window.is_fullscreen().unwrap_or(false);
    let _ = window.set_fullscreen(next);
    let _ = window.emit("fullscreen-changed", next);
}

#[tauri::command]
fn minimize_window(window: tauri::WebviewWindow) {
    let _ = window.minimize();
}

#[tauri::command]
fn quit_app(app: AppHandle) {
    app.exit(0);
}

// ── Helper: launch windows based on mode ──

fn get_display_count() -> usize {
    // Tauri doesn't expose monitor list directly in commands easily;
    // return a fixed count for now — UI uses this for dual-mode display
    1
}

fn launch_windows(app: &AppHandle, mode: &str) {
    // Close all existing monitor windows and re-create
    for (label, win) in app.webview_windows() {
        if label.starts_with("monitor-") {
            let _ = win.close();
        }
    }

    let monitors: Vec<tauri::Monitor> = app.available_monitors()
        .unwrap_or_default();

    if mode == "dual" && monitors.len() >= 2 {
        for (i, monitor) in monitors.iter().take(2).enumerate() {
            create_monitor_window(app, i + 1, Some(monitor));
        }
    } else {
        let primary = app.primary_monitor().unwrap_or_else(|_| None);
        create_monitor_window(app, 1, primary.as_ref());
    }
}

fn create_monitor_window(app: &AppHandle, id: usize, monitor: Option<&tauri::Monitor>) {
    let label = format!("monitor-{}", id);
    let url = WebviewUrl::App("index.html".into());

    let mut builder = WebviewWindowBuilder::new(app, &label, url)
        .title(format!("Sentinel - MON-{}", id))
        .decorations(false)
        .fullscreen(true)
        .initialization_script(&format!(
            "window.__MONITOR_ID__ = '{}';",
            id
        ));

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
fn save_cred_windows(key: &str, username: &str, password: &str) -> Result<(), String> {
    use windows::core::PWSTR;
    use windows::Win32::Security::Credentials::{
        CredWriteW, CREDENTIALW, CRED_TYPE_GENERIC, CRED_PERSIST_LOCAL_MACHINE,
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

    unsafe {
        CredWriteW(&mut cred, 0).map_err(|e| e.to_string())
    }
}

#[cfg(windows)]
fn load_cred_windows(key: &str) -> Result<Option<(String, String)>, String> {
    use windows::core::PCWSTR;
    use windows::Win32::Security::Credentials::{
        CredFree, CredReadW, CREDENTIALW, CRED_TYPE_GENERIC,
    };

    let target: Vec<u16> = format!("sentinel:{}", key)
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    unsafe {
        let mut pcred: *mut CREDENTIALW = std::ptr::null_mut();
        let result = CredReadW(PCWSTR(target.as_ptr()), CRED_TYPE_GENERIC, None, &mut pcred);

        if result.is_err() {
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
fn delete_cred_windows(key: &str) -> Result<(), String> {
    use windows::core::PCWSTR;
    use windows::Win32::Security::Credentials::{CredDeleteW, CRED_TYPE_GENERIC};

    let target: Vec<u16> = format!("sentinel:{}", key)
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    unsafe {
        CredDeleteW(PCWSTR(target.as_ptr()), CRED_TYPE_GENERIC, None)
            .map_err(|e| e.to_string())
    }
}

#[cfg(not(windows))]
fn save_cred_windows(_key: &str, _username: &str, _password: &str) -> Result<(), String> {
    Err("Credential storage only supported on Windows".to_string())
}

#[cfg(not(windows))]
fn load_cred_windows(_key: &str) -> Result<Option<(String, String)>, String> {
    Ok(None)
}

#[cfg(not(windows))]
fn delete_cred_windows(_key: &str) -> Result<(), String> {
    Ok(())
}

// ── Entry point ──

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(AppState::new())
        .setup(|app| {
            // Build tray menu
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

            // Launch monitor windows
            let state = app.state::<AppState>();
            let mode = {
                let map = state.settings.lock().unwrap();
                map.get("layout")
                    .and_then(|s| s.layout.clone())
                    .unwrap_or_else(|| "single".to_string())
            };
            launch_windows(app.handle(), &mode);

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            load_settings,
            save_settings,
            get_layout_mode,
            set_layout_mode,
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
