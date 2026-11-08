//! MangoHud.conf 配置管理：游戏内悬浮 HUD 的显示项与布局。
//! 裸键（fps / cpu_stats …）为显示项开关；key=value 为参数（position/toggle_hud 等）。
//! MangoHud 带 inotify watcher，改动即时热重载。

use std::path::PathBuf;

static CONF_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub fn conf_path() -> PathBuf {
    crate::sysapi::home().join(".config/MangoHud/MangoHud.conf")
}

/// HUD 显示项（裸键）：key → 标签
pub const ITEMS: &[(&str, &str)] = &[
    ("fps", "帧率 FPS"),
    ("fps_metrics", "1% Low / 帧指标"),
    ("cpu_stats", "CPU 使用率"),
    ("cpu_temp", "CPU 温度"),
    ("gpu_stats", "GPU 使用率"),
    ("gpu_temp", "GPU 温度"),
    ("vram", "显存"),
    ("ram", "内存"),
    ("ram_temp", "内存温度"),
    ("frame_timing", "帧时间曲线"),
    ("gpu_power", "GPU 功耗"),
    ("cpu_power", "CPU 功耗"),
    ("arch", "CPU 架构"),
    ("engine_version", "引擎版本"),
    ("vulkan_driver", "Vulkan 驱动"),
    ("wine", "Wine 版本"),
    ("battery", "电量"),
];

/// 参数键（key=value）：key → (标签, 可选项或提示)
pub const PARAMS: &[(&str, &str, &str)] = &[
    ("position", "HUD 位置", "top-left|top-right|bottom-left|bottom-right|top-center|bottom-center"),
    ("toggle_hud", "HUD 开关快捷键", "如 Shift_R+F12（注意：Wayland 原生窗口下按键通道不可用，请用总开关）"),
    ("no_display", "HUD 总开关", "true=隐藏悬浮窗 false=显示（热重载即时生效）"),
    ("font_size", "字号 px", "数字，如 24"),
    ("font_scale", "文字缩放（0=隐藏文字但继续采集）", "0.0"),
    ("background_alpha", "背景不透明度", "0.0"),
    ("custom_layout", "游戏内自定义布局 JSON 路径", "~/.config/dlssnr-panel/hud-layout.json"),
    ("font_file", "自定义 HUD 中文字体（auto=容器可见 LXGW）", "auto"),
    ("font_glyph_ranges", "字符范围（chinese=完整中文）", "chinese"),
    ("output_folder", "帧日志目录（auto=按当前会话）", "auto"),
    ("autostart_log", "自动开始日志延迟秒", "1"),
    ("log_duration", "日志持续时间秒（0=不限时）", "0"),
    ("log_interval", "日志间隔毫秒（1=高频采样）", "1"),
    ("fps_metrics", "帧指标参数", "avg,0.01（=平均帧 + 1% Low）"),
    ("fps_sampling_period", "采样周期 ms", "默认 500"),
    ("hud_compact", "紧凑布局", "true/false（本版本常见项组合下无明显视觉差异）"),
    ("hud_no_margin", "无边距", "true/false"),
];

fn set_param_line(lines: &mut Vec<String>, key: &str, val: &str) {
    let new_line = format!("{key}={val}");
    let mut replaced = false;
    let mut out = Vec::new();
    for l in lines.iter() {
        // 同键的裸键行与 key=value 行都视为旧值：首个原位替换，其余丢弃
        let is_same_key = param_kv(l).map_or(false, |(kk, _)| kk == key) || bare_key(l) == Some(key);
        if is_same_key {
            if !replaced {
                out.push(new_line.clone());
                replaced = true;
            }
            continue;
        }
        out.push(l.clone());
    }
    if !replaced {
        out.push(new_line);
    }
    *lines = out;
}

fn del_key_lines(lines: &mut Vec<String>, key: &str) {
    lines.retain(|l| {
        bare_key(l) != Some(key) && !param_kv(l).map_or(false, |(kk, _)| kk == key)
    });
}

#[derive(Clone)]
pub struct Conf {
    pub lines: Vec<String>,
}

pub fn read() -> Conf {
    Conf {
        lines: std::fs::read_to_string(conf_path())
            .unwrap_or_default()
            .lines()
            .map(|l| l.to_string())
            .collect(),
    }
}

fn save(lines: &[String]) -> Result<(), String> {
    let p = conf_path();
    let parent = p.parent().ok_or("MangoHud.conf 没有父目录")?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    // 同目录唯一临时文件 + persist，避免并发请求共用 conf.tmp 互相覆盖。
    let mut tmp = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    use std::io::Write as _;
    tmp.write_all((lines.join("\n") + "\n").as_bytes()).map_err(|e| e.to_string())?;
    tmp.flush().map_err(|e| e.to_string())?;
    tmp.persist(&p).map_err(|e| e.to_string())?;
    crate::log("MangoHud.conf 已更新（inotify 热重载）");
    Ok(())
}

fn is_comment_or_blank(s: &str) -> bool {
    s.trim().is_empty() || s.trim_start().starts_with('#')
}

fn bare_key(line: &str) -> Option<&str> {
    let t = line.trim();
    if is_comment_or_blank(t) || t.contains('=') {
        return None;
    }
    if t.is_empty() {
        return None;
    }
    Some(t)
}

fn param_kv(line: &str) -> Option<(&str, &str)> {
    let t = line.trim();
    if is_comment_or_blank(t) {
        return None;
    }
    t.split_once('=').map(|(k, v)| (k.trim(), v.trim()))
}

pub fn status() -> serde_json::Value {
    let c = read();
    let mut items = serde_json::Map::new();
    for line in &c.lines {
        if let Some(k) = bare_key(line) {
            items.insert(k.to_string(), serde_json::json!(true));
        }
    }
    let mut params = serde_json::Map::new();
    for line in &c.lines {
        if let Some((k, v)) = param_kv(line) {
            params.insert(k.to_string(), serde_json::json!(v));
        }
    }
    // fps_metrics 的生效状态 = 参数行存在（chip 开关映射到参数，裸键会被 MangoHud 忽略）
    if !items.contains_key("fps_metrics") && params.contains_key("fps_metrics") {
        items.insert("fps_metrics".to_string(), serde_json::json!(true));
    }
    serde_json::json!({
        "conf_path": conf_path().display().to_string(),
        "items": items,
        "params": params,
    })
}

/// 显示项开关（裸键增删）+ 参数键值设置，一次读改写。
pub fn apply(changes: &serde_json::Map<String, serde_json::Value>) -> Result<String, String> {
    let _guard = CONF_LOCK.lock().map_err(|e| e.to_string())?;
    let mut lines = read().lines;
    let mut touched = Vec::new();
    for (k, v) in changes {
        if k.contains('=') || k.contains(' ') {
            return Err(format!("非法键名: {k}"));
        }
        let is_item = ITEMS.iter().any(|(i, _)| *i == k);
        let is_param = PARAMS.iter().any(|(i, _, _)| *i == k);
        if !is_item && !is_param {
            return Err(format!("未知 MangoHud 键: {k}"));
        }
        match v {
            serde_json::Value::Bool(b) => {
                if !is_item {
                    return Err(format!("{k} 是参数键，不能布尔"));
                }
                if k == "fps_metrics" {
                    // 显示项 chip 语义映射：开 = 写默认参数 avg,0.01；关 = 删除参数行
                    // （fps_metrics 不是裸键项，裸 "fps_metrics" 会被 MangoHud 忽略）
                    if *b {
                        set_param_line(&mut lines, k, "avg,0.01");
                    } else {
                        del_key_lines(&mut lines, k);
                    }
                    touched.push(format!("{k}={}", if *b { "on" } else { "off" }));
                } else if *b {
                    // 开：写裸键（=true），清掉可能存在的 =false 行
                    del_key_lines(&mut lines, k);
                    lines.push(k.clone());
                    touched.push(format!("{k}=on"));
                } else {
                    // 关：一律写显式 key=false（默认开的项删裸键无效，会回落默认显示）
                    set_param_line(&mut lines, k, "false");
                    touched.push(format!("{k}=off"));
                }
            }
            serde_json::Value::String(_) | serde_json::Value::Number(_) => {
                let val = match v {
                    serde_json::Value::String(s) => s.clone(),
                    _ => v.to_string(),
                };
                if val.contains('\n') || val.contains('\r') || val.contains('\0') {
                    return Err(format!("{k} 含非法换行或空字符"));
                }
                if k == "custom_layout" && val != crate::hud::layout_path().display().to_string() {
                    return Err("custom_layout 只能使用面板自己的 hud-layout.json 路径".into());
                }
                let val = match (k.as_str(), val.as_str()) {
                    // MangoHud 的 no_display 用 strtol，true/false 必须归一为 1/0。
                    ("no_display", "true") => "1".to_string(),
                    ("no_display", "false") => "0".to_string(),
                    _ => val,
                };
                let val = if k == "output_folder" && val == "auto" {
                    crate::frame_log::log_dir().to_string_lossy().to_string()
                } else if k == "font_file" && val == "auto" {
                    let p = crate::sysapi::home().join(".local/share/fonts/LXGWWenKai-Regular.ttf");
                    if !p.is_file() {
                        return Err("未找到容器可见的 LXGWWenKai-Regular.ttf，请先安装该字体".into());
                    }
                    p.display().to_string()
                } else {
                    val
                };
                set_param_line(&mut lines, k, &val);
                touched.push(format!("{k}={val}"));
            }
            _ => return Err(format!("{k} 的值类型不支持")),
        }
    }
    save(&lines)?;
    Ok(format!("HUD 已更新（{}）", touched.join(", ")))
}
