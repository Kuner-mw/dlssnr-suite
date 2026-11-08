//! vkBasalt 滤镜配置管理（conf 文本级编辑，保留注释与顺序）。

use std::path::{Path, PathBuf};

pub fn conf_path() -> PathBuf {
    crate::sysapi::home().join(".config/vkBasalt/vkBasalt.conf")
}

pub fn luts_dir() -> PathBuf {
    crate::sysapi::home().join(".config/vkBasalt/luts")
}

/// 效果参数规则表：键 → (最小, 最大, 步进, 是否整数, 默认值)
/// 键名与范围以 /usr/share/vkBasalt/vkBasalt.conf.example 为准
pub const PARAM_RULES: &[(&str, f64, f64, f64, bool, &str)] = &[
    ("casSharpness",               0.0,   1.0,   0.05,  false, "0.4"),
    ("dlsSharpness",               0.0,   1.0,   0.05,  false, "0.5"),
    ("dlsDenoise",                 0.0,   1.0,   0.01,  false, "0.17"),
    ("fxaaQualitySubpix",          0.0,   1.0,   0.05,  false, "0.75"),
    ("fxaaQualityEdgeThreshold",   0.0312, 0.333, 0.005, false, "0.125"),
    ("fxaaQualityEdgeThresholdMin",0.005, 0.1,   0.005, false, "0.0312"),
    ("smaaThreshold",              0.01,  0.2,   0.005, false, "0.05"),
    ("smaaMaxSearchSteps",         0.0,   98.0,  1.0,   true,  "32"),
    ("smaaMaxSearchStepsDiag",     0.0,   28.0,  1.0,   true,  "16"),
    ("smaaCornerRounding",         0.0,   100.0, 1.0,   true,  "25"),
    ("debandAvgdiff",              0.0,   0.05,  0.001, false, "0.015"),
    ("debandMaxdiff",              0.0,   0.05,  0.001, false, "0.006"),
    ("debandMiddiff",              0.0,   0.05,  0.001, false, "0.008"),
    ("debandIterations",           0.0,   4.0,   1.0,   true,  "1"),
    ("debandRange",                1.0,   1000.0, 1.0,  true,  "1000"),
];

// ── 滤镜预设（命名保存/修改/删除/应用）────────────────────────────────────
fn presets_file() -> PathBuf {
    crate::sysapi::home().join(".config/dlssnr/filter-presets.json")
}

/// 首次读取时播种两个默认风格（写实 / 梦幻）
fn load_presets() -> Vec<serde_json::Value> {
    if let Ok(t) = std::fs::read_to_string(presets_file()) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&t) {
            if let Some(arr) = v.as_array() {
                return arr.clone();
            }
        }
    }
    let seeded = vec![
        serde_json::json!({"name":"写实","lut":"realistic","cas":0.65}),
        serde_json::json!({"name":"梦幻","lut":"dreamy_pink","cas":0.45}),
    ];
    let _ = std::fs::create_dir_all(presets_file().parent().unwrap());
    let _ = std::fs::write(presets_file(), serde_json::to_string_pretty(&seeded).unwrap());
    seeded
}

fn save_presets(list: &[serde_json::Value]) -> Result<(), String> {
    let _ = std::fs::create_dir_all(presets_file().parent().unwrap());
    std::fs::write(presets_file(), serde_json::to_string_pretty(list).unwrap())
        .map_err(|e| e.to_string())
}

/// 从当前 conf 抓快照：effects + lut 系列 + 全部效果参数
pub fn preset_snapshot() -> serde_json::Value {
    let conf = parse_conf_pub();
    let mut params = serde_json::Map::new();
    for (k, .., d) in PARAM_RULES {
        let v = conf.get(*k).cloned().unwrap_or_else(|| d.to_string());
        if let Ok(n) = v.parse::<f64>() {
            params.insert(k.to_string(), serde_json::json!(n));
        }
    }
    let lut = conf.get("lutFile").cloned().unwrap_or_default();
    let lut_series = lut_series_of(Path::new(&lut)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default());
    serde_json::json!({
        "effects": conf.get("effects").cloned().unwrap_or_default(),
        "lut": lut_series,
        "params": params,
    })
}

/// 应用预设：定位 lut 系列最高版本文件，写入 effects/lutFile/参数
pub fn apply_preset(name: &str) -> Result<String, String> {
    if !live_enabled() {
        return Err("实时停用中：请先启用滤镜，再应用预设".into());
    }
    let list = load_presets();
    let p = list
        .iter()
        .find(|p| p.get("name").and_then(|n| n.as_str()) == Some(name))
        .ok_or_else(|| format!("预设不存在: {name}"))?;
    let series = p.get("lut").and_then(|v| v.as_str()).unwrap_or("");
    if !series.is_empty() {
        match pick_latest(series) {
            Some(n) => set_key("lutFile", &luts_dir().join(n).display().to_string())?,
            None => return Err(format!("luts 目录下没有 {series} 系列的 .cube")),
        }
    }
    // effects 允许空链（全关预设的恢复）；params 出错即整体报错，不留半套
    if let Some(effects) = p.get("effects").and_then(|v| v.as_str()) {
        set_key("effects", effects)?;
    }
    if let Some(params) = p.get("params").and_then(|v| v.as_object()) {
        for (k, v) in params {
            let val = v.as_f64()
                .map(|n| n.to_string())
                .ok_or_else(|| format!("预设参数 {k} 不是数字"))?;
            set_key(k, &val)?;
        }
    }
    // 兼容旧格式：cas 字段
    if let Some(cas) = p.get("cas").and_then(|v| v.as_f64()) {
        let _ = set_key("casSharpness", &cas.to_string());
    }
    Ok(format!("已应用预设「{name}」（实时生效）"))
}

/// 从 LUT 文件名提取系列名：去掉 .cube 后缀与结尾版本号（dreamy_pink_v2 → dreamy_pink）
pub fn lut_series_of(name: &str) -> String {
    let n = name.strip_suffix(".cube").unwrap_or(name);
    // 去掉 _v123 / _123 结尾（版本号）
    match n.rfind('_') {
        Some(i) if n[i + 1..].chars().all(|c| c.is_ascii_digit()) || n[i + 1..].starts_with('v') && n[i + 2..].chars().all(|c| c.is_ascii_digit()) => n[..i].to_string(),
        _ => n.to_string(),
    }
}

/// 系列名 → luts 目录下数字感知最高版本的完整文件名（找不到返回 None）
pub fn pick_latest(series: &str) -> Option<String> {
    crate::sysapi::entries(&luts_dir())
        .iter()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.ends_with(".cube") && lut_series_of(n) == series)
        .max_by(|a, b| human_ver(a, b))
}

/// 数字感知版本比较（v2 < v10）
fn human_ver(a: &str, b: &str) -> std::cmp::Ordering {
    let av: Vec<u64> = a.split(|c: char| !c.is_ascii_digit()).filter(|s| !s.is_empty())
        .filter_map(|s| s.parse().ok()).collect();
    let bv: Vec<u64> = b.split(|c: char| !c.is_ascii_digit()).filter(|s| !s.is_empty())
        .filter_map(|s| s.parse().ok()).collect();
    av.cmp(&bv)
}

/// 预设管理：save（另存当前）/ update（用当前覆盖同名）/ delete
pub fn preset_action(action: &str, name: &str) -> Result<String, String> {
    if (action == "save" || action == "update") && !live_enabled() {
        return Err("实时停用中：当前快照是直通态，请先启用滤镜再保存".into());
    }
    let name = name.trim();
    if name.is_empty() || name.len() > 24 {
        return Err("预设名需 1~24 字符".into());
    }
    let mut list = load_presets();
    match action {
        "save" => {
            if list.iter().any(|p| p.get("name").and_then(|n| n.as_str()) == Some(name)) {
                return Err(format!("预设「{name}」已存在，请换名或用「改」覆盖"));
            }
            let mut snap = preset_snapshot();
            snap["name"] = serde_json::json!(name);
            list.push(snap);
            save_presets(&list)?;
            Ok(format!("已保存预设「{name}」（当前效果参数 + LUT + 效果链）"))
        }
        "update" => {
            let idx = list
                .iter()
                .position(|p| p.get("name").and_then(|n| n.as_str()) == Some(name))
                .ok_or_else(|| format!("预设不存在: {name}"))?;
            let mut snap = preset_snapshot();
            snap["name"] = serde_json::json!(name);
            list[idx] = snap;
            save_presets(&list)?;
            Ok(format!("预设「{name}」已更新为当前参数"))
        }
        "delete" => {
            let before = list.len();
            list.retain(|p| p.get("name").and_then(|n| n.as_str()) != Some(name));
            if list.len() == before {
                return Err(format!("预设不存在: {name}"));
            }
            save_presets(&list)?;
            Ok(format!("已删除预设「{name}」"))
        }
        _ => Err(format!("未知操作: {action}")),
    }
}

// ── 实时开关：效果链保持不动，只走热重载可靠路径（LUT 文件替换 + cas 参数）──
fn live_state_file() -> PathBuf {
    crate::sysapi::home().join(".config/dlssnr/filter-state.json")
}

fn read_live_state() -> serde_json::Value {
    let v = std::fs::read_to_string(live_state_file())
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    // 必须是对象：坏文件（如 []）会导致 state["live"]=.. panic
    if !v.is_object() {
        serde_json::json!({"live": true})
    } else {
        v
    }
}

fn write_live_state(v: &serde_json::Value) -> Result<(), String> {
    let _ = std::fs::create_dir_all(live_state_file().parent().unwrap());
    std::fs::write(live_state_file(), serde_json::to_string_pretty(v).unwrap())
        .map_err(|e| e.to_string())
}

pub fn live_enabled() -> bool {
    read_live_state().get("live").and_then(|v| v.as_bool()).unwrap_or(true)
}

/// 实时停用 = 记住当前 LUT/cas → LUT 切 identity + cas 归零（画面立即直通）
/// 实时启用 = 恢复停用前保存的 LUT/cas。效果链不动（vkBasalt 热重载不支持新增效果）。
pub fn set_live(enable: bool) -> Result<String, String> {
    // 整个状态迁移持锁：防并发停用互相覆盖 saved、与 set_key 的 CONF_LOCK 一致
    static LIVE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = LIVE_LOCK.lock().map_err(|_| "滤镜开关锁异常")?;
    let mut state = read_live_state();
    let was_live = state.get("live").and_then(|v| v.as_bool()).unwrap_or(true);
    if enable == was_live {
        // 幂等：重复停用只重放直通化（不重写 saved），重复启用直接确认
        if !enable {
            let identity = luts_dir().join("identity.cube");
            if identity.is_file() {
                set_key("lutFile", &identity.display().to_string())?;
                set_key("casSharpness", "0")?;
            }
            return Ok("滤镜已处于停用状态（已重放直通化）".into());
        }
        return Ok("滤镜已处于启用状态".into());
    }
    if !enable {
        // 停用：先记录原值（避免二次停用把 identity 存进去），再直通化
        let cur_lut = conf_lut();
        let cur_cas = conf_cas();
        state["live"] = serde_json::json!(false);
        state["saved_lut"] = serde_json::json!(cur_lut);
        state["saved_cas"] = serde_json::json!(cur_cas);
        let identity = luts_dir().join("identity.cube");
        if !identity.is_file() {
            return Err("luts 目录缺 identity.cube（无法直通化；状态未改变）".into());
        }
        set_key("lutFile", &identity.display().to_string())?;
        set_key("casSharpness", "0")?;
        write_live_state(&state)?;
        Ok("滤镜已实时停用（LUT 直通 + 锐化归零，游戏画面 ~2s 内还原）".into())
    } else {
        // 启用：先恢复 conf，成功后才提交 live:true（失败可重试）
        let saved_lut = state.get("saved_lut").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let saved_cas = state.get("saved_cas").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if saved_lut.is_empty() {
            return Err("没有停用前保存的 LUT，请先在预设里选一个风格".into());
        }
        if saved_cas.is_empty() {
            return Err("停用前未保存 casSharpness，无法恢复".into());
        }
        let lut_path = std::path::Path::new(&saved_lut);
        if !lut_path.is_file() {
            return Err(format!("停用前保存的 LUT 已不存在: {saved_lut}（状态未改变）"));
        }
        set_key("lutFile", &saved_lut)?;
        set_key("casSharpness", &saved_cas)?;
        state["live"] = serde_json::json!(true);
        write_live_state(&state)?;
        Ok("滤镜已实时启用（恢复停用前的 LUT 与锐化）".into())
    }
}

fn conf_lut() -> String {
    parse_conf().get("lutFile").cloned().unwrap_or_default()
}
fn conf_cas() -> String {
    parse_conf().get("casSharpness").cloned().unwrap_or_else(|| "0.45".into())
}

pub fn presets_json() -> serde_json::Value {
    serde_json::json!(load_presets())
}

fn parse_conf_pub() -> std::collections::HashMap<String, String> {
    // parse_conf 是私有的，预设快照复用一个公开入口
    parse_conf()
}

/// 动态发现 runner（compatibilitytools.d）目录：含 proton 可执行脚本的目录。
/// DW-Proton/Proton 只加载 runner 自身目录下的 user_settings.py。
pub fn runner_dirs() -> Vec<PathBuf> {
    let ctd = crate::sysapi::home().join(".local/share/Steam/compatibilitytools.d");
    let mut out = Vec::new();
    for e in crate::sysapi::entries(&ctd) {
        let dir = e.path();
        if dir.join("proton").is_file() {
            out.push(dir);
        }
    }
    out
}

/// user_settings.py 候选：runner 目录（真实生效位）+ 历史 compatdata 位（仅提示）。
/// 返回 (路径, 是否真正生效)。
pub fn user_settings_files() -> Vec<(PathBuf, bool)> {
    let mut out: Vec<(PathBuf, bool)> = runner_dirs()
        .into_iter()
        .map(|d| (d.join("user_settings.py"), true))
        .collect();
    // 兼容展示历史 compatdata 写法（对 DW-Proton 实际无效）
    for e in crate::sysapi::entries(&crate::sysapi::home().join(".local/share/Steam/steamapps/compatdata")) {
        let f = e.path().join("user_settings.py");
        if f.is_file() {
            out.push((f, false));
        }
    }
    out
}

fn parse_conf() -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    if let Ok(text) = std::fs::read_to_string(conf_path()) {
        for line in text.lines() {
            let s = line.trim();
            if s.is_empty() || s.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = s.split_once('=') {
                map.entry(k.trim().to_string())
                    .or_insert_with(|| v.trim().trim_matches('"').to_string());
            }
        }
    }
    map
}

pub fn status() -> serde_json::Value {
    let conf = parse_conf();
    let luts: Vec<String> = crate::sysapi::entries(&luts_dir())
        .iter()
        .filter_map(|e| {
            let n = e.file_name().into_string().ok()?;
            n.ends_with(".cube").then_some(n)
        })
        .collect();
    let lut = conf.get("lutFile").cloned().unwrap_or_default();
    let lut_name = Path::new(&lut)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let enables: Vec<serde_json::Value> = user_settings_files()
        .into_iter()
        .map(|(f, effective)| {
            let mut enabled = None;
            if let Ok(text) = std::fs::read_to_string(&f) {
                for line in text.lines() {
                    // user_settings.py 是 Python 字典：键值用冒号不是等号
                    if line.contains("ENABLE_VKBASALT") && line.contains(':') {
                        enabled = Some(!line.trim_start().starts_with('#'));
                        break;
                    }
                }
            }
            serde_json::json!({"path": f.display().to_string(), "exists": f.exists(), "enabled": enabled, "effective": effective})
        })
        .collect();
    let mut params = serde_json::Map::new();
    for (k, _, _, _, _, d) in PARAM_RULES {
        let v = conf.get(*k).cloned().unwrap_or_else(|| d.to_string());
        if let Ok(n) = v.parse::<f64>() {
            params.insert(k.to_string(), serde_json::json!(n));
        }
    }
    params.insert(
        "smaaEdgeDetection".into(),
        serde_json::json!(conf.get("smaaEdgeDetection").map(|s| s.as_str()).unwrap_or("luma")),
    );
    serde_json::json!({
        "conf_path": conf_path().display().to_string(),
        "luts_dir": luts_dir().display().to_string(),
        "params": params,
        "presets": load_presets(),
        "param_rules": PARAM_RULES.iter().map(|(k, lo, hi, st, is_int, d)| serde_json::json!({
            "key": k, "min": lo, "max": hi, "step": st, "int": is_int, "def": d
        })).collect::<Vec<_>>(),
        "effects": conf.get("effects").cloned().unwrap_or_default(),
        "cas_sharpness": conf.get("casSharpness").cloned().unwrap_or_default(),
        "lut_file": lut,
        "lut_name": lut_name,
        "lut_series": lut_series_of(&lut_name),
        "live_enabled": live_enabled(),
        "luts": luts,
        "toggle_key": conf.get("toggleKey").cloned().unwrap_or_else(|| "Home".into()),
        "enable_on_launch": conf.get("enableOnLaunch").cloned().unwrap_or_else(|| "True".into()),
        "enable": enables,
    })
}

pub fn set_key(key: &str, value: &str) -> Result<(), String> {
    if key == "smaaEdgeDetection" && !["luma", "color", "depth"].contains(&value) {
        return Err("smaaEdgeDetection 仅限 luma/color/depth".into());
    }
    const ALLOWED: [&str; 5] = ["effects", "lutFile", "toggleKey", "enableOnLaunch", "smaaEdgeDetection"];
    if !ALLOWED.contains(&key) && !PARAM_RULES.iter().any(|(k, ..)| *k == key) {
        return Err(format!("未知滤镜键: {key}"));
    }
    if let Some((_, lo, hi, _step, is_int, _)) = PARAM_RULES.iter().find(|(k, ..)| *k == key) {
        let v: f64 = value.parse().map_err(|_| format!("{key} 需数字"))?;
        if !v.is_finite() || v < *lo - 1e-9 || v > *hi + 1e-9 {
            return Err(format!("{key} 需为 {lo}~{hi} 的有限数字"));
        }
        if *is_int && v.fract() != 0.0 {
            return Err(format!("{key} 需整数"));
        }
    }
    let quoted = key == "lutFile";
    let value = if quoted {
        // vkBasalt 不展开 ~，统一转绝对路径
        let p = std::path::absolute(std::path::Path::new(value).strip_prefix("\"").unwrap_or(Path::new(value)))
            .map_err(|e| e.to_string())?;
        format!("\"{}\"", p.display())
    } else {
        value.to_string()
    };
    // 配置值禁止换行/回车/NUL：防止把额外配置行注入 conf
    if value.bytes().any(|b| b == b'\r' || b == b'\n' || b == 0) {
        return Err("配置值包含非法控制字符".into());
    }
    // lutFile 只允许指向受信 LUT 目录内的既有文件（拒绝任意绝对路径/穿越）
    if key == "lutFile" {
        let p = std::path::absolute(Path::new(value.trim_matches('"'))).map_err(|e| e.to_string())?;
        let parent_ok = p.parent().map(|d| d == luts_dir()).unwrap_or(false);
        if !parent_ok || !p.is_file() {
            return Err("lutFile 必须是 luts 目录下的既有 .cube 文件".into());
        }
    }
    if !conf_path().exists() {
        return Err("vkBasalt.conf 不存在".into());
    }
    // 进程级锁：读-改-写全程串行，防多 worker 并发丢写/临时文件竞态
    static CONF_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = CONF_LOCK.lock().map_err(|_| "vkBasalt 配置锁异常")?;
    let text = std::fs::read_to_string(conf_path()).map_err(|e| e.to_string())?;
    let pat = regex_line_start(key);
    let mut replaced = false;
    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        if pat(line) {
            if !replaced {
                let indent = &line[..line.len() - line.trim_start().len()];
                out.push(format!("{indent}{key} = {value}"));
                replaced = true;
            }
            // 其余重复键直接丢弃：vkBasalt 只认第一个，留着会让后续追加永远无效
        } else {
            out.push(line.to_string());
        }
    }
    if !replaced {
        out.push(format!("{key} = {value}"));
    }
    let tmp = conf_path().with_extension("conf.tmp");
    std::fs::write(&tmp, out.join("\n") + "\n").map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, conf_path()).map_err(|e| e.to_string())?;
    crate::log(format!("vkBasalt 更新: {key} = {value}"));
    Ok(())
}

fn regex_line_start(key: &str) -> impl Fn(&str) -> bool + '_ {
    // 匹配「key = value」或「key=value」（忽略缩进与等号两侧空格），注释行不会到这
    move |line: &str| match line.trim_start().split_once('=') {
        Some((k, _)) => k.trim() == key,
        None => false,
    }
}

pub fn set_enabled(enable: bool) -> Result<String, String> {
    // 只操作真实生效位：runner 目录下的 user_settings.py（不存在则创建）
    let dirs = runner_dirs();
    let target = dirs
        .iter()
        .find_map(|d| {
            let f = d.join("user_settings.py");
            f.is_file().then_some(f)
        })
        .or_else(|| dirs.first().map(|d| d.join("user_settings.py")))
        .ok_or_else(|| "找不到 Steam compatibilitytools.d 下的 runner 目录".to_string())?;
    let text = if target.exists() {
        std::fs::read_to_string(&target).map_err(|e| e.to_string())?
    } else {
        // 模板：DW-Proton/Proton 启动时 import 并注入环境变量
        "user_settings = {\n    \"ENABLE_VKBASALT\": \"1\",\n}\n".to_string()
    };
    let mut changed = false;
    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        if !changed && line.contains("ENABLE_VKBASALT") && line.contains(':') {
            let body = line.trim_start().trim_start_matches("# ").to_string();
            let indent = &line[..line.len() - line.trim_start().len()];
            out.push(if enable {
                format!("{indent}{body}")
            } else {
                format!("{indent}# {body}")
            });
            changed = true;
        } else {
            out.push(line.to_string());
        }
    }
    if !changed {
        return Ok("状态未变".into());
    }
    let bak = if target.exists() {
        let bak = target.with_extension("py.bak_webui");
        std::fs::copy(&target, &bak).map_err(|e| e.to_string())?;
        Some(bak)
    } else {
        None
    };
    let tmp = target.with_extension("py.tmp");
    std::fs::write(&tmp, out.join("\n") + "\n").map_err(|e| e.to_string())?;
    // py_compile 校验
    let chk = std::process::Command::new("python3")
        .args(["-m", "py_compile"])
        .arg(&tmp)
        .output()
        .map_err(|e| e.to_string())?;
    if !chk.status.success() {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!(
            "py_compile 校验失败，未写入: {}",
            String::from_utf8_lossy(&chk.stderr)
        ));
    }
    std::fs::rename(&tmp, &target).map_err(|e| e.to_string())?;
    crate::log(format!(
        "ENABLE_VKBASALT → {}（runner: {}）",
        if enable { "启用" } else { "停用" },
        target.display()
    ));
    Ok(format!(
        "已{}于 {}（重启游戏生效{}）",
        if enable { "启用" } else { "停用" },
        target.display(),
        bak.as_ref().map(|b| format!("；备份 {}", b.display())).unwrap_or_default()
    ))
}
