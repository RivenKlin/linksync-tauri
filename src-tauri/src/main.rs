#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod screen_share;

use std::collections::HashSet;
use std::fs;
use std::io::{Cursor, Read, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::Value;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, Runtime, WindowEvent, Wry};

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

#[derive(Default)]
struct AppState {
    child: Mutex<Option<Child>>,
    rate: Arc<Mutex<Option<(u64, Instant)>>>,
    paused: Arc<AtomicBool>,
    pause_item: Mutex<Option<MenuItem<Wry>>>,
    latest: Arc<Mutex<Option<Status>>>,
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// UTC 秒 -> "YYYY-MM-DD HH:MM:SS"（Syncthing 时间戳均为 UTC）
fn civil_from_days(z0: i64) -> (i64, u32, u32) {
    let z = z0 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 146097;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn fmt_time(secs: u64) -> String {
    let (y, m, d) = civil_from_days((secs / 86400) as i64);
    let rem = secs % 86400;
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        y, m, d,
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

fn fmt_compact(secs: u64) -> String {
    let (y, m, d) = civil_from_days((secs / 86400) as i64);
    let rem = secs % 86400;
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        y, m, d,
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

// 相对路径安全拼接：拒绝绝对路径与 ..，防止越出同步目录
fn safe_join(folder: &str, rel: &str) -> Result<PathBuf, String> {
    let rel = rel.replace('\\', "/");
    if rel.is_empty() || rel.starts_with('/') || rel.contains(":\\") {
        return Err("非法路径".into());
    }
    let mut p = PathBuf::from(folder);
    for c in rel.split('/') {
        if c == ".." {
            return Err("非法路径".into());
        }
        if c.is_empty() || c == "." {
            continue;
        }
        p.push(c);
    }
    Ok(p)
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
    if let Some(item) = state.pause_item.lock().unwrap().as_ref() {
        let _ = item.set_text("暂停同步");
    }
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
    let key = match read_apikey() {
        Some(k) => k,
        None => return Err("无法读取 apikey".into()),
    };
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
fn rest_fire(method: &str, path: &str, body: Option<&str>) -> Result<(), String> {
    let key = read_apikey().ok_or("无法读取 apikey")?;
    let url = format!("http://127.0.0.1:8384{}", path);
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_millis(800))
        .timeout_read(std::time::Duration::from_millis(1500))
        .build();
    let mut req = match method {
        "PUT" => agent.put(&url),
        _ => agent.post(&url),
    }
    .set("X-API-Key", &key);
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
    version: String,
}

#[derive(Serialize, Clone)]
struct Status {
    connected: bool,
    peers: usize,
    paused: bool,
    state: String,
    need_bytes: u64,
    in_sync_bytes: u64,
    local_files: u64,
    rate: u64,
    recent: Vec<String>,
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
        version: env!("CARGO_PKG_VERSION").to_string(),
    }
}

// 绑定成员身份：部署配置并重启同步引擎（登录/切换身份共用）
fn bind_member(app: &AppHandle, state: &AppState, letter: &str) -> Result<(), String> {
    // 运行中切换身份：先停引擎，释放 config 目录占用
    stop_sync(state);
    std::thread::sleep(std::time::Duration::from_millis(500));
    // 记住用户自定义过的同步目录，重绑后恢复
    let keep_path = folder_path_from_config();
    deploy_member(letter)?;
    if !keep_path.is_empty() && keep_path != folder_path_from_config() {
        patch_folder_path(&keep_path)?;
    }
    start_sync(state)?;
    let _ = app.emit("member-set", ());
    Ok(())
}

#[tauri::command]
fn set_member(app: AppHandle, state: tauri::State<AppState>, letter: String) -> Result<(), String> {
    let letter = letter.trim().to_uppercase();
    if !["A", "B", "C", "D"].contains(&letter.as_str()) {
        return Err("无效成员".into());
    }
    bind_member(&app, &state, &letter)
}

// 后台线程每 2 秒计算一次并缓存；窗口隐藏时 WebView 会节流前端定时器，
// 若只靠前端轮询，同步日志/最近动态会停摆，因此计算逻辑必须放在 Rust 侧。
#[tauri::command]
fn status(state: tauri::State<AppState>) -> Status {
    if let Some(s) = state.latest.lock().unwrap().as_ref() {
        return s.clone();
    }
    compute_status(&state.rate, state.paused.load(Ordering::SeqCst))
}

fn compute_status(rate: &Arc<Mutex<Option<(u64, Instant)>>>, paused: bool) -> Status {
    let mut out = Status {
        connected: false,
        peers: 0,
        paused: false,
        state: "offline".into(),
        need_bytes: 0,
        in_sync_bytes: 0,
        local_files: 0,
        rate: 0,
        recent: Vec::new(),
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
    // 最近同步动态：扫描同步目录，收集 15 秒内修改过的文件
    collect_recent_files(&mut out.recent);
    out
}

// 最近同步动态：本机保存与队友传来的文件都会更新 mtime，直接按 mtime 判定，
// 不依赖 /rest/events（Syncthing v2 不发 LocalChangeDetected 事件）
fn collect_recent_files(recent: &mut Vec<String>) {
    let dir = folder_path_from_config();
    if dir.is_empty() {
        return;
    }
    let base = Path::new(&dir);
    let now = unix_now();
    let mut hits: Vec<(u64, String)> = Vec::new();
    let mut temps: Vec<String> = Vec::new();
    walk_recent(base, base, now, 0, &mut hits, &mut temps);
    hits.sort_by(|a, b| b.0.cmp(&a.0));
    let mut seen: Vec<String> = Vec::new();
    for (_, name) in &hits {
        if !seen.iter().any(|s| s == name) {
            seen.push(name.clone());
        }
        if seen.len() >= 10 {
            break;
        }
    }
    *recent = seen;
    // 同步日志：新出现的变更写入本地日志，方向按下载临时文件判定
    let marked: Vec<(u64, String, bool)> = hits
        .into_iter()
        .map(|(t, rel)| {
            let incoming = temps.iter().any(|x| x == &rel);
            (t, rel, incoming)
        })
        .collect();
    log_changes(&marked);
}

fn walk_recent(base: &Path, dir: &Path, now: u64, depth: u32, hits: &mut Vec<(u64, String)>, temps: &mut Vec<String>) {
    if depth > 6 {
        return;
    }
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let p = e.path();
        if p.is_dir() {
            if name == ".stfolder" || name == ".stversions" || name.starts_with(".syncthing~") {
                continue;
            }
            walk_recent(base, &p, now, depth + 1, hits, temps);
        } else if let Ok(rel) = p.strip_prefix(base) {
            let rel = rel.to_string_lossy().replace('\\', "/");
            // 下载中的临时文件 ".syncthing~<原名>.tmp" → 记录目标名，用于判定方向
            if let Some(t) = rel.strip_prefix(".syncthing~") {
                temps.push(t.strip_suffix(".tmp").unwrap_or(t).to_string());
                continue;
            }
            if let Ok(meta) = e.metadata() {
                if let Ok(tt) = meta.modified() {
                    let secs = tt.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
                    if now.saturating_sub(secs) <= 15 {
                        hits.push((secs, rel));
                    }
                }
            }
        }
    }
}

// 同步日志：%LOCALAPPDATA%\linksync\synclog.jsonl，每行 {"t":秒,"name":"相对路径","dir":"in|local"}
fn logged_set() -> &'static Mutex<HashSet<String>> {
    static LOGGED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    LOGGED.get_or_init(Default::default)
}

fn synclog_file() -> PathBuf {
    root().join("synclog.jsonl")
}

fn log_changes(hits: &[(u64, String, bool)]) {
    let mut lines = String::new();
    {
        let mut guard = logged_set().lock().unwrap();
        if guard.len() > 5000 {
            guard.clear();
        }
        for (secs, rel, incoming) in hits {
            let key = format!("{}@{}", rel, secs);
            if !guard.insert(key) {
                continue;
            }
            lines.push_str(&format!(
                "{{\"t\":{},\"name\":{},\"dir\":\"{}\"}}\n",
                secs,
                serde_json::Value::String(rel.clone()),
                if *incoming { "in" } else { "local" }
            ));
        }
    }
    if lines.is_empty() {
        return;
    }
    let f = synclog_file();
    if let Ok(meta) = fs::metadata(&f) {
        if meta.len() > 512 * 1024 {
            if let Ok(txt) = fs::read_to_string(&f) {
                let keep: Vec<&str> = txt.lines().collect();
                let start = keep.len().saturating_sub(800);
                let tail = keep[start..].join("\n");
                let _ = fs::write(&f, format!("{}\n", tail));
            }
        }
    }
    if let Ok(mut o) = fs::OpenOptions::new().create(true).append(true).open(&f) {
        let _ = o.write_all(lines.as_bytes());
    }
}

#[tauri::command]
fn sync_log() -> Vec<Value> {
    let txt = fs::read_to_string(synclog_file()).unwrap_or_default();
    let mut out: Vec<Value> = Vec::new();
    for l in txt.lines().rev() {
        if let Ok(mut v) = serde_json::from_str::<Value>(l) {
            if let Some(t) = v.get("t").and_then(|x| x.as_u64()) {
                v["time"] = Value::String(fmt_time(t));
            }
            out.push(v);
            if out.len() >= 100 {
                break;
            }
        }
    }
    out
}

#[tauri::command]
fn sync_log_clear() {
    let _ = fs::remove_file(synclog_file());
    if let Ok(mut g) = logged_set().lock() {
        g.clear();
    }
}

// 暂停/恢复统一入口：REST 触发 + 状态记录 + 托盘菜单文案同步
fn apply_paused(app: &AppHandle, paused: bool) -> Result<(), String> {
    let path = if paused { "/rest/system/pause" } else { "/rest/system/resume" };
    rest_fire("POST", path, None)?;
    let state = app.state::<AppState>();
    state.paused.store(paused, Ordering::SeqCst);
    if let Some(item) = state.pause_item.lock().unwrap().as_ref() {
        let _ = item.set_text(if paused { "恢复同步" } else { "暂停同步" });
    }
    Ok(())
}

#[tauri::command]
fn set_paused(app: AppHandle, paused: bool) -> Result<(), String> {
    apply_paused(&app, paused)
}

#[derive(Serialize)]
struct ConflictInfo {
    copy: String,
    original: String,
}

// 从冲突副本相对路径推算原文件相对路径
// Syncthing 命名规则：原文件名 + ".sync-conflict-YYYYMMDD-HHMMSS-设备ID" + 原扩展名
fn conflict_original(rel: &str) -> Option<String> {
    let name = rel.rsplit('/').next()?;
    let i = name.find(".sync-conflict-")?;
    let prefix = &name[..i];
    let rest = &name[i + ".sync-conflict-".len()..];
    let ext = match rest.find('.') {
        Some(j) => &rest[j..],
        None => "",
    };
    let dir = &rel[..rel.len() - name.len()];
    Some(format!("{}{}{}", dir, prefix, ext))
}

// 扫描同步目录中的 sync-conflict 冲突副本
fn scan_conflicts(base: &Path, dir: &Path, out: &mut Vec<ConflictInfo>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let p = e.path();
        if p.is_dir() {
            if name == ".stfolder" || name == ".stversions" || name.starts_with(".syncthing~") {
                continue;
            }
            scan_conflicts(base, &p, out);
        } else if name.contains("sync-conflict") {
            if let Ok(rel) = p.strip_prefix(base) {
                let rel = rel.to_string_lossy().replace('\\', "/");
                let original = conflict_original(&rel).unwrap_or_default();
                out.push(ConflictInfo { copy: rel, original });
            }
        }
    }
}

#[tauri::command]
fn conflicts() -> Vec<ConflictInfo> {
    let folder = folder_path_from_config();
    let mut out = Vec::new();
    if !folder.is_empty() {
        scan_conflicts(Path::new(&folder), Path::new(&folder), &mut out);
    }
    out.sort_by(|a, b| a.copy.cmp(&b.copy));
    out.truncate(200);
    out
}

// 冲突处理：delete=删除副本；replace=用副本覆盖原文件后删副本；reveal=在文件夹中定位副本
#[tauri::command]
fn conflict_action(action: String, copy: String) -> Result<(), String> {
    let folder = folder_path_from_config();
    if folder.is_empty() {
        return Err("未配置同步目录".into());
    }
    let copy_abs = safe_join(&folder, &copy)?;
    match action.as_str() {
        "delete" => {
            fs::remove_file(&copy_abs).map_err(|e| e.to_string())?;
        }
        "replace" => {
            let orig = conflict_original(&copy).ok_or("无法解析原文件")?;
            let orig_abs = safe_join(&folder, &orig)?;
            fs::copy(&copy_abs, &orig_abs).map_err(|e| e.to_string())?;
            let _ = fs::remove_file(&copy_abs);
        }
        "reveal" => {
            #[cfg(windows)]
            let _ = Command::new("explorer")
                .arg(format!("/select,{}", copy_abs.display()))
                .spawn();
            #[cfg(target_os = "macos")]
            let _ = Command::new("open")
                .args(["-R", &copy_abs.to_string_lossy()])
                .spawn();
            #[cfg(all(unix, not(target_os = "macos")))]
            if let Some(p) = copy_abs.parent() {
                open_path(&p.to_string_lossy());
            }
        }
        _ => return Err("未知操作".into()),
    }
    Ok(())
}

// 拖拽导入：把文件/文件夹复制到同步目录根，重名时自动加时间戳后缀
fn unique_target(base: &Path, name: &str) -> PathBuf {
    let t = base.join(name);
    if !t.exists() {
        return t;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    base.join(format!("{}_{}{}", stem, fmt_compact(unix_now()), ext))
}

fn import_paths(paths: &[String]) -> Result<usize, String> {
    let folder = folder_path_from_config();
    if folder.is_empty() {
        return Err("未配置同步目录".into());
    }
    let base = Path::new(&folder);
    let mut n = 0;
    for p in paths {
        let src = Path::new(p);
        if !src.exists() {
            continue;
        }
        // 已在同步目录内的文件不重复导入
        if src.starts_with(base) {
            continue;
        }
        let name = match src.file_name() {
            Some(x) => x.to_string_lossy().to_string(),
            None => continue,
        };
        let target = unique_target(base, &name);
        if src.is_dir() {
            copy_dir(src, &target).map_err(|e| format!("导入 {} 失败：{}", name, e))?;
        } else {
            fs::copy(src, &target).map_err(|e| format!("导入 {} 失败：{}", name, e))?;
        }
        n += 1;
    }
    Ok(n)
}

#[tauri::command]
fn import_files(paths: Vec<String>) -> Result<usize, String> {
    import_paths(&paths)
}

// 文件历史：扫描 .stversions（staggered 版本库），文件名形如 "a.txt~20261016-120000"
#[derive(Serialize)]
struct HistoryItem {
    version: String,
    original: String,
    time: String,
    size: u64,
}

fn is_version_ts(ts: &str) -> bool {
    ts.len() == 15
        && ts[..8].chars().all(|c| c.is_ascii_digit())
        && ts.as_bytes()[8] == b'-'
        && ts[9..].chars().all(|c| c.is_ascii_digit())
}

fn walk_history(dir: &Path, rel_prefix: &str, depth: u32, out: &mut Vec<HistoryItem>) {
    if depth > 8 {
        return;
    }
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let p = e.path();
        let rel = if rel_prefix.is_empty() {
            name.clone()
        } else {
            format!("{}/{}", rel_prefix, name)
        };
        if p.is_dir() {
            if name == ".stfolder" || name.starts_with(".syncthing~") {
                continue;
            }
            walk_history(&p, &rel, depth + 1, out);
        } else if let Some(i) = name.rfind('~') {
            let ts = &name[i + 1..];
            if !is_version_ts(ts) {
                continue;
            }
            let dir_rel = rel[..rel.len() - name.len()].to_string();
            let time = format!(
                "{}-{}-{} {}:{}:{}",
                &ts[0..4], &ts[4..6], &ts[6..8], &ts[9..11], &ts[11..13], &ts[13..15]
            );
            let size = e.metadata().map(|m| m.len()).unwrap_or(0);
            out.push(HistoryItem {
                version: rel,
                original: format!("{}{}", dir_rel, &name[..i]),
                time,
                size,
            });
        }
    }
}

#[tauri::command]
fn history_list() -> Vec<HistoryItem> {
    let folder = folder_path_from_config();
    let mut out = Vec::new();
    if !folder.is_empty() {
        let vs = Path::new(&folder).join(".stversions");
        if vs.exists() {
            walk_history(&vs, "", 0, &mut out);
        }
    }
    out.sort_by(|a, b| b.time.cmp(&a.time));
    out.truncate(300);
    out
}

fn history_original(version: &str) -> Result<String, String> {
    let v = version.trim_start_matches(".stversions/");
    let name = v.rsplit('/').next().ok_or("非法路径")?;
    let i = name.rfind('~').ok_or("非法版本名")?;
    let dir = &v[..v.len() - name.len()];
    Ok(format!("{}{}", dir, &name[..i]))
}

#[tauri::command]
fn history_restore(version: String) -> Result<(), String> {
    let folder = folder_path_from_config();
    if folder.is_empty() {
        return Err("未配置同步目录".into());
    }
    let orig = history_original(&version)?;
    let src = safe_join(&folder, &version)?;
    let dst = safe_join(&folder, &orig)?;
    if !src.exists() {
        return Err("版本文件不存在".into());
    }
    if let Some(p) = dst.parent() {
        let _ = fs::create_dir_all(p);
    }
    fs::copy(&src, &dst).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn notify(app: AppHandle, title: String, body: String) {
    use tauri_plugin_notification::NotificationExt;
    let _ = app.notification().builder().title(&title).body(&body).show();
}

#[tauri::command]
fn get_folder() -> String {
    folder_path_from_config()
}

const SYNCINFO_URL: &str = "http://103.236.70.249:8766/syncinfo.json";
const SYNCFILES_URL: &str = "http://103.236.70.249:8766/syncfiles.json";
const PEERS_URL: &str = "http://103.236.70.249:8766/peers.json";
const VERSION_URL: &str = "http://103.236.70.249:8766/version.json";
const LOGIN_URL: &str = "http://103.236.70.249:8766/login.php";
const PASSWD_URL: &str = "http://103.236.70.249:8766/passwd.php";
const SHARE_URL: &str = "http://103.236.70.249:8766/share.php";
const SIGNAL_URL: &str = "http://103.236.70.249:8766/signal.php";

fn sha256_hex(s: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    h.finalize().iter().map(|b| format!("{:02x}", b)).collect()
}

// 登录凭证（账号 + 密码哈希），供分享链接等需再次向服务器证明身份的接口复用；
// 保存的是 sha256 摘要而非明文，与服务器存储内容一致
fn cred_file() -> PathBuf {
    root().join("cred.txt")
}

fn read_cred() -> Option<(String, String)> {
    let txt = fs::read_to_string(cred_file()).ok()?;
    let mut it = txt.lines();
    let user = it.next().unwrap_or("").trim().to_string();
    let hash = it.next().unwrap_or("").trim().to_string();
    if user.is_empty() || hash.is_empty() {
        return None;
    }
    Some((user, hash))
}

fn post_json(url: &str, body: &str) -> Result<Value, String> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_millis(4000))
        .timeout_read(std::time::Duration::from_millis(8000))
        .build();
    agent
        .post(url)
        .set("Content-Type", "application/json")
        .send_string(body)
        .map_err(|_| "无法连接服务器，请检查网络后重试".to_string())?
        .into_json::<Value>()
        .map_err(|_| "服务器响应异常".to_string())
}

// 账号密码登录：客户端只发送 sha256(密码)，服务器比对存储的哈希。
// 登录成功即绑定该账号对应的成员身份并启动同步。
#[tauri::command]
fn login(app: AppHandle, state: tauri::State<AppState>, user: String, pass: String) -> Result<String, String> {
    let user = user.trim().to_uppercase();
    if user.is_empty() || pass.is_empty() {
        return Err("请输入账号和密码".into());
    }
    let hash = sha256_hex(&pass);
    let body = serde_json::json!({ "user": user, "passhash": hash }).to_string();
    let v = post_json(LOGIN_URL, &body)?;
    if v.get("ok").and_then(|b| b.as_bool()) != Some(true) {
        let msg = v
            .get("error")
            .and_then(|e| e.as_str())
            .unwrap_or("账号或密码错误")
            .to_string();
        return Err(msg);
    }
    let letter = v
        .get("member")
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .trim()
        .to_uppercase();
    if letter.is_empty() {
        return Err("服务器未返回成员信息，请联系管理员".into());
    }
    bind_member(&app, &state, &letter)?;
    let _ = fs::write(cred_file(), format!("{}\n{}", user, hash));
    Ok(letter)
}

// 自助修改密码：验证旧密码后更新服务器上的哈希，只发送 sha256，不明文过网
#[tauri::command]
fn change_password(user: String, oldpass: String, newpass: String) -> Result<(), String> {
    let user = user.trim().to_uppercase();
    if user.is_empty() || oldpass.is_empty() || newpass.is_empty() {
        return Err("请填写完整".into());
    }
    if newpass.len() < 6 {
        return Err("新密码至少 6 位".into());
    }
    let newhash = sha256_hex(&newpass);
    let body = serde_json::json!({
        "user": user,
        "oldhash": sha256_hex(&oldpass),
        "newhash": newhash
    })
    .to_string();
    let v = post_json(PASSWD_URL, &body)?;
    if v.get("ok").and_then(|b| b.as_bool()) != Some(true) {
        let msg = v
            .get("error")
            .and_then(|e| e.as_str())
            .unwrap_or("旧密码错误")
            .to_string();
        return Err(msg);
    }
    // 同步更新本地凭证，保证分享等接口继续可用
    let _ = fs::write(cred_file(), format!("{}\n{}", user, newhash));
    Ok(())
}

// 分享链接：对同步目录文件生成带有效期的临时下载链接
#[tauri::command]
fn share_link(path: String, days: u32) -> Result<Value, String> {
    let (user, hash) = read_cred().ok_or("请先登录后再使用分享功能")?;
    let body = serde_json::json!({ "user": user, "passhash": hash, "path": path, "days": days }).to_string();
    let v = post_json(SHARE_URL, &body)?;
    if v.get("ok").and_then(|b| b.as_bool()) != Some(true) {
        let msg = v
            .get("error")
            .and_then(|e| e.as_str())
            .unwrap_or("生成失败")
            .to_string();
        return Err(msg);
    }
    Ok(v)
}

#[derive(Serialize)]
struct RateLimit {
    send: u64,
    recv: u64,
}

// 读取当前带宽限制（KB/s，0 表示不限速）
#[tauri::command]
fn get_rate_limit() -> Result<RateLimit, String> {
    let cfg = rest_json("GET", "/rest/config", None)?;
    let o = cfg.get("options");
    Ok(RateLimit {
        send: o.and_then(|x| x.get("maxSendKbps")).and_then(|n| n.as_u64()).unwrap_or(0),
        recv: o.and_then(|x| x.get("maxRecvKbps")).and_then(|n| n.as_u64()).unwrap_or(0),
    })
}

// 设置带宽限制：改 Syncthing 全局配置 options.maxSendKbps/maxRecvKbps 并回写
#[tauri::command]
fn set_rate_limit(send: u64, recv: u64) -> Result<(), String> {
    if send > 10_485_760 || recv > 10_485_760 {
        return Err("限速数值过大（上限 10485760 KB/s）".into());
    }
    let mut cfg = rest_json("GET", "/rest/config", None)?;
    let opts = cfg
        .get_mut("options")
        .and_then(|c| c.as_object_mut())
        .ok_or("配置缺少 options")?;
    opts["maxSendKbps"] = serde_json::json!(send);
    opts["maxRecvKbps"] = serde_json::json!(recv);
    let body = serde_json::to_string(&cfg).map_err(|e| e.to_string())?;
    rest_fire("PUT", "/rest/config", Some(&body))
}

// 新版本检测：服务器 version.json 形如 {"latest":"2.1.0","notes":"更新说明"}
#[tauri::command]
fn check_update() -> Result<Value, String> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_millis(3000))
        .timeout_read(std::time::Duration::from_millis(5000))
        .build();
    agent
        .get(VERSION_URL)
        .call()
        .map_err(|e| e.to_string())?
        .into_json::<Value>()
        .map_err(|e| e.to_string())
}

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
fn server_peers() -> Result<Value, String> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_millis(3000))
        .timeout_read(std::time::Duration::from_millis(5000))
        .build();
    agent
        .get(PEERS_URL)
        .call()
        .map_err(|e| e.to_string())?
        .into_json::<Value>()
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn set_folder(path: String) -> Result<(), String> {
    let mut cfg = rest_json("GET", "/rest/config", None)?;
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
    rest_fire("PUT", "/rest/config", Some(&body))?;
    let _ = fs::create_dir_all(&path);
    Ok(())
}

#[tauri::command]
fn pick_folder() -> Option<String> {
    rfd::FileDialog::new().pick_folder().map(|p| p.display().to_string())
}

fn open_path(path: &str) {
    #[cfg(windows)]
    let _ = Command::new("explorer").arg(path).spawn();
    #[cfg(target_os = "macos")]
    let _ = Command::new("open").arg(path).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = Command::new("xdg-open").arg(path).spawn();
}

#[tauri::command]
fn open_folder(path: String) {
    if path.is_empty() {
        return;
    }
    open_path(&path);
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

// 屏幕共享：启动（信令走 8766 信箱轮询）与停止。
// target 为对端成员字母（A-D）；本端 offer 写入对方信箱，后台线程逐条 trickle
// 本端 ICE candidate 并轮询自己信箱，收到 answer/candidate 即提交给 WebRTC 会话。
#[tauri::command]
fn share_start(target: String, cfg: screen_share::ShareConfig) -> Result<Value, String> {
    let (user, hash) = read_cred().ok_or("请先登录后再使用屏幕共享")?;
    let target = target.trim().to_uppercase();
    if target.len() != 1 || !target.as_bytes()[0].is_ascii_alphabetic() {
        return Err("目标成员无效".into());
    }
    // 清空自己信箱，避免收到上一场会话的残留消息
    let _ = post_json(
        SIGNAL_URL,
        &serde_json::json!({ "user": user, "passhash": hash, "action": "reset" }).to_string(),
    );
    let offer = screen_share::start(cfg)?;
    let sdp = offer.get("sdp").and_then(|s| s.as_str()).unwrap_or("").to_string();
    if sdp.is_empty() {
        screen_share::stop();
        return Err("offer 为空".into());
    }
    let send_ok = match post_json(
        SIGNAL_URL,
        &serde_json::json!({
            "user": user, "passhash": hash, "action": "send",
            "to": target, "kind": "offer", "data": sdp
        })
        .to_string(),
    ) {
        Ok(v) => v.get("ok").and_then(|b| b.as_bool()) == Some(true),
        Err(_) => false,
    };
    if !send_ok {
        screen_share::stop();
        return Err("信令服务器不可用，请稍后重试".into());
    }
    // 后台信令线程：会话停止（is_running 变 false）即退出
    std::thread::spawn(move || {
        let mut since: u64 = 0;
        let mut tick: u64 = 0;
        while screen_share::is_running() {
            // 每 10 秒全量重发一次，防止对端晚到或信箱过期收不到早期 candidate
            let cands = if tick % 10 == 0 {
                screen_share::all_candidates()
            } else {
                screen_share::take_candidates()
            };
            for c in cands {
                let _ = post_json(
                    SIGNAL_URL,
                    &serde_json::json!({
                        "user": user, "passhash": hash, "action": "send",
                        "to": target, "kind": "candidate", "data": c
                    })
                    .to_string(),
                );
            }
            if let Ok(v) = post_json(
                SIGNAL_URL,
                &serde_json::json!({
                    "user": user, "passhash": hash, "action": "poll", "since": since
                })
                .to_string(),
            ) {
                if let Some(msgs) = v.get("msgs").and_then(|m| m.as_array()) {
                    for m in msgs {
                        if let Some(id) = m.get("id").and_then(|i| i.as_u64()) {
                            if id > since {
                                since = id;
                            }
                        }
                        let kind = m.get("kind").and_then(|k| k.as_str()).unwrap_or("");
                        let data = m.get("data").and_then(|d| d.as_str()).unwrap_or("");
                        match kind {
                            "answer" => {
                                let _ = screen_share::submit_answer(data.to_string());
                            }
                            "candidate" => {
                                let _ = screen_share::submit_candidate(data.to_string());
                            }
                            _ => {}
                        }
                    }
                }
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
            tick += 1;
        }
        // 会话结束后通知观看方
        let _ = post_json(
            SIGNAL_URL,
            &serde_json::json!({
                "user": user, "passhash": hash, "action": "send",
                "to": target, "kind": "stop", "data": ""
            })
            .to_string(),
        );
    });
    Ok(offer)
}

#[tauri::command]
fn share_stop() {
    screen_share::stop();
}

// 屏幕共享是否在运行（前端打开弹窗时恢复按钮状态）
#[tauri::command]
fn share_status() -> bool {
    screen_share::is_running()
}

// 未接受的共享邀请缓存：轮询游标前进后邀请不会消失，接受成功或收到对方 stop 才移除
fn pending_invites() -> &'static std::sync::Mutex<Vec<Value>> {
    static P: std::sync::OnceLock<std::sync::Mutex<Vec<Value>>> = std::sync::OnceLock::new();
    P.get_or_init(Default::default)
}

// 观看端：拉取信箱中的共享邀请（kind=offer），返回全部待接受邀请
#[tauri::command]
fn viewer_invites() -> Result<Value, String> {
    let (user, hash) = read_cred().ok_or("请先登录后再使用屏幕共享")?;
    static SINCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let since = SINCE.load(std::sync::atomic::Ordering::Relaxed);
    let v = post_json(
        SIGNAL_URL,
        &serde_json::json!({ "user": user, "passhash": hash, "action": "poll", "since": since })
            .to_string(),
    )?;
    let mut maxid = since;
    if let Some(msgs) = v.get("msgs").and_then(|m| m.as_array()) {
        for m in msgs {
            let id = m.get("id").and_then(|i| i.as_u64()).unwrap_or(0);
            if id > maxid {
                maxid = id;
            }
            let kind = m.get("kind").and_then(|k| k.as_str()).unwrap_or("");
            let from = m.get("from").and_then(|f| f.as_str()).unwrap_or("").to_string();
            let mut p = pending_invites().lock().unwrap();
            if kind == "offer" {
                if !p.iter().any(|x| x.get("id").and_then(|i| i.as_u64()) == Some(id)) {
                    p.push(serde_json::json!({
                        "from": from,
                        "id": id,
                        "sdp": m.get("data").and_then(|d| d.as_str()).unwrap_or("")
                    }));
                }
            } else if kind == "stop" {
                // 对方共享已结束，移除其所有待接受邀请
                p.retain(|x| x.get("from").and_then(|f| f.as_str()) != Some(from.as_str()));
            }
        }
    }
    SINCE.store(maxid, std::sync::atomic::Ordering::Relaxed);
    Ok(Value::Array(pending_invites().lock().unwrap().clone()))
}

// 观看端：接受邀请。answer 写回邀请方信箱，后台线程 trickle candidate 并轮询
#[tauri::command]
fn viewer_start(from: String, id: u64, sdp: String) -> Result<Value, String> {
    let (user, hash) = read_cred().ok_or("请先登录后再使用屏幕共享")?;
    let from = from.trim().to_uppercase();
    if from.len() != 1 || !from.as_bytes()[0].is_ascii_alphabetic() {
        return Err("来源成员无效".into());
    }
    if sdp.is_empty() {
        return Err("offer 为空".into());
    }
    let answer = screen_share::viewer_start(sdp)?;
    let sdp_out = answer.get("sdp").and_then(|s| s.as_str()).unwrap_or("").to_string();
    if sdp_out.is_empty() {
        screen_share::viewer_stop();
        return Err("answer 为空".into());
    }
    let send_ok = match post_json(
        SIGNAL_URL,
        &serde_json::json!({
            "user": user, "passhash": hash, "action": "send",
            "to": from, "kind": "answer", "data": sdp_out
        })
        .to_string(),
    ) {
        Ok(v) => v.get("ok").and_then(|b| b.as_bool()) == Some(true),
        Err(_) => false,
    };
    if !send_ok {
        screen_share::viewer_stop();
        return Err("信令服务器不可用，请稍后重试".into());
    }
    // 接受成功，从待接受邀请缓存中移除
    pending_invites().lock().unwrap().retain(|x| {
        x.get("id").and_then(|i| i.as_u64()) != Some(id)
    });
    std::thread::spawn(move || {
        // 以邀请消息 id 为起点，只收其后产生的 candidate
        let mut since: u64 = id;
        let mut tick: u64 = 0;
        while screen_share::viewer_is_running() {
            // 每 10 秒全量重发一次，防止对方早期 candidate 因信箱过期丢失
            let cands = if tick % 10 == 0 {
                screen_share::viewer_all_candidates()
            } else {
                screen_share::viewer_take_candidates()
            };
            for c in cands {
                let _ = post_json(
                    SIGNAL_URL,
                    &serde_json::json!({
                        "user": user, "passhash": hash, "action": "send",
                        "to": from, "kind": "candidate", "data": c
                    })
                    .to_string(),
                );
            }
            if let Ok(v) = post_json(
                SIGNAL_URL,
                &serde_json::json!({
                    "user": user, "passhash": hash, "action": "poll", "since": since
                })
                .to_string(),
            ) {
                if let Some(msgs) = v.get("msgs").and_then(|m| m.as_array()) {
                    for m in msgs {
                        if let Some(mid) = m.get("id").and_then(|i| i.as_u64()) {
                            if mid > since {
                                since = mid;
                            }
                        }
                        let kind = m.get("kind").and_then(|k| k.as_str()).unwrap_or("");
                        let data = m.get("data").and_then(|d| d.as_str()).unwrap_or("");
                        match kind {
                            "candidate" => {
                                let _ = screen_share::viewer_submit_candidate(data.to_string());
                            }
                            "stop" => {
                                screen_share::viewer_stop();
                                break;
                            }
                            _ => {}
                        }
                    }
                }
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
            tick += 1;
        }
    });
    Ok(answer)
}

// 观看会话是否在运行（前端据此自动关闭观看画面）
#[tauri::command]
fn viewer_status() -> bool {
    screen_share::viewer_is_running()
}

// 观看端：取走已重组的视频帧（base64 Annex-B + 关键帧标记）
#[tauri::command]
fn viewer_frames() -> Vec<Value> {
    screen_share::frames_base64(screen_share::viewer_take_frames())
        .into_iter()
        .map(|(b, k)| serde_json::json!({ "b": b, "k": k }))
        .collect()
}

#[tauri::command]
fn viewer_stop_share() {
    screen_share::viewer_stop();
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

// 从命令行参数中挑出真实存在的文件路径（排除 /tray 等启动参数与 exe 自身）
fn file_args<I: Iterator<Item = S>, S: AsRef<str>>(iter: I) -> Vec<String> {
    let exe = std::env::current_exe()
        .ok()
        .map(|p| p.to_string_lossy().to_lowercase());
    iter.filter(|a| {
        let l = a.as_ref().to_lowercase();
        if l == "/tray" || l == "--tray" || l.starts_with('-') || Some(&l) == exe.as_ref() {
            return false;
        }
        Path::new(a.as_ref()).exists()
    })
    .map(|a| a.as_ref().to_string())
    .collect()
}

// Windows：在"发送到"菜单里放置 LinkSync 快捷方式，右键文件即可加入同步
#[cfg(windows)]
fn ensure_sendto() {
    let appdata = match std::env::var("APPDATA") {
        Ok(a) => a,
        Err(_) => return,
    };
    let dir = Path::new(&appdata).join("Microsoft\\Windows\\SendTo");
    let lnk = dir.join("LinkSync.lnk");
    if lnk.exists() {
        return;
    }
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(_) => return,
    };
    let _ = fs::create_dir_all(&dir);
    // 写临时 ps1 用 -File 执行，避免跨进程传参时引号被破坏；BOM 保证中文描述正确解析
    let script = format!(
        "$w=New-Object -ComObject WScript.Shell;$s=$w.CreateShortcut('{}');$s.TargetPath='{}';$s.Description='LinkSync 文件同步';$s.Save()",
        lnk.display().to_string().replace('\'', "''"),
        exe.display().to_string().replace('\'', "''")
    );
    let tmp = std::env::temp_dir().join("linksync_sendto.ps1");
    let _ = fs::write(&tmp, format!("\u{feff}{}", script));
    let _ = Command::new("powershell")
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-WindowStyle",
            "Hidden",
            "-File",
            &tmp.to_string_lossy(),
        ])
        .creation_flags(NO_WINDOW)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .output();
    let _ = fs::remove_file(&tmp);
}

#[cfg(not(windows))]
fn ensure_sendto() {}

fn main() {
    let mut builder = tauri::Builder::default();
    #[cfg(not(target_os = "macos"))]
    {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            // 右键"发送到 LinkSync"：把文件复制进同步目录并通知，不弹主窗口
            let paths = file_args(args.iter().skip(1));
            if !paths.is_empty() {
                use tauri_plugin_notification::NotificationExt;
                if let Ok(n) = import_paths(&paths) {
                    if n > 0 {
                        let _ = app
                            .notification()
                            .builder()
                            .title("已加入同步")
                            .body(format!("{} 项已放入同步目录", n))
                            .show();
                    }
                }
                return;
            }
            show_main(app);
        }));
    }
    builder = builder
        .plugin(tauri_plugin_notification::init())
        .manage(AppState {
            child: Mutex::new(None),
            rate: Arc::new(Mutex::new(None)),
            paused: Arc::new(AtomicBool::new(false)),
            pause_item: Mutex::new(None),
            latest: Arc::new(Mutex::new(None)),
        })
        .invoke_handler(tauri::generate_handler![
            app_info, set_member, login, change_password, status, set_paused, get_folder, set_folder,
            pick_folder, open_folder, open_advanced, set_autostart, quit, server_info, server_files,
            server_peers, conflicts, conflict_action, notify,
            import_files, history_list, history_restore, sync_log, sync_log_clear, check_update,
            share_link, get_rate_limit, set_rate_limit, share_start, share_stop, share_status,
            viewer_invites, viewer_start, viewer_status, viewer_frames, viewer_stop_share
        ])
        .setup(|app| {
            extract_payload()?;
            ensure_sendto();
            // 老配置被修正时，杀掉可能在跑的旧引擎，让下面 start_sync 以新配置重启
            if fix_watcher_delay() {
                kill_all_syncthing();
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
            // 已选过成员则直接拉起同步
            let state = app.state::<AppState>();
            if read_member().is_some() {
                let _ = start_sync(&state);
                // 首次启动时带文件参数（右键发送）：直接导入
                let paths = file_args(std::env::args().skip(1));
                if !paths.is_empty() {
                    let _ = import_paths(&paths);
                }
            }
            // 后台状态轮询线程：不依赖前端定时器，窗口隐藏时同步日志照常记录
            {
                let rate = state.rate.clone();
                let paused = state.paused.clone();
                let latest = state.latest.clone();
                std::thread::spawn(move || loop {
                    let s = compute_status(&rate, paused.load(Ordering::SeqCst));
                    *latest.lock().unwrap() = Some(s);
                    std::thread::sleep(std::time::Duration::from_secs(2));
                });
            }
            // 托盘
            let show = MenuItem::with_id(app, "show", "显示主界面", true, None::<&str>)?;
            let open_i = MenuItem::with_id(app, "open", "打开同步文件夹", true, None::<&str>)?;
            let pause_i = MenuItem::with_id(app, "pause", "暂停同步", true, None::<&str>)?;
            let quit_i = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            {
                let st = app.state::<AppState>();
                let paused = st.paused.load(Ordering::SeqCst);
                let _ = pause_i.set_text(if paused { "恢复同步" } else { "暂停同步" });
                *st.pause_item.lock().unwrap() = Some(pause_i.clone());
            }
            let menu = Menu::with_items(app, &[&show, &open_i, &pause_i, &quit_i])?;
            TrayIconBuilder::with_id("tray")
                .menu(&menu)
                .tooltip("LinkSync 文件同步")
                .icon(app.default_window_icon().unwrap().clone())
                .on_menu_event(|app, ev| match ev.id.as_ref() {
                    "show" => show_main(app),
                    "open" => {
                        let p = folder_path_from_config();
                        if !p.is_empty() {
                            open_path(&p);
                        }
                    }
                    "pause" => {
                        let cur = app.state::<AppState>().paused.load(Ordering::SeqCst);
                        let _ = apply_paused(app, !cur);
                    }
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
