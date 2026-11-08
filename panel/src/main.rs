//! DLSSNR Web 控制面板（Rust 重写版）。
//!
//! 路由规范：路径段 kebab-case（/api/legacy/kill-gui），JSON 键统一 snake_case。
//! 静态资源 include_str! 嵌入，单二进制交付。

mod defs;
mod games;
mod models;
mod shm;
mod sysapi;
mod vkbasalt;
mod mangohud;
mod hud;
mod metrics;
mod frame_log;

use serde_json::json;
use std::io::Read;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tiny_http::{Header, Method, Response, Server};

pub fn log(msg: impl Into<String>) {
    let line = format!("[{}] {}", chrono_now(), msg.into());
    println!("{line}");
    // O_APPEND 追加写：整行（含换行）单次 write，多线程并发不丢行不拼行（旧版 read+rewrite 有竞态）
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(sysapi::log_file())
    {
        let mut buf = line.into_bytes();
        buf.push(b'\n');
        let _ = f.write_all(&buf);
    }
}

fn chrono_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        + 8 * 3600; // UTC+8
    let days = secs / 86400;
    let s = secs % 86400;
    let (y, mo, d) = civil_from_days(days as i64);
    format!("{y:04}-{mo:02}-{d:02} {:02}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
}

fn chrono_fmt_time(t: &std::time::SystemTime) -> String {
    let secs = t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() + 8 * 3600;
    let s = secs % 86400;
    let (y, mo, d) = civil_from_days((secs / 86400) as i64);
    format!("{y:04}-{mo:02}-{d:02} {:02}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
}

fn civil_from_days(z: i64) -> (i64, u64, u64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

struct AppState {
    shm: shm::Shm,
    token: String,
    liveness: Mutex<(u64, u64)>,
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let port: u16 = args
        .iter()
        .position(|a| a == "--port")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(8099);
    let no_token = args.iter().any(|a| a == "--no-token");

    let _ = std::fs::create_dir_all(sysapi::state_dir());
    let token = if no_token {
        String::new()
    } else {
        sysapi::load_or_create_token()
    };
    let bind = if no_token { "127.0.0.1" } else { "0.0.0.0" };

    let state = Arc::new(AppState {
        shm: shm::Shm::new(),
        token,
        liveness: Mutex::new((0, 0)),
    });

    games::migrate_from_config(&state.shm);
    games::spawn_watcher(Arc::new(shm::Shm::new()));

    let server = Arc::new(Server::http(format!("{bind}:{port}")).expect("绑定端口失败"));
    println!("==============================================================");
    println!("  DLSSNR 控制面板 v{} (Rust)  端口 {port}  绑定 {bind}", env!("CARGO_PKG_VERSION"));
    if !state.token.is_empty() {
        println!("  token: {}", state.token);
    }
    println!("==============================================================");
    log(format!("面板启动 port={port} bind={bind}"));

    let mut workers = Vec::new();
    for _ in 0..8 {
        let server = Arc::clone(&server);
        let state = Arc::clone(&state);
        workers.push(std::thread::spawn(move || loop {
            match server.recv() {
                Ok(req) => handle(&state, req),
                Err(e) => log(format!("recv 错误: {e}")),
            }
        }));
    }
    for w in workers {
        let _ = w.join();
    }
}

fn handle(state: &Arc<AppState>, mut req: tiny_http::Request) {
    let method = req.method().clone();
    let (url, query) = match req.url().split_once('?') {
        Some((u, q)) => (u.to_string(), q.to_string()),
        None => (req.url().to_string(), String::new()),
    };
    let qs_token = query
        .split('&')
        .find_map(|kv| kv.strip_prefix("token=").map(String::from))
        .unwrap_or_default();
    let header_token = req
        .headers()
        .iter()
        .find(|h| h.field.as_str().as_str().eq_ignore_ascii_case("x-dlss-token"))
        .map(|h| h.value.as_str().to_string())
        .unwrap_or_default();

    if url.starts_with("/api/") && !state.token.is_empty() {
        let ok = header_token == state.token || qs_token == state.token;
        if !ok {
            respond(req, 401, &json!({"ok": false, "error": "令牌无效"}));
            return;
        }
    }

    if method == Method::Get && (url == "/" || url == "/index.html") {
        serve_index(req);
        return;
    }

    let mut body = Vec::new();
    if method == Method::Post {
        // 请求体上限 1MB：防超大 body 在 JSON 校验前耗尽内存
        let _ = req.as_reader().take(1024 * 1024 + 1).read_to_end(&mut body);
        if body.len() > 1024 * 1024 {
            respond(req, 413, &json!({"ok": false, "error": "请求体过大（>1MB）"}));
            return;
        }
    }
    let body_json: serde_json::Value = if body.is_empty() {
        json!({})
    } else {
        match serde_json::from_slice(&body) {
            Ok(v) => v,
            Err(e) => {
                respond(req, 400, &json!({"ok": false, "error": format!("JSON 无效: {e}")}));
                return;
            }
        }
    };

    match route(state, &method, &url, &body_json) {
        Ok((code, v)) => respond(req, code, &v),
        Err((code, msg)) => {
            log(format!("API 错误: {}", log_field(&msg)));
            respond(req, code, &json!({"ok": false, "error": msg}))
        }
    }
}

fn respond(req: tiny_http::Request, code: u16, v: &serde_json::Value) {
    let body = serde_json::to_string(v).unwrap_or_default();
    let response = Response::from_string(body)
        .with_status_code(code)
        .with_header(
            Header::from_bytes(&b"Content-Type"[..], &b"application/json; charset=utf-8"[..])
                .unwrap(),
        );
    let _ = req.respond(response);
}

fn serve_index(req: tiny_http::Request) {
    let html = include_str!("../static/index.html");
    let response = Response::from_string(html).with_header(
        Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..]).unwrap(),
    );
    let _ = req.respond(response);
}

type RouteResult = Result<(u16, serde_json::Value), (u16, String)>;

fn err(msg: impl Into<String>) -> RouteResult {
    Err((400, msg.into()))
}

/// 日志字段净化：控制字符转空格并截断，防换行伪造日志
pub fn log_field(s: &str) -> String {
    s.chars().map(|c| if c.is_control() { ' ' } else { c }).take(120).collect()
}

/// Result<String,String> → 路由成功值
fn sr(r: Result<String, String>) -> RouteResult {
    match r {
        Ok(m) => Ok((200, json!({"ok": true, "msg": m}))),
        Err(e) => Err((400, e)),
    }
}

fn route(
    state: &Arc<AppState>,
    method: &Method,
    path: &str,
    body: &serde_json::Value,
) -> RouteResult {
    let shm = &state.shm;
    match (method, path) {
        (Method::Get, "/api/status") => Ok((200, json!({"ok": true, "status": status_view(state)}))),
        (Method::Get, "/api/settings") => {
            if !shm.available() {
                return Ok((200, json!({"ok": true, "available": false})));
            }
            Ok((200, json!({
                "ok": true,
                "available": true,
                "global": shm.read_settings(),
                "passes": shm.read_passes(),
                "defs": &*defs::DEFS,
                "pass_defs": pass_defs(),
                "override_bits": override_bits(),
                "drift": drift_map(state),
            })))
        }
        (Method::Post, "/api/settings") => {
            let empty = serde_json::Map::new();
            let changes = body.get("set").and_then(|v| v.as_object()).unwrap_or(&empty);
            let passes = body.get("passes").and_then(|v| v.as_object()).unwrap_or(&empty);
            if changes.is_empty() && passes.is_empty() {
                return err("空修改");
            }
            let (n, tuning) = shm.write_settings(changes, passes).map_err(|e| (400u16, e))?;
            let persist_msg = if body.get("persist").and_then(|v| v.as_bool()).unwrap_or(true) {
                match sysapi::snapshot_config(shm) {
                    Ok(m) => m,
                    Err(e) => format!("（警告：参数已生效，但写 config.ini 存档失败: {e}）"),
                }
            } else {
                String::new()
            };
            // 活跃游戏双写（调参即回写当前游戏 profile）
            let active = games::ACTIVE_GAME.lock().unwrap().clone();
            if let Some(appid) = active {
                if let Some(mut p) = games::load_profile(&appid) {
                    for (k, v) in changes {
                        p.settings.insert(k.clone(), v.clone());
                    }
                    if !passes.is_empty() {
                        let mut pl = p.passes.clone();
                        for (idx, fields) in passes {
                            if let Ok(i) = idx.parse::<usize>() {
                                if i < defs::MAX_PASSES as usize {
                                    if let Some(f) = fields.as_object() {
                                        while pl.len() <= i {
                                            pl.push(serde_json::Map::new());
                                        }
                                        for (k, v) in f {
                                            pl[i].insert(k.clone(), v.clone());
                                        }
                                    }
                                }
                            }
                        }
                        p.passes = pl;
                    }
                    p.updated_at = chrono_now();
                    let _ = games::save_profile(&p);
                }
            }
            log(format!("settings 写入: {} {} → {n} 处 {persist_msg}",
                      serde_json::to_string(changes).unwrap_or_default(),
                      serde_json::to_string(passes).unwrap_or_default()));
            Ok((200, json!({
                "ok": true, "changed": n, "tuning": tuning,
                "msg": format!("已写入 {n} 处"),
            })))
        }
        (Method::Post, "/api/settings/persist") => sr(sysapi::snapshot_config(shm)),
        (Method::Post, "/api/settings/apply-config") => {
            let ini = sysapi::read_config_ini();
            let mut changes = serde_json::Map::new();
            let mut passes = serde_json::Map::new();
            for (k, v) in ini {
                if let Some(rest) = k.strip_prefix("set_pass_") {
                    let mut it = rest.splitn(2, '_');
                    if let (Some(idx), Some(suf)) = (it.next(), it.next()) {
                        if let Some(field) = pass_ini_suffix_to_id(suf) {
                            if let Ok(i) = idx.parse::<usize>() {
                                if i < defs::MAX_PASSES as usize {
                                    let entry = passes.entry(i.to_string()).or_insert_with(|| json!({}));
                                    if let Some(o) = entry.as_object_mut() {
                                        // 整数型 pass 字段按 i64 解析（bool/枚举不接受浮点）
                                        let val = if matches!(field,
                                            "override_mask" | "style" | "preset" | "auto_mask")
                                        {
                                            json!(v.parse::<i64>().unwrap_or(0))
                                        } else {
                                            json!(v.parse::<f64>().unwrap_or(0.0))
                                        };
                                        o.insert(field.to_string(), val);
                                    }
                                }
                            }
                        }
                    }
                } else if let Some(d) = defs::def_by_ini_key(&k) {
                    let val = match d.kind {
                        defs::Kind::Float | defs::Kind::Percent => json!(v.parse::<f64>().unwrap_or(0.0)),
                        _ => json!(v.parse::<i64>().unwrap_or(0)),
                    };
                    changes.insert(d.id.to_string(), val);
                }
            }
            let (n, _tuning) = shm.write_settings(&changes, &passes).map_err(|e| (400u16, e))?;
            let _ = sysapi::snapshot_config(shm);
            log(format!("apply-config: {n} 处"));
            Ok((200, json!({"ok": true, "changed": n,
                "msg": format!("已把 config.ini 存档写入共享内存（{n} 处）")})))
        }
        (Method::Post, "/api/helper") => {
            let action = body.get("action").and_then(|v| v.as_str()).unwrap_or("");
            let any_game = game_running_any(state);
            let v = match action {
                "start" => {
                    // 随时可启动：helper_start 幂等（已运行只提示），watcher 自动联动也是同一条路径。
                    // 游戏运行中若联动未触发（面板重启错过启动边沿等），这里是用户唯一的补救入口。
                    match sysapi::helper_start() {
                        Ok(m) => json!({"ok": true, "msg": m}),
                        Err(e) => json!({"ok": false, "msg": e}),
                    }
                }
                "stop" => {
                    if any_game {
                        json!({"ok": false, "msg": "游戏正在运行：停止会让画面旁路成原画，请先退出游戏再停"})
                    } else {
                        match sysapi::helper_stop() {
                            Ok(m) => json!({"ok": true, "msg": m}),
                            Err(e) => json!({"ok": false, "msg": e}),
                        }
                    }
                }
                "resume" => match shm.resume() {
                    Ok(()) => {
                        log("resume：已清 quit");
                        json!({"ok": true, "msg": "已清除退出标记，层将在下一帧重新接管（若助手已停请先启动）"})
                    }
                    Err(e) => json!({"ok": false, "msg": e}),
                },
                _ => return err("未知 action"),
            };
            Ok((200, v))
        }
        (Method::Get, "/api/models") => Ok((200, json!({"ok": true, "models": models::scan()}))),
        (Method::Post, "/api/models") => {
            let action = body.get("action").and_then(|v| v.as_str()).unwrap_or("");
            match action {
                "activate" => {
                    if game_running_any(state) {
                        return err("游戏正在运行，禁止切换模型（会误杀会话）");
                    }
                    let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    let msg = models::activate(name).map_err(|e| (400u16, e))?;
                    Ok((200, json!({"ok": true, "msg": msg})))
                }
                "refresh" => {
                    if let Some(lib) = body.get("lib").and_then(|v| v.as_object()) {
                        models::merge_lib(lib).map_err(|e| (400u16, e))?;
                    }
                    Ok((200, json!({"ok": true, "models": models::scan()})))
                }
                "import" => {
                    if game_running_any(state) {
                        return err("游戏正在运行，禁止导入模型（会误杀会话）");
                    }
                    let path = body.get("path").and_then(|v| v.as_str()).unwrap_or("");
                    let msg = models::import_from_path(path).map_err(|e| (400u16, e))?;
                    crate::log(format!("模型导入: {}", log_field(&msg)));
                    Ok((200, json!({"ok": true, "msg": msg, "models": models::scan()})))
                }
                _ => err("未知 action"),
            }
        }
        (Method::Get, "/api/filter") => {
            let status = vkbasalt::status();
            // 前端契约：enable 数组在顶层（Python 版既有形状），filter 内保留副本
            let enable = status.get("enable").cloned().unwrap_or(json!([]));
            Ok((200, json!({"ok": true, "filter": status, "enable": enable})))
        }
        (Method::Post, "/api/filter") => {
            let mut msgs = Vec::new();
            if let Some(set) = body.get("set").and_then(|v| v.as_object()) {
                for (k, v) in set {
                    let val = match v {
                        serde_json::Value::Number(n) => n.to_string(),
                        serde_json::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    vkbasalt::set_key(k, &val).map_err(|e| (400u16, e))?;
                    msgs.push(format!("{k}={val}"));
                }
            }
            // effects 链结构变化需重启；其余参数/LUT 热重载实时生效
            let tail = if msgs.iter().any(|m| m.starts_with("effects=")) { "（重启游戏生效）" } else { "（实时生效）" };
            Ok((200, json!({"ok": true, "msg": format!("已写入 {}{}", msgs.join(", "), tail)})))
        }
        (Method::Get, "/api/mangohud") => {
            Ok((200, json!({"ok": true, "mangohud": mangohud::status()})))
        }
        (Method::Post, "/api/mangohud") => {
            let changes = body.get("set").and_then(|v| v.as_object()).ok_or((400u16, "需要 set 对象".to_string()))?;
            let msg = mangohud::apply(changes).map_err(|e| (400u16, e))?;
            Ok((200, json!({"ok": true, "msg": msg})))
        }
        (Method::Get, "/api/metrics") => {
            Ok((200, json!({"ok": true, "metrics": metrics::to_json(&metrics::snapshot())})))
        }
        (Method::Get, "/api/hud") => {
            // 磁盘布局过一遍校验再返回（防手改文件注入非法值）
            let stored = hud::load_json().and_then(|v| hud::validate(&v).ok());
            let layout = stored.clone().unwrap_or_else(hud::default_layout);
            let mh = mangohud::status();
            let mp = mh.get("params").cloned().unwrap_or(json!({}));
            let ingame_enabled = !matches!(mp.get("no_display").and_then(|v| v.as_str()), Some("true") | Some("1"))
                && mp.get("custom_layout").and_then(|v| v.as_str()).map(|s| !s.is_empty()).unwrap_or(false);
            Ok((200, json!({
                "ok": true,
                "layout": layout,
                "is_default": stored.is_none(),
                "ingame_hud": {"enabled": ingame_enabled},
                "placeholders": metrics::PLACEHOLDERS,
                "layout_path": hud::layout_path().display().to_string(),
            })))
        }
        (Method::Post, "/api/hud") => {
            // reset：恢复默认中文布局；否则按提交的 layout 校验保存
            if body.get("action").and_then(|v| v.as_str()) == Some("reset") {
                let def = hud::default_layout();
                hud::save_json(&def).map_err(|e| (500u16, e))?;
                log("悬浮窗布局已恢复默认");
                return Ok((200, json!({"ok": true, "msg": "已恢复默认中文布局（游戏内 HUD ~0.5s 内热重载）", "layout": def})));
            }
            let layout = body.get("layout").ok_or((400u16, "需要 layout 对象".to_string()))?;
            let valid = hud::validate(layout).map_err(|e| (400u16, e))?;
            hud::save_json(&valid).map_err(|e| (500u16, e))?;
            log(format!("悬浮窗布局已保存（items={}）",
                      valid.get("items").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0)));
            Ok((200, json!({"ok": true, "msg": "布局已保存，悬浮窗 ~1s 内热加载", "layout": valid})))
        }
        (Method::Get, "/api/fps") => {
            // DLSS 链路帧计数（u64 LO/HI），前端 1s 轮询差分算 FPS
            let frames = read_split_u64(&shm, defs::status_off::HELPER_FRAMES_LO, defs::status_off::HELPER_FRAMES_HI).unwrap_or(0);
            let mon = json!({
                "ok": true,
                "frames": frames,
                "helper_state": shm.read_u32(defs::status_off::HELPER_STATE).unwrap_or(0),
                "model_up": shm.read_u32(defs::status_off::MODEL_UP).unwrap_or(0),
                "composition_up": shm.read_u32(defs::status_off::LAYER_COMPOSITION_UP).unwrap_or(0),
                "eval_ms": shm.read_f32(defs::status_off::EVAL_MS).unwrap_or(0.0),
                "vram_mb": shm.read_f32(defs::status_off::VRAM_MB).unwrap_or(0.0),
                "layer_ms": shm.read_f32(defs::status_off::LAYER_MS).unwrap_or(0.0),
                "helper_running": !crate::sysapi::helper_pids().is_empty(),
            });
            Ok((200, mon))
        }
        (Method::Post, "/api/filter/presets") => {
            let action = body.get("action").and_then(|v| v.as_str()).unwrap_or("");
            let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("");
            match action {
                "apply" => {
                    let msg = vkbasalt::apply_preset(name).map_err(|e| (400u16, e))?;
                    Ok((200, json!({"ok": true, "msg": msg})))
                }
                "save" | "update" | "delete" => {
                    let msg = vkbasalt::preset_action(action, name).map_err(|e| (400u16, e))?;
                    Ok((200, json!({"ok": true, "msg": msg})))
                }
                _ => Err((400u16, "action 需为 apply/save/update/delete".to_string())),
            }
        }
        (Method::Post, "/api/filter/enable") => {
            // 实时开关：LUT→identity + cas→0（热重载可靠路径），链不动
            let enable = body.get("enable").and_then(|v| v.as_bool()).ok_or((400u16, "需要 enable: bool".to_string()))?;
            let msg = vkbasalt::set_live(enable).map_err(|e| (400u16, e))?;
            Ok((200, json!({"ok": true, "msg": msg})))
        }
        (Method::Post, "/api/filter/launch") => {
            // 随游戏启动开关：runner user_settings.py 的 ENABLE_VKBASALT（重启游戏生效）
            let enable = body.get("enable").and_then(|v| v.as_bool()).ok_or((400u16, "需要 enable: bool".to_string()))?;
            let msg = vkbasalt::set_enabled(enable).map_err(|e| (400u16, e))?;
            Ok((200, json!({"ok": true, "msg": msg})))
        }
        (Method::Get, "/api/games") => {
            // 自愈：联动位指向已不存在的档（外部删除 / 档损坏）→ 清掉
            {
                let mut ag = games::ACTIVE_GAME.lock().unwrap();
                if let Some(id) = ag.as_deref() {
                    if games::load_profile(id).is_none() {
                        *ag = None;
                    }
                }
            }
            Ok((200, json!({
                "ok": true,
                "games": games::list_profiles(),
                "prefixes": games::scan_prefixes(),
                "active_game": *games::ACTIVE_GAME.lock().unwrap(),
            })))
        }
        (Method::Post, "/api/games") => {
            let action = body.get("action").and_then(|v| v.as_str()).unwrap_or("");
            match action {
                "save" | "update" => {
                    // patch 模式：磁盘现值为底，只覆盖请求携带的顶层字段（防并发整档回滚）
                    if let Some(patch) = body.get("patch").and_then(|v| v.as_object()) {
                        let appid = patch
                            .get("appid")
                            .and_then(|v| v.as_str())
                            .ok_or((400u16, "patch 缺少 appid".to_string()))?;
                        let mut p = games::load_profile(appid)
                            .ok_or((400u16, format!("档不存在: {appid}")))?;
                        if let Some(v) = patch.get("name") { p.name = v.as_str().unwrap_or(&p.name).to_string(); }
                        if let Some(v) = patch.get("auto_helper") { p.auto_helper = v.as_bool().unwrap_or(p.auto_helper); }
                        if let Some(v) = patch.get("auto_apply") { p.auto_apply = v.as_bool().unwrap_or(p.auto_apply); }
                        if let Some(v) = patch.get("auto_filter") { p.auto_filter = v.as_bool().unwrap_or(p.auto_filter); }
                        if let Some(v) = patch.get("auto_close_popups") { p.auto_close_popups = v.as_bool().unwrap_or(p.auto_close_popups); }
                        if let Some(v) = patch.get("popup_titles") {
                            p.popup_titles = serde_json::from_value(v.clone()).unwrap_or(p.popup_titles);
                        }
                        if let Some(v) = patch.get("match_exe") {
                            p.match_exe = serde_json::from_value(v.clone()).unwrap_or(p.match_exe);
                        }
                        games::save_profile(&p).map_err(|e| (400u16, e))?;
                        log(format!(
                            "游戏档字段更新: {} ({})",
                            log_field(&p.name),
                            log_field(&p.appid)
                        ));
                        return Ok((200, json!({"ok": true, "msg": format!("已更新 {} 的游戏档", p.name)})));
                    }
                    let p: games::GameProfile =
                        serde_json::from_value(body.get("profile").cloned().unwrap_or(json!({})))
                            .map_err(|e| (400u16, format!("profile 解析失败: {e}")))?;
                    games::save_profile(&p).map_err(|e| (400u16, e))?;
                    log(format!("游戏档保存: {} ({})", log_field(&p.name), log_field(&p.appid)));
                    Ok((200, json!({"ok": true, "msg": format!("已保存 {} 的游戏档", p.name)})))
                }
                "snapshot" => {
                    let shm_settings = shm.read_settings().ok_or((400u16, "共享内存不可用".to_string()))?;
                    let shm_passes = shm.read_passes().ok_or((400u16, "共享内存不可用".to_string()))?;
                    // 手动建档：启动目录（Steam 前缀 compatdata/<appid>）+ 游戏本体 exe 路径；
                    // 旧式调用（仅 appid + match_exe，无目录）保持兼容
                    let launch_dir = body.get("launch_dir").and_then(|v| v.as_str()).unwrap_or("").trim().trim_end_matches('/').to_string();
                    let game_exe = body.get("game_exe").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
                    let body_name = body.get("name").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
                    let body_match_exe: Vec<String> = body
                        .get("match_exe")
                        .and_then(|v| v.as_array())
                        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                        .unwrap_or_default();
                    let explicit_appid = body.get("appid").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();

                    let (appid, new_match_exe, fallback_name) = if launch_dir.is_empty() {
                        // 旧式：appid + match_exe 直接给定，不做文件校验
                        if !game_exe.is_empty() {
                            return err("game_exe 需与启动目录（launch_dir）同时填写");
                        }
                        if explicit_appid.is_empty() {
                            return err("缺少启动目录（steamapps/compatdata/<appid>）或 appid");
                        }
                        (explicit_appid, body_match_exe, String::new())
                    } else {
                        if game_exe.is_empty() {
                            return err("先填游戏本体位置（游戏主程序 exe 路径）");
                        }
                        // 规范化真实路径（消解 .. 与符号链接），容忍把启动目录填到 …/pfx
                        let dir_canon = std::fs::canonicalize(&launch_dir)
                            .map_err(|e| (400u16, format!("启动目录无效：{e}")))?;
                        let mut dir_str = dir_canon.to_string_lossy().into_owned();
                        if dir_str.ends_with("/pfx") {
                            dir_str.truncate(dir_str.len() - 4);
                        }
                        // appid：显式 appid > compatdata/<id> 提取 > 目录末段
                        let appid = if !explicit_appid.is_empty() {
                            explicit_appid
                        } else if let Some(pos) = dir_str.rfind("/compatdata/") {
                            dir_str[pos + "/compatdata/".len()..].split('/').next().unwrap_or("").to_string()
                        } else {
                            dir_str.rsplit('/').next().unwrap_or("").to_string()
                        };
                        if appid.is_empty() || appid == "pfx" {
                            return err("无法确定 appid：启动目录请填 Steam 库的 steamapps/compatdata/<appid>");
                        }
                        let pfx = PathBuf::from(&dir_str).join("pfx");
                        if !pfx.is_dir() {
                            return err(format!("启动目录不对：{} 不存在（请填 Steam 库的 steamapps/compatdata/<appid>）", pfx.display()));
                        }
                        // 本体：绝对路径直接校验；相对路径按 drive_c 内相对定位
                        let exe_path = if game_exe.starts_with('/') {
                            PathBuf::from(&game_exe)
                        } else {
                            pfx.join("drive_c").join(&game_exe)
                        };
                        if !exe_path.is_file() {
                            return err(format!("游戏本体不存在：{}", exe_path.display()));
                        }
                        let exe_name = exe_path
                            .file_name()
                            .map(|s| s.to_string_lossy().into_owned())
                            .ok_or((400u16, "本体路径异常".to_string()))?;
                        let fallback = exe_name.trim_end_matches(".exe").trim_end_matches(".EXE").to_string();
                        (appid, vec![exe_name], fallback)
                    };

                    let existing = games::load_profile(&appid);
                    let mut p = existing.clone().unwrap_or(games::GameProfile {
                        appid: appid.clone(),
                        name: if body_name.is_empty() { "游戏".to_string() } else { body_name.clone() },
                        match_exe: new_match_exe.clone(),
                        auto_apply: true,
                        auto_helper: true,
                        auto_filter: false,
                        auto_close_popups: true,
                        popup_titles: games::default_popup_titles(),
                        filter: None,
                        settings: shm_settings.clone(),
                        passes: shm_passes.clone(),
                        initial: None,
                        updated_at: chrono_now(),
                    });
                    if existing.is_none() {
                        p.match_exe = new_match_exe; // 新档以填写为准
                    } else if !new_match_exe.is_empty() && !launch_dir.is_empty() {
                        p.match_exe = new_match_exe; // 更新档时本次显式指定了本体 → 刷新匹配
                    }
                    if !body_name.is_empty() {
                        p.name = body_name;
                    } else if p.name == "游戏" && !fallback_name.is_empty() {
                        p.name = fallback_name;
                    }
                    p.settings = shm_settings;
                    p.passes = shm_passes;
                    p.updated_at = chrono_now();
                    // 滤镜：null/缺省 = 保留既有配置；显式对象 = 覆盖
                    match body.get("filter") {
                        None | Some(serde_json::Value::Null) => {}
                        Some(v) if v.is_object() => p.filter = Some(v.clone()),
                        Some(_) => return err("filter 必须为对象"),
                    }
                    if p.initial.is_none() {
                        p.initial = Some(json!({
                            "settings": p.settings,
                            "passes": p.passes,
                        }));
                    }
                    games::save_profile(&p).map_err(|e| (400u16, e))?;
                    log(format!("游戏档保存: {} ({})", log_field(&p.name), log_field(&p.appid)));
                    Ok((200, json!({"ok": true, "msg": format!("已把当前设置存为 {} 的游戏档（匹配 {}）", p.name, p.match_exe.join(", "))})))
                }
                "reset" | "reset-initial" => {   // "reset" 为历史别名，防前端旧版
                    let appid = body.get("appid").and_then(|v| v.as_str()).unwrap_or("");
                    let mut p = games::load_profile(appid).ok_or((400u16, "档不存在".to_string()))?;
                    let init = p.initial.clone().ok_or((400u16, "没有初始快照".to_string()))?;
                    if let Some(s) = init.get("settings") {
                        p.settings = s.as_object().cloned().unwrap_or_default();
                    }
                    if let Some(s) = init.get("passes") {
                        p.passes = serde_json::from_value(s.clone()).unwrap_or_default();
                    }
                    p.updated_at = chrono_now();
                    games::save_profile(&p).map_err(|e| (400u16, e))?;
                    if games::ACTIVE_GAME.lock().unwrap().as_deref() == Some(appid) {
                        if let Ok(msg) = games::apply_profile(shm, &p) {
                            log(format!("reset-initial 应用: {msg}"));
                        }
                    }
                    Ok((200, json!({"ok": true, "msg": "已还原该游戏的初始参数"})))
                }
                "apply" => {
                    let appid = body.get("appid").and_then(|v| v.as_str()).unwrap_or("");
                    let p = games::load_profile(appid).ok_or((400u16, "档不存在".to_string()))?;
                    let msg = games::apply_profile(shm, &p).map_err(|e| (400u16, e))?;
                    log(format!("手动应用游戏档: {} → {msg}", p.name));
                    Ok((200, json!({"ok": true, "msg": msg})))
                }
                "delete" => {
                    let appid = body.get("appid").and_then(|v| v.as_str()).unwrap_or("");
                    let name = games::load_profile(appid).map(|p| p.name).unwrap_or_default();
                    games::delete_profile(appid).map_err(|e| (400u16, e))?;
                    // 删除的是当前联动游戏 → 同步清位，避免联动横幅残留
                    if games::ACTIVE_GAME.lock().unwrap().as_deref() == Some(appid) {
                        *games::ACTIVE_GAME.lock().unwrap() = None;
                    }
                    log(format!("游戏档删除: {} ({})", log_field(&name), log_field(appid)));
                    Ok((200, json!({"ok": true, "msg": "已删除"})))
                }
                _ => err("未知 action"),
            }
        }
        (Method::Get, "/api/profiles") => {
            Ok((200, json!({"ok": true, "profiles": profiles_list()})))
        }
        (Method::Post, "/api/profiles") => {
            let action = body.get("action").and_then(|v| v.as_str()).unwrap_or("");
            match action {
                "save" => {
                    let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    if name.trim().is_empty() {
                        return err("配置档名不合法");
                    }
                    let settings = shm.read_settings().ok_or((400u16, "共享内存不可用".to_string()))?;
                    let passes = shm.read_passes().ok_or((400u16, "共享内存不可用".to_string()))?;
                    let data = json!({
                        "name": name,
                        "note": body.get("note").and_then(|v| v.as_str()).unwrap_or(""),
                        "time": chrono_now(),
                        "global": settings,
                        "passes": passes,
                    });
                    std::fs::create_dir_all(profiles_dir()).map_err(|e| (400u16, e.to_string()))?;
                    let safe: String = name
                        .chars()
                        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' || (c as u32) > 0x4E00 { c } else { '_' })
                        .collect();
                    let path = profiles_dir().join(format!("{safe}.json"));
                    std::fs::write(&path, serde_json::to_string_pretty(&data).map_err(|e| (400u16, e.to_string()))?)
                        .map_err(|e| (400u16, e.to_string()))?;
                    log(format!("配置档保存: {name}"));
                    Ok((200, json!({"ok": true, "profiles": profiles_list()})))
                }
                "load" => {
                    let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    let safe: String = name
                        .chars()
                        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' || (c as u32) > 0x4E00 { c } else { '_' })
                        .collect();
                    let path = profiles_dir().join(format!("{safe}.json"));
                    let data: serde_json::Value = serde_json::from_str(
                        &std::fs::read_to_string(&path).map_err(|_| (400u16, "配置档不存在".to_string()))?,
                    )
                    .map_err(|e| (400u16, e.to_string()))?;
                    let empty = serde_json::Map::new();
                    let changes = data.get("global").and_then(|v| v.as_object()).unwrap_or(&empty);
                    let mut passes = serde_json::Map::new();
                    if let Some(list) = data.get("passes").and_then(|v| v.as_array()) {
                        for (i, it) in list.iter().enumerate() {
                            passes.insert(i.to_string(), it.clone());
                        }
                    }
                    let (n, tuning) = shm.write_settings(changes, &passes).map_err(|e| (400u16, e))?;
                    let _ = sysapi::snapshot_config(shm);
                    log(format!("配置档载入: {name} → {n} 处"));
                    Ok((200, json!({"ok": true, "msg": format!("已载入 {name}（{n} 处）")})))
                }
                "delete" => {
                    let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    let safe: String = name
                        .chars()
                        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' || (c as u32) > 0x4E00 { c } else { '_' })
                        .collect();
                    let _ = std::fs::remove_file(profiles_dir().join(format!("{safe}.json")));
                    log(format!("配置档删除: {name}"));
                    Ok((200, json!({"ok": true, "profiles": profiles_list()})))
                }
                _ => err("未知 action"),
            }
        }
        (Method::Post, "/api/service") => {
            let action = body.get("action").and_then(|v| v.as_str()).unwrap_or("status");
            let v = sysapi::service(action).map_err(|e| (400u16, e))?;
            Ok((200, json!({"ok": true, "service": v})))
        }
        (Method::Post, "/api/capture") => {
            let n = body.get("n").and_then(|v| v.as_i64()).unwrap_or(2) as u32;
            if !(1..=8).contains(&n) {
                return err("n 限 1~8");
            }
            shm.request_capture(n).map_err(|e| (400u16, e))?;
            Ok((200, json!({"ok": true, "msg": format!("抓帧请求已发出（{n} 组）。需游戏与层活跃；文件将出现在 captures 目录。")})))
        }
        (Method::Get, "/api/captures") => {
            let dir = sysapi::state_dir().join("captures");
            let mut files: Vec<(std::time::SystemTime, serde_json::Value)> =
                sysapi::entries(&dir)
                    .iter()
                    .filter_map(|e| {
                        let md = e.metadata().ok()?;
                        let mtime = md.modified().ok()?;
                        Some((
                            mtime.clone(),
                            json!({
                                "name": e.file_name().into_string().ok()?,
                                "size_kb": md.len() / 1024,
                                "mtime": chrono_fmt_time(&mtime),
                            }),
                        ))
                    })
                    .collect();
            files.sort_by(|a, b| b.0.cmp(&a.0));
            files.truncate(12);
            let files: Vec<_> = files.into_iter().map(|(_, v)| v).collect();
            Ok((200, json!({"ok": true, "captures": files, "dir": dir.display().to_string()})))
        }
        (Method::Post, "/api/legacy/kill-gui") => {
            let pids = sysapi::legacy_gui_pids();
            for p in &pids {
                let _ = std::process::Command::new("kill")
                    .args(["-TERM", &p.to_string()])
                    .output();
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
            let left = sysapi::legacy_gui_pids();
            Ok((200, json!({"ok": left.is_empty(),
                "msg": if left.is_empty() { "旧 GUI 已关闭".to_string() } else { format!("仍有残留: {left:?}") }})))
        }
        _ => {
            log(format!("404 未知接口: {method:?} {path}"));
            Err((404, "未知接口".into()))
        }
    }
}

fn profiles_dir() -> PathBuf {
    sysapi::home().join(".config/dlssnr/profiles")
}

fn profiles_list() -> serde_json::Value {
    let mut out: Vec<serde_json::Value> = sysapi::entries(&profiles_dir())
        .iter()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| {
            let d: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(e.path()).ok()?).ok()?;
            Some(json!({
                "name": d.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                "note": d.get("note").and_then(|v| v.as_str()).unwrap_or(""),
                "time": d.get("time").and_then(|v| v.as_str()).unwrap_or(""),
            }))
        })
        .collect();
    out.sort_by_key(|p| p["name"].as_str().unwrap_or("").to_string());
    json!(out)
}

fn game_running_any(state: &Arc<AppState>) -> bool {
    // 档匹配优先；匹配失效（未建档/档删除/换 exe）但层已接管（model_up=1）时
    // 仍视为有活跃会话，避免安全闸误判为「无游戏」而拆掉正在使用的 helper
    if games::list_profiles()
        .iter()
        .any(|p| !p.match_exe.is_empty() && sysapi::game_running(&p.match_exe))
    {
        return true;
    }
    state
        .shm
        .read_u32(defs::status_off::MODEL_UP)
        .map(|v| v == 1)
        .unwrap_or(false)
}

fn read_model_lib() -> serde_json::Map<String, serde_json::Value> {
    std::fs::read_to_string(models::models_dir().join("library.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn pass_ini_suffix_to_id(suffix: &str) -> Option<&'static str> {
    Some(match suffix {
        "override_mask" => "override_mask",
        "intensity" => "intensity",
        "local_structure" => "local_structure",
        "local_tone" => "local_tone",
        "skin_structure" => "skin_structure",
        "sharpness" => "sharpness",
        "style" => "style",
        "preset" => "preset",
        "auto_mask" => "auto_mask",
        _ => return None,
    })
}

fn numeric(v: String) -> serde_json::Value {
    if let Ok(f) = v.parse::<f64>() {
        json!(f)
    } else {
        json!(v.parse::<i64>().unwrap_or(0))
    }
}

fn pass_defs() -> serde_json::Value {
    json!([
        {"key": "override_mask", "label": "覆盖掩码", "kind": "mask"},
        {"key": "intensity", "label": "强度", "kind": "float", "lo": 0.0, "hi": 4.0, "step": 0.05},
        {"key": "local_tone", "label": "局部影调", "kind": "float", "lo": 0.0, "hi": 4.0, "step": 0.05},
        {"key": "local_structure", "label": "局部结构", "kind": "float", "lo": 0.0, "hi": 4.0, "step": 0.05},
        {"key": "skin_structure", "label": "皮肤结构", "kind": "float", "lo": -1.0, "hi": 4.0, "step": 0.05},
        {"key": "sharpness", "label": "锐化", "kind": "float", "lo": 0.0, "hi": 1.0, "step": 0.05},
        {"key": "style", "label": "风格", "kind": "enum", "options": ["Default", "Natural", "Cinematic"]},
        {"key": "preset", "label": "预设", "kind": "int", "lo": 0, "hi": 15, "step": 1},
        {"key": "auto_mask", "label": "皮肤蒙版", "kind": "bool"},
    ])
}

fn override_bits() -> serde_json::Value {
    let m: serde_json::Map<String, serde_json::Value> = defs::PASS_OVERRIDE_BITS
        .iter()
        .map(|(k, v)| (k.to_string(), json!(v)))
        .collect();
    json!(m)
}

fn status_view(state: &Arc<AppState>) -> serde_json::Value {
    let shm = &state.shm;
    let mut st = serde_json::Map::new();
    let available = shm.available();
    st.insert("available".into(), json!(available));
    if !available {
        st.insert("reason".into(), json!(shm.ensure_error().unwrap_or_default()));
        st.insert("helper_liveness".into(), json!("stopped"));
        st.insert("game_running".into(), json!(game_running_any(state)));
        return serde_json::Value::Object(st);
    }
    let r = |off| shm.read_u32(off).unwrap_or(0);
    st.insert("abi_ok".into(), json!(shm.abi_ok()));
    st.insert("width".into(), json!(r(defs::status_off::WIDTH)));
    st.insert("height".into(), json!(r(defs::status_off::HEIGHT)));
    st.insert("quit".into(), json!(r(defs::status_off::QUIT)));
    st.insert("heartbeat".into(), json!(r(defs::status_off::HEARTBEAT)));
    let helper_frames =
        ((r(defs::status_off::HELPER_FRAMES_HI) as u64) << 32) | r(defs::status_off::HELPER_FRAMES_LO) as u64;
    let layer_frames =
        ((r(defs::status_off::LAYER_FRAMES_HI) as u64) << 32) | r(defs::status_off::LAYER_FRAMES_LO) as u64;
    st.insert("helper_frames".into(), json!(helper_frames));
    st.insert("layer_frames".into(), json!(layer_frames));
    st.insert("eval_ms".into(), json!(shm.read_f32(defs::status_off::EVAL_MS).unwrap_or(0.0)));
    st.insert("vram_mb".into(), json!(shm.read_f32(defs::status_off::VRAM_MB).unwrap_or(0.0)));
    st.insert("features".into(), json!(r(defs::status_off::FEATURES)));
    st.insert("model_up".into(), json!(r(defs::status_off::MODEL_UP)));
    st.insert("layer_pid".into(), json!(r(defs::status_off::LAYER_PID)));
    st.insert("layer_composition_up".into(), json!(r(defs::status_off::LAYER_COMPOSITION_UP)));
    st.insert("seq_ok".into(), json!(r(defs::status_off::SEQ_OK)));
    st.insert("hdr_active".into(), json!(r(defs::status_off::HDR_ACTIVE)));
    st.insert("proxy_format".into(), json!(r(defs::status_off::PROXY_FORMAT)));
    let state_names = ["启动中", "无 Vulkan 设备", "找不到模型 DLL", "模型创建失败", "运行中", "已停止"];
    let hs = r(defs::status_off::HELPER_STATE) as usize;
    st.insert("helper_state".into(), json!(hs));
    st.insert("helper_state_text".into(), json!(state_names.get(hs).unwrap_or(&"?")));
    st.insert("game_name".into(), json!(shm.read_str(defs::status_off::GAME_NAME, 128).unwrap_or_default()));
    st.insert("helper_reason".into(), json!(shm.read_str(defs::status_off::HELPER_REASON, 192).unwrap_or_default()));
    st.insert("layer_reason".into(), json!(shm.read_str(defs::status_off::LAYER_REASON, 192).unwrap_or_default()));

    let helper_procs = sysapi::helper_pids();
    st.insert("helper_procs".into(), json!(helper_procs));
    st.insert("helper_running".into(), json!(!helper_procs.is_empty()));
    st.insert("game_running".into(), json!(game_running_any(state)));
    st.insert("legacy_gui_pids".into(), json!(sysapi::legacy_gui_pids()));
    st.insert("now".into(), json!(chrono_now()));

    // helper 三态：进程在但心跳/seq_ok 不动 = silent（卡住），判活标准来自 upstream issue#37
    let key = r(defs::status_off::HEARTBEAT) as u64 + r(defs::status_off::SEQ_OK) as u64;
    let mut lv = state.liveness.lock().unwrap();
    let (prev, count) = *lv;
    let liveness = if !helper_procs.is_empty() {
        if count == 0 {
            "unknown"
        } else if key != prev {
            "running"
        } else {
            "silent"
        }
    } else {
        "stopped"
    };
    *lv = (key, count + 1);
    st.insert("helper_liveness".into(), json!(liveness));
    st.insert("active_game".into(), json!(*games::ACTIVE_GAME.lock().unwrap()));
    serde_json::Value::Object(st)
}

fn drift_map(state: &Arc<AppState>) -> serde_json::Value {
    let ini = sysapi::read_config_ini();
    let Some(cur) = state.shm.read_settings() else { return json!({}) };
    let mut out = serde_json::Map::new();
    for d in defs::DEFS.iter() {
        if !d.persist {
            continue;
        }
        let Some(now) = cur.get(d.id).and_then(|v| v.as_f64()) else { continue };
        let Some(saved) = ini.get(d.ini_key).and_then(|v| v.parse::<f64>().ok()) else {
            out.insert(d.id.to_string(), json!(true));
            continue;
        };
        let saved = if d.kind == defs::Kind::Percent { saved * 100.0 } else { saved };
        out.insert(d.id.to_string(), json!((saved - now).abs() > 1e-4));
    }
    json!(out)
}

/// 双读高位的稳定 u64 快照（LO/HI 分开存时防回绕撕裂）
fn read_split_u64(shm: &shm::Shm, lo: usize, hi: usize) -> Option<u64> {
    for _ in 0..4 {
        let hi1 = shm.read_u32(hi)?;
        let low = shm.read_u32(lo)?;
        let hi2 = shm.read_u32(hi)?;
        if hi1 == hi2 {
            return Some(((hi1 as u64) << 32) | low as u64);
        }
    }
    None
}
