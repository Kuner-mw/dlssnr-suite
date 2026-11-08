//! 系统与 GPU 指标采样：面板 /api/metrics 与悬浮窗（hud 模式）共用。
//! 采样全部走 /proc、/sys、nvidia-smi 与 dlssnr shm 帧计数，无额外依赖。
//! GPU 查询带 900ms 进程内缓存；CPU 使用率按调用间隔差分。

use std::sync::{Mutex, RwLock};
use std::time::{Duration, Instant};


#[derive(Default, Clone)]
pub struct Metrics {
    pub gpu_util: String,
    pub gpu_temp: String,
    pub gpu_mem: String,
    pub gpu_mem_total: String,
    pub gpu_power: String,
    pub gpu_clock: String,
    pub cpu_util: String,
    pub cpu_temp: String,
    pub cpu_power: String,
    pub cpu_clock: String,
    pub cpu_freqs: Vec<u32>,
    pub ram_used: String,
    pub ram_total: String,
    pub ram_temp: String,
    pub swap: String,
    pub fps: String,
    pub fps_avg: String,
    pub fps_low1: String,
    pub frametime_ms: String,
    pub frame_samples: Vec<f32>,
    pub time: String,
}

static GPU_LATEST: RwLock<Option<[String; 6]>> = RwLock::new(None);
static GPU_THREAD: std::sync::Once = std::sync::Once::new();
static CPU_PREV: Mutex<Option<(u64, u64, f32)>> = Mutex::new(None); // (total, idle, 最近一次利用率)
static HWMON_PATHS: Mutex<Option<(Option<std::path::PathBuf>, Option<std::path::PathBuf>)>> =
    Mutex::new(None); // (cpu_temp, ram_temp)
static FPS_HIST: Mutex<Vec<(Instant, u64)>> = Mutex::new(Vec::new());
static RAPL_PREV: Mutex<Option<(u64, Instant, Option<f32>)>> = Mutex::new(None); // (微焦, 时刻, 上次瓦特)
static RAPL_PATH: Mutex<Option<Option<std::path::PathBuf>>> = Mutex::new(None); // 探测缓存

/// 带超时的 nvidia-smi：驱动异常卡死时 kill，不拖垮调用方
fn run_nvidia_smi() -> Option<[String; 6]> {
    let mut child = std::process::Command::new("nvidia-smi")
        .args([
            "--query-gpu=utilization.gpu,temperature.gpu,memory.used,memory.total,power.draw,clocks.sm",
            "--format=csv,noheader,nounits",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + Duration::from_millis(450);
    let exited = loop {
        match child.try_wait() {
            Ok(Some(st)) => break Some(st),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(15)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    if !exited.map(|s| s.success()).unwrap_or(false) {
        return None;
    }
    let mut raw = Vec::new();
    if let Some(mut pipe) = child.stdout.take() {
        use std::io::Read as _;
        let _ = pipe.take(64 * 1024).read_to_end(&mut raw);
    }
    let line = String::from_utf8_lossy(&raw);
    let ok = line.contains(',') || !line.trim().is_empty();
    if !ok {
        return None;
    }
    let _ = exited;
    let fields: Vec<String> = line
        .split(',')
        .map(|s| s.trim().trim_end_matches('W').trim().to_string())
        .collect();
    if fields.len() < 6 {
        return None;
    }
    let mut vals: [String; 6] = Default::default();
    for (i, slot) in vals.iter_mut().enumerate() {
        *slot = fields[i].clone();
    }
    Some(vals)
}

/// 常驻采样线程（900ms 一次）：调用方只读共享结果，锁内绝不跑子进程
fn gpu_snapshot() -> [String; 6] {
    GPU_THREAD.call_once(|| {
        std::thread::spawn(|| loop {
            let vals = run_nvidia_smi().unwrap_or_default();
            if let Ok(mut w) = GPU_LATEST.write() {
                *w = Some(vals);
            }
            std::thread::sleep(Duration::from_millis(900));
        });
    });
    GPU_LATEST.read().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_default()
}

fn hwmon_paths() -> (Option<std::path::PathBuf>, Option<std::path::PathBuf>) {
    let mut guard = HWMON_PATHS.lock().unwrap();
    if let Some(p) = guard.as_ref() {
        return p.clone();
    }
    let (mut cpu, mut ram) = (None, None);
    for entry in std::fs::read_dir("/sys/class/hwmon").into_iter().flatten().flatten() {
        let dir = entry.path();
        let name = std::fs::read_to_string(dir.join("name")).unwrap_or_default();
        let name = name.trim().to_string();
        if cpu.is_none() && (name == "k10temp" || name == "coretemp" || name == "zenpower") {
            cpu = Some(dir.join("temp1_input"));
        }
        if ram.is_none() && name == "spd5118" {
            ram = Some(dir.join("temp1_input"));
        }
    }
    *guard = Some((cpu.clone(), ram.clone()));
    (cpu, ram)
}

fn read_temp_milli(path: &Option<std::path::PathBuf>) -> Option<f32> {
    let p = path.as_ref()?;
    let s = std::fs::read_to_string(p).ok()?;
    s.trim().parse::<f32>().ok().map(|v| v / 1000.0)
}

fn fmt_f1(v: f32) -> String {
    format!("{:.1}", (v * 10.0).round() / 10.0)
}

/// RAPL package 域 energy_uj 路径（AMD/Intel 都在 /sys/class/powercap/*-rapl:N，name=package-0）
fn rapl_energy_path() -> Option<std::path::PathBuf> {
    let mut guard = RAPL_PATH.lock().unwrap();
    if let Some(cached) = guard.as_ref() {
        return cached.clone();
    }
    let mut found = None;
    for entry in std::fs::read_dir("/sys/class/powercap").into_iter().flatten().flatten() {
        let dir = entry.path();
        let name = std::fs::read_to_string(dir.join("name")).unwrap_or_default();
        if name.trim().starts_with("package") && dir.join("energy_uj").exists() {
            found = Some(dir.join("energy_uj"));
            break;
        }
    }
    *guard = Some(found.clone());
    found
}

/// RAPL 能量差分包功耗（W）。读取与基线更新在同一锁内串行，防并发样本倒序；
/// 回绕 / 窗口过短 / 长时间未采样（32 位计数约 48s 回绕一圈）丢弃该窗口沿用上次值；
/// 首次采样显示 "--"（energy_uj 默认 root-only，授权可读后自动生效）
fn cpu_power_watts() -> String {
    let Some(path) = rapl_energy_path() else { return "--".into() };
    let mut prev = RAPL_PREV.lock().unwrap();
    let Ok(s) = std::fs::read_to_string(&path) else { return "--".into() };
    let Ok(uj) = s.trim().parse::<u64>() else { return "--".into() };
    let now = Instant::now();
    // max_energy_range_uj：单窗口最大可追踪能量；超过其 1000W 等效时长即可能已回绕
    let range_uj = std::fs::read_to_string(path.with_file_name("max_energy_range_uj"))
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok());
    let Some((pj, pt, last)) = *prev else {
        *prev = Some((uj, now, None));
        return "--".into();
    };
    let dt = now.duration_since(pt);
    let wrap_untraceable = range_uj
        .map(|r| dt.as_secs_f64() > r as f64 / 1_000_000.0 / 1000.0)
        .unwrap_or(false);
    if uj < pj || dt.as_secs_f32() < 0.05 || wrap_untraceable {
        if uj < pj || wrap_untraceable {
            // 回绕/超时不可追踪：以当前值为新基线，下一窗口恢复正常计算
            //（短窗 <0.05s 保留旧基线，能量留给下次累计）
            *prev = Some((uj, now, last));
        } else {
            *prev = Some((pj, pt, last));
        }
        return last.map(fmt_f1).unwrap_or_else(|| "--".into());
    }
    let w = (uj - pj) as f32 / 1_000_000.0 / dt.as_secs_f32();
    let val = if w <= 1000.0 { Some(w) } else { last };
    *prev = Some((uj, now, val));
    val.map(fmt_f1).unwrap_or_else(|| "--".into())
}

/// 全核平均频率（MHz）：由每核频率数组求平均（数组只解析一次，与 cpu_freqs 同源）
fn cpu_clock_mhz(freqs: &[u32]) -> String {
    if freqs.is_empty() {
        return "--".into();
    }
    let sum: u64 = freqs.iter().map(|&f| f as u64).sum();
    format!("{}", ((sum as f32 / freqs.len() as f32).round() as u64))
}

/// 每逻辑核当前频率（MHz）：/proc/cpuinfo 的 cpu MHz 行序即 cpu0..cpuN
fn cpu_freqs_per_core() -> Vec<u32> {
    let info = std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
    let mut v = Vec::new();
    for l in info.lines() {
        if let Some(rest) = l.strip_prefix("cpu MHz") {
            let rest = rest.trim_start_matches(|c: char| c == '\t' || c == ' ' || c == ':');
            if let Ok(f) = rest.parse::<f32>() {
                v.push(f.round() as u32);
            }
        }
    }
    v
}

/// /proc/stat 差分 CPU 利用率（%）
fn cpu_util_percent() -> String {
    let stat = std::fs::read_to_string("/proc/stat").unwrap_or_default();
    let line = stat.lines().next().unwrap_or_default();
    let vals: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .filter_map(|x| x.parse().ok())
        .collect();
    if vals.len() < 4 {
        return "--".into();
    }
    let idle = vals[3] + vals.get(4).copied().unwrap_or(0); // idle + iowait
    let total: u64 = vals.iter().sum();
    let mut prev = CPU_PREV.lock().unwrap();
    match *prev {
        Some((pt, pi, last)) => {
            let dt = total.saturating_sub(pt);
            let di = idle.saturating_sub(pi);
            let util = if dt > 0 {
                (dt.saturating_sub(di) as f32 / dt as f32 * 100.0).clamp(0.0, 100.0)
            } else {
                last
            };
            *prev = Some((total, idle, util));
            format!("{}", util.round() as u64)
        }
        None => {
            *prev = Some((total, idle, 0.0));
            "--".into()
        }
    }
}

/// dlssnr shm 帧计数（HELPER_FRAMES），不可用时 None
fn helper_frames_total() -> Option<u64> {
    // 先打开文件再量尺寸、只映射 4KB 协议头：与 open/metadata 之间的替换/截断竞态收窄
    let file = std::fs::File::open(crate::shm::shm_path()).ok()?;
    let len = file.metadata().ok()?.len();
    if len < 4096 {
        return None;
    }
    let map = unsafe { memmap2::MmapOptions::new().len(4096).map(&file).ok()? };
    let rd_u32 = |off: usize| -> u32 {
        u32::from_le_bytes([map[off], map[off + 1], map[off + 2], map[off + 3]])
    };
    // HI→LO→HI 双读防 64 位拼接撕裂
    let hi1 = rd_u32(crate::defs::status_off::HELPER_FRAMES_HI) as u64;
    let lo = rd_u32(crate::defs::status_off::HELPER_FRAMES_LO) as u64;
    let hi2 = rd_u32(crate::defs::status_off::HELPER_FRAMES_HI) as u64;
    if hi1 != hi2 {
        return None;
    }
    Some((hi1 << 32) | lo)
}

/// 帧率：瞬时（上次调用间隔）+ 10s 平均
fn fps_pair() -> (String, String) {
    let Some(frames) = helper_frames_total() else {
        return ("--".into(), "--".into());
    };
    let now = Instant::now();
    let mut hist = FPS_HIST.lock().unwrap();
    let need_push = match hist.last() {
        None => true,
        Some(&(t, _)) => now.duration_since(t) >= Duration::from_millis(100),
    };
    if need_push {
        hist.push((now, frames));
    }
    while hist.len() > 256 {
        hist.remove(0);
    }
    hist.retain(|(t, _)| now.duration_since(*t).as_secs_f32() <= 12.0);
    if hist.len() < 2 {
        return ("--".into(), "--".into());
    }
    let (t0, f0) = hist[0];
    let (t1, f1) = *hist.last().unwrap();
    // 帧计数超过 2 秒未增长，立即判为无数据，不能回放旧的平均 FPS。
    if let Some((last_change, _)) = hist.iter().rev().find(|(_, f)| *f != f1) {
        if now.duration_since(*last_change) > Duration::from_secs(2) {
            hist.clear();
            return ("--".into(), "--".into());
        }
    }
    let dt = t1.duration_since(t0).as_secs_f32();
    if dt < 0.2 {
        return ("--".into(), "--".into());
    }
    let Some(delta) = f1.checked_sub(f0) else {
        hist.clear(); // 计数回退（助手/游戏重启）→ 丢弃旧窗口
        return ("--".into(), "--".into());
    };
    let avg = delta as f32 / dt;
    if avg <= 0.0 {
        // 计数冻结（助手直通/游戏暂停）→ 显示 --，避免误报 0 帧
        return ("--".into(), "--".into());
    }
    // 瞬时：取最近 ≤2s 的窗口
    let recent = hist.iter().rev().take_while(|(t, _)| now.duration_since(*t).as_secs_f32() <= 2.0);
    let oldest = recent.last().copied().unwrap_or((t0, f0));
    let rdt = now.duration_since(oldest.0).as_secs_f32();
    let inst = if rdt >= 0.2 {
        frames.checked_sub(oldest.1).map(|d| d as f32 / rdt).unwrap_or(avg)
    } else {
        avg
    };
    (format!("{}", inst.round() as u64), format!("{}", avg.round() as u64))
}

pub fn snapshot() -> Metrics {
    let g = gpu_snapshot();
    let (cpu_p, ram_p) = hwmon_paths();
    let meminfo = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
    let mut mem_total_kb = 0u64;
    let mut mem_avail_kb = 0u64;
    let mut swap_total_kb = 0u64;
    let mut swap_free_kb = 0u64;
    for l in meminfo.lines() {
        let mut it = l.split_whitespace();
        match it.next() {
            Some("MemTotal:") => mem_total_kb = it.next().and_then(|v| v.parse().ok()).unwrap_or(0),
            Some("MemAvailable:") => mem_avail_kb = it.next().and_then(|v| v.parse().ok()).unwrap_or(0),
            Some("SwapTotal:") => swap_total_kb = it.next().and_then(|v| v.parse().ok()).unwrap_or(0),
            Some("SwapFree:") => swap_free_kb = it.next().and_then(|v| v.parse().ok()).unwrap_or(0),
            _ => {}
        }
    }
    let gib = 1024.0 * 1024.0;
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        + 8 * 3600;
    let s = now_secs % 86400;
    let cpu_freqs = cpu_freqs_per_core();
    let (mut fps, mut fps_avg) = fps_pair();
    let mut fps_low1 = "--".to_string();
    let mut frametime_ms = "--".to_string();
    let mut frame_samples = Vec::new();
    // 优先使用 MangoHud 不可见采集器的真实逐帧日志；助手帧计数仅作回退。
    let frame = crate::frame_log::snapshot();
    if frame.active {
        fps = frame.fps;
        fps_avg = frame.fps_avg;
        fps_low1 = frame.fps_low1;
        frametime_ms = frame.frametime_ms;
        frame_samples = frame.samples;
    }
    Metrics {
        gpu_util: g.first().cloned().unwrap_or_else(|| "--".into()),
        gpu_temp: g.get(1).cloned().unwrap_or_else(|| "--".into()),
        gpu_mem: g.get(2).and_then(|v| v.parse::<f64>().ok()).map(|mb| fmt_f1(mb as f32 / 1024.0)).unwrap_or_else(|| "--".into()),
        gpu_mem_total: g.get(3).and_then(|v| v.parse::<f64>().ok()).map(|mb| fmt_f1(mb as f32 / 1024.0)).unwrap_or_else(|| "--".into()),
        gpu_power: g.get(4).and_then(|v| v.parse::<f64>().ok()).map(|w| format!("{}", w.round() as u64)).unwrap_or_else(|| "--".into()),
        gpu_clock: g.get(5).cloned().unwrap_or_else(|| "--".into()),
        cpu_util: cpu_util_percent(),
        cpu_temp: read_temp_milli(&cpu_p).map(|v| format!("{}", v.round() as u64)).unwrap_or_else(|| "--".into()),
        cpu_power: cpu_power_watts(),
        cpu_clock: cpu_clock_mhz(&cpu_freqs),
        cpu_freqs,
        ram_used: if mem_total_kb > 0 { fmt_f1((mem_total_kb - mem_avail_kb) as f32 / gib) } else { "--".into() },
        ram_total: if mem_total_kb > 0 { fmt_f1(mem_total_kb as f32 / gib) } else { "--".into() },
        ram_temp: read_temp_milli(&ram_p).map(|v| format!("{}", v.round() as u64)).unwrap_or_else(|| "--".into()),
        swap: if swap_total_kb > 0 { fmt_f1((swap_total_kb - swap_free_kb) as f32 / gib) } else { "0.0".into() },
        fps,
        fps_avg,
        fps_low1,
        frametime_ms,
        frame_samples,
        time: format!("{:02}:{:02}", (s % 86400) / 3600, (s % 3600) / 60),
    }
}

pub fn to_json(m: &Metrics) -> serde_json::Value {
    serde_json::json!({
        "gpu_util": m.gpu_util, "gpu_temp": m.gpu_temp,
        "gpu_mem": m.gpu_mem, "gpu_mem_total": m.gpu_mem_total,
        "gpu_power": m.gpu_power, "gpu_clock": m.gpu_clock,
        "cpu_util": m.cpu_util, "cpu_temp": m.cpu_temp,
        "cpu_power": m.cpu_power, "cpu_clock": m.cpu_clock,
        "cpu_freqs": m.cpu_freqs,
        "ram_used": m.ram_used, "ram_total": m.ram_total,
        "ram_temp": m.ram_temp, "swap": m.swap,
        "fps": m.fps, "fps_avg": m.fps_avg,
        "fps_low1": m.fps_low1, "frametime_ms": m.frametime_ms,
        "frame_samples": m.frame_samples,
        "time": m.time,
    })
}

/// 模板占位符白名单（编辑器与悬浮窗共用同一套）
pub const PLACEHOLDERS: &[&str] = &[
    "gpu_util", "gpu_temp", "gpu_mem", "gpu_mem_total", "gpu_power", "gpu_clock",
    "cpu_util", "cpu_temp", "cpu_power", "cpu_clock", "ram_used", "ram_total", "ram_temp", "swap",
    "fps", "fps_avg", "fps_low1", "frametime_ms", "time",
];

/// 把模板里的 {key} 替换成实时值；未知占位符替换为空串（前端编辑器已白名单约束）
pub fn fill_template(tpl: &str, m: &Metrics) -> String {
    let mut out = String::with_capacity(tpl.len() + 16);
    let bytes = tpl.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{' {
            if let Some(end) = tpl[i + 1..].find('}') {
                let key = &tpl[i + 1..i + 1 + end];
                let val = match key {
                    "gpu_util" => &m.gpu_util,
                    "gpu_temp" => &m.gpu_temp,
                    "gpu_mem" => &m.gpu_mem,
                    "gpu_mem_total" => &m.gpu_mem_total,
                    "gpu_power" => &m.gpu_power,
                    "gpu_clock" => &m.gpu_clock,
                    "cpu_util" => &m.cpu_util,
                    "cpu_temp" => &m.cpu_temp,
                    "cpu_power" => &m.cpu_power,
                    "cpu_clock" => &m.cpu_clock,
                    "ram_used" => &m.ram_used,
                    "ram_total" => &m.ram_total,
                    "ram_temp" => &m.ram_temp,
                    "swap" => &m.swap,
                    "fps" => &m.fps,
                    "fps_avg" => &m.fps_avg,
                    "fps_low1" => &m.fps_low1,
                    "frametime_ms" => &m.frametime_ms,
                    "time" => &m.time,
                    _ => "",
                };
                out.push_str(val);
                i += end + 2;
                continue;
            }
        }
        let ch = tpl[i..].chars().next().unwrap_or('{');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}
