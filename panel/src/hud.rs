//! 悬浮窗布局模型：持久化 + 白名单校验 + 默认中文布局。
//! 布局文件 ~/.config/dlssnr-panel/hud-layout.json 由悬浮窗进程轮询 mtime 热加载。

use serde_json::json;
use std::path::PathBuf;

pub fn layout_path() -> PathBuf {
    sysapi_home().join(".config/dlssnr-panel/hud-layout.json")
}

pub fn sysapi_home() -> PathBuf {
    crate::sysapi::home()
}

fn clamp_i(v: &serde_json::Value, lo: i64, hi: i64, def: i64) -> i64 {
    // 兼容整型与浮点 JSON 数字（前端滑杆/输入框可能发 16.0）
    let n = match v {
        serde_json::Value::Number(n) => n.as_f64(),
        _ => None,
    };
    n.map(|x| (x.round() as i64).clamp(lo, hi)).unwrap_or(def)
}
fn clamp_f01(v: &serde_json::Value, def: f64) -> f64 {
    v.as_f64().map(|x| x.clamp(0.0, 1.0)).unwrap_or(def)
}

/// 颜色：仅接受 #RRGGBB / #RRGGBBAA，非法回退 def
fn norm_color(v: &serde_json::Value, def: &str) -> String {
    let s = v.as_str().unwrap_or("");
    let ok = s.len() == 7 || s.len() == 9;
    if ok && s.starts_with('#') && s[1..].chars().all(|c| c.is_ascii_hexdigit()) {
        s.to_ascii_lowercase()
    } else {
        def.to_string()
    }
}

/// 深度校验并规整布局 JSON（不动原值，返回规整后的新对象）；错误返回中文消息
pub fn validate(v: &serde_json::Value) -> Result<serde_json::Value, String> {
    let obj = v
        .as_object()
        .ok_or("布局必须是 JSON 对象")?;

    // ── window ──
    let w = obj.get("window").ok_or("缺少 window（悬浮窗本体）")?;
    let wo = w.as_object().ok_or("window 必须是对象")?;
    let win_w = clamp_i(wo.get("w").unwrap_or(&json!(420)), 80, 3840, 420);
    let win_h = clamp_i(wo.get("h").unwrap_or(&json!(260)), 60, 2160, 260);
    let win_x = clamp_i(wo.get("x").unwrap_or(&json!(24)), -4096, 8192, 24);
    let win_y = clamp_i(wo.get("y").unwrap_or(&json!(24)), -4096, 8192, 24);
    let layer = match wo.get("layer").and_then(|x| x.as_str()) {
        Some("top") => "top",
        Some("overlay") | None => "overlay",
        Some(_) => return Err("window.layer 只能是 top 或 overlay".into()),
    };

    // ── shape ──
    let sh = obj.get("shape").cloned().unwrap_or(json!({}));
    let sho = sh.as_object().ok_or("shape 必须是对象")?;
    let kind = match sho.get("kind").and_then(|x| x.as_str()) {
        Some("none") => "none",
        Some("rect") | None => "rect",
        Some("ellipse") => "ellipse",
        Some("polygon") => "polygon",
        Some(_) => return Err("shape.kind 只能是 none/rect/ellipse/polygon".into()),
    };
    let radius = clamp_i(sho.get("radius").unwrap_or(&json!(18)), 0, 300, 18) as f64;
    let fill = norm_color(sho.get("fill").unwrap_or(&json!("#101418")), "#101418");
    let fill_alpha = clamp_f01(sho.get("fill_alpha").unwrap_or(&json!(0.55)), 0.55);
    let border = norm_color(sho.get("border").unwrap_or(&json!("#57d9c5")), "#57d9c5");
    let border_alpha = clamp_f01(sho.get("border_alpha").unwrap_or(&json!(0.9)), 0.9);
    let border_w = clamp_i(sho.get("border_w").unwrap_or(&json!(2)), 0, 12, 2) as f64;
    let mut points: Vec<serde_json::Value> = Vec::new();
    if let Some(arr) = sho.get("points").and_then(|x| x.as_array()) {
        if arr.len() > 64 {
            return Err("shape.points 最多 64 个点".into());
        }
        for p in arr {
            let (px, py) = match (p.get(0).and_then(|x| x.as_i64()), p.get(1).and_then(|x| x.as_i64())) {
                (Some(a), Some(b)) => (a.clamp(-8192, 8192), b.clamp(-8192, 8192)),
                _ => return Err("shape.points 每项必须是 [x, y] 数字对".into()),
            };
            points.push(json!([px, py]));
        }
    }
    if kind == "polygon" && points.len() < 3 {
        return Err("polygon 形状至少需要 3 个点".into());
    }

    // ── items ──
    let items_raw = obj.get("items").ok_or("缺少 items（悬浮窗内容项）")?;
    let arr = items_raw.as_array().ok_or("items 必须是数组")?;
    if arr.len() > 64 {
        return Err("items 最多 64 项".into());
    }
    let mut items: Vec<serde_json::Value> = Vec::with_capacity(arr.len());
    let mut ids = std::collections::HashSet::new();
    for it in arr {
        let io = it.as_object().ok_or("items 每项必须是对象")?;
        let id = io.get("id").and_then(|x| x.as_str()).unwrap_or("");
        if id.is_empty() || id.len() > 32 || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
            return Err("item.id 必须是 1~32 位字母数字_-".into());
        }
        if !ids.insert(id.to_string()) {
            return Err(format!("item.id 重复: {id}"));
        }
        let tpl = io.get("tpl").and_then(|x| x.as_str()).unwrap_or("");
        if tpl.chars().count() > 240 {
            return Err("item.tpl 最长 240 字符".into());
        }
        if tpl.chars().any(|c| c == '\n' || c == '\r' || c == '\0') {
            return Err("item.tpl 不能含换行/空字符".into());
        }
        let kind = match io.get("kind").and_then(|x| x.as_str()) {
            Some("text") | None => "text",
            Some("graph") => "graph",
            Some("cpu_bars") => "cpu_bars",
            Some(_) => return Err("item.kind 只能是 text、graph 或 cpu_bars".into()),
        };
        let mut item = json!({
            "id": id,
            "kind": kind,
            "tpl": tpl,
            "x": clamp_i(io.get("x").unwrap_or(&json!(16)), -8192, 8192, 16),
            "y": clamp_i(io.get("y").unwrap_or(&json!(16)), -8192, 8192, 16),
            "size": clamp_i(io.get("size").unwrap_or(&json!(20)), 8, 96, 20),
            "color": norm_color(io.get("color").unwrap_or(&json!("#e8e4d8")), "#e8e4d8"),
        });
        if kind == "graph" {
            // 组件类：w/h 为区域尺寸
            item["w"] = json!(clamp_i(io.get("w").unwrap_or(&json!(220)), 16, 2048, 220));
            item["h"] = json!(clamp_i(io.get("h").unwrap_or(&json!(48)), 8, 512, 48));
        }
        if kind == "cpu_bars" {
            // 律动条：顶部 14px 标签带，高度下限给足空间
            item["w"] = json!(clamp_i(io.get("w").unwrap_or(&json!(260)), 16, 2048, 260));
            item["h"] = json!(clamp_i(io.get("h").unwrap_or(&json!(44)), 22, 512, 44));
        }
        if kind == "graph" {
            // 曲线组件：max_ms=0 表示按当前帧时间自动缩放。
            item["max_ms"] = json!(clamp_i(io.get("max_ms").unwrap_or(&json!(0)), 0, 1000, 0));
        }
        if kind == "cpu_bars" {
            // 每核频率律动条：max_mhz=0 表示按当前最大核频自动缩放
            item["max_mhz"] = json!(clamp_i(io.get("max_mhz").unwrap_or(&json!(0)), 0, 20000, 0));
        }
        items.push(item);
    }

    Ok(json!({
        "version": 1,
        "window": {"w": win_w, "h": win_h, "x": win_x, "y": win_y, "layer": layer},
        "shape": {
            "kind": kind, "radius": radius, "points": points,
            "fill": fill, "fill_alpha": fill_alpha,
            "border": border, "border_alpha": border_alpha, "border_w": border_w,
        },
        "items": items,
    }))
}

pub fn load_json() -> Option<serde_json::Value> {
    let s = std::fs::read_to_string(layout_path()).ok()?;
    serde_json::from_str(&s).ok()
}

pub fn save_json(v: &serde_json::Value) -> Result<(), String> {
    let p = layout_path();
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let body = serde_json::to_string_pretty(v).map_err(|e| e.to_string())?;
    let mut tmp = tempfile::NamedTempFile::new_in(p.parent().unwrap_or(std::path::Path::new(".")))
        .map_err(|e| e.to_string())?;
    use std::io::Write as _;
    tmp.write_all(body.as_bytes()).map_err(|e| e.to_string())?;
    tmp.flush().map_err(|e| e.to_string())?;
    tmp.persist(&p).map_err(|e| e.to_string())?;
    Ok(())
}

/// 默认布局：中文九项，仿 MangoHud 信息密度，自带圆角面板
pub fn default_layout() -> serde_json::Value {
    let it = |id: &str, tpl: &str, x: i64, y: i64, size: i64, color: &str| {
        json!({"id": id, "tpl": tpl, "x": x, "y": y, "size": size, "color": color})
    };
    json!({
        "version": 1,
        "window": {"w": 380, "h": 320, "x": 24, "y": 24, "layer": "overlay"},
        "shape": {
            "kind": "rect", "radius": 16, "points": [],
            "fill": "#101418", "fill_alpha": 0.62,
            "border": "#57d9c5", "border_alpha": 0.9, "border_w": 2,
        },
        "items": [
            it("gpu", "显卡 {gpu_util}% {gpu_temp}度", 20, 18, 22, "#7ee8a8"),
            it("gpu2", "{gpu_power}W {gpu_clock}MHz", 20, 48, 15, "#9adfc0"),
            it("cpu", "处理器 {cpu_util}% {cpu_temp}度", 20, 74, 22, "#7fb8f0"),
            it("vram", "显存 {gpu_mem} / {gpu_mem_total} GiB", 20, 104, 18, "#d8b4f0"),
            it("ram", "内存 {ram_used} / {ram_total} GiB {ram_temp}度", 20, 130, 18, "#f0c890"),
            it("fps", "帧率 {fps}（平均 {fps_avg}）", 20, 158, 22, "#f09a8a"),
            it("fps_low", "1%低帧 {fps_low1} FPS", 20, 186, 18, "#f0b090"),
            {"id":"frametime_graph", "kind":"graph", "tpl":"", "x":20, "y":216, "w":220, "h":52, "max_ms":0, "size":20, "color":"#57d9c5"},
            it("clock", "{time}", 20, 286, 14, "#c8c4b8"),
        ],
    })
}

