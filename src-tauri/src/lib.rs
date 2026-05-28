use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::TrayIconBuilder,
    AppHandle, Emitter, Manager, State, WebviewUrl,
    WebviewWindowBuilder,
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

#[derive(Debug, Serialize, Deserialize, Clone)]
struct HoverZone {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

struct AppState {
    settings: Mutex<HashMap<String, MonitorSettings>>,
    settings_path: PathBuf,
    hover_zones: Mutex<HashMap<String, Vec<HoverZone>>>,
    stop_flags: Mutex<HashMap<String, Arc<AtomicBool>>>,
}

impl AppState {
    fn new() -> Self {
        let path = data_path();
        let settings = load_settings_from_disk(&path);
        AppState {
            settings: Mutex::new(settings),
            settings_path: path,
            hover_zones: Mutex::new(HashMap::new()),
            stop_flags: Mutex::new(HashMap::new()),
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
fn get_layout_mode(state: State<AppState>, app: AppHandle) -> serde_json::Value {
    let settings = state.settings.lock().unwrap();
    let mode = settings
        .get("layout")
        .and_then(|s| s.layout.clone())
        .unwrap_or_else(|| "single".to_string());
    let display_count = app.available_monitors()
        .map(|m| m.len())
        .unwrap_or(1);
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

    // IPC 응답 완료 후 창 전환
    let app_handle = app.app_handle().clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(100));
        let handle = app_handle.clone();
        let _ = app_handle.run_on_main_thread(move || {
            launch_windows(&handle, &mode);
        });
    });
}

// JS가 오버레이 창의 인터랙티브 영역을 Rust에 알려주면
// 폴링 스레드가 커서 위치에 따라 set_ignore_cursor_events를 토글함
#[tauri::command]
fn set_hover_zones(monitor_id: String, zones: Vec<HoverZone>, state: State<AppState>) {
    state.hover_zones.lock().unwrap().insert(monitor_id, zones);
}

// ── Content window navigation ──

#[tauri::command]
async fn navigate_content(
    monitor_id: String,
    url: String,
    app: AppHandle,
) -> Result<(), String> {
    let label = format!("content-{}", monitor_id);
    if let Some(wv) = app.get_webview_window(&label) {
        let target: url::Url = if url.starts_with("http") {
            url.parse::<url::Url>().map_err(|e| e.to_string())?
        } else {
            "about:blank".parse().unwrap()
        };
        wv.navigate(target).map_err(|e| e.to_string())?;
    }
    Ok(())
}

// 백화현상 방지: JS loadSettings 완료 후 창 표시
#[tauri::command]
fn show_window(monitor_id: String, app: AppHandle) {
    let label = format!("monitor-{}", monitor_id);
    if let Some(w) = app.get_webview_window(&label) {
        let _ = w.show();
        let _ = w.set_focus();
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
fn get_fullscreen(monitor_id: String, app: AppHandle) -> bool {
    let label = format!("monitor-{}", monitor_id);
    app.get_webview_window(&label)
        .and_then(|w| w.is_fullscreen().ok())
        .unwrap_or(false)
}

#[tauri::command]
fn toggle_fullscreen(monitor_id: String, app: AppHandle) {
    let label = format!("monitor-{}", monitor_id);
    if let Some(w) = app.get_webview_window(&label) {
        let next = !w.is_fullscreen().unwrap_or(false);
        let _ = w.set_fullscreen(next);
        let _ = w.emit("fullscreen-changed", next);
    }
}

#[tauri::command]
fn minimize_window(monitor_id: String, app: AppHandle) {
    let label = format!("monitor-{}", monitor_id);
    if let Some(w) = app.get_webview_window(&label) {
        let _ = w.minimize();
    }
}

#[tauri::command]
fn quit_app(app: AppHandle) {
    for (_, win) in app.webview_windows() {
        let _ = win.close();
    }
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
    // destroy()는 동기적으로 즉시 파괴 → close()의 비동기 race 조건 방지
    for (label, win) in app.webview_windows() {
        if label.starts_with("monitor-") || label.starts_with("content-") {
            let _ = win.destroy();
        }
    }

    let monitors: Vec<tauri::Monitor> = app.available_monitors().unwrap_or_default();

    if mode == "dual" && monitors.len() >= 2 {
        for (i, monitor) in monitors.iter().take(2).enumerate() {
            create_control_window(app, i + 1, Some(monitor));
        }
    } else {
        let primary = app.primary_monitor().unwrap_or(None);
        let monitor = primary.or_else(|| monitors.into_iter().next());
        create_control_window(app, 1, monitor.as_ref());
    }
}

// 커서가 인터랙티브 존에 있을 때만 오버레이 창이 이벤트를 수신
// 그 외 영역은 set_ignore_cursor_events(true)로 content 창에 이벤트 전달
fn start_hover_monitor(app: AppHandle, monitor_id: String) {
    let stop_flag = {
        let state = app.state::<AppState>();
        let mut flags = state.stop_flags.lock().unwrap();
        if let Some(old) = flags.get(&monitor_id) {
            old.store(true, Ordering::Relaxed);
        }
        let flag = Arc::new(AtomicBool::new(false));
        flags.insert(monitor_id.clone(), flag.clone());
        flag
    };

    std::thread::spawn(move || {
        loop {
            if stop_flag.load(Ordering::Relaxed) { break; }
            std::thread::sleep(std::time::Duration::from_millis(50));
            if stop_flag.load(Ordering::Relaxed) { break; }

            let label = format!("monitor-{}", monitor_id);
            let Some(win) = app.get_webview_window(&label) else { break; };

            let Ok(cursor) = win.cursor_position() else { continue; };
            let Ok(win_pos) = win.outer_position() else { continue; };
            let Ok(scale) = win.scale_factor() else { continue; };

            // 스크린 좌표 → 창 내부 논리 좌표
            let lx = (cursor.x - win_pos.x as f64) / scale;
            let ly = (cursor.y - win_pos.y as f64) / scale;

            let in_zone = {
                let state = app.state::<AppState>();
                let zones = state.hover_zones.lock().unwrap();
                zones.get(&monitor_id)
                    .map(|zs| zs.iter().any(|z| lx >= z.x && lx < z.x + z.w && ly >= z.y && ly < z.y + z.h))
                    .unwrap_or(false)
            };

            let _ = win.set_ignore_cursor_events(!in_zone);
        }
    });
}

fn create_control_window(app: &AppHandle, id: usize, monitor: Option<&tauri::Monitor>) {
    let mon_label = format!("monitor-{}", id);
    let content_label = format!("content-{}", id);

    let (lx, ly, lw, lh) = if let Some(m) = monitor {
        let pos = m.position();
        let size = m.size();
        let scale = m.scale_factor();
        (
            pos.x as f64 / scale,
            pos.y as f64 / scale,
            size.width as f64 / scale,
            size.height as f64 / scale,
        )
    } else {
        (0.0, 0.0, 1920.0, 1080.0)
    };

    // 컨텐츠 창
    {
        let mut builder = WebviewWindowBuilder::new(app, &content_label, WebviewUrl::External("about:blank".parse().unwrap()))
            .decorations(false)
            .skip_taskbar(true)
            .visible(true);

        if monitor.is_some() {
            builder = builder
                .position(lx, ly)
                .inner_size(lw, lh);
        } else {
            builder = builder.fullscreen(true);
        }
        let _ = builder.build();
    }

    // 컨트롤 오버레이 창
    {
        let mut builder = WebviewWindowBuilder::new(app, &mon_label, WebviewUrl::App("index.html".into()))
            .title(format!("Sentinel - MON-{}", id))
            .decorations(false)
            .always_on_top(true)
            .transparent(true)
            .visible(false)
            .initialization_script(&format!("window.__MONITOR_ID__ = '{}';", id));

        if monitor.is_some() {
            builder = builder
                .position(lx, ly)
                .inner_size(lw, lh);
        } else {
            builder = builder.fullscreen(true);
        }
        let _ = builder.build();
    }

    start_hover_monitor(app.clone(), id.to_string());
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
            set_hover_zones,
            navigate_content,
            show_window,
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
