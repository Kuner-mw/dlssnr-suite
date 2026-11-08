//! 统一参数 schema：单一真相表。
//!
//! 命名规范（三层映射，全项目唯一来源）：
//! * `id`      —— snake_case，API / JSON / 配置档统一键名（前端即此名）
//! * `offset`  —— 共享内存 ShmHeader 字段偏移（shm_protocol.h v20，实机核对）
//! * `ini_key` —— config.ini 物理键名（官方 GUI 的硬编码 ABI，不可改动）
//!
//! config.ini 物理键名与本 id 不同是官方历史命名（如 detail_strength ↔
//! set_detail_strength、max_ratio ↔ set_highlight_guard），必须原样保留。

use serde::Serialize;

pub const SHM_MAGIC: u32 = 0x3252_4E47; // 'GNR2'
pub const SHM_VERSION: u32 = 20;
pub const HEADER_BYTES: usize = 65536;
pub const PASS_BASE: usize = 780;
pub const PASS_STRIDE: usize = 36;
pub const MAX_PASSES: u32 = 30;

// pass 块内字段偏移（源码序：local_tone 在 local_structure 之前，勿按字母序）
pub const PASS_FIELDS: [(&str, usize, bool); 9] = [
    ("override_mask", 0, false),
    ("intensity", 4, true),
    ("local_tone", 8, true),
    ("local_structure", 12, true),
    ("skin_structure", 16, true),
    ("sharpness", 20, true),
    ("style", 24, false),
    ("preset", 28, false),
    ("auto_mask", 32, false),
];

/// 覆盖掩码位序（shm_protocol.h PassOverrideBit）
pub const PASS_OVERRIDE_BITS: [(&str, u32); 8] = [
    ("intensity", 1),
    ("local_structure", 2),
    ("local_tone", 4),
    ("skin_structure", 8),
    ("style", 16),
    ("preset", 32),
    ("auto_mask", 64),
    ("sharpness", 128),
];

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Bool,
    Int,
    Float,
    Percent,
    Enum,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Latch {
    Live,
    AtCreate,
}

#[derive(Clone, Serialize)]
pub struct SettingDef {
    pub id: &'static str,
    pub ini_key: &'static str,
    pub offset: usize,
    pub kind: Kind,
    pub latch: Latch,
    pub group: &'static str,
    pub label: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tip: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lo: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hi: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<&'static str>>,
    /// proven=确认有效 / weak=效果偏弱 / 无=未标注工具项
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effect: Option<&'static str>,
    /// 逻辑开关存反位（合成开关 ↔ composition_bypass）
    pub invert: bool,
    /// 是否持久化进 config.ini（hold_frame 是实时对比控制，不落盘）
    pub persist: bool,
}

macro_rules! def {
    ($id:literal, $ini:literal, $off:literal, $kind:ident, $latch:ident, $group:literal,
     $label:literal, $effect:expr, $invert:expr, $persist:expr,
     lo:$lo:expr, hi:$hi:expr, step:$st:expr, options:$opts:expr, tip:$tip:expr) => {
        SettingDef {
            id: $id, ini_key: $ini, offset: $off,
            kind: Kind::$kind, latch: Latch::$latch, group: $group,
            label: $label, tip: $tip, lo: $lo, hi: $hi, step: $st,
            options: $opts, effect: $effect, invert: $invert, persist: $persist,
        }
    };
}

pub static DEFS: LazyLock<Vec<SettingDef>> = std::sync::LazyLock::new(|| vec![
    // ── core ────────────────────────────────────────────────────────────
    def!("enabled", "set_enabled", 44, Bool, Live, "core", "模型总开关",
         Some("proven"), false, true, lo:None, hi:None, step:None, options:None,
         tip:Some("效果：A. 开=NR 接管画面；B. 关=瞬间变回原画。生效：下一帧，即时。B 用来排查『NR 到底有没有在干活』。")),
    def!("style", "set_style", 60, Enum, AtCreate, "core", "风格档位",
         Some("proven"), false, true, lo:None, hi:None, step:None,
         options:Some(vec!["Default(0)", "Natural(1)", "Cinematic(2)"]),
         tip:Some("三档官方风格——A. Default(0)=均衡通用；B. Natural(1)=自然挂，皮肤过渡更细腻；C. Cinematic(2)=电影感，对比更柔。生效：改完约 1 秒（模型重建），画面直接变。")),
    def!("intensity", "set_intensity", 68, Float, AtCreate, "core", "强度 intensity",
         Some("proven"), false, true, lo:Some(0.0), hi:Some(4.0), step:Some(0.05), options:None,
         tip:Some("效果：整体剂量旋钮——数值越大改动越强，噪点也随之增多；接近 0 时画面几乎无变化。验证 NR 是否生效请用【模型总开关】A/B 或抓帧对比，不要靠目测低强度。生效：改完约 1 秒（重建）。")),
    def!("local_tone", "set_local_tone", 72, Float, AtCreate, "core", "局部影调 local tone",
         Some("proven"), false, true, lo:Some(0.0), hi:Some(4.0), step:Some(0.05), options:None,
         tip:Some("效果：明暗重塑主力——A. 0=画面完全无变化；数值越大明暗对比越强。生效：改完约 1 秒（重建）。")),
    def!("local_structure", "set_local_structure", 76, Float, AtCreate, "core", "局部结构 local structure",
         Some("proven"), false, true, lo:Some(0.0), hi:Some(4.0), step:Some(0.05), options:None,
         tip:Some("效果：轮廓与纹理的立体感——数值越大边缘越硬。生效：改完约 1 秒（重建）。")),
    // ── compose ─────────────────────────────────────────────────────────
    def!("detail_strength", "set_detail_strength", 88, Float, Live, "compose", "落地强度 detail",
         Some("proven"), false, true, lo:Some(0.0), hi:Some(4.0), step:Some(0.05), options:None,
         tip:Some("效果：模型改动落地几成——A. 0=一丝不加（纯原画）；B. 0.85=落地 85%；C. 1.8=剂量接近翻倍。生效：拖完下一帧，即时。")),
    def!("colour_strength", "set_colour_strength", 92, Float, Live, "compose", "色彩强度 colour",
         Some("proven"), false, true, lo:Some(0.0), hi:Some(2.0), step:Some(0.05), options:None,
         tip:Some("效果：模型的色彩变化落地几成——数值越大色彩改动越明显，过高易泛黄，慎用。生效：即时。")),
    def!("max_ratio", "set_highlight_guard", 96, Float, Live, "compose", "高光闸 guard",
         Some("proven"), false, true, lo:Some(0.5), hi:Some(8.0), step:Some(0.1), options:None,
         tip:Some("效果：单像素最大亮度变化上限——A. 3.0=默认；B. 1.2=收敛高光；C. 6.0=放开。注意：1 层时怎么调都无感，passes≥2 才看得出差别。生效：即时。")),
    def!("transfer", "set_enlargement", 100, Enum, Live, "compose", "合成方式 transfer",
         Some("proven"), false, true, lo:None, hi:None, step:None,
         options:Some(vec!["classic(0)", "matched residual(1)", "native+edit(2)"]),
         tip:Some("合成方式三选一——A. classic(0)=老式整幅缩放合成；B. matched residual(1)=残差匹配；C. native+edit(2)=原画面只叠模型改动，保锐度（推荐）。生效：即时。")),
    def!("composition_enabled", "set_composition_enabled", 1876, Bool, Live, "compose", "合成开关",
         None, true, true, lo:None, hi:None, step:None, options:None,
         tip:Some("效果：A. 开=按上面参数正常合成（平时用这个）；B. 关=模型原始输出直接上屏（绕过强度/色彩/高光闸，检查模型在干嘛用）。生效：即时。")),
    def!("apply_model", "set_apply_model", 164, Bool, Live, "compose", "应用模型改动",
         None, false, true, lo:None, hi:None, step:None, options:None,
         tip:Some("效果：A. 开=正常 NR 画面；B. 关=上屏无 NR 的干净帧——开销完全一样，A/B 对比『模型到底改了什么』专用。生效：即时。")),
    // ── advanced ────────────────────────────────────────────────────────
    def!("passes", "set_passes", 48, Int, AtCreate, "advanced", "推理层数 passes",
         Some("proven"), false, true, lo:Some(1.0), hi:Some(30.0), step:Some(1.0), options:None,
         tip:Some("效果：同一帧跑几遍模型——A. 1 层=最快最稳；B. 2 层=立体感更好、帧率略降；C. 3 层=精调但平坦区噪声会更明显。生效：改完约 1 秒（全部层重建）。")),
    def!("working_scale", "set_model_resolution", 128, Percent, AtCreate, "advanced", "模型分辨率",
         None, false, true, lo:Some(25.0), hi:Some(200.0), step:Some(5.0), options:None,
         tip:Some("效果：模型用多大分辨率工作——A. 100%=全分辨率；B. 降低=减少计算量但画质可能变软；C. 提高=超采样更细、开销更大。游戏画面本身永不缩放。生效：改完约 1 秒（重建，日志打 neural ready 新尺寸）。")),
    def!("sdr16_multipass", "set_sdr_16bit_multipass", 1960, Bool, AtCreate, "advanced", "SDR 多层 16-bit",
         Some("proven"), false, true, lo:None, hi:None, step:None, options:None,
         tip:Some("效果：多层时层间图用什么精度——A. 开=16-bit，多层时噪声略降；B. 关=省显存带宽。生效：改完约 1 秒（重建）。")),
    def!("hdr_mode", "set_hdr_mode", 1932, Enum, AtCreate, "advanced", "HDR 输入",
         None, false, true, lo:None, hi:None, step:None,
         options:Some(vec!["auto(0)", "off(1)", "force float16(2)"]),
         tip:Some("HDR 输入三选一——A. auto(0)=游戏给 HDR 才走 HDR；B. off(1)=永远 8-bit；C. force(2)=强推 float16。注意：部分模型（如社区 Ada retarget 版 DLL）会拒绝 float16 合约并自动回退 8-bit，此时 C 仅用于 A/B 对比验证。生效：改完约 1 秒（重建）。")),
    def!("colour_mode", "set_colour_mode", 148, Enum, Live, "advanced", "色彩模式",
         None, false, true, lo:None, hi:None, step:None,
         options:Some(vec!["auto(0)", "display-referred(1)", "linear HDR(2)"]), tip:None),
    def!("reversible_mode", "set_proxy", 160, Enum, Live, "advanced", "可逆代理",
         None, false, true, lo:None, hi:None, step:None,
         options:Some(vec!["knee(0)", "neutwo(1)", "neutwo+replace(2)", "hybrid(3)", "hybrid+replace(4)"]),
         tip:Some("模型输出怎么回画面——A. knee(0)=软膝压缩（默认最稳）；B. neutwo(1)/hybrid(3)=保留未压缩高光；C. neutwo+replace(2)/hybrid+replace(4)=直接替换（上游注明强光会闪，慎用）。生效：即时。")),
    def!("mvec_enabled", "set_motion_enabled", 1860, Bool, Live, "advanced", "光流运动矢量",
         Some("proven"), false, true, lo:None, hi:None, step:None, options:None,
         tip:Some("效果：给模型供运动矢量——A. 开=时域效果更稳，但增加每帧开销（幅度取决于硬件、分辨率与场景）；B. 关=省算力，画面可能轻微不稳。生效：即时（日志打 enabled/disabled）。")),
    def!("mvec_quality", "set_motion_quality", 1868, Enum, Live, "advanced", "光流质量",
         None, false, true, lo:None, hi:None, step:None,
         options:Some(vec!["fast(0)", "balanced(1)", "quality(2)"]), tip:None),
    def!("scaling_downscaler", "set_down_leg_filter", 172, Enum, Live, "advanced", "下采样滤波",
         None, false, true, lo:None, hi:None, step:None,
         options:Some(vec!["FSR1(0,保留)", "Bicubic(1)", "Catmull(2)", "Lanczos2(3)", "Lanczos3(4)", "Kaiser2(5)", "Kaiser3(6)", "Magic(7)"]),
         tip:Some("效果：模型分辨率 >100% 时把结果缩回原尺寸的滤波器——A. Lanczos3(4)=锐利；B. Bicubic(1)=柔和。≤100% 时完全用不到，随便放。生效：即时。")),
    def!("rebuild_settle_ms", "set_rebuild_settle_ms", 1880, Int, Live, "advanced", "重建冷却 ms",
         None, false, true, lo:Some(0.0), hi:Some(2000.0), step:Some(10.0), options:None,
         tip:Some("效果：连续调参后等多久重建模型——A. 100ms=默认（推荐）；B. 调大=拖滑杆更不容易连环重建。生效：即时。")),
    def!("toggle_key", "set_toggle_key", 156, Int, Live, "advanced", "层热键 KEY_ 码",
         None, false, true, lo:Some(0.0), hi:Some(300.0), step:Some(1.0), options:None,
         tip:Some("效果：绑一个键盘键在游戏里随时开关 NR——A. 0=不绑（默认，防误触）；B. 102=Home 键。生效：即时（需层能读到键盘，X11/XWayland 会话可以）。")),
    // ── legacy（效果偏弱/偏皮肤向，折叠展示） ────────────────────────────
    def!("preset", "set_preset", 56, Int, AtCreate, "legacy", "预设 preset",
         None, false, true, lo:Some(0.0), hi:Some(15.0), step:Some(1.0), options:None,
         tip:Some("效果：模型内置渲染预设编号（0~15），每个编号对应一组模型内部的处理参数组合。生效：改完约 1 秒（重建）。配合【风格档位】使用，不同游戏可试不同编号找观感。")),
    def!("sharpness", "set_sharpness", 84, Float, Live, "legacy", "锐化 sharpness",
         None, false, true, lo:Some(0.0), hi:Some(1.0), step:Some(0.05), options:None,
         tip:Some("效果：模型输出的锐化强度——A. 0=不锐化；数值越大越锐。生效：下一帧，即时。想要更明显的锐化效果可配合【滤镜】页的 cas。")),
    def!("skin_structure", "set_skin_structure", 80, Float, AtCreate, "legacy", "皮肤结构 skin structure",
         None, false, true, lo:Some(-1.0), hi:Some(4.0), step:Some(0.05), options:None,
         tip:Some("效果：皮肤区域的结构强度，单独控制人脸/皮肤处的纹理细节——A. -1=跟随局部结构；B. 0.35=柔和（默认）；C. 调高=皮肤更硬更细节。生效：改完约 1 秒（重建）。")),
    def!("auto_mask", "set_auto_mask", 64, Bool, AtCreate, "legacy", "自动皮肤蒙版",
         None, false, true, lo:None, hi:None, step:None, options:None,
         tip:Some("效果：开=模型自动识别皮肤区域并单独处理（配合皮肤结构用）；关=不做皮肤区分。生效：改完约 1 秒（重建）。")),
    // ── diag ────────────────────────────────────────────────────────────
    def!("compare_mode", "set_compare", 132, Enum, Live, "diag", "对比模式",
         None, false, true, lo:None, hi:None, step:None,
         options:Some(vec!["off(0)", "并排 side-by-side(1)", "擦除 wipe(2)"]),
         tip:Some("对比方式三选一——A. off(0)=正常画面（平时必须是它）；B. 并排(1)=左右分屏看差异；C. 擦除(2)=滑动分割线对比。生效：即时。注意 B/C 开着会干扰抓帧统计，看完关掉。")),
    def!("compare_split", "set_compare_split", 136, Float, Live, "diag", "分割位置",
         None, false, true, lo:Some(0.0), hi:Some(1.0), step:Some(0.01), options:None, tip:None),
    def!("compare_zoom", "set_compare_zoom", 140, Float, Live, "diag", "并排缩放",
         None, false, true, lo:Some(1.0), hi:Some(2.0), step:Some(0.05), options:None, tip:None),
    def!("compare_swap", "set_compare_swap", 144, Bool, Live, "diag", "交换左右",
         None, false, true, lo:None, hi:None, step:None, options:None, tip:None),
    def!("hold_frame", "set_hold", 168, Bool, Live, "diag", "冻结帧",
         None, false, false, lo:None, hi:None, step:None, options:None,
         tip:Some("效果：把当前画面定住，之后改任何参数都作用在同一帧上——A. 开=画面冻住随便试；B. 关=恢复实时。生效：即时，抓帧统计不受它影响。")),
    def!("debug_view", "set_debug_view", 104, Enum, Live, "diag", "调试视图",
         None, false, true, lo:None, hi:None, step:None,
         options:Some(vec!["off(0)", "proxy(1)", "model(2)", "放大改动(3)"]), tip:None),
    def!("debug_scale", "set_debug_scale", 108, Float, Live, "diag", "调试增益",
         None, false, true, lo:Some(0.1), hi:Some(10.0), step:Some(0.1), options:None, tip:None),
    def!("white_point_source", "set_white_point_source", 120, Enum, Live, "diag", "白点来源",
         None, false, true, lo:None, hi:None, step:None,
         options:Some(vec!["手动(0)", "帧内测量(1)"]), tip:None),
    def!("white_point", "set_paper_white", 112, Float, Live, "diag", "纸白 paper white",
         None, false, true, lo:Some(0.2), hi:Some(4.0), step:Some(0.05), options:None, tip:None),
    def!("white_point_scale", "set_white_point_scale", 116, Float, Live, "diag", "白点倍率",
         None, false, true, lo:Some(0.2), hi:Some(4.0), step:Some(0.05), options:None, tip:None),
    def!("white_point_trim", "set_white_point_trim", 124, Float, Live, "diag", "白点修剪",
         None, false, true, lo:Some(0.2), hi:Some(4.0), step:Some(0.05), options:None, tip:None),
    def!("colour_trust_percent", "set_colour_trust_percent", 1952, Int, Live, "diag", "色彩信任度 %",
         None, false, true, lo:Some(0.0), hi:Some(400.0), step:Some(10.0), options:None,
         tip:Some("效果：模型颜色多大程度被采纳——A. 0=全采信模型色；B. 200=默认；C. 400=尽量跟原画面色。整体作用较弱，保持默认即可。生效：即时。")),
    def!("ratio_smooth_percent", "set_ratio_smooth_percent", 1956, Int, Live, "diag", "邻域比率平滑 %",
         None, false, true, lo:Some(0.0), hi:Some(100.0), step:Some(5.0), options:None,
         tip:Some("效果：亮度比率怎么取——A. 100=取邻域（默认，改动幅度大）；B. 0=逐像素（改动更收敛）。生效：即时。")),
    def!("mvec_scale_mode", "set_motion_units", 1864, Enum, Live, "diag", "矢量单位",
         None, false, true, lo:None, hi:None, step:None,
         options:Some(vec!["normalized(0)", "pixels(1)", "uv 0..1(2)"]), tip:None),
    def!("mvec_pixel_size", "set_mvec_pixel_size", 1964, Enum, Live, "diag", "光流网格",
         None, false, true, lo:None, hi:None, step:None,
         options:Some(vec!["1px(0)", "2px(1)", "4px(2)", "8px(3)"]),
         tip:Some("效果：给模型的光流网格精度——A. 1px(0)=最细最贵；B. 4px(2)=默认平衡。生效：即时。")),
]);

pub fn def_by_id(id: &str) -> Option<&'static SettingDef> {
    DEFS.iter().find(|d| d.id == id)
}

pub fn def_by_ini_key(key: &str) -> Option<&'static SettingDef> {
    DEFS.iter().find(|d| d.ini_key == key)
}

// ── 共享内存状态字段偏移（非设置区） ────────────────────────────────────────
pub mod status_off {
    pub const MAGIC: usize = 0;
    pub const VERSION: usize = 4;
    pub const WIDTH: usize = 16;
    pub const HEIGHT: usize = 20;
    pub const QUIT: usize = 28;
    pub const HEARTBEAT: usize = 32;
    pub const CONTROL_SEQ: usize = 36;
    pub const TUNING_SEQ: usize = 40;
    pub const HELPER_STATE: usize = 176;
    pub const MODEL_UP: usize = 180;
    pub const HELPER_FRAMES_LO: usize = 184;
    pub const HELPER_FRAMES_HI: usize = 188;
    pub const EVAL_MS: usize = 192;
    pub const UPLOAD_MS: usize = 196;
    pub const READBACK_MS: usize = 200;
    pub const VRAM_MB: usize = 204;
    pub const FEATURES: usize = 208;
    pub const PASS_CEILING: usize = 212;
    pub const LAYER_PID: usize = 216;
    pub const LAYER_FRAMES_LO: usize = 220;
    pub const LAYER_FRAMES_HI: usize = 224;
    pub const LAYER_W: usize = 228;
    pub const LAYER_H: usize = 232;
    pub const LAYER_COMPOSITION_UP: usize = 240;
    pub const LAYER_MS: usize = 244;
    pub const HELPER_REASON: usize = 260;
    pub const LAYER_REASON: usize = 456;
    pub const GAME_NAME: usize = 652;
    pub const MVEC_ENABLED: usize = 1860;
    pub const SEQ_OK: usize = 1872;
    pub const HDR_MODE: usize = 1932;
    pub const HDR_ACTIVE: usize = 1940;
    pub const PROXY_FORMAT: usize = 1944;
    pub const CAPTURE_REQUEST: usize = 152;
}

use std::sync::LazyLock;
