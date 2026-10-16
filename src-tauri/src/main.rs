#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::fs;
use std::io::{Cursor, Read, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::Serialize;
use serde_json::Value;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, Runtime, WindowEvent};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

const PAYLOAD: &[u8] = include_bytes!(env!("LINKSYNC_PAYLOAD"));
const FOLDER_ID: &str = "duixiang";
static EXITING: AtomicBool = AtomicBool::new(false);

#[cfg(windows)]
const NO_WINDOW: u32 = 0x08000000; // CREATE_NO_WINDOW

#[cfg(windows)]
const SYNC_BIN: &str = "syncthing.exe";
#[cfg(not(windows))]
const SYNC_BIN: &str = "syncthing";

struct AppState {
    child: Mutex<Option<Child>>,
    rate: Arc<Mutex<Option<(u64, Instant)>>>,
    paused: AtomicBool,
}

fn root() -> PathBuf {
    #[cfg(windows)]
    {
        let local = std::env::var("LOCALAPPDATA").expect("LOCALAPPDATA");
        Path::new(&local).join("linksync")
    }
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME").expect("HOME");
        Path::new(&home).join("Library/Application Support/linksync")
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Ok(x) = std::env::var("XDG_DATA_HOME") {
            Path::new(&x).join("linksync")
        } else {
            let home = std::env::var("HOME").expect("HOME");
            Path::new(&home).join(".local/share/linksync")
        }
    }
}

fn config_dir() -> PathBuf {
    root().join("config")
}

fn member_file() -> PathBuf {
    root().join("member.txt")
}

fn stamp_file() -> PathBuf {
    root().join(".payload_stamp")
}

fn read_member() -> Option<String> {
    fs::read_to_string(member_file())
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn kill_all_syncthing() {
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/F", "/IM", SYNC_BIN])
            .creation_flags(NO_WINDOW)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(unix)]
    {
        let _ = Command::new("pkill")
            .args(["-x", "syncthing"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

fn extract_payload() -> Result<(), String> {
    let root = root();
    fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let want = PAYLOAD.len().to_string();
    let have = fs::read_to_string(stamp_file()).unwrap_or_default();
    if have.trim() == want {
        return Ok(());
    }
    kill_all_syncthing();
    let mut z = zip::ZipArchive::new(Cursor::new(PAYLOAD)).map_err(|e| e.to_string())?;
    for i in 0..z.len() {
        let mut f = z.by_index(i).map_err(|e| e.to_string())?;
        let name = f.name().replace('\\', "/");
        let dest = root.join(&name);
        if f.is_dir() {
            fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
        } else {
            if let Some(p) = dest.parent() {
                fs::create_dir_all(p).map_err(|e| e.to_string())?;
            }
            let mut out = fs::File::create(&dest).map_err(|e| e.to_string())?;
            let mut buf = Vec::new();
            f.read_to_end(&mut buf).map_err(|e| e.to_string())?;
            out.write_all(&buf).map_err(|e| e.to_string())?;
        }
    }
    // 非 Windows：给 syncthing 可执行权限
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let bin = root.join(SYNC_BIN);
        if bin.exists() {
            let _ = fs::set_permissions(&bin, fs::Permissions::from_mode(0o755));
        }
    }
    fs::write(stamp_file(), want).map_err(|e| e.to_string())?;
    Ok(())
}

fn copy_dir(src: &Path, dst: &Path) -> Result<(), String> {
    fs::create_dir_all(dst).map_err(|e| e.to_string())?;
    for entry in fs::read_dir(src).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let p = entry.path();
        let target = dst.join(entry.file_name());
        if p.is_dir() {
            copy_dir(&p, &target)?;
        } else {
            fs::copy(&p, &target).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn deploy_member(letter: &str) -> Result<(), String> {
    let cfg_src = root().join(format!("cfg-{}", letter));
    if !cfg_src.exists() {
        return Err("成员配置缺失".into());
    }
    let cfg = config_dir();
    if cfg.exists() {
        let _ = fs::remove_dir_all(&cfg);
    }
    copy_dir(&cfg_src, &cfg)?;
    // 非 Windows：预置配置里的 Windows 默认目录不可用，替换为本平台默认同步目录
    #[cfg(not(windows))]
    {
        let def = default_folder();
        if !def.is_empty() {
            let _ = fs::create_dir_all(&def);
            patch_folder_path(&def)?;
        }
    }
    fs::write(member_file(), letter).map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(not(windows))]
fn default_folder() -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    format!("{}/duixiang", home)
}

fn start_sync(state: &AppState) -> Result<(), String> {
    kill_all_syncthing();
    std::thread::sleep(std::time::Duration::from_millis(400));
    let exe = root().join(SYNC_BIN);
    let home = config_dir();
    let mut cmd = Command::new(&exe);
    #[cfg(windows)]
    cmd.creation_flags(NO_WINDOW);
    let child = cmd
        .args(["serve", "--no-browser", "--home"])
        .arg(&home)
        .current_dir(&root())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    state.paused.store(false, Ordering::SeqCst);
    *state.child.lock().unwrap() = Some(child);
    Ok(())
}

fn stop_sync(state: &AppState) {
    if let Some(mut c) = state.child.lock().unwrap().take() {
        let _ = c.kill();
        let _ = c.wait();
    }
    kill_all_syncthing();
}

fn read_apikey() -> Option<String> {
    let xml = fs::read_to_string(config_dir().join("config.xml")).ok()?;
    let i = xml.find("<apikey>")? + "<apikey>".len();
    let j = xml[i..].find("</apikey>")?;
    Some(xml[i..i + j].to_string())
}

fn rest_json(method: &str, path: &str, body: Option<&str>) -> Result<Value, String> {
    let key = read_apikey().ok_or("无法读取 apikey")?;
    let url = format!("http://127.0.0.1:8384{}", path);
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_millis(800))
        .timeout_read(std::time::Duration::from_millis(1500))
        .build();
    let mut req = match method {
        "POST" => agent.post(&url),
        _ => agent.get(&url),
    };
    req = req.set("X-API-Key", &key);
    let resp = match body {
        Some(b) => {
            req = req.set("Content-Type", "application/json");
            req.send_string(b)
        }
        None => req.call(),
    };
    let resp = resp.map_err(|e| e.to_string())?;
    resp.into_json::<Value>().map_err(|e| e.to_string())
}

// 仅触发、不解析响应体（pause/resume/config 回写等返回空 body 的端点）
fn rest_fire(path: &str, body: Option<&str>) -> Result<(), String> {
    let key = read_apikey().ok_or("无法读取 apikey")?;
    let url = format!("http://127.0.0.1:8384{}", path);
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_millis(800))
        .timeout_read(std::time::Duration::from_millis(1500))
        .build();
    let mut req = agent.post(&url).set("X-API-Key", &key);
    if let Some(b) = body {
        req = req.set("Content-Type", "application/json");
        req.send_string(b).map_err(|e| e.to_string())?;
    } else {
        req.call().map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[derive(Serialize)]
struct AppInfo {
    member: Option<String>,
    folder: String,
    autostart: bool,
}

#[derive(Serialize)]
struct Status {
    connected: bool,
    peers: usize,
    paused: bool,
    state: String,
    need_bytes: u64,
    in_sync_bytes: u64,
    local_files: u64,
    rate: u64,
}

fn folder_path_from_config() -> String {
    let xml = fs::read_to_string(config_dir().join("config.xml")).unwrap_or_default();
    // path 是 <folder id="duixiang" ...> 标签上的属性
    for line in xml.lines() {
        let t = line.trim();
        if t.starts_with("<folder ") && t.contains("id=\"duixiang\"") {
            if let Some(i) = t.find("path=\"") {
                let rest = &t[i + 6..];
                if let Some(j) = rest.find('"') {
                    return rest[..j].to_string();
                }
            }
        }
    }
    String::new()
}

// 老用户升级：修正已部署 config.xml —— 监视延迟 1 秒；关闭全局发现与中继（境内连不上，只会拖慢建连）
// 返回 true 表示配置有改动，需要重启引擎才能生效
fn fix_watcher_delay() -> bool {
    let file = config_dir().join("config.xml");
    let xml = match fs::read_to_string(&file) {
        Ok(x) => x,
        Err(_) => return false,
    };
    let mut changed = false;
    let mut out = String::with_capacity(xml.len());
    for line in xml.lines() {
        let t = line.trim();
        // 中继监听行整行删除
        if t == "<listenAddress>dynamic+https://relays.syncthing.net/endpoint</listenAddress>" {
            changed = true;
            continue;
        }
        let mut l = line.to_string();
        if t == "<globalAnnounceEnabled>true</globalAnnounceEnabled>" {
            l = "<globalAnnounceEnabled>false</globalAnnounceEnabled>".to_string();
            changed = true;
        } else if t == "<relaysEnabled>true</relaysEnabled>" {
            l = "<relaysEnabled>false</relaysEnabled>".to_string();
            changed = true;
        }
        if t.starts_with("<folder ") && t.contains("id=\"duixiang\"") {
            if let Some(i) = l.find("fsWatcherDelayS=\"") {
                let vs = i + "fsWatcherDelayS=\"".len();
                let ve = l[vs..].find('"').map(|x| vs + x).unwrap_or(vs);
                if &l[vs..ve] != "1" {
                    l.replace_range(vs..ve, "1");
                    changed = true;
                }
            }
        }
        out.push_str(&l);
        out.push('\n');
    }
    if changed {
        let _ = fs::write(&file, out);
    }
    changed
}

// 把当前 config.xml 中 duixiang 文件夹的 path 属性改为指定目录（切换身份时保留用户自定义目录）
fn patch_folder_path(new_path: &str) -> Result<(), String> {
    let file = config_dir().join("config.xml");
    let xml = fs::read_to_string(&file).map_err(|e| e.to_string())?;
    let mut out = String::with_capacity(xml.len());
    for line in xml.lines() {
        let t = line.trim();
        if t.starts_with("<folder ") && t.contains("id=\"duixiang\"") && t.contains("path=\"") {
            let i = line.find("path=\"").unwrap() + "path=\"".len();
            let j = line[i..].find('"').map(|x| i + x).unwrap_or(line.len());
            let mut l = line.to_string();
            l.replace_range(i..j, new_path);
            out.push_str(&l);
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    fs::write(&file, out).map_err(|e| e.to_string())
}

#[tauri::command]
fn app_info() -> AppInfo {
    AppInfo {
        member: read_member(),
        folder: folder_path_from_config(),
        autostart: autostart_get(),
    }
}

#[tauri::command]
fn set_member(app: AppHandle, state: tauri::State<AppState>, letter: String) -> Result<(), String> {
    let letter = letter.trim().to_uppercase();
    if !["A", "B", "C", "D"].contains(&letter.as_str()) {
        return Err("无效成员".into());
    }
    // 运行中切换身份：先停引擎，释放 config 目录占用
    stop_sync(&state);
    std::thread::sleep(std::time::Duration::from_millis(500));
    // 记住用户自定义过的同步目录，重绑后恢复
    let keep_path = folder_path_from_config();
    deploy_member(&letter)?;
    if !keep_path.is_empty() && keep_path != folder_path_from_config() {
        patch_folder_path(&keep_path)?;
    }
    start_sync(&state)?;
    let _ = app.emit("member-set", ());
    Ok(())
}

#[tauri::command]
fn status(state: tauri::State<AppState>) -> Status {
    let rate = state.rate.clone();
    let paused = state.paused.load(Ordering::SeqCst);
    let mut out = Status {
        connected: false,
        peers: 0,
        paused: false,
        state: "offline".into(),
        need_bytes: 0,
        in_sync_bytes: 0,
        local_files: 0,
        rate: 0,
    };
    let conns = match rest_json("GET", "/rest/system/connections", None) {
        Ok(v) => v,
        Err(_) => return out,
    };
    if let Some(map) = conns.get("connections").and_then(|c| c.as_object()) {
        out.peers = map.values().filter(|d| d.get("connected").and_then(|b| b.as_bool()).unwrap_or(false)).count();
        out.connected = out.peers > 0;
    }
    out.paused = paused;
    if let Ok(db) = rest_json("GET", &format!("/rest/db/status?folder={}", FOLDER_ID), None) {
        out.state = db.get("state").and_then(|s| s.as_str()).unwrap_or("unknown").to_string();
        out.need_bytes = db.get("needBytes").and_then(|n| n.as_u64()).unwrap_or(0);
        out.in_sync_bytes = db.get("inSyncBytes").and_then(|n| n.as_u64()).unwrap_or(0);
        out.local_files = db.get("localFiles").and_then(|n| n.as_u64()).unwrap_or(0);
        let now = Instant::now();
        let mut g = rate.lock().unwrap();
        if let Some((prev, t0)) = *g {
            let dt = now.duration_since(t0).as_secs_f64();
            if dt > 0.5 && out.in_sync_bytes >= prev {
                out.rate = ((out.in_sync_bytes - prev) as f64 / dt) as u64;
            }
        }
        *g = Some((out.in_sync_bytes, now));
    }
    out
}

#[tauri::command]
fn set_paused(app: AppHandle, paused: bool) -> Result<(), String> {
    let path = if paused { "/rest/system/pause" } else { "/rest/system/resume" };
    rest_fire(path, None)?;
    app.state::<AppState>().paused.store(paused, Ordering::SeqCst);
    Ok(())
}

#[tauri::command]
fn get_folder() -> String {
    folder_path_from_config()
}

const SYNCINFO_URL: &str = "http://103.236.70.249:8766/syncinfo.json";
const SYNCFILES_URL: &str = "http://103.236.70.249:8766/syncfiles.json";

#[tauri::command]
fn server_info() -> Result<Value, String> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_millis(3000))
        .timeout_read(std::time::Duration::from_millis(5000))
        .build();
    agent
        .get(SYNCINFO_URL)
        .call()
        .map_err(|e| e.to_string())?
        .into_json::<Value>()
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn server_files() -> Result<Value, String> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_millis(3000))
        .timeout_read(std::time::Duration::from_millis(8000))
        .build();
    agent
        .get(SYNCFILES_URL)
        .call()
        .map_err(|e| e.to_string())?
        .into_json::<Value>()
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn set_folder(path: String) -> Result<(), String> {
    let mut cfg = rest_json("GET", "/rest/system/config", None)?;
    let folders = cfg
        .get_mut("folders")
        .and_then(|f| f.as_array_mut())
        .ok_or("配置缺少 folders")?;
    let f0 = folders
        .iter_mut()
        .find(|f| f.get("id").and_then(|i| i.as_str()) == Some(FOLDER_ID))
        .ok_or("未找到共享文件夹")?;
    f0["path"] = Value::String(path.clone());
    let body = serde_json::to_string(&cfg).map_err(|e| e.to_string())?;
    rest_fire("/rest/system/config", Some(&body))?;
    let _ = fs::create_dir_all(&path);
    Ok(())
}

#[tauri::command]
fn pick_folder() -> Option<String> {
    rfd::FileDialog::new().pick_folder().map(|p| p.display().to_string())
}

#[tauri::command]
fn open_folder(path: String) {
    if path.is_empty() {
        return;
    }
    #[cfg(windows)]
    let _ = Command::new("explorer").arg(&path).spawn();
    #[cfg(target_os = "macos")]
    let _ = Command::new("open").arg(&path).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = Command::new("xdg-open").arg(&path).spawn();
}

#[tauri::command]
fn open_advanced() {
    #[cfg(windows)]
    let _ = Command::new("explorer").arg("http://127.0.0.1:8384/").spawn();
    #[cfg(target_os = "macos")]
    let _ = Command::new("open").arg("http://127.0.0.1:8384/").spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = Command::new("xdg-open").arg("http://127.0.0.1:8384/").spawn();
}

#[tauri::command]
fn set_autostart(on: bool) -> Result<(), String> {
    autostart_set(on)
}

#[cfg(target_os = "macos")]
fn autostart_file() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    Path::new(&home)
        .join("Library/LaunchAgents")
        .join("cn.duixiang.linksync.plist")
}

#[cfg(all(unix, not(target_os = "macos")))]
fn autostart_file() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    let cfg = std::env::var("XDG_CONFIG_HOME").unwrap_or_else(|_| format!("{}/.config", home));
    Path::new(&cfg).join("autostart").join("linksync.desktop")
}

fn autostart_get() -> bool {
    #[cfg(windows)]
    {
        use winreg::enums::HKEY_CURRENT_USER;
        use winreg::RegKey;
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        hkcu.open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Run")
            .and_then(|k| k.get_value::<String, _>("LinkSync"))
            .is_ok()
    }
    #[cfg(not(windows))]
    {
        autostart_file().exists()
    }
}

fn autostart_set(on: bool) -> Result<(), String> {
    #[cfg(windows)]
    {
        use winreg::enums::HKEY_CURRENT_USER;
        use winreg::RegKey;
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let (run, _) = hkcu
            .create_subkey(r"Software\Microsoft\Windows\CurrentVersion\Run")
            .map_err(|e| e.to_string())?;
        // 清理旧版遗留项
        let _ = run.delete_value("duixiang-sync");
        if on {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            run.set_value("LinkSync", &format!("\"{}\" /tray", exe.display()))
                .map_err(|e| e.to_string())?;
        } else {
            let _ = run.delete_value("LinkSync");
        }
    }
    #[cfg(target_os = "macos")]
    {
        if on {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            let plist = format!(
                concat!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
                    "<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n",
                    "<plist version=\"1.0\">\n<dict>\n",
                    "  <key>Label</key><string>cn.duixiang.linksync</string>\n",
                    "  <key>ProgramArguments</key><array>\n",
                    "    <string>{}</string>\n    <string>--tray</string>\n",
                    "  </array>\n  <key>RunAtLoad</key><true/>\n",
                    "</dict>\n</plist>\n"
                ),
                exe.display()
            );
            if let Some(p) = autostart_file().parent() {
                fs::create_dir_all(p).map_err(|e| e.to_string())?;
            }
            fs::write(autostart_file(), plist).map_err(|e| e.to_string())?;
        } else {
            let _ = fs::remove_file(autostart_file());
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if on {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            let desktop = format!(
                "[Desktop Entry]\nType=Application\nName=LinkSync\nExec={} --tray\nX-GNOME-Autostart-enabled=true\n",
                exe.display()
            );
            if let Some(p) = autostart_file().parent() {
                fs::create_dir_all(p).map_err(|e| e.to_string())?;
            }
            fs::write(autostart_file(), desktop).map_err(|e| e.to_string())?;
        } else {
            let _ = fs::remove_file(autostart_file());
        }
    }
    Ok(())
}

#[tauri::command]
fn quit(app: AppHandle, state: tauri::State<AppState>) {
    EXITING.store(true, Ordering::SeqCst);
    stop_sync(&state);
    app.exit(0);
}

fn show_main<R: Runtime>(app: &AppHandle<R>) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

fn main() {
    let mut builder = tauri::Builder::default();
    #[cfg(not(target_os = "macos"))]
    {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main(app);
        }));
    }
    builder = builder
        .manage(AppState {
            child: Mutex::new(None),
            rate: Arc::new(Mutex::new(None)),
            paused: AtomicBool::new(false),
        })
        .invoke_handler(tauri::generate_handler![
            app_info, set_member, status, set_paused, get_folder, set_folder,
            pick_folder, open_folder, open_advanced, set_autostart, quit, server_info, server_files
        ])
        .setup(|app| {
            extract_payload()?;
            // 老配置被修正时，杀掉可能在跑的旧引擎，让下面 start_sync 以新配置重启
            if fix_watcher_delay() {
                kill_all_syncthing();
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
            // 已选过成员则直接拉起同步
            let state = app.state::<AppState>();
            if read_member().is_some() {
                let _ = start_sync(&state);
            }
            // 托盘
            let show = MenuItem::with_id(app, "show", "显示主界面", true, None::<&str>)?;
            let quit_i = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &quit_i])?;
            TrayIconBuilder::with_id("tray")
                .menu(&menu)
                .tooltip("LinkSync 文件同步")
                .icon(app.default_window_icon().unwrap().clone())
                .on_menu_event(|app, ev| match ev.id.as_ref() {
                    "show" => show_main(app),
                    "quit" => {
                        EXITING.store(true, Ordering::SeqCst);
                        let st = app.state::<AppState>();
                        stop_sync(&st);
                        app.exit(0);
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, ev| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = ev
                    {
                        show_main(tray.app_handle());
                    }
                })
                .build(app)?;
            // 关闭 = 隐藏到托盘
            if let Some(w) = app.get_webview_window("main") {
                let w2 = w.clone();
                w.on_window_event(move |event| {
                    if let WindowEvent::CloseRequested { api, .. } = event {
                        if !EXITING.load(Ordering::SeqCst) {
                            api.prevent_close();
                            let _ = w2.hide();
                        }
                    }
                });
            }
            // /tray（Windows）或 --tray（macOS/Linux）启动参数：开机自启时不显示窗口
            if std::env::args().any(|a| a.eq_ignore_ascii_case("/tray") || a == "--tray") {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.hide();
                }
            }
            Ok(())
        });
    builder
        .build(tauri::generate_context!())
        .expect("LinkSync 启动失败")
        .run(|app_handle, event| {
            // 退出时兜底杀掉 syncthing
            if let tauri::RunEvent::Exit = event {
                let st = app_handle.state::<AppState>();
                stop_sync(&st);
            }
        });
}
