//! 共享内存直读写层。
//!
//! 安全设计：
//! * 每次访问前校验文件身份（dev+ino）与大小，helper 重建 shm 时自动重映射
//!   ——防止读旧 inode / 写入被截断的映射触发 SIGBUS。
//! * ABI 校验锁：magic/version 不匹配拒绝一切写入。
//! * tuning 去抖：AtCreate 参数经 mpsc 通道交给后台线程，500ms 合并后 bump
//!   tuning_seq（严禁在 HTTP 线程里 sleep）。

use crate::defs::*;
use memmap2::MmapMut;
use std::fs::{OpenOptions};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};
use std::sync::Mutex;
use std::time::Duration;

pub struct Shm {
    path: PathBuf,
    map: Mutex<Option<Map>>,
    tuning_tx: Sender<()>,
}

struct Map {
    mmap: MmapMut,
    dev: u64,
    ino: u64,
}

fn uid() -> u32 {
    if let Ok(s) = std::fs::read_to_string("/proc/self/status") {
        for line in s.lines() {
            if let Some(v) = line.strip_prefix("Uid:") {
                if let Some(f) = v.split_whitespace().next() {
                    if let Ok(u) = f.parse::<u32>() {
                        return u;
                    }
                }
            }
        }
    }
    1000
}

pub fn shm_path() -> PathBuf {
    PathBuf::from(format!("/tmp/dlssnr-{}", uid())).join("shm.bin")
}

impl Shm {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel::<()>();
        {
            // 独立实例专用于去抖线程写 tuning_seq
            let debouncer = Shm {
                path: shm_path(),
                map: Mutex::new(None),
                tuning_tx: tx.clone(),
            };
            std::thread::spawn(move || loop {
                if rx.recv().is_err() {
                    break;
                }
                while rx.recv_timeout(Duration::from_millis(500)).is_ok() {}
                if let Ok(()) = debouncer.with_map(|m| {
                    let s = m.read_u32(status_off::TUNING_SEQ);
                    m.write_u32(status_off::TUNING_SEQ, s.wrapping_add(1));
                    crate::log("tuning_seq bumped（合并重建）");
                }) {}
            });
        }
        Shm {
            path: shm_path(),
            map: Mutex::new(None),
            tuning_tx: tx,
        }
    }

    /// 打开/校验/重映射。返回 MutexGuard 供单次访问。
    fn ensure(&self) -> Result<std::sync::MutexGuard<'_, Option<Map>>, String> {
        let mut guard = self.map.lock().map_err(|e| e.to_string())?;
        let meta = std::fs::metadata(&self.path)
            .map_err(|e| format!("共享内存不可用: {e} path={}", self.path.display()))?;
        let (dev, ino) = (meta.dev(), meta.ino());
        let need = match guard.as_ref() {
            Some(m) => m.dev != dev || m.ino != ino || meta.len() < HEADER_BYTES as u64,
            None => true,
        };
        if need {
            if meta.len() < HEADER_BYTES as u64 {
                return Err(format!("shm 过小（{} 字节）", meta.len()));
            }
            let f = OpenOptions::new()
                .read(true)
                .write(true)
                .mode(0o600)
                .open(&self.path)
                .map_err(|e| format!("打开 shm 失败: {e}"))?;
            // SAFETY: 常规文件、长度已校验 >= HEADER_BYTES；u32 对齐读写，
            // 与官方 GUI/shmctl 的访问模式一致。
            let mmap = unsafe { MmapMut::map_mut(&f) }.map_err(|e| format!("映射失败: {e}"))?;
            *guard = Some(Map { mmap, dev, ino });
        }
        Ok(guard)
    }

    fn with_map<T>(&self, f: impl FnOnce(&mut Map) -> T) -> Result<T, String> {
        let mut guard = self.ensure()?;
        let map = guard.as_mut().ok_or("shm 不可用")?;
        Ok(f(map))
    }

    fn check_abi(&self) -> Result<(), String> {
        self.with_map(|m| {
            let (magic, version) = (m.read_u32(status_off::MAGIC), m.read_u32(status_off::VERSION));
            if magic != SHM_MAGIC || version != SHM_VERSION {
                Err(format!("ABI 不匹配（magic={magic:#x} version={version}），拒绝写入"))
            } else {
                Ok(())
            }
        })?
    }

    pub fn read_u32(&self, off: usize) -> Option<u32> {
        self.with_map(|m| m.read_u32(off)).ok()
    }

    pub fn read_f32(&self, off: usize) -> Option<f32> {
        self.read_u32(off).map(f32::from_bits)
    }

    pub fn read_str(&self, off: usize, cap: usize) -> Option<String> {
        self.with_map(|m| {
            let end = (off + cap).min(HEADER_BYTES);
            let raw = &m.mmap[off..end];
            let nul = raw.iter().position(|&b| b == 0).unwrap_or(cap);
            String::from_utf8_lossy(&raw[..nul]).into_owned()
        })
        .ok()
    }

    pub fn resume(&self) -> Result<(), String> {
        self.check_abi()?;
        self.with_map(|m| {
            let (c, t) = (
                m.read_u32(status_off::CONTROL_SEQ),
                m.read_u32(status_off::TUNING_SEQ),
            );
            m.write_u32(status_off::QUIT, 0);
            m.write_u32(status_off::CONTROL_SEQ, c.wrapping_add(1));
            m.write_u32(status_off::TUNING_SEQ, t.wrapping_add(1));
        })?;
        Ok(())
    }

    pub fn request_capture(&self, n: u32) -> Result<(), String> {
        self.check_abi()?;
        self.with_map(|m| {
            let c = m.read_u32(status_off::CONTROL_SEQ);
            m.write_u32(status_off::CAPTURE_REQUEST, n);
            m.write_u32(status_off::CONTROL_SEQ, c.wrapping_add(1));
        })?;
        Ok(())
    }

    pub fn available(&self) -> bool {
        self.with_map(|_| ()).is_ok()
    }

    /// 带真实错误的可用性检查（诊断用）
    pub fn ensure_error(&self) -> Option<String> {
        self.with_map(|_| ()).err()
    }

    pub fn abi_ok(&self) -> bool {
        self.check_abi().is_ok()
    }

    /// 读全局设置：id → JSON 值（percent 还原百分数、invert 反读）。
    pub fn read_settings(&self) -> Option<serde_json::Map<String, serde_json::Value>> {
        self.with_map(|m| {
            let mut out = serde_json::Map::new();
            for d in DEFS.iter() {
                let raw = m.read_u32(d.offset);
                out.insert(d.id.to_string(), if d.invert {
                    serde_json::json!(if raw == 0 { 1u32 } else { 0u32 })
                } else {
                    match d.kind {
                        Kind::Float => {
                            let v = f32::from_bits(raw);
                            serde_json::json!((v * 10000.0).round() / 10000.0)
                        }
                        Kind::Percent => {
                            let v = f32::from_bits(raw) * 100.0;
                            serde_json::json!((v * 10.0).round() / 10.0)
                        }
                        _ => serde_json::json!(raw),
                    }
                });
            }
            out
        })
        .ok()
    }

    pub fn read_passes(&self) -> Option<Vec<serde_json::Map<String, serde_json::Value>>> {
        self.with_map(|m| {
            (0..MAX_PASSES)
                .map(|p| {
                    let base = PASS_BASE + p as usize * PASS_STRIDE;
                    PASS_FIELDS
                        .iter()
                        .map(|(name, rel, is_float)| {
                            let raw = m.read_u32(base + rel);
                            (
                                name.to_string(),
                                if *is_float {
                                    serde_json::json!(f32::from_bits(raw))
                                } else {
                                    serde_json::json!(raw)
                                },
                            )
                        })
                        .collect::<serde_json::Map<_, _>>()
                })
                .collect()
        })
        .ok()
    }

    /// 写设置。changes: id → 值；passes: 下标 → 字段表。
    /// 返回 (改动数, 触发重建的字段列表)。
    pub fn write_settings(
        &self,
        changes: &serde_json::Map<String, serde_json::Value>,
        passes: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(u32, Vec<String>), String> {
        self.check_abi()?;
        const AT_CREATE_PASS_FIELDS: [&str; 7] = [
            "intensity", "local_tone", "local_structure", "skin_structure",
            "style", "preset", "auto_mask",
        ];
        let mut changed = 0u32;
        let mut tuning: Vec<String> = Vec::new();
        self.with_map(|m| {
            for (id, val) in changes {
                let d = def_by_id(id).ok_or_else(|| format!("未知设置: {id}"))?;
                let mut raw: u32 = match d.kind {
                    Kind::Float => (val.as_f64().unwrap_or(0.0) as f32).to_bits(),
                    Kind::Percent => ((val.as_f64().unwrap_or(0.0) / 100.0) as f32).to_bits(),
                    Kind::Bool => u32::from(val.as_i64().unwrap_or(0) != 0),
                    _ => val
                        .as_i64()
                        .ok_or_else(|| format!("{id}: 需要整数"))? as u32,
                };
                if d.invert {
                    raw = if raw == 0 { 1 } else { 0 };
                }
                if m.read_u32(d.offset) != raw {
                    m.write_u32(d.offset, raw);
                    changed += 1;
                }
                if d.latch == Latch::AtCreate {
                    tuning.push(d.id.to_string());
                }
            }
            for (idx_s, fields) in passes {
                let idx: usize =
                    idx_s.parse().map_err(|_| format!("pass 下标不合法: {idx_s}"))?;
                if idx >= MAX_PASSES as usize {
                    return Err(format!("pass 越界: {idx}"));
                }
                let base = PASS_BASE + idx * PASS_STRIDE;
                let obj = fields.as_object().ok_or("pass 字段需为对象")?;
                for (name, val) in obj {
                    let (rel, is_float) = PASS_FIELDS
                        .iter()
                        .find(|(n, _, _)| n == name)
                        .map(|(_, r, f)| (*r, *f))
                        .ok_or_else(|| format!("未知 pass 字段: {name}"))?;
                    let raw = if is_float {
                        (val.as_f64().unwrap_or(0.0) as f32).to_bits()
                    } else {
                        val.as_i64().ok_or_else(|| format!("{name}: 需要整数"))? as u32
                    };
                    if m.read_u32(base + rel) != raw {
                        m.write_u32(base + rel, raw);
                        changed += 1;
                    }
                    if AT_CREATE_PASS_FIELDS.contains(&name.as_str()) {
                        tuning.push(format!("pass{idx}.{name}"));
                    }
                }
            }
            if changed > 0 {
                let c = m.read_u32(status_off::CONTROL_SEQ);
                m.write_u32(status_off::CONTROL_SEQ, c.wrapping_add(1));
            }
            Ok(())
        })??;
        // 只在确实有值变化时才请求重建：零变化的 auto_apply（shm 已是目标值）不该在
        // 游戏刚启动、前几帧在管线里时白打一次重建（易撞上格式协商期）
        if !tuning.is_empty() && changed > 0 {
            let _ = self.tuning_tx.send(());
        }
        Ok((changed, tuning))
    }
}

impl Map {
    fn read_u32(&self, off: usize) -> u32 {
        u32::from_le_bytes([
            self.mmap[off],
            self.mmap[off + 1],
            self.mmap[off + 2],
            self.mmap[off + 3],
        ])
    }
    fn write_u32(&mut self, off: usize, v: u32) {
        self.mmap[off..off + 4].copy_from_slice(&v.to_le_bytes());
    }
}
