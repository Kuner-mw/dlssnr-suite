#include "custom_hud.h"

#include <json-c/json.h>
#include <spdlog/spdlog.h>
#include <algorithm>
#include <chrono>
#include <cmath>
#include <cstdio>
#include <ctime>
#include <fstream>
#include <mutex>
#include <string>
#include <vector>

#include "filesystem.h"
#include "fps_metrics.h"
#include "gpu.h"
#include "logging.h"
#include "memory.h"
#include "cpu.h"

namespace fs = ghc::filesystem;
using namespace std;
using namespace std::chrono;

namespace {

// CPU 额外指标快照（本文件内可见）：硬件线程发布，渲染线程 atomic_load
struct CpuExtraSnapshot {
    std::vector<int> mhz;   // 每逻辑核频率 MHz
    float power = 0;        // 包功耗 W
    float avg_clock = 0;    // 全核平均 MHz
};
static std::shared_ptr<const CpuExtraSnapshot> s_cpu_extra;

struct Item {
    string kind = "text";
    string tpl;
    float x = 16, y = 16, w = 220, h = 48, max_ms = 0, max_mhz = 0, size = 20;
    ImVec4 color = ImVec4(0.91f, 0.89f, 0.85f, 1.0f);
};

struct Shape {
    string kind = "rect";
    float radius = 16, fill_alpha = 0.62f, border_alpha = 0.9f, border_w = 2;
    ImVec4 fill = ImVec4(0.06f, 0.08f, 0.09f, 1.0f);
    ImVec4 border = ImVec4(0.34f, 0.85f, 0.77f, 1.0f);
    vector<ImVec2> points;
};

struct Layout {
    bool valid = false;
    float w = 400, h = 260, x = 24, y = 24;
    Shape shape;
    vector<Item> items;
};

Layout current;
string loaded_path;
fs::file_time_type loaded_mtime {};
steady_clock::time_point next_check {};
std::mutex state_mtx;

string jstr(json_object* obj, const char* key, const string& fallback = "") {
    json_object* v = nullptr;
    if (obj && json_object_object_get_ex(obj, key, &v) && json_object_is_type(v, json_type_string))
        return json_object_get_string(v);
    return fallback;
}

double jnum(json_object* obj, const char* key, double fallback) {
    json_object* v = nullptr;
    double out = fallback;
    if (obj && json_object_object_get_ex(obj, key, &v) && json_object_is_type(v, json_type_double))
        out = json_object_get_double(v);
    else if (obj && json_object_object_get_ex(obj, key, &v) && json_object_is_type(v, json_type_int))
        out = json_object_get_int64(v);
    return std::isfinite(out) ? out : fallback;
}

ImVec4 rgba(string s) {
    if (!s.empty() && s[0] == '#') s.erase(s.begin());
    auto hex = [&](size_t pos) -> int {
        if (pos + 2 > s.size()) return 255;
        try { return stoi(s.substr(pos, 2), nullptr, 16); } catch (...) { return 255; }
    };
    if (s.size() == 6 || s.size() == 8) {
        return ImVec4(hex(0) / 255.0f, hex(2) / 255.0f, hex(4) / 255.0f,
                      (s.size() == 8 ? hex(6) : 255) / 255.0f);
    }
    return ImVec4(0.91f, 0.89f, 0.85f, 1.0f);
}

ImU32 with_alpha(ImVec4 c, float multiplier) {
    c.w = clamp(c.w * multiplier, 0.0f, 1.0f);
    return ImGui::ColorConvertFloat4ToU32(c);
}

string fmt_int(double v) {
    if (!std::isfinite(v)) return "--";
    return to_string(static_cast<long long>(llround(v)));
}

string fmt1(double v) {
    if (!std::isfinite(v)) return "--";
    char buf[32];
    snprintf(buf, sizeof(buf), "%.1f", v);
    return buf;
}

string fps_metric(const char* name) {
    auto fm = std::atomic_load_explicit(&fpsmetrics, std::memory_order_acquire);
    if (!fm) return "--";
    for (const auto& m : fm->copy_metrics()) {
        if (m.name == name) return fmt_int(m.value);
    }
    return "--";
}


string metric_value(const string& key, const swapchain_stats& data) {
    if (key == "cpu_power") {
        auto snap = std::atomic_load_explicit(&s_cpu_extra, std::memory_order_acquire);
        float p = snap ? snap->power : 0.0f;
        return p > 0 ? fmt1(p) : "--";
    }
    if (key == "cpu_clock") {
        auto snap = std::atomic_load_explicit(&s_cpu_extra, std::memory_order_acquire);
        float mhz = snap ? snap->avg_clock : 0.0f;
        return mhz > 0 ? fmt_int((int)(mhz + 0.5f)) : "--";
    }
    if (key == "gpu_util") return fmt_int(currentLogData.gpu_load);
    if (key == "gpu_temp") return fmt_int(currentLogData.gpu_temp);
    if (key == "gpu_mem") return fmt1(currentLogData.gpu_vram_used);
    if (key == "gpu_mem_total") {
        if (gpus && gpus->active_gpu()) return fmt1(gpus->active_gpu()->metrics.memoryTotal);
        return "--";
    }
    if (key == "gpu_power") return fmt_int(currentLogData.gpu_power);
    if (key == "gpu_clock") return fmt_int(currentLogData.gpu_core_clock);
    if (key == "cpu_util") return fmt_int(currentLogData.cpu_load);
    if (key == "cpu_temp") return fmt_int(currentLogData.cpu_temp);
    if (key == "ram_used") return fmt1(currentLogData.ram_used);
    if (key == "ram_total") return fmt1(memmax);
    if (key == "ram_temp") return fmt_int(mem_temp);
    if (key == "swap") return fmt1(currentLogData.swap_used);
    if (key == "fps") return data.fps > 0 ? fmt_int(data.fps) : "--";
    if (key == "fps_avg") return fps_metric("AVG");
    if (key == "fps_low1") return fps_metric("0.01");
    if (key == "frametime_ms") return frametime_data.empty() ? "--" : fmt1(frametime_data.back());
    if (key == "time") {
        time_t now = time(nullptr);
        struct tm tm {};
        localtime_r(&now, &tm);
        char buf[16];
        snprintf(buf, sizeof(buf), "%02d:%02d", tm.tm_hour, tm.tm_min);
        return buf;
    }
    return "";
}

string fill_template(const string& tpl, const swapchain_stats& data) {
    string out;
    size_t pos = 0;
    while (pos < tpl.size()) {
        size_t begin = tpl.find('{', pos);
        if (begin == string::npos) {
            out.append(tpl, pos, string::npos);
            break;
        }
        size_t end = tpl.find('}', begin + 1);
        if (end == string::npos) {
            out.append(tpl, pos, string::npos);
            break;
        }
        out.append(tpl, pos, begin - pos);
        out += metric_value(tpl.substr(begin + 1, end - begin - 1), data);
        pos = end + 1;
    }
    return out;
}

bool load_layout(const string& path) {
    std::ifstream file(path, std::ios::binary);
    if (!file) return false;
    std::string data(64 * 1024 + 1, '\0');
    file.read(data.data(), static_cast<std::streamsize>(data.size()));
    auto n = file.gcount();
    if (n <= 0 || n > 64 * 1024) return false;
    json_tokener* tok = json_tokener_new();
    json_object* root = json_tokener_parse_ex(tok, data.data(), static_cast<int>(n));
    auto err = json_tokener_get_error(tok);
    json_tokener_free(tok);
    if (!root || err != json_tokener_success) return false;

    Layout next;
    json_object *window = nullptr, *shape = nullptr, *items = nullptr;
    json_object_object_get_ex(root, "window", &window);
    json_object_object_get_ex(root, "shape", &shape);
    json_object_object_get_ex(root, "items", &items);
    if (!window || !items || !json_object_is_type(items, json_type_array)) {
        json_object_put(root);
        return false;
    }
    next.w = clamp(jnum(window, "w", 400), 80.0, 3840.0);
    next.h = clamp(jnum(window, "h", 260), 60.0, 2160.0);
    next.x = clamp(jnum(window, "x", 24), -4096.0, 8192.0);
    next.y = clamp(jnum(window, "y", 24), -4096.0, 8192.0);

    next.shape.kind = jstr(shape, "kind", "rect");
    next.shape.radius = clamp(jnum(shape, "radius", 16), 0.0, 300.0);
    next.shape.fill_alpha = clamp(jnum(shape, "fill_alpha", 0.62), 0.0, 1.0);
    next.shape.border_alpha = clamp(jnum(shape, "border_alpha", 0.9), 0.0, 1.0);
    next.shape.border_w = clamp(jnum(shape, "border_w", 2), 0.0, 12.0);
    next.shape.fill = rgba(jstr(shape, "fill", "#101418"));
    next.shape.border = rgba(jstr(shape, "border", "#57d9c5"));
    json_object* points = nullptr;
    if (shape && json_object_object_get_ex(shape, "points", &points) && json_object_is_type(points, json_type_array)) {
        size_t count = min<size_t>(json_object_array_length(points), 64);
        for (size_t i = 0; i < count; i++) {
            json_object* pair = json_object_array_get_idx(points, i);
            if (!pair || !json_object_is_type(pair, json_type_array) || json_object_array_length(pair) < 2) continue;
            json_object* xv = json_object_array_get_idx(pair, 0);
            json_object* yv = json_object_array_get_idx(pair, 1);
            if (!xv || !yv) continue;
            double x = json_object_get_double(xv), y = json_object_get_double(yv);
            if (!std::isfinite(x) || !std::isfinite(y) || x < -8192 || x > 8192 || y < -8192 || y > 8192) {
                json_object_put(root);
                return false;
            }
            next.shape.points.emplace_back(x, y);
        }
    }

    size_t count = min<size_t>(json_object_array_length(items), 64);
    for (size_t i = 0; i < count; i++) {
        json_object* src = json_object_array_get_idx(items, i);
        if (!src) continue;
        Item item;
        item.kind = jstr(src, "kind", "text");
        item.tpl = jstr(src, "tpl");
        item.x = clamp(jnum(src, "x", 16), -8192.0, 8192.0);
        item.y = clamp(jnum(src, "y", 16), -8192.0, 8192.0);
        item.w = clamp(jnum(src, "w", 220), 16.0, 2048.0);
        item.h = clamp(jnum(src, "h", 48), 8.0, 512.0); // cpu_bars 的下限在渲染侧以 band=14 兜底
        item.max_ms = clamp(jnum(src, "max_ms", 0), 0.0, 1000.0);
        item.max_mhz = clamp(jnum(src, "max_mhz", 0), 0.0, 20000.0);
        item.size = clamp(jnum(src, "size", 20), 8.0, 96.0);
        item.color = rgba(jstr(src, "color", "#e8e4d8"));
        next.items.push_back(item);
    }
    next.valid = true;
    current = std::move(next);
    json_object_put(root);
    return true;
}

void reload_if_needed(const string& path) {
    std::lock_guard<std::mutex> guard(state_mtx);
    if (path.empty()) {
        current.valid = false;
        loaded_path.clear();
        loaded_mtime = {};
        next_check = {};
        return;
    }
    if (path != loaded_path) {
        loaded_path = path;
        loaded_mtime = {};
        next_check = {};
        current.valid = false;
    }
    auto now = steady_clock::now();
    if (now < next_check) return;
    next_check = now + milliseconds(500);
    std::error_code ec;
    auto mtime = fs::last_write_time(path, ec);
    if (ec || mtime == loaded_mtime) return;
    if (load_layout(path)) {
        loaded_mtime = mtime;
    } else {
        SPDLOG_WARN("custom_hud: failed to load layout {}", path);
        // 防止坏文件每秒刷屏；保留上一次有效布局，下个 mtime 再尝试。
        loaded_mtime = mtime;
    }
}

void draw_shape(ImDrawList* dl, const Layout& l) {
    const auto& s = l.shape;
    ImVec2 a(l.x, l.y), b(l.x + l.w, l.y + l.h);
    ImU32 fill = with_alpha(s.fill, s.fill_alpha);
    ImU32 border = with_alpha(s.border, s.border_alpha);
    if (s.kind == "rect") {
        dl->AddRectFilled(a, b, fill, s.radius);
        if (s.border_w > 0 && s.border_alpha > 0) dl->AddRect(a, b, border, s.radius, 0, s.border_w);
    } else if (s.kind == "ellipse") {
        ImVec2 center(l.x + l.w / 2, l.y + l.h / 2), radius(l.w / 2, l.h / 2);
        dl->AddEllipseFilled(center, radius, fill);
        if (s.border_w > 0 && s.border_alpha > 0) dl->AddEllipse(center, radius, border, 0.0f, 0, s.border_w);
    } else if (s.kind == "polygon" && s.points.size() >= 3) {
        vector<ImVec2> points;
        points.reserve(s.points.size());
        for (const auto& p : s.points) points.emplace_back(l.x + p.x, l.y + p.y);
        dl->AddConcavePolyFilled(points.data(), static_cast<int>(points.size()), fill);
        if (s.border_w > 0 && s.border_alpha > 0)
            dl->AddPolyline(points.data(), static_cast<int>(points.size()), border, ImDrawFlags_Closed, s.border_w);
    }
}

void draw_text_item(ImDrawList* dl, const Layout& l, const Item& item, const swapchain_stats& data) {
    string text = fill_template(item.tpl, data);
    if (text.empty()) return;
    ImFont* font = ImGui::GetFont();
    ImVec2 pos(l.x + item.x, l.y + item.y);
    ImU32 color = ImGui::ColorConvertFloat4ToU32(item.color);
    ImU32 outline = with_alpha(ImVec4(0, 0, 0, 1), 0.85f);
    dl->AddText(font, item.size, ImVec2(pos.x - 1, pos.y), outline, text.c_str());
    dl->AddText(font, item.size, ImVec2(pos.x + 1, pos.y), outline, text.c_str());
    dl->AddText(font, item.size, ImVec2(pos.x, pos.y - 1), outline, text.c_str());
    dl->AddText(font, item.size, ImVec2(pos.x, pos.y + 1), outline, text.c_str());
    dl->AddText(font, item.size, pos, color, text.c_str());
}

void draw_graph_item(ImDrawList* dl, const Layout& l, const Item& item) {
    if (frametime_data.empty()) return;
    size_t first = frametime_data.size() > 60 ? frametime_data.size() - 60 : 0;
    float max_ms = item.max_ms;
    if (max_ms <= 0) {
        max_ms = 20.0f;
        for (size_t i = first; i < frametime_data.size(); i++) max_ms = max(max_ms, frametime_data[i]);
    }
    if (max_ms <= 0) return;
    ImU32 color = ImGui::ColorConvertFloat4ToU32(item.color);
    float x0 = l.x + item.x, y0 = l.y + item.y;
    float gw = item.w, gh = item.h;
    dl->AddLine(ImVec2(x0, y0 + gh), ImVec2(x0 + gw, y0 + gh), with_alpha(item.color, 0.18f), 1.0f);
    size_t n = frametime_data.size() - first;
    auto point = [&](size_t i) {
        float x = x0 + (n > 1 ? (i - first) * (gw - 1) / (n - 1) : 0);
        float y = y0 + gh - clamp(frametime_data[i] / max_ms, 0.0f, 1.0f) * gh;
        return ImVec2(x, y);
    };
    if (n == 1) {
        dl->AddCircleFilled(point(first), 2.0f, color);
        return;
    }
    for (size_t i = first + 1; i < frametime_data.size(); i++)
        dl->AddLine(point(i - 1), point(i), color, 2.0f);
}

// 每核心频率律动条：逻辑核数自适应，柱高 = 该核当前频率 / 上限（max_mhz=0 时按当前最大核频自动缩放）。
// 顶部 14px 为标签带；零频核画 1px 低透明度刻度保持 24 核位置可追踪
void draw_cpu_bars_item(ImDrawList* dl, const Layout& l, const Item& item) {
    auto snap = std::atomic_load_explicit(&s_cpu_extra, std::memory_order_acquire);
    if (!snap || snap->mhz.empty()) return;
    float x0 = l.x + item.x, y0 = l.y + item.y + 14.0f; // 顶部留标签带
    float gw = item.w, gh = max(1.0f, item.h - 14.0f);
    size_t n = snap->mhz.size();
    float max_mhz = item.max_mhz;
    if (max_mhz <= 0) {
        for (int m : snap->mhz) max_mhz = max(max_mhz, (float)m);
        max_mhz = max(max_mhz, 1.0f) * 1.15f; // 留出律动余量
    }
    float slot = gw / n;
    float bw = min(max(2.0f, slot - 2.0f), slot);
    ImU32 color = ImGui::ColorConvertFloat4ToU32(item.color);
    ImU32 dim = with_alpha(item.color, 0.28f);
    dl->AddLine(ImVec2(x0, y0 + gh), ImVec2(x0 + gw, y0 + gh), with_alpha(item.color, 0.18f), 1.0f);
    for (size_t i = 0; i < n; i++) {
        float ratio = clamp(snap->mhz[i] / max_mhz, 0.0f, 1.0f);
        float bx = x0 + i * slot + (slot - bw) * 0.5f;
        if (ratio <= 0.0f) {
            dl->AddRectFilled(ImVec2(bx, y0 + gh - 1.0f), ImVec2(bx + bw, y0 + gh), dim);
            continue;
        }
        dl->AddRectFilled(ImVec2(bx, y0 + gh - ratio * gh), ImVec2(bx + bw, y0 + gh), color);
    }
}

} // namespace

// CPU 额外指标快照：由 update_hw_info() 硬件线程采集并发布，渲染线程只 atomic_load，
// 避免与 mango 更新线程并发访问 m_cpuData（vector 结构可能 resize/重建）。
// cpu_power 走 MangoHud 采集链（k10temp/zenergy/RAPL；RAPL energy_uj 需 root 授权，无权限时显示 --）
void custom_hud_publish_cpu_extra() {
    auto next = std::make_shared<CpuExtraSnapshot>();
    uint64_t sum = 0;
    for (const auto& core : cpuStats.GetCPUData()) {
        next->mhz.push_back(core.mhz);
        sum += core.mhz;
    }
    if (!next->mhz.empty())
        next->avg_clock = (float)(sum / next->mhz.size());
    next->power = cpuStats.GetCPUDataTotal().power;
    std::atomic_store_explicit(&s_cpu_extra,
        std::shared_ptr<const CpuExtraSnapshot>(std::move(next)),
        std::memory_order_release);
}


void custom_hud_render(swapchain_stats& data, const overlay_params& params) {
    reload_if_needed(params.custom_layout);
    Layout snapshot;
    {
        std::lock_guard<std::mutex> guard(state_mtx);
        snapshot = current;
    }
    if (!snapshot.valid) return;
    if (!std::atomic_load_explicit(&fpsmetrics, std::memory_order_acquire)) {
        std::atomic_store_explicit(&fpsmetrics,
            std::make_shared<fpsMetrics>(std::vector<std::string>{"AVG", "0.01"}), std::memory_order_release);
    }
    ImDrawList* dl = ImGui::GetForegroundDrawList();
    draw_shape(dl, snapshot);
    for (const auto& item : snapshot.items) {
        if (item.kind == "graph") draw_graph_item(dl, snapshot, item);
        else if (item.kind == "cpu_bars") draw_cpu_bars_item(dl, snapshot, item);
        else draw_text_item(dl, snapshot, item, data);
    }
}
