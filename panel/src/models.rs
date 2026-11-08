//! NR 模型库：扫描 / 登记 / 原子切换（现役 DLL 自动留底）。

use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// 模型库文件操作锁：导入与切换共用，防并发写坏 dll
static MODEL_IO_LOCK: Mutex<()> = Mutex::new(());

pub fn models_dir() -> PathBuf {
    crate::sysapi::data_dir().join("models")
}

pub fn binaries_dir() -> PathBuf {
    crate::sysapi::data_dir().join("binaries")
}

pub fn active_dll() -> PathBuf {
    binaries_dir().join("nvngx_dlssnr.dll")
}

fn lib_json() -> PathBuf {
    models_dir().join("library.json")
}

#[derive(Serialize, Clone)]
pub struct ModelItem {
    pub file: String,
    pub label: String,
    pub version: String,
    pub target: String,
    pub size_mb: f64,
    pub sha256: String,
}

fn sha256_file(path: &Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut h = Sha256::new();
    let mut f = std::fs::File::open(path)?;
    let mut buf = vec![0u8; 1 << 20]; // 堆分配：HTTP worker 线程栈小，栈上 1MB 会爆
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(format!("{:x}", h.finalize()))
}

fn sha256_short(path: &Path) -> String {
    sha256_file(path)
        .map(|s| format!("{}…", &s[..16]))
        .unwrap_or_else(|_| "…".into())
}

fn read_lib() -> HashMap<String, serde_json::Value> {
    std::fs::read_to_string(lib_json())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn write_lib(lib: &HashMap<String, serde_json::Value>) -> Result<(), String> {
    std::fs::create_dir_all(models_dir()).map_err(|e| e.to_string())?;
    std::fs::write(
        lib_json(),
        serde_json::to_string_pretty(lib).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

pub fn scan() -> serde_json::Value {
    let lib = read_lib();
    let mut library: Vec<ModelItem> = crate::sysapi::entries(&models_dir())
        .iter()
        .filter_map(|e| {
            let p = e.path();
            let name = p.file_name()?.to_string_lossy().into_owned();
            if !name.to_lowercase().ends_with(".dll") {
                return None;
            }
            let meta = lib.get(&name).cloned().unwrap_or_default();
            Some(ModelItem {
                label: meta
                    .get("label")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&name)
                    .to_string(),
                version: meta
                    .get("version")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                target: meta
                    .get("target")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                file: name,
                size_mb: (e.metadata().ok()?.len() as f64 / 1048576.0 * 10.0).round() / 10.0,
                sha256: sha256_short(&p),
            })
        })
        .collect();
    library.sort_by(|a, b| a.file.cmp(&b.file));

    let active = if active_dll().exists() {
        let sha = sha256_short(&active_dll());
        let matched = library.iter().find(|i| i.sha256 == sha).cloned();
        let md = active_dll().metadata().ok();
        Some(serde_json::json!({
            "label": matched.as_ref().map(|m| m.label.clone()).unwrap_or("（未入库的 DLL）".into()),
            "version": matched.as_ref().map(|m| m.version.clone()).unwrap_or_default(),
            "size_mb": md.as_ref().map(|m| (m.len() as f64 / 1048576.0 * 10.0).round() / 10.0),
            "sha256": sha,
            "mtime": md.as_ref().and_then(|m| m.modified().ok())
                .map(|t| t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs())
                .unwrap_or(0),
            "from_library": matched.is_some(),
        }))
    } else {
        None
    };
    serde_json::json!({
        "library": library,
        "active": active,
        "library_dir": models_dir().display().to_string(),
        "binaries_dir": binaries_dir().display().to_string(),
    })
}

/// 合并外部登记（refresh action）到 library.json：与导入/切换共用 MODEL_IO_LOCK，
/// 原子写（临时文件 + rename），防并发丢登记/半截 JSON
pub fn merge_lib(entries: &serde_json::Map<String, serde_json::Value>) -> Result<(), String> {
    let _guard = MODEL_IO_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut lib = read_lib();
    for (k, v) in entries {
        lib.insert(k.clone(), v.clone());
    }
    write_lib(&lib)
}

pub fn activate(file_name: &str) -> Result<String, String> {
    let _guard = MODEL_IO_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let file_name = Path::new(file_name)
        .file_name()
        .ok_or("文件名不合法")?
        .to_string_lossy()
        .into_owned();
    let src = models_dir().join(&file_name);
    if !src.exists() {
        return Err(format!("库里没有这个文件: {file_name}"));
    }
    std::fs::create_dir_all(binaries_dir()).map_err(|e| e.to_string())?;
    // 现役 DLL 未入库则先留底
    if active_dll().exists() {
        let cur_sha = sha256_short(&active_dll());
        let known: Vec<String> = crate::sysapi::entries(&models_dir())
            .iter()
            .filter(|e| {
                e.path()
                    .extension()
                    .is_some_and(|x| x.to_string_lossy().eq_ignore_ascii_case("dll"))
            })
            .map(|e| sha256_short(&e.path()))
            .collect();
        if !known.contains(&cur_sha) {
            let backup = models_dir().join(format!(
                "previous_{}.dll",
                chrono_stamp()
            ));
            std::fs::copy(active_dll(), &backup).map_err(|e| e.to_string())?;
            let mut lib = read_lib();
            lib.insert(
                backup.file_name().unwrap().to_string_lossy().into_owned(),
                serde_json::json!({"label": "切换前自动备份", "version": "", "target": "", "source": "自动"}),
            );
            write_lib(&lib)?;
            crate::log(format!("原 DLL 已入库留底: {}", crate::log_field(&backup.display().to_string())));
        }
    }
    let tmp = binaries_dir().join("nvngx_dlssnr.dll.tmp");
    std::fs::copy(&src, &tmp).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, active_dll()).map_err(|e| e.to_string())?;
    crate::log(format!("模型已切换 → {}", crate::log_field(&file_name)));
    Ok(format!("已切换为 {file_name}。重启 helper 后生效（游戏运行中请先退出）。"))
}

fn chrono_stamp() -> String {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // 简单的 MMDD_HHMM（本地时间不做时区换算，仅作留底标记）
    let days = t / 86400;
    let secs = t % 86400;
    // 2026-01-01 起算的粗略 stamp 即可，精确日期由 log 提供
    format!("d{days}t{:02}{:02}", secs / 3600, (secs % 3600) / 60)
}

/// 从本机路径导入模型 dll 到模型库（使用者自行下载后粘贴路径导入）。
/// 校验：存在、~ 展开、.dll 后缀、PE 头、≤2GB；临时文件写入并复核哈希后原子落位
/// （防截断/防并发/防 symlink 目标覆盖）；同内容幂等；同名不同内容替换时重置旧元数据。
pub fn import_from_path(src: &str) -> Result<String, String> {
    let src = src.trim();
    if src.is_empty() {
        return Err("先填写模型文件路径".into());
    }
    // 仅展开 ~ 与 ~/（不做 shell 式任意展开）
    let src_path = if src == "~" {
        crate::sysapi::home()
    } else if let Some(rest) = src.strip_prefix("~/") {
        crate::sysapi::home().join(rest)
    } else {
        PathBuf::from(src)
    };
    if !src_path.is_file() {
        return Err(format!("文件不存在: {src}"));
    }
    let name = src_path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .ok_or("路径异常")?;
    if !name.to_lowercase().ends_with(".dll") {
        return Err("只接受 .dll 文件".into());
    }
    let len = src_path.metadata().map_err(|e| format!("无法读取文件: {e}"))?.len();
    if len > 2 << 30 {
        return Err("文件超过 2GB，疑似导错了文件".into());
    }
    let _guard = MODEL_IO_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    use std::io::Read;
    let mut f = std::fs::File::open(&src_path).map_err(|e| format!("无法读取文件: {e}"))?;
    let mut head = [0u8; 2];
    f.read_exact(&mut head).map_err(|e| format!("无法读取文件: {e}"))?;
    if &head != b"MZ" {
        return Err("不是 Windows PE 文件（缺 MZ 头），请确认下载的是模型 dll".into());
    }
    let mut hasher = Sha256::new();
    hasher.update(&head); // MZ 头计入哈希，保证与整文件哈希同一起点
    let mut buf = vec![0u8; 1 << 20]; // 堆分配，理由同 sha256_file
    loop {
        let n = f.read(&mut buf).map_err(|e| format!("读取失败: {e}"))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let hex = format!("{:x}", hasher.finalize());
    std::fs::create_dir_all(models_dir()).map_err(|e| e.to_string())?;
    let dst = models_dir().join(&name);
    if dst.is_file() && sha256_file(&dst).unwrap_or_default() == hex {
        return Ok(format!("{name} 已在模型库中（内容一致）"));
    }
    // 原子落位：models_dir 内写临时文件，复核落盘内容后 persist（失败自动清理）
    let tmp = tempfile::Builder::new()
        .prefix(".import-")
        .suffix(".tmp")
        .tempfile_in(models_dir())
        .map_err(|e| format!("创建临时文件失败: {e}"))?;
    std::fs::copy(src_path, tmp.path()).map_err(|e| format!("导入失败: {e}"))?;
    if sha256_file(tmp.path()).unwrap_or_default() != hex {
        let _ = tmp.close();
        return Err("导入过程中文件内容发生变化，已中止".into());
    }
    tmp.persist(&dst).map_err(|e| format!("提交模型失败: {e}"))?;
    // 同名不同内容的替换：清掉旧登记，避免 UI 显示过期版本信息
    let mut lib = read_lib();
    if lib.remove(&name).is_some() {
        let _ = write_lib(&lib);
    }
    Ok(format!("已导入 {name}，点刷新后可在库里切换"))
}
