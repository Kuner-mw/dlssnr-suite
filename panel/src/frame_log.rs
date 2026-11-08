//! MangoHud 不可见采集器的 CSV 帧日志读取。
//! log_interval=1 为高频采样（MangoHud 没有逐帧 CSV 导出，1% Low 是估算值）。
//! 只消费接管后的新增日志，绝不把历史行当实时数据。
//! 文件按 dev+inode 识别；当前文件仍活跃时保持粘滞选择，避免多日志来回跳。

use std::collections::VecDeque;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Once, RwLock};
use std::time::{Duration, Instant, SystemTime};

const MAX_READ_PER_POLL: u64 = 2 * 1024 * 1024;
// 逐帧模式下 12 秒窗口的有界容量；极高帧率超出时只保留最新样本。
const MAX_SAMPLES: usize = 16384;
const MAX_DIR_BYTES: u64 = 256 * 1024 * 1024;
const MAX_ACTIVE_BYTES: u64 = 16 * 1024 * 1024;

pub fn log_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("/run/user/{}", unsafe { libc::geteuid() })))
        .join("dlssnr/mangohud-logs")
}

#[derive(Default, Clone)]
pub struct FrameStats {
    pub active: bool,
    pub fps: String,
    pub fps_avg: String,
    pub fps_low1: String,
    pub frametime_ms: String,
    pub samples: Vec<f32>,
}

static LATEST: RwLock<Option<FrameStats>> = RwLock::new(None);
static THREAD: Once = Once::new();

#[derive(Clone, Copy, PartialEq, Eq)]
struct FileId {
    dev: u64,
    ino: u64,
}

#[derive(Default)]
struct Cursor {
    path: Option<PathBuf>,
    id: Option<FileId>,
    offset: u64,
    pending: String,
    align_next: bool,
}

fn fresh(mtime: SystemTime) -> bool {
    match SystemTime::now().duration_since(mtime) {
        Ok(age) => age <= Duration::from_secs(3),
        // 只容忍 1 秒时钟偏差，未来文件不会永久压住真实日志。
        Err(future) => future.duration() <= Duration::from_secs(1),
    }
}

fn csv_candidates() -> Vec<(PathBuf, SystemTime, u64)> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(log_dir()).into_iter().flatten().flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(".csv") || name.ends_with("_summary.csv") {
            continue;
        }
        let Ok(meta) = std::fs::symlink_metadata(entry.path()) else { continue };
        if !meta.file_type().is_file() {
            continue;
        }
        let Ok(mtime) = meta.modified() else { continue };
        if fresh(mtime) {
            out.push((path, mtime, meta.len()));
        }
    }
    out
}

fn choose_csv(current: Option<&Path>) -> Option<(PathBuf, u64)> {
    let candidates = csv_candidates();
    if let Some(cur) = current {
        // 当前会话仍活跃就保持它；两个游戏同时写时不在文件之间抖动。
        if let Some((path, _, len)) = candidates.iter().find(|(p, _, _)| p.as_path() == cur) {
            return Some((path.clone(), *len));
        }
    }
    candidates
        .into_iter()
        .max_by_key(|(_, mtime, _)| *mtime)
        .map(|(path, _, len)| (path, len))
}

fn open_log_rw(path: &Path) -> Option<std::fs::File> {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .ok()
}

fn trim_file_handle(file: &std::fs::File) -> bool {
    let lock_path = log_dir().join(".trim.lock");
    let Ok(lock) = std::fs::OpenOptions::new().create(true).write(true).open(lock_path) else {
        return false;
    };
    if unsafe { libc::flock(std::os::fd::AsRawFd::as_raw_fd(&lock), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return false;
    }
    // 只截断已经打开并通过身份校验的句柄，绝不按路径二次打开。
    let result = file.set_len(0).is_ok();
    unsafe {
        libc::flock(std::os::fd::AsRawFd::as_raw_fd(&lock), libc::LOCK_UN);
    }
    result
}

fn trim_path_safely(path: &Path) -> bool {
    let Some(file) = open_log_rw(path) else { return false };
    let Ok(meta) = file.metadata() else { return false };
    if !meta.is_file() || meta.len() <= MAX_ACTIVE_BYTES {
        return false;
    }
    let Ok(mtime) = meta.modified() else { return false };
    if !fresh(mtime) {
        return false;
    }
    trim_file_handle(&file)
}

fn poll_file(cursor: &mut Cursor, samples: &mut VecDeque<(Instant, f32, f32)>) {
    let Some((path, len)) = choose_csv(cursor.path.as_deref()) else {
        *cursor = Cursor::default();
        samples.clear();
        return;
    };
    if cursor.path.as_deref() != Some(path.as_path()) {
        // 首次接管已有文件从 EOF 开始；历史日志不属于当前 12 秒窗口。
        *cursor = Cursor {
            path: Some(path),
            id: None,
            offset: len,
            pending: String::new(),
            align_next: false,
        };
        samples.clear();
        return;
    }
    let Some(mut file) = open_log_rw(&path) else { return };
    let Ok(meta) = file.metadata() else { return };
    let id = FileId { dev: meta.dev(), ino: meta.ino() };
    if cursor.id != Some(id) {
        // 同路径原子替换也必须视为新会话，不能沿用旧 offset 或旧样本。
        cursor.id = Some(id);
        cursor.offset = meta.len();
        cursor.pending.clear();
        cursor.align_next = false;
        samples.clear();
        return;
    }
    let len = meta.len();
    // 已消费内容超过 16MB 时截断活跃日志；内存中的近期样本保留，磁盘不无限增长。
    if len > MAX_ACTIVE_BYTES && cursor.offset >= len && trim_file_handle(&file) {
        cursor.offset = 0;
        cursor.pending.clear();
        cursor.align_next = false;
        return;
    }
    if len < cursor.offset {
        cursor.offset = 0;
        cursor.pending.clear();
        cursor.align_next = false;
        // 截断后样本最多自然保留 12 秒，不会把很久以前的数据当实时数据。
    }
    if len.saturating_sub(cursor.offset) > MAX_READ_PER_POLL {
        cursor.offset = len - MAX_READ_PER_POLL;
        cursor.pending.clear();
        cursor.align_next = true;
    }
    if file.seek(SeekFrom::Start(cursor.offset)).is_err() {
        return;
    }
    let mut raw = Vec::new();
    if file.take(MAX_READ_PER_POLL).read_to_end(&mut raw).is_err() {
        return;
    }
    if raw.is_empty() {
        return;
    }
    cursor.offset += raw.len() as u64;
    cursor.pending.push_str(&String::from_utf8_lossy(&raw));
    if cursor.align_next {
        // 积压跳转后的首个残行不解析，先对齐到下一个完整行。
        if let Some(nl) = cursor.pending.find('\n') {
            cursor.pending = cursor.pending[nl + 1..].to_string();
            cursor.align_next = false;
        } else {
            if cursor.pending.len() > 64 * 1024 {
                cursor.pending.clear();
                cursor.align_next = false;
            }
            return;
        }
    }
    let Some(last_newline) = cursor.pending.rfind('\n') else {
        if cursor.pending.len() > 64 * 1024 {
            cursor.pending.clear();
        }
        return;
    };
    let complete = cursor.pending[..=last_newline].to_string();
    cursor.pending = cursor.pending[last_newline + 1..].to_string();
    let now = Instant::now();
    for line in complete.lines() {
        let line = line.trim_end_matches('\r').trim();
        if line.is_empty() || line.starts_with("fps,") || line.starts_with("os,") {
            continue;
        }
        let mut fields = line.split(',');
        let (Some(fps), Some(ms)) = (
            fields.next().and_then(|v| v.trim().parse::<f32>().ok()),
            fields.next().and_then(|v| v.trim().parse::<f32>().ok()),
        ) else {
            continue;
        };
        if fps.is_finite() && ms.is_finite() && fps > 0.0 && fps <= 100000.0 && ms > 0.0 && ms <= 10000.0 {
            samples.push_back((now, fps, ms));
            while samples.len() > MAX_SAMPLES {
                samples.pop_front();
            }
        }
    }
}

fn dir_csv_size() -> u64 {
    std::fs::read_dir(log_dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.metadata().ok())
        .map(|m| m.len())
        .sum()
}

fn cleanup_logs() {
    let lock_path = log_dir().join(".cleanup.lock");
    let Ok(lock) = std::fs::OpenOptions::new().create(true).write(true).open(lock_path) else {
        return;
    };
    // 面板与悬浮窗各自有采集线程，跨进程 flock 防止并发清理超额删除。
    if unsafe { libc::flock(std::os::fd::AsRawFd::as_raw_fd(&lock), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return;
    }
    struct LockGuard(std::fs::File);
    impl Drop for LockGuard {
        fn drop(&mut self) {
            unsafe {
                libc::flock(std::os::fd::AsRawFd::as_raw_fd(&self.0), libc::LOCK_UN);
            }
        }
    }
    let _guard = LockGuard(lock);

    let mut files: Vec<(PathBuf, u64, SystemTime)> = Vec::new();
    for entry in std::fs::read_dir(log_dir()).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.extension().and_then(|x| x.to_str()) != Some("csv") {
            continue;
        }
        let Ok(meta) = std::fs::symlink_metadata(entry.path()) else { continue };
        if !meta.file_type().is_file() {
            continue;
        }
        let Ok(mtime) = meta.modified() else { continue };
        let is_fresh = fresh(mtime);
        // 年龄清理与容量清理都保护所有近期仍在写入的文件，而非只保护当前选中项。
        if !is_fresh
            && SystemTime::now().duration_since(mtime).map(|d| d > Duration::from_secs(86400)).unwrap_or(false)
        {
            let _ = std::fs::remove_file(&path);
            continue;
        }
        if is_fresh && meta.len() > MAX_ACTIVE_BYTES {
            // 其他活跃会话也限制磁盘体积；对应读取方会从文件头继续增量消费。
            let _ = trim_path_safely(&path);
        }
        files.push((path, meta.len(), mtime));
    }
    files.sort_by_key(|(_, _, t)| *t);
    for (path, _, mtime) in files {
        if dir_csv_size() <= MAX_DIR_BYTES {
            break;
        }
        if fresh(mtime) {
            continue;
        }
        // 活跃大文件不删；此时目录可能暂时超过软上限，优先保证数据完整。
        let _ = std::fs::remove_file(&path);
    }
}

fn summarize(samples: &VecDeque<(Instant, f32, f32)>) -> FrameStats {
    let now = Instant::now();
    let recent: Vec<(Instant, f32, f32)> = samples
        .iter()
        .copied()
        .filter(|(t, _, _)| now.duration_since(*t) <= Duration::from_secs(12))
        .collect();
    let Some((t, fps, ms)) = recent.last().copied() else {
        return FrameStats::default();
    };
    if now.duration_since(t) > Duration::from_secs(2) {
        return FrameStats::default();
    }
    let n = recent.len();
    let total_ms: f32 = recent.iter().map(|(_, _, ms)| *ms).sum();
    let avg = if total_ms > 0.0 {
        1000.0 * n as f32 / total_ms
    } else {
        recent.iter().map(|(_, f, _)| *f).sum::<f32>() / n as f32
    };
    let low1 = if n >= 20 {
        let mut times: Vec<f32> = recent.iter().map(|(_, _, ms)| *ms).collect();
        times.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let worst_n = ((n as f32) * 0.01).ceil().max(1.0) as usize;
        let worst = &times[n - worst_n..];
        let mean_ms = worst.iter().sum::<f32>() / worst_n as f32;
        if mean_ms > 0.0 { format!("{}", (1000.0 / mean_ms).round() as u64) } else { "--".into() }
    } else {
        "--".into()
    };
    FrameStats {
        active: true,
        fps: format!("{}", fps.round() as u64),
        fps_avg: format!("{}", avg.round() as u64),
        fps_low1: low1,
        frametime_ms: format!("{:.1}", ms),
        samples: recent.iter().rev().take(60).map(|(_, _, ms)| *ms).collect::<Vec<_>>().into_iter().rev().collect(),
    }
}

pub fn snapshot() -> FrameStats {
    THREAD.call_once(|| {
        let _ = std::fs::create_dir_all(log_dir());
        std::thread::spawn(|| {
            let mut cursor = Cursor::default();
            let mut samples = VecDeque::new();
            let mut cleanup_tick = 0u32;
            loop {
                poll_file(&mut cursor, &mut samples);
                cleanup_tick += 1;
                if cleanup_tick % 120 == 0 {
                    cleanup_logs();
                }
                let now = Instant::now();
                samples.retain(|(t, _, _)| now.duration_since(*t) <= Duration::from_secs(12));
                let stats = summarize(&samples);
                if let Ok(mut w) = LATEST.write() {
                    *w = Some(stats);
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        });
    });
    LATEST.read().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_default()
}
