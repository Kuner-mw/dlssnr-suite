//! 系统交互：进程检测、helper 生命周期、config.ini 快照、systemd 用户服务。

use crate::defs::*;
use std::path::{Path, PathBuf};

pub fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/root".into()))
}

fn xdg_dir(var: &str, default_tail: &str) -> PathBuf {
    match std::env::var(var) {
        Ok(v) if !v.trim().is_empty() && v.starts_with('/') => PathBuf::from(v),
        _ => home().join(default_tail),
    }
}

/// 与 install.sh/uninstall.sh 相同的 XDG 口径（HOME 回退保证自包含运行也可用）
pub fn state_dir() -> PathBuf {
    xdg_dir("XDG_STATE_HOME", ".local/state").join("dlssnr")
}

pub fn config_ini() -> PathBuf {
    xdg_dir("XDG_CONFIG_HOME", ".config").join("dlssnr/config.ini")
}

pub fn games_dir() -> PathBuf {
    xdg_dir("XDG_CONFIG_HOME", ".config").join("dlssnr-panel/games")
}

pub fn data_dir() -> PathBuf {
    xdg_dir("XDG_DATA_HOME", ".local/share").join("dlssnr")
}

pub fn token_file() -> PathBuf {
    state_dir().join("webui.token")
}

pub fn log_file() -> PathBuf {
    state_dir().join("webui.log")
}

pub fn helper_bin() -> PathBuf {
    let user = home().join(".local/bin/dlssnr-helper");
    if user.exists() {
        return user;
    }
    // 系统级安装（AUR dlssnr-bin 等系统路径）回退
    for p in [
        "/usr/bin/dlssnr-helper",
        "/usr/local/bin/dlssnr-helper",
        "/usr/lib/dlssnr/bin/dlssnr-helper",
        "/usr/lib64/dlssnr/bin/dlssnr-helper",
    ] {
        let pb = PathBuf::from(p);
        if pb.exists() {
            return pb;
        }
    }
    user
}

// ── 进程检测 ────────────────────────────────────────────────────────────────

/// 目录条目安全读取（read_dir 链式 flatten 在 Rust 里类型不直观，统一走这里）
pub fn entries(dir: &Path) -> Vec<std::fs::DirEntry> {
    std::fs::read_dir(dir)
        .map(|rd| rd.filter_map(|e| e.ok()).collect())
        .unwrap_or_default()
}

pub fn pgrep(pattern: &str, exact: bool) -> Vec<u32> {
    let out = std::process::Command::new("pgrep")
        .arg(if exact { "-x" } else { "-f" })
        .arg(pattern)
        .output();
    match out {
        Ok(o) => String::from_utf8_lossy(&o.stdout)
            .split_whitespace()
            .filter_map(|s| s.parse().ok())
            .collect(),
        Err(_) => vec![],
    }
}

/// pgrep ERE 元字符转义，防特殊文件名改变匹配语义（Game(1).exe 之类）
fn regex_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        if "\\.^$|?*+()[]{}".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// 游戏运行判定：任一 profile 的 match_exe 在跑（首字符转 [] 防 pgrep 自匹配）。
pub fn game_running(match_exes: &[String]) -> bool {
    for exe in match_exes {
        let name = exe.rsplit('/').next().unwrap_or(exe).trim();
        if name.is_empty() {
            continue;
        }
        // 整名正则转义 + 边界匹配（参数开头/路径分隔符之后，参数结尾/空格之前），
        // 防 "Game.exe" 作为其他进程的参数子串被误判；pgrep 排除自身且不经 shell
        let p = format!(r#"(^|[ /\\]){}($|[ "])"#, regex_escape(name));
        let out = std::process::Command::new("pgrep")
            .args(["-f", "-i", "--"])
            .arg(&p)
            .output();
        if let Ok(o) = out {
            if String::from_utf8_lossy(&o.stdout)
                .split_whitespace()
                .any(|s| s.parse::<u32>().is_ok())
            {
                return true;
            }
        }
    }
    false
}

/// helper launcher 身份校验：helper.pid + /proc/<pid>/cmdline 含 "dlssnr"。
pub fn helper_pids() -> Vec<u32> {
    // helper.pid 与 shm 同目录（/tmp/dlssnr-$UID/），不写死 UID
    let pid_file = crate::shm::shm_path().with_file_name("helper.pid");
    let pid: u32 = match std::fs::read_to_string(pid_file) {
        Ok(s) => match s.trim().parse() {
            Ok(p) => p,
            Err(_) => return vec![],
        },
        Err(_) => return vec![],
    };
    match std::fs::read_to_string(format!("/proc/{pid}/cmdline")) {
        Ok(cmd) if cmd.contains("dlssnr") => vec![pid],
        _ => vec![],
    }
}

pub fn legacy_gui_pids() -> Vec<u32> {
    pgrep("dlssnr-gui", true)
}

/// 前缀占用判定：有 wine 进程的 WINEPREFIX 指向该前缀
/// （pfx.lock 有崩溃残留会误判，不可单独依赖）。
pub fn prefix_busy(compat_dir: &Path) -> bool {
    let pfx = match compat_dir.join("pfx").canonicalize() {
        Ok(p) => p,
        Err(_) => return false,
    };
    let me = std::process::id();
    for entry in entries(Path::new("/proc")) {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Ok(pid) = name.parse::<u32>() else { continue };
        if pid == me {
            continue;
        }
        let Ok(env) = std::fs::read(format!("/proc/{pid}/environ")) else { continue };
        for kv in String::from_utf8_lossy(&env).split('\0') {
            if let Some(v) = kv.strip_prefix("WINEPREFIX=") {
                if let Ok(true) = Path::new(v).canonicalize().map(|p| p == pfx) {
                    return true;
                }
            }
        }
    }
    false
}

// ── helper 生命周期 ─────────────────────────────────────────────────────────

pub fn helper_env() -> std::collections::HashMap<String, String> {
    let mut env: std::collections::HashMap<String, String> = std::env::vars().collect();
    env.remove("VKLayer_DLSS5"); // 与 wrapper 一致：防层递归加载进 helper
    env.insert("PROTON_USE_XALIA".into(), "0".into()); // Xalia SDL 显示崩溃噪音
    env.entry("DISPLAY".into()).or_insert_with(|| ":0".into());
    env
}

pub fn helper_start() -> Result<String, String> {
    if !helper_bin().exists() {
        return Err(format!("找不到 {}", helper_bin().display()));
    }
    if !helper_pids().is_empty() {
        return Ok("helper 已在运行".into());
    }
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_file())
        .map_err(|e| e.to_string())?;
    let mut child = std::process::Command::new(helper_bin())
        .arg("start")
        .env_clear()
        .envs(helper_env())
        .stdout(log.try_clone().map_err(|e| e.to_string())?)
        .stderr(log)
        .spawn()
        .map_err(|e| format!("spawn 失败: {e}"))?;
    // 专用线程等待子进程退出，防僵尸
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    crate::log("helper start 已发起（异步，就绪约 15s）");
    Ok("helper 启动中（约 15s 就绪），状态条稍后自动更新".into())
}

pub fn helper_stop() -> Result<String, String> {
    let out = std::process::Command::new(helper_bin())
        .arg("stop")
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        crate::log(format!("helper stop 非零退出（{}）", crate::log_field(&stderr)));
        return Err(format!("helper stop 失败（退出码 {}）", out.status.code().unwrap_or(-1)));
    }
    crate::log("helper stop 完成");
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

// ── config.ini 快照（物理键名 = 官方 ABI，不可改动） ────────────────────────

fn fmt_float(v: f64) -> String {
    format!("{v:.9}")
}

pub fn snapshot_config(shm: &crate::shm::Shm) -> Result<String, String> {
    let (globals, passes) = (
        shm.read_settings().ok_or("shm 不可用")?,
        shm.read_passes().ok_or("shm 不可用")?,
    );
    let mut values: Vec<(String, String)> = Vec::new();
    for d in DEFS.iter() {
        if !d.persist {
            continue;
        }
        let v = globals.get(d.id).ok_or("缺字段")?;
        let s = match d.kind {
            Kind::Float | Kind::Percent => fmt_float(v.as_f64().unwrap_or(0.0)),
            _ => format!("{}", v.as_i64().unwrap_or(0)),
        };
        values.push((d.ini_key.to_string(), s));
    }
    let suffixes = [
        "override_mask", "intensity", "local_structure", "local_tone",
        "skin_structure", "sharpness", "style", "preset", "auto_mask",
    ];
    for (i, pass) in passes.iter().enumerate() {
        for suf in suffixes {
            let id = pass.get(suf).map(|v| v.to_string()).ok_or("pass 缺字段")?;
            values.push((format!("set_pass_{i}_{suf}"), id));
        }
    }
    // 保留非 set_* 行与注释，仅替换已知键
    let old = std::fs::read_to_string(config_ini()).unwrap_or_default();
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for line in old.lines() {
        let key = line.split('=').next().unwrap_or("").trim().to_string();
        if let Some((_, v)) = values.iter().find(|(k, _)| *k == key) {
            out.push(format!("{key}={v}"));
            seen.insert(key);
        } else {
            out.push(line.to_string());
        }
    }
    for (k, v) in &values {
        if !seen.contains(k) {
            out.push(format!("{k}={v}"));
        }
    }
    let tmp = config_ini().with_extension("ini.tmp");
    std::fs::write(&tmp, out.join("\n") + "\n").map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, config_ini()).map_err(|e| e.to_string())?;
    Ok(format!("config.ini 已快照（{} 键）", values.len()))
}

pub fn read_config_ini() -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    if let Ok(text) = std::fs::read_to_string(config_ini()) {
        for line in text.lines() {
            let mut it = line.splitn(2, '=');
            if let (Some(k), Some(v)) = (it.next(), it.next()) {
                map.insert(k.trim().to_string(), v.trim().to_string());
            }
        }
    }
    map
}

// ── systemd 用户服务（面板自启） ────────────────────────────────────────────

const UNIT: &str = "\
[Unit]
Description=DLSSNR Web Control Panel
After=graphical-session.target
PartOf=graphical-session.target

[Service]
Type=simple
ExecStart=%h/.local/bin/dlssnr-panel serve
Restart=on-failure
RestartSec=3
Environment=DISPLAY=:0

[Install]
WantedBy=graphical-session.target
";

pub fn service(action: &str) -> Result<serde_json::Value, String> {
    let unit_dir = home().join(".config/systemd/user");
    let unit_path = unit_dir.join("dlssnr-panel.service");
    match action {
        "enable" => {
            std::fs::create_dir_all(&unit_dir).map_err(|e| e.to_string())?;
            std::fs::write(&unit_path, UNIT).map_err(|e| e.to_string())?;
            run_ctl(&["daemon-reload"])?;
            run_ctl(&["enable", "--now", "dlssnr-panel.service"])?;
            Ok(serde_json::json!({"enabled": true, "msg": "开机自启已启用（graphical-session 登录后拉起）"}))
        }
        "disable" => {
            let _ = run_ctl(&["disable", "--now", "dlssnr-panel.service"]);
            let _ = std::fs::remove_file(&unit_path);
            let _ = run_ctl(&["daemon-reload"]);
            Ok(serde_json::json!({"enabled": false, "msg": "开机自启已停用"}))
        }
        "status" => {
            let out = std::process::Command::new("systemctl")
                .args(["--user", "is-enabled", "dlssnr-panel.service"])
                .output()
                .map_err(|e| e.to_string())?;
            let enabled = String::from_utf8_lossy(&out.stdout).trim() == "enabled";
            Ok(serde_json::json!({"enabled": enabled}))
        }
        _ => Err("未知 action".into()),
    }
}

fn run_ctl(args: &[&str]) -> Result<String, String> {
    let out = std::process::Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

// ── token ───────────────────────────────────────────────────────────────────

pub fn load_or_create_token() -> String {
    std::fs::create_dir_all(state_dir()).ok();
    if let Ok(t) = std::fs::read_to_string(token_file()) {
        let t = t.trim().to_string();
        if !t.is_empty() {
            return t;
        }
    }
    let t: String = (0..32)
        .map(|_| format!("{:x}", rand_byte() % 16))
        .collect();
    let _ = std::fs::write(token_file(), &t);
    let _ = std::fs::set_permissions(&token_file(), std::os::unix::fs::PermissionsExt::from_mode(0o600));
    t
}

fn rand_byte() -> u8 {
    // /dev/urandom 兜底 std 时间熵
    use std::io::Read;
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let mut b = [0u8; 1];
        if f.read_exact(&mut b).is_ok() {
            return b[0];
        }
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as u8;
    nanos.wrapping_mul(31).wrapping_add(7)
}
