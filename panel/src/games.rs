//! 按游戏配置 + 启动监视器（自动 helper / 自动应用 / 自动滤镜 / 自动关小窗 / 退出回收）。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Serialize, Deserialize, Clone)]
pub struct GameProfile {
    pub appid: String,
    pub name: String,
    pub match_exe: Vec<String>,
    #[serde(default = "default_true")]
    pub auto_apply: bool,
    #[serde(default = "default_true")]
    pub auto_helper: bool,
    #[serde(default)]
    pub auto_filter: bool,
    /// 启动器弹出的小窗（公告/活动等）自动关闭
    #[serde(default = "default_true")]
    pub auto_close_popups: bool,
    /// 小窗标题关键词（含匹配，不区分大小写）
    #[serde(default = "default_popup_titles")]
    pub popup_titles: Vec<String>,
    #[serde(default)]
    pub filter: Option<serde_json::Value>,
    #[serde(default)]
    pub settings: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    pub passes: Vec<serde_json::Map<String, serde_json::Value>>,
    #[serde(default)]
    pub initial: Option<serde_json::Value>,
    #[serde(default)]
    pub updated_at: String,
}

fn default_true() -> bool {
    true
}

pub fn default_popup_titles() -> Vec<String> {
    vec!["公告".to_string()]
}

pub fn games_dir() -> PathBuf {
    crate::sysapi::games_dir()
}

fn profile_path(appid: &str) -> PathBuf {
    games_dir().join(format!(
        "{}.json",
        appid.replace(['/', '\\', ':'], "_")
    ))
}

pub fn load_profile(appid: &str) -> Option<GameProfile> {
    let text = std::fs::read_to_string(profile_path(appid)).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn save_profile(p: &GameProfile) -> Result<(), String> {
    std::fs::create_dir_all(games_dir()).map_err(|e| e.to_string())?;
    let tmp = profile_path(&p.appid).with_extension("json.tmp");
    std::fs::write(
        &tmp,
        serde_json::to_string_pretty(p).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, profile_path(&p.appid)).map_err(|e| e.to_string())
}

pub fn delete_profile(appid: &str) -> Result<(), String> {
    std::fs::remove_file(profile_path(appid)).map_err(|e| e.to_string())
}

fn library_dirs() -> Vec<PathBuf> {
    let steam = crate::sysapi::home().join(".local/share/Steam/steamapps");
    let mut dirs = vec![steam.clone()];
    if let Ok(text) = std::fs::read_to_string(steam.join("libraryfolders.vdf")) {
        for line in text.lines() {
            let mut it = line.trim().splitn(2, '"');
            let _ = it.next();
            if let Some(Some(v)) = it.next().map(|s| s.split('"').next()) {
                let p = v.replace("\\\\", "\\");
                if p.starts_with('/') {
                    let d = PathBuf::from(&p).join("steamapps");
                    if d.exists() && !dirs.contains(&d) {
                        dirs.push(d);
                    }
                }
            }
        }
    }
    dirs.retain(|d| d.join("compatdata").exists());
    dirs
}
fn find_game_exes(drive_c: &Path, limit: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    fn walk(dir: &Path, drive_c: &Path, out: &mut Vec<String>, depth: usize, limit: usize) {
        if out.len() >= limit || depth > 5 {
            return;
        }
        for e in crate::sysapi::entries(dir) {
            let p = e.path();
            let name = p.file_name().unwrap_or_default().to_string_lossy().into_owned();
            if p.is_dir() {
                if matches!(name.as_str(), "windows" | "users" | "ProgramData" | "Program Files (x86)") {
                    continue;
                }
                walk(&p, drive_c, out, depth + 1, limit);
            } else if name.to_lowercase().ends_with(".exe") {
                if let Ok(md) = e.metadata() {
                    if md.len() > 20 * 1048576 {
                        if let Ok(rel) = p.strip_prefix(drive_c) {
                            out.push(rel.to_string_lossy().into_owned());
                        }
                    }
                }
            }
        }
    }
    walk(drive_c, drive_c, &mut out, 0, limit);
    out
}
pub fn scan_prefixes() -> Vec<serde_json::Value> {
    let mut result = Vec::new();
    for apps in library_dirs() {
        let mut ids: Vec<PathBuf> =
            crate::sysapi::entries(&apps.join("compatdata")).iter().map(|e| e.path()).collect();
        ids.sort();
        for appdir in ids {
            if !appdir.is_dir() {
                continue;
            }
            let appid = appdir.file_name().unwrap_or_default().to_string_lossy().into_owned();
            if appid.starts_with('.') {
                continue;
            }
            let pfx = appdir.join("pfx");
            let drive_c = pfx.join("drive_c");
            let games = drive_c
                .exists()
                .then(|| find_game_exes(&drive_c, 4))
                .unwrap_or_default();
            let deployed = deployed_summary(&appdir);
            let has_content = pfx.exists()
                || deployed.as_object().is_some_and(|m| !m.is_empty());
            if !has_content {
                continue;
            }
            result.push(serde_json::json!({
                "compat_dir": appdir.display().to_string(),
                "appid": appid,
                "games": games,
                "deployed": deployed,
                "busy": crate::sysapi::prefix_busy(&appdir),
            }));
        }
    }
    result
}


pub fn deployed_summary(compat_dir: &Path) -> serde_json::Value {
    let p = compat_dir.join("upscaler_files");
    let tracked: serde_json::Value = std::fs::read_to_string(&p)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let mut out = serde_json::Map::new();
    for (section, files) in tracked.as_object().unwrap_or(&serde_json::Map::new()) {
        let Some(files) = files.as_object() else { continue };
        if files.is_empty() {
            continue;
        }
        let mut vers = std::collections::BTreeSet::new();
        for v in files.values() {
            if let Some(ver) = v.get("version").and_then(|v| v.as_str()) {
                vers.insert(ver.to_string());
            }
        }
        out.insert(
            section.clone(),
            serde_json::json!({"files": files.len(), "versions": vers.into_iter().collect::<Vec<_>>()}),
        );
    }
    serde_json::Value::Object(out)
}

pub fn list_profiles() -> Vec<GameProfile> {
    let mut out: Vec<GameProfile> = crate::sysapi::entries(&games_dir())
        .iter()
        .filter_map(|e| {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "json") {
                std::fs::read_to_string(&p)
                    .ok()
                    .and_then(|s| serde_json::from_str(&s).ok())
            } else {
                None
            }
        })
        .collect();
    out.sort_by(|a, b| a.appid.cmp(&b.appid));
    out
}

/// 活跃游戏（监视器置位）：面板调参时双写到该游戏 profile。
pub static ACTIVE_GAME: Mutex<Option<String>> = Mutex::new(None);

/// 首次迁移：games 目录为空且 config.ini 存在 → 为检测到的游戏建默认档。
pub fn migrate_from_config(shm: &crate::shm::Shm) {
    if !games_dir().exists() || list_profiles().is_empty() {
        let ini = crate::sysapi::read_config_ini();
        if ini.is_empty() {
            return;
        }
        // 从 upscaler 扫描找一个有游戏的前缀做默认档
        let prefixes = scan_prefixes();
        let target = prefixes
            .iter()
            .find(|p| p["games"].as_array().is_some_and(|g| !g.is_empty()));
        let (appid, exe) = target
            .map(|p| {
                (
                    p["appid"].as_str().unwrap_or("default").to_string(),
                    p["games"]
                        .as_array()
                        .and_then(|g| g.first())
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                )
            })
            .unwrap_or_else(|| ("default".into(), String::new()));
        let mut settings = serde_json::Map::new();
        for d in crate::defs::DEFS.iter() {
            if let Some(v) = ini.get(d.ini_key) {
                let val: serde_json::Value = match d.kind {
                    crate::defs::Kind::Float | crate::defs::Kind::Percent => {
                        serde_json::json!(v.parse::<f64>().unwrap_or(0.0))
                    }
                    _ => serde_json::json!(v.parse::<i64>().unwrap_or(0)),
                };
                settings.insert(d.id.to_string(), val);
            }
        }
        let name = exe
            .rsplit('/')
            .next()
            .unwrap_or("游戏")
            .trim_end_matches(".exe")
            .to_string();
        let p = GameProfile {
            appid,
            name,
            match_exe: if exe.is_empty() { vec![] } else { vec![exe] },
            auto_apply: true,
            auto_helper: true,
            auto_filter: false,
            auto_close_popups: true,
            popup_titles: default_popup_titles(),
            filter: None,
            settings,
            passes: vec![],
            initial: None,
            updated_at: now(),
        };
        if save_profile(&p).is_ok() {
            crate::log(format!("已从 config.ini 迁移默认游戏档: {}", p.appid));
        }
    }
    let _ = shm;
}

fn now() -> String {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("{t}")
}

/// 把某游戏的档应用到 shm（auto_apply 主体）。
pub fn apply_profile(shm: &crate::shm::Shm, p: &GameProfile) -> Result<String, String> {
    let changes: serde_json::Map<String, serde_json::Value> = p.settings.clone();
    let mut passes_obj = serde_json::Map::new();
    for (i, pass) in p.passes.iter().enumerate() {
        passes_obj.insert(i.to_string(), serde_json::Value::Object(pass.clone()));
    }
    let (n, tuning) = shm.write_settings(&changes, &passes_obj)?;
    let _ = crate::sysapi::snapshot_config(shm);
    Ok(format!(
        "已应用 {} 的配置（{} 处{}）",
        p.name,
        n,
        if tuning.is_empty() {
            String::new()
        } else {
            format!("，重建 {}", tuning.len())
        }
    ))
}

/// 运行短命令并等待退出（超时 kill 回收），防止 niri 卡死监视器线程。
fn run_with_timeout(cmd: &mut std::process::Command, ms: u64) -> Option<bool> {
    let mut child = cmd
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(s)) => return Some(s.success()),
            Ok(None) => {
                if start.elapsed() > Duration::from_millis(ms) {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => return None,
        }
    }
}

/// 列一次 niri 窗口（带 1.5s 超时防卡死监视器；niri 不可用返回 None）。
/// 返回 (窗口 id, 标题, app_id) 三元组列表。
fn list_windows() -> Option<Vec<(u64, String, String)>> {
    use std::io::Read;
    let mut child = std::process::Command::new("niri")
        .args(["msg", "--json", "windows"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let start = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) => {
                if start.elapsed() > Duration::from_millis(1500) {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => return None,
        }
    };
    if !status.success() {
        return None;
    }
    // 窗口列表通常远小于 64KB 管道缓冲，先等退出再读不会堵；超长输出会被超时兜底
    let mut out = String::new();
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    let v: serde_json::Value = serde_json::from_str(&out).ok()?;
    let arr = v.as_array()?;
    Some(
        arr.iter()
            .filter_map(|w| {
                Some((
                    w.get("id")?.as_u64()?,
                    w.get("title")?.as_str()?.to_string(),
                    w.get("app_id")?.as_str()?.to_string(),
                ))
            })
            .collect(),
    )
}

/// 自动关小窗：游戏运行期间，把该游戏弹出的启动器小窗（公告/活动等，标题命中
/// popup_titles）直接关掉。Steam 档按窗口类 `steam_app_<appid>` 精确匹配防误伤
/// 其他应用的同名窗口；手动档（manual-*，无真实 appid）退化为仅按标题匹配——
/// 关键词务必保持具体（如「公告」而非泛词）。
fn close_popups(
    wins: &[(u64, String, String)],
    p: &GameProfile,
    closed_ids: &mut std::collections::HashSet<u64>,
) {
    if !p.auto_close_popups || p.popup_titles.is_empty() {
        return;
    }
    let app_tag = format!("steam_app_{}", p.appid);
    let numeric = p.appid.chars().all(|c| c.is_ascii_digit());
    for (id, title, app_id) in wins {
        if closed_ids.contains(id) || title.is_empty() {
            continue;
        }
        if numeric && app_id != &app_tag && app_id != &format!("{app_tag}.exe") {
            continue;
        }
        let hit = p.popup_titles.iter().any(|k| {
            let k = k.trim();
            !k.is_empty() && title.to_lowercase().contains(&k.to_lowercase())
        });
        if !hit {
            continue;
        }
        let ok = run_with_timeout(
            std::process::Command::new("niri")
                .args(["msg", "action", "close-window", "--id"])
                .arg(id.to_string()),
            800,
        )
        .unwrap_or(false);
        if ok {
            closed_ids.insert(*id);
            // 标题净化：控制字符替换为空格并截断，防日志行伪造
            let safe: String =
                title.chars().map(|c| if c.is_control() { ' ' } else { c }).take(120).collect();
            crate::log(format!("auto_close_popups: 已关闭小窗「{safe}」(窗口 {id})"));
        }
    }
}

/// 监视器：每 2s 扫描；边沿触发 → helper 动态探测（1.5s）→ 自动应用 → 退出回收；
/// 游戏运行期间持续关闭启动器小窗（面板重启后首轮扫描也会补关）。
pub fn spawn_watcher(shm: Arc<crate::shm::Shm>) {
    std::thread::spawn(move || {
        let shm_cleanup = shm.clone(); // 启动清理线程用
        let mut prev: HashMap<String, bool> = HashMap::new();
        let mut first_scan = true;
        // 启动清理：无游戏在跑而 helper 挂着 = 上次会话残留，回收（省 ~700MB 显存）
        std::thread::spawn(move || {
            let shm = shm_cleanup;
            std::thread::sleep(Duration::from_secs(4));
            let any_game = list_profiles()
                .iter()
                .any(|p| !p.match_exe.is_empty() && crate::sysapi::game_running(&p.match_exe));
            // 层进程存活 = 有活跃会话（可能未建档或档匹配失效），不回收；
            // 用 LAYER_PID 存活判断而非 MODEL_UP（空转 helper 也置 1，会永不回收）
            let layer_pid = shm.read_u32(crate::defs::status_off::LAYER_PID).unwrap_or(0);
            let layer_active =
                layer_pid > 0 && std::path::Path::new(&format!("/proc/{layer_pid}")).exists();
            if !any_game && !layer_active && !crate::sysapi::helper_pids().is_empty() {
                match crate::sysapi::helper_stop() {
                    Ok(_) => crate::log("启动清理：无游戏在跑，已回收残留 helper"),
                    Err(e) => crate::log(format!("启动清理失败: {e}")),
                }
            }
        });
        loop {
            std::thread::sleep(Duration::from_secs(2));
            let profiles = list_profiles();
            let mut popup_targets: Vec<&GameProfile> = Vec::new();
            for p in &profiles {
                if p.match_exe.is_empty() {
                    continue;
                }
                let running = crate::sysapi::game_running(&p.match_exe);
                if running {
                    popup_targets.push(p);
                }
                let was = prev.insert(p.appid.clone(), running).unwrap_or(false);
                // 首轮扫描（面板刚启动/重启）：正在运行的游戏不算"启动边沿"，但要自愈补齐联动——
                // 否则游戏先于面板运行时（重启窗口错过边沿）没人拉 helper，DLSS5 一直"共享内存不可用"。
                if first_scan {
                    if running {
                        let helper_up = !crate::sysapi::helper_pids().is_empty();
                        if helper_up {
                            // 会话本就健康：只补联动目标位（调参双写），不重复应用/重建
                            *ACTIVE_GAME.lock().unwrap() = Some(p.appid.clone());
                            crate::log(format!("自愈: 游戏已在运行且助手健康，补联动位 {}", p.name));
                        } else if p.auto_helper || p.auto_apply {
                            crate::log(format!("自愈: 检测到游戏 {} 在跑但助手未启动，补齐联动", p.name));
                            on_game_up(&shm, &p);
                        }
                    }
                    continue;
                }
                if running && !was {
                    on_game_up(&shm, &p);
                } else if !running && was {
                    on_game_down(&shm, &p);
                }
            }
            // 自动关小窗：每轮只列一次窗口（带超时），各运行中档依次匹配关闭
            if !popup_targets.is_empty() {
                if let Some(wins) = list_windows() {
                    let mut closed_ids = std::collections::HashSet::new();
                    for p in popup_targets {
                        close_popups(&wins, p, &mut closed_ids);
                    }
                }
            }
            first_scan = false;
        }
    });
}

static STOPPING: AtomicBool = AtomicBool::new(false);

fn on_game_up(shm: &Arc<crate::shm::Shm>, p: &GameProfile) {
    crate::log(format!("检测到游戏启动: {} ({})", crate::log_field(&p.name), crate::log_field(&p.appid)));
    let shm = shm.clone();
    let p = p.clone();
    std::thread::spawn(move || {
        // 异步任务最长可挂 45s+，期间游戏可能已退出：每阶段前确认仍运行，
        // 避免把旧档参数应用到新会话或抢回联动位（P1：跨会话写状态）
        let alive = || crate::sysapi::game_running(&p.match_exe);
        if !alive() {
            crate::log("auto_link: 游戏在联动开始前已退出，跳过本次联动");
            return;
        }
        // helper 动态探测：1.5s 内出现 = wrapper 托管；否则面板拉起
        if p.auto_helper {
            let mut external = false;
            for _ in 0..8 {
                if !crate::sysapi::helper_pids().is_empty() {
                    external = true;
                    break;
                }
                if !alive() {
                    crate::log("auto_link: 游戏在联动过程中退出，中止");
                    return;
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            if !alive() {
                crate::log("auto_link: 游戏在探测末次等待中退出，中止联动");
                return;
            }
            if !external {
                match crate::sysapi::helper_start() {
                    Ok(msg) => crate::log(format!("auto_helper: {msg}")),
                    Err(e) => crate::log(format!("auto_helper 失败: {e}")),
                }
            } else {
                crate::log("auto_helper: helper 已在运行（面板早前拉起或手动启动）");
            }
        }
        // 自动滤镜：游戏建交换链（20s+）前写入 conf 无竞态
        if p.auto_filter && alive() {
            if let Some(f) = &p.filter {
                for (k, v) in f.as_object().unwrap_or(&serde_json::Map::new()) {
                    let s = match v {
                        serde_json::Value::String(s) => Some(s.clone()),
                        serde_json::Value::Number(n) => Some(n.to_string()),
                        _ => None,
                    };
                    if let Some(s) = s {
                        if let Err(e) = crate::vkbasalt::set_key(k, &s) {
                            crate::log(format!("auto_filter {k} 失败: {e}"));
                        }
                    }
                }
                crate::log("auto_filter: vkBasalt 配置已按该游戏档写入");
            }
        }
        // 自动应用：等 model_up（最长 45s），期间游戏退出则中止
        if p.auto_apply {
            let mut ok = false;
            for _ in 0..90 {
                if !alive() {
                    crate::log("auto_apply: 游戏在等待模型就绪期间退出，中止联动");
                    return;
                }
                if shm.read_u32(crate::defs::status_off::MODEL_UP) == Some(1)
                    && shm.available()
                {
                    ok = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(500));
            }
            if ok && alive() {
                match apply_profile(&shm, &p) {
                    Ok(msg) => crate::log(format!("auto_apply: {msg}")),
                    Err(e) => crate::log(format!("auto_apply 失败: {e}")),
                }
            } else if !ok {
                crate::log("auto_apply 超时：model_up 45s 内未就绪");
            }
        }
        if !alive() {
            crate::log("auto_link: 游戏在联动完成前退出，不置联动位");
            return;
        }
        // 活跃游戏置位（面板调参双写目标）
        *ACTIVE_GAME.lock().unwrap() = Some(p.appid.clone());
    });
}

fn on_game_down(shm: &Arc<crate::shm::Shm>, p: &GameProfile) {
    crate::log(format!(
        "检测到游戏退出: {} ({})",
        crate::log_field(&p.name),
        crate::log_field(&p.appid)
    ));
    // 多游戏并存：转移判断与置位在同一把锁内完成（防启动任务并发抢位后又被清掉）
    let profiles = list_profiles();
    let still_active: Vec<&GameProfile> = profiles
        .iter()
        .filter(|q| q.appid != p.appid && !q.match_exe.is_empty() && crate::sysapi::game_running(&q.match_exe))
        .collect();
    let mut next: Option<GameProfile> = None;
    {
        let mut cur = ACTIVE_GAME.lock().unwrap();
        if cur.as_deref() == Some(p.appid.as_str()) {
            match still_active.first() {
                Some(n) => {
                    *cur = Some(n.appid.clone());
                    next = Some((*n).clone());
                }
                None => *cur = None,
            }
        }
    }
    if let Some(nb) = next {
        crate::log(format!(
            "联动位转移 → {}（helper 继续服务，按新游戏档应用参数）",
            crate::log_field(&nb.name)
        ));
        if nb.auto_apply {
            match apply_profile(shm, &nb) {
                Ok(msg) => crate::log(format!("auto_apply(转移): {msg}")),
                Err(e) => crate::log(format!("auto_apply(转移) 失败: {e}")),
            }
        }
        return; // helper 继续服务下一个游戏
    }
    if still_active.is_empty() && !crate::sysapi::game_running(&p.match_exe) {
        if !STOPPING.swap(true, Ordering::SeqCst) {
            std::thread::spawn(move || {
                match crate::sysapi::helper_stop() {
                    Ok(_) => crate::log("auto_helper: 游戏退出，已回收 helper"),
                    Err(e) => crate::log(format!("helper 回收失败: {e}")),
                }
                STOPPING.store(false, Ordering::SeqCst);
            });
        }
    }
}
