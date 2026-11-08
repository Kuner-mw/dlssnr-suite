# 游戏内自定义 HUD（MangoHud v0.8.4）

这是把自定义悬浮窗融入 MangoHud 游戏内 ImGui 的实现。不会创建独立 Wayland/layer-shell 窗口。

## 文件

- `custom_hud.cpp` / `custom_hud.h`：新增的布局 JSON 解析与 ImGui 绘制实现。
- `mangohud-v0.8.4-custom.patch`：对上游 `v0.8.4`（commit `992103e4fb744897826de04ea00a2f71e7018214`）的接入补丁，包含参数、采集条件、渲染入口和 Meson/JSON-C 依赖。

## 构建

```sh
git clone --depth 1 --branch v0.8.4 https://github.com/flightlessmango/MangoHud.git /tmp/mangohud-custom-src
git -C /tmp/mangohud-custom-src apply "$PWD/panel/custom-hud/mangohud-v0.8.4-custom.patch"   # 在 suite 仓库根执行
cp panel/custom-hud/custom_hud.{cpp,h} /tmp/mangohud-custom-src/src/
python3 -m venv /tmp/mangohud-build-venv
/tmp/mangohud-build-venv/bin/pip install mako
PATH=/tmp/mangohud-build-venv/bin:$PATH meson setup /tmp/mangohud-custom-build /tmp/mangohud-custom-src \
  -Dinclude_doc=false -Dtests=disabled -Dmangoplot=disabled \
  -Dappend_libdir_mangohud=false -Dwith_xnvctrl=disabled
PATH=/tmp/mangohud-build-venv/bin:$PATH ninja -C /tmp/mangohud-custom-build
```

注意：`subprojects/implot-0.16/meson.build` 若缺失（WrapDB 不可达时 meson 会卡在下载 implot 补丁），
可手写一份：static_library 编 implot.cpp/implot_items.cpp + dependency('imgui') + `-DIMGUI_DEFINE_MATH_OPERATORS`，
并 declare_dependency 导出 `implot_dep`（主工程按变量名取）。

安装前备份原库，再替换用户级 `mangohud64/libMangoHud.so`（64 位游戏用 mangohud64，JSON-C 由系统 libjson-c 提供）。

## 配置

`MangoHud.conf` 至少需要：

```ini
custom_layout=~/.config/dlssnr-panel/hud-layout.json
font_file=~/.local/share/fonts/LXGWWenKai-Regular.ttf
font_glyph_ranges=chinese
font_scale=1.0
no_display=0
```

字体必须对 Steam pressure-vessel 容器可见；宿主机的 `/usr/share/fonts/noto-cjk/` 不在该容器内。

面板「悬浮窗」页的「开启 HUD / 热关闭」直接修改 `no_display`。布局保存、导出、导入仍使用面板的 `hud-layout.json`，自定义 HUD 每 500ms 检查文件 mtime 并热重载。

首次更换运行库后需要重启游戏；之后无需为布局修改重启。
