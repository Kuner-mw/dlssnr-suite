#pragma once

#include "overlay.h"
#include "overlay_params.h"

// 在游戏内 ImGui 渲染 JSON 自定义布局；不创建任何独立窗口或 layer-shell 表面。
void custom_hud_render(swapchain_stats& data, const overlay_params& params);

// 由 update_hw_info() 硬件线程调用：采集每核频率/包功耗并发布不可变快照（渲染侧只读）
void custom_hud_publish_cpu_extra();
