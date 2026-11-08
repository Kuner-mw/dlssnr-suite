//! NR 模型库：扫描 / 登记 / 原子切换（现役 DLL 自动留底）。

use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub fn models_dir() -> PathBuf {
    crate::sysapi::home().join(".local/share/dlssnr/models")
}

pub fn binaries_dir() -> PathBuf {
    crate::sysapi::home().join(".local/share/dlssnr/binaries")
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

fn sha256_short(path: &Path) -> String {
    use std::io::Read;
    let mut h = Sha256::new();
    if let Ok(mut f) = std::fs::File::open(path) {
        let mut buf = [0u8; 1 << 20];
        while let Ok(n) = f.read(&mut buf) {
            if n == 0 {
                break;
            }
            h.update(&buf[..n]);
        }
    }
    format!("{:x}…", h.finalize())[..16].to_string() + "…"
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
            if !name.ends_with(".dll") {
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

pub fn activate(file_name: &str) -> Result<String, String> {
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
            .filter(|e| e.path().extension().is_some_and(|x| x == "dll"))
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
            crate::log(format!("原 DLL 已入库留底: {}", backup.display()));
        }
    }
    let tmp = binaries_dir().join("nvngx_dlssnr.dll.tmp");
    std::fs::copy(&src, &tmp).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, active_dll()).map_err(|e| e.to_string())?;
    crate::log(format!("模型已切换 → {file_name}"));
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
