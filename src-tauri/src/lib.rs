//! Lionhold Uploader: keeps the Lionhold Export addon installed in WoW Forever, and uploads its saved file to
//! Lionhold Armory whenever the game rewrites it (every logout and /reload). Lives in the tray; the window is only for
//! setup and status.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};
use tauri_plugin_updater::UpdaterExt;

const SITE: &str = "https://lionhold-armory.drodenberg92.workers.dev";
const ADDON_TOC: &str = include_str!("../../addon/LionholdExport/LionholdExport_Forever.toc");
const ADDON_LUA: &str = include_str!("../../addon/LionholdExport/LionholdExport.lua");
const KEYRING_SERVICE: &str = "Lionhold Uploader";
const KEYRING_USER: &str = "upload-key";
const CHECK_EVERY: Duration = Duration::from_secs(15);
const UPDATE_EVERY: Duration = Duration::from_secs(6 * 60 * 60);

#[derive(Default, Serialize, Deserialize, Clone)]
struct Config {
    wow_dir: Option<PathBuf>,
    battletag: Option<String>,
    last_upload_at: Option<u64>,
    last_message: Option<String>,
    /// SavedVariables path -> modified time already uploaded, so each logout uploads once.
    uploaded: HashMap<String, u64>,
    autostart_set: bool,
    /// Whether the player lets their guild see their bags (an account-wide setting on the site).
    #[serde(default)]
    share: Option<bool>,
}

struct AppState {
    config: Mutex<Config>,
    linking: Mutex<Option<String>>,
    /// The tray's "Check for updates" item, so its label can show progress.
    update_item: Mutex<Option<MenuItem<tauri::Wry>>>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Status {
    version: String,
    wow_dir: Option<String>,
    addon: String,
    connected: bool,
    battletag: Option<String>,
    last_upload_at: Option<u64>,
    last_message: Option<String>,
    linking_code: Option<String>,
    share: Option<bool>,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

// ---------- config and secrets ----------

fn config_path(app: &AppHandle) -> PathBuf {
    app.path().app_config_dir().unwrap_or_else(|_| PathBuf::from(".")).join("config.json")
}

fn load_config(app: &AppHandle) -> Config {
    fs::read_to_string(config_path(app))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_config(app: &AppHandle, cfg: &Config) {
    let path = config_path(app);
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_string_pretty(cfg) {
        let _ = fs::write(path, json);
    }
}

fn update_config(app: &AppHandle, f: impl FnOnce(&mut Config)) -> Config {
    let state: State<AppState> = app.state();
    let mut cfg = state.config.lock().unwrap();
    f(&mut cfg);
    save_config(app, &cfg);
    cfg.clone()
}

fn config(app: &AppHandle) -> Config {
    let state: State<AppState> = app.state();
    let cfg = state.config.lock().unwrap();
    cfg.clone()
}

/// The upload key lives in the system keychain (macOS Keychain, Windows Credential Manager), not in a file.
fn upload_key() -> Option<String> {
    keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER).ok()?.get_password().ok()
}
fn set_upload_key(key: &str) -> Result<(), String> {
    keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER)
        .and_then(|e| e.set_password(key))
        .map_err(|e| format!("Couldn't save to the keychain: {e}"))
}
fn clear_upload_key() {
    if let Ok(e) = keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER) {
        let _ = e.delete_credential();
    }
}

// ---------- finding WoW and the addon ----------

/// Blizzard records "...\World of Warcraft\_retail_\" in the registry; the game root is its parent.
fn game_root(p: PathBuf) -> PathBuf {
    let flavor = p
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.starts_with('_') && n.ends_with('_'))
        .unwrap_or(false);
    if flavor {
        if let Some(parent) = p.parent() {
            return parent.to_path_buf();
        }
    }
    p
}

fn find_wow_dir() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    #[cfg(windows)]
    {
        use winreg::enums::HKEY_LOCAL_MACHINE;
        use winreg::RegKey;
        for sub in [
            "SOFTWARE\\WOW6432Node\\Blizzard Entertainment\\World of Warcraft",
            "SOFTWARE\\Blizzard Entertainment\\World of Warcraft",
        ] {
            if let Ok(key) = RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey(sub) {
                if let Ok(path) = key.get_value::<String, _>("InstallPath") {
                    candidates.push(game_root(PathBuf::from(path)));
                }
            }
        }
        for drive in ["C", "D", "E", "F"] {
            for rest in ["Program Files (x86)\\World of Warcraft", "Program Files\\World of Warcraft", "World of Warcraft", "Games\\World of Warcraft"] {
                candidates.push(PathBuf::from(format!("{drive}:\\{rest}")));
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        candidates.push(PathBuf::from("/Applications/World of Warcraft"));
        if let Some(home) = dirs::home_dir() {
            candidates.push(home.join("Applications/World of Warcraft"));
        }
    }
    candidates.into_iter().find(|p| p.join("_forever_").is_dir())
}

fn forever(dir: &Path) -> PathBuf {
    dir.join("_forever_")
}

fn addon_version(toc: &str) -> Option<String> {
    toc.lines().find_map(|l| l.strip_prefix("## Version:").map(|v| v.trim().to_string()))
}

/// Installs or updates the bundled addon. Only writes when the installed version differs.
fn ensure_addon(wow: &Path) -> String {
    let target = forever(wow).join("Interface").join("AddOns").join("LionholdExport");
    let bundled = addon_version(ADDON_TOC).unwrap_or_default();
    let installed = fs::read_to_string(target.join("LionholdExport_Forever.toc"))
        .ok()
        .and_then(|t| addon_version(&t));
    if installed.as_deref() == Some(bundled.as_str()) {
        return format!("Addon {bundled} installed");
    }
    let result = fs::create_dir_all(&target)
        .and_then(|_| fs::write(target.join("LionholdExport_Forever.toc"), ADDON_TOC))
        .and_then(|_| fs::write(target.join("LionholdExport.lua"), ADDON_LUA));
    match result {
        Ok(_) if installed.is_some() => format!("Addon updated to {bundled}. Reload the game to use it."),
        Ok(_) => format!("Addon {bundled} installed. Restart the game if it's open."),
        Err(e) => format!("Couldn't install the addon: {e}"),
    }
}

fn saved_variable_files(wow: &Path) -> Vec<PathBuf> {
    let pattern = forever(wow).join("WTF").join("Account").join("*").join("SavedVariables").join("LionholdExport.lua");
    glob::glob(&pattern.to_string_lossy())
        .map(|paths| paths.filter_map(Result::ok).collect())
        .unwrap_or_default()
}

fn modified_secs(path: &Path) -> u64 {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ---------- talking to the site ----------

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(concat!("LionholdUploader/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(30))
        .build()
        .expect("http client")
}

#[derive(Deserialize)]
struct UploadResult {
    #[serde(default)]
    saved: Vec<String>,
    #[serde(default)]
    skipped: Vec<String>,
    error: Option<String>,
}

/// Uploads any saved file the game has rewritten since the last upload.
async fn sync_once(app: &AppHandle, force: bool) {
    let cfg = config(app);
    let (Some(wow), Some(key)) = (cfg.wow_dir.clone(), upload_key()) else { return };
    for file in saved_variable_files(&wow) {
        let id = file.to_string_lossy().to_string();
        let mtime = modified_secs(&file);
        if !force && cfg.uploaded.get(&id) == Some(&mtime) {
            continue;
        }
        let Ok(body) = fs::read_to_string(&file) else { continue };
        if !body.contains("LHX1:") {
            continue; // the addon hasn't written an export yet (log out once)
        }
        let response = http()
            .post(format!("{SITE}/api/upload"))
            .bearer_auth(&key)
            .header("Content-Type", "text/plain; charset=utf-8")
            .body(body)
            .send()
            .await;
        let message = match response {
            Ok(r) if r.status() == reqwest::StatusCode::UNAUTHORIZED => {
                clear_upload_key();
                update_config(app, |c| c.battletag = None);
                "This computer was disconnected. Click Connect to link it again.".to_string()
            }
            Ok(r) => match r.json::<UploadResult>().await {
                Ok(UploadResult { error: Some(e), .. }) => e,
                Ok(res) => {
                    update_config(app, |c| {
                        c.uploaded.insert(id.clone(), mtime);
                        c.last_upload_at = Some(now());
                    });
                    let mut m = format!("Uploaded {} character{}", res.saved.len(), if res.saved.len() == 1 { "" } else { "s" });
                    if !res.skipped.is_empty() {
                        m.push_str(&format!(". Skipped: {}", res.skipped.join("; ")));
                    }
                    m
                }
                Err(_) => "The site sent an unexpected reply. Will retry.".to_string(),
            },
            Err(_) => "Couldn't reach Lionhold Armory. Will retry.".to_string(),
        };
        update_config(app, |c| c.last_message = Some(message));
    }
    emit_status(app);
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StartResponse {
    device_code: String,
    user_code: String,
    verify_url: String,
    interval: Option<u64>,
    expires_in: Option<u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PollResponse {
    status: String,
    upload_key: Option<String>,
}

#[derive(Deserialize)]
struct MeResponse {
    battletag: Option<String>,
    share: Option<bool>,
}

async fn fetch_me(key: &str) -> Option<MeResponse> {
    let r = http().get(format!("{SITE}/api/uploader/me")).bearer_auth(key).send().await.ok()?;
    r.json::<MeResponse>().await.ok()
}

// ---------- status for the window and tray ----------

fn status(app: &AppHandle) -> Status {
    let cfg = config(app);
    let state: State<AppState> = app.state();
    let linking_code = state.linking.lock().unwrap().clone();
    Status {
        version: app.package_info().version.to_string(),
        wow_dir: cfg.wow_dir.as_ref().map(|p| p.to_string_lossy().to_string()),
        addon: match &cfg.wow_dir {
            Some(dir) if forever(dir).join("Interface/AddOns/LionholdExport/LionholdExport_Forever.toc").exists() => {
                format!("Installed ({})", addon_version(ADDON_TOC).unwrap_or_default())
            }
            Some(_) => "Not installed".into(),
            None => "Waiting for the WoW folder".into(),
        },
        connected: upload_key().is_some(),
        battletag: cfg.battletag.clone(),
        last_upload_at: cfg.last_upload_at,
        last_message: cfg.last_message.clone(),
        linking_code,
        share: cfg.share,
    }
}

fn emit_status(app: &AppHandle) {
    let s = status(app);
    let tip = match (&s.battletag, s.connected, s.wow_dir.is_some()) {
        (_, _, false) => "Lionhold Uploader: choose your WoW folder".to_string(),
        (_, false, _) => "Lionhold Uploader: not connected".to_string(),
        (Some(tag), true, _) => format!("Lionhold Uploader: connected as {tag}"),
        (None, true, _) => "Lionhold Uploader: connected".to_string(),
    };
    if let Some(tray) = app.tray_by_id("main") {
        let _ = tray.set_tooltip(Some(&tip));
    }
    let _ = app.emit("status", s);
}

fn show_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.set_focus();
    }
}

// ---------- commands called from the window ----------

#[tauri::command]
fn get_status(app: AppHandle) -> Status {
    status(&app)
}

#[tauri::command]
fn set_wow_dir(app: AppHandle, path: String) -> Result<Status, String> {
    let dir = game_root(PathBuf::from(path));
    if !forever(&dir).is_dir() {
        return Err("That folder has no _forever_ inside. Pick the main \"World of Warcraft\" folder.".into());
    }
    let message = ensure_addon(&dir);
    update_config(&app, |c| {
        c.wow_dir = Some(dir);
        c.last_message = Some(message);
    });
    emit_status(&app);
    Ok(status(&app))
}

#[tauri::command]
async fn upload_now(app: AppHandle) -> Status {
    sync_once(&app, true).await;
    status(&app)
}

/// The "Let my guild see my bags" switch. Applies to every character on the account, now and later.
#[tauri::command]
async fn set_share(app: AppHandle, share: bool) -> Result<Status, String> {
    let key = upload_key().ok_or("Connect this computer first.")?;
    let r = http()
        .post(format!("{SITE}/api/uploader/share"))
        .bearer_auth(&key)
        .json(&serde_json::json!({ "share": share }))
        .send()
        .await
        .map_err(|_| "Couldn't reach Lionhold Armory.".to_string())?;
    if !r.status().is_success() {
        return Err("Couldn't change sharing. Try again.".into());
    }
    update_config(&app, |c| {
        c.share = Some(share);
        c.last_message = Some(if share { "Your guild can now see your bags." } else { "Your bags are private." }.into());
    });
    emit_status(&app);
    Ok(status(&app))
}

#[tauri::command]
fn disconnect(app: AppHandle) -> Status {
    clear_upload_key();
    update_config(&app, |c| {
        c.battletag = None;
        c.last_message = Some("Disconnected.".into());
    });
    emit_status(&app);
    status(&app)
}

/// Starts linking: opens the website with a code; the player approves while signed in; the app then gets its key.
#[tauri::command]
async fn connect(app: AppHandle) -> Result<String, String> {
    let name = gethostname::gethostname().to_string_lossy().to_string();
    let started: StartResponse = http()
        .post(format!("{SITE}/api/device/start"))
        .json(&serde_json::json!({ "name": name }))
        .send()
        .await
        .map_err(|_| "Couldn't reach Lionhold Armory. Check your internet connection.".to_string())?
        .json()
        .await
        .map_err(|_| "Unexpected reply from Lionhold Armory.".to_string())?;
    {
        let state: State<AppState> = app.state();
        *state.linking.lock().unwrap() = Some(started.user_code.clone());
    }
    emit_status(&app);
    let _ = tauri_plugin_opener::open_url(&started.verify_url, None::<&str>);

    let handle = app.clone();
    let code = started.user_code.clone();
    tauri::async_runtime::spawn(async move {
        let interval = Duration::from_secs(started.interval.unwrap_or(3).max(2));
        let deadline = SystemTime::now() + Duration::from_secs(started.expires_in.unwrap_or(600));
        let mut result = "The code expired. Click Connect to try again.".to_string();
        while SystemTime::now() < deadline {
            tokio_sleep(interval).await;
            let poll = http()
                .post(format!("{SITE}/api/device/poll"))
                .json(&serde_json::json!({ "deviceCode": started.device_code }))
                .send()
                .await;
            let Ok(resp) = poll else { continue };
            let Ok(p) = resp.json::<PollResponse>().await else { continue };
            match (p.status.as_str(), p.upload_key) {
                ("approved", Some(key)) => {
                    result = match set_upload_key(&key) {
                        Ok(_) => {
                            let me = fetch_me(&key).await;
                            update_config(&handle, |c| {
                                c.battletag = me.as_ref().and_then(|m| m.battletag.clone());
                                c.share = me.as_ref().and_then(|m| m.share);
                            });
                            "Connected. Your bags upload after every logout. Choose below whether your guild can see them.".to_string()
                        }
                        Err(e) => e,
                    };
                    break;
                }
                ("expired", _) => break,
                _ => {}
            }
        }
        {
            let state: State<AppState> = handle.state();
            *state.linking.lock().unwrap() = None;
        }
        update_config(&handle, |c| c.last_message = Some(result));
        sync_once(&handle, true).await;
        emit_status(&handle);
    });
    Ok(code)
}

async fn tokio_sleep(d: Duration) {
    // tauri's async runtime is tokio; this avoids a direct tokio dependency.
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    std::thread::spawn(move || {
        std::thread::sleep(d);
        let _ = tx.send(());
    });
    let _ = tauri::async_runtime::spawn_blocking(move || rx.recv()).await;
}

// ---------- background work ----------

async fn background(app: AppHandle) {
    // Find WoW and install the addon on first run.
    if config(&app).wow_dir.is_none() {
        if let Some(dir) = find_wow_dir() {
            let message = ensure_addon(&dir);
            update_config(&app, |c| {
                c.wow_dir = Some(dir);
                c.last_message = Some(message);
            });
        }
    } else if let Some(dir) = config(&app).wow_dir {
        let message = ensure_addon(&dir);
        if message.contains("updated") || message.contains("Couldn't") {
            update_config(&app, |c| c.last_message = Some(message));
        }
    }
    if let Some(key) = upload_key() {
        if let Some(me) = fetch_me(&key).await {
            update_config(&app, |c| {
                c.battletag = me.battletag.or(c.battletag.clone());
                c.share = me.share;
            });
        }
    }
    emit_status(&app);
    loop {
        sync_once(&app, false).await;
        tokio_sleep(CHECK_EVERY).await;
    }
}

fn set_update_label(app: &AppHandle, text: &str) {
    let state: State<AppState> = app.state();
    if let Some(item) = state.update_item.lock().unwrap().as_ref() {
        let _ = item.set_text(text);
    }
}

/// Checks for a newer version; if there is one, installs it and restarts. Returns what happened, for the window.
async fn check_for_update(app: &AppHandle) -> String {
    set_update_label(app, "Checking for updates…");
    let result = match app.updater() {
        Ok(updater) => match updater.check().await {
            Ok(Some(update)) => {
                set_update_label(app, &format!("Installing {}…", update.version));
                update_config(app, |c| c.last_message = Some(format!("Installing version {}…", update.version)));
                emit_status(app);
                match update.download_and_install(|_, _| {}, || {}).await {
                    Ok(_) => app.restart(),
                    Err(_) => "The update didn't install. Will try again later.".to_string(),
                }
            }
            Ok(None) => format!("You're up to date (version {}).", app.package_info().version),
            Err(_) => "Couldn't check for updates. Will try again later.".to_string(),
        },
        Err(_) => "Updates aren't available in this build.".to_string(),
    };
    set_update_label(app, "Check for updates");
    result
}

#[tauri::command]
async fn check_updates(app: AppHandle) -> Status {
    let message = check_for_update(&app).await;
    update_config(&app, |c| c.last_message = Some(message));
    emit_status(&app);
    status(&app)
}

async fn keep_updated(app: AppHandle) {
    tokio_sleep(Duration::from_secs(20)).await;
    loop {
        let _ = check_for_update(&app).await;
        tokio_sleep(UPDATE_EVERY).await;
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| show_window(app)))
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, Some(vec!["--background"])))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_process::init())
        .invoke_handler(tauri::generate_handler![get_status, set_wow_dir, upload_now, disconnect, connect, set_share, check_updates])
        .setup(|app| {
            let handle = app.handle().clone();
            app.manage(AppState {
                config: Mutex::new(load_config(&handle)),
                linking: Mutex::new(None),
                update_item: Mutex::new(None),
            });

            // A tray app: no Dock icon on macOS.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            // Start with the computer, once, on first run.
            if !config(&handle).autostart_set {
                let _ = handle.autolaunch().enable();
                update_config(&handle, |c| c.autostart_set = true);
            }

            let version = MenuItem::with_id(
                app,
                "version",
                format!("Lionhold Uploader {}", app.package_info().version),
                false,
                None::<&str>,
            )?;
            let updates = MenuItem::with_id(app, "updates", "Check for updates", true, None::<&str>)?;
            {
                let state: State<AppState> = app.state();
                *state.update_item.lock().unwrap() = Some(updates.clone());
            }
            let upload = MenuItem::with_id(app, "upload", "Upload now", true, None::<&str>)?;
            let site = MenuItem::with_id(app, "site", "Open Lionhold Armory", true, None::<&str>)?;
            let settings = MenuItem::with_id(app, "settings", "Settings…", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(
                app,
                &[
                    &version,
                    &updates,
                    &PredefinedMenuItem::separator(app)?,
                    &upload,
                    &site,
                    &settings,
                    &PredefinedMenuItem::separator(app)?,
                    &quit,
                ],
            )?;
            TrayIconBuilder::with_id("main")
                .icon(app.default_window_icon().cloned().expect("app icon"))
                .tooltip("Lionhold Uploader")
                .menu(&menu)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "updates" => {
                        let h = app.clone();
                        tauri::async_runtime::spawn(async move {
                            let message = check_for_update(&h).await;
                            update_config(&h, |c| c.last_message = Some(message));
                            emit_status(&h);
                        });
                    }
                    "upload" => {
                        let h = app.clone();
                        tauri::async_runtime::spawn(async move { sync_once(&h, true).await });
                    }
                    "site" => {
                        let _ = tauri_plugin_opener::open_url(SITE, None::<&str>);
                    }
                    "settings" => show_window(app),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .build(app)?;

            // Closing the window keeps the uploader running in the tray.
            if let Some(window) = app.get_webview_window("main") {
                let w = window.clone();
                window.on_window_event(move |event| {
                    if let WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        let _ = w.hide();
                    }
                });
            }

            // Show the window until setup is done; afterwards start silently.
            let started_in_background = std::env::args().any(|a| a == "--background");
            if !started_in_background || upload_key().is_none() || config(&handle).wow_dir.is_none() {
                show_window(&handle);
            }

            tauri::async_runtime::spawn(background(handle.clone()));
            tauri::async_runtime::spawn(keep_updated(handle));
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Lionhold Uploader");
}
