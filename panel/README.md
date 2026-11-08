# dlssnr-panel

Linux + Proton 下 DLSS5 神经渲染（NR）的 Web 调参面板：单 Rust 二进制（约 1.3MB）、
无需额外运行时环境、systemd 用户服务常驻，浏览器访问 `http://<主机>:8099`。

> 面板是 [dlssnr-suite](../README.md) 的一部分；`DLSS5 数值调节` 功能需要配套的
> helper + Vulkan 层（上游 [DLSS5VKLayer](https://github.com/bmitch87/DLSS5VKLayer)），
> 建议用 suite 的 `install.sh` 一键安装。面板本身可独立运行——helper/层缺失时
> 各页面正常显示引导文案，vkBasalt 滤镜、游戏档、服务管理完全可用。

## 功能

- **DLSS5 实时调参**：intensity / local_tone / structure / colour / guard /
  推理层数 / 模型分辨率 / HDR 等全部共享内存参数（含 30 层 pass 覆盖），
  改完即时生效并双写 `config.ini` 防重启丢失；
- **按游戏配置档**：`~/.config/dlssnr-panel/games/<appid>.json`，手动填启动目录与
  游戏主程序路径即可建档；游戏进程出现后自动拉起 helper、写入该档参数、
  退出自动回收；活跃游戏调参实时回写档；
- **启动器小窗自动关闭**：游戏运行期间按标题关键词自动关闭启动器弹出的
  公告/活动小窗（niri 会话）；
- **游戏内 HUD**（custom-hud/）：把布局编辑器画好的监控面板直接嵌进 MangoHud 的
  ImGui 渲染，含每核心频率律动条、CPU/GPU 功耗频率、帧率曲线等；
- **vkBasalt 滤镜管理**：cas / LUT 热切换，与 NR 管线相互独立；
- **配置档 / 抓帧对比 / 模型切换**：快照一键还原、A/B 抓帧、NR 模型切换。

## 系统指标说明

CPU 功耗读 RAPL（`/sys/class/powercap/*rapl*/energy_uj`）。部分发行版该文件默认
仅 root 可读，需放行一次（重启后由 udev 规则保持）：

```bash
sudo chmod a+r /sys/class/powercap/*rapl*/energy_uj
echo 'ACTION=="add", SUBSYSTEM=="powercap", RUN+="/bin/sh -c '"'"'chmod a+r /sys/class/powercap/*rapl*/energy_uj'"'"'"' | sudo tee /etc/udev/rules.d/99-rapl-read.rules
```

未授权时功耗显示 `--`，其余指标（频率/温度/利用率）不受影响。

## 构建 / 安装 / 运行

```bash
cd panel          # 在 dlssnr-suite 仓库根执行
cargo build --release
mkdir -p ~/.local/bin
cp target/release/dlssnr-panel ~/.local/bin/dlssnr-panel
```

运行形态二选一：

- systemd 用户服务（推荐，开机自启）：面板「游戏」页有启用/停用按钮，
  单元文件在 `~/.config/systemd/user/dlssnr-panel.service`（登录后拉起）；
- 手动：`dlssnr-panel serve`（`--port 8099`、`--no-token` 可选）。

首次启动生成访问令牌 `~/.local/state/dlssnr/webui.token`（0600），
带令牌地址形如 `http://127.0.0.1:8099/?token=<token>`。

## 数据与文件布局

| 路径 | 内容 |
|---|---|
| `~/.config/dlssnr-panel/games/*.json` | 游戏档（参数 + pass + 开关 + 初始快照） |
| `~/.config/dlssnr-panel/hud-layout.json` | 游戏 HUD 布局（编辑器保存目标） |
| `~/.local/state/dlssnr/webui.log` | 面板日志 |
| `~/.local/state/dlssnr/webui.token` | 访问令牌（0600） |

## 目录结构

```
src/        Rust 源码（main 入口 / defs 单一真相表 / shm / games / hud / metrics / sysapi / vkbasalt …）
static/     单页前端（index.html，编译期内嵌进二进制）
custom-hud/ 游戏 HUD 的 MangoHud v0.8.4 补丁与构建说明（见其 README）
```

## License

GPL-3.0（见仓库根 LICENSE）。上游 DLSS5VKLayer 为 AGPL-3.0、`nvngx_dlssnr.dll`
为 NVIDIA 专有文件：本仓库不存储、不分发，也不提供下载——模型由使用者从面板
【模型】页列出的来源自行获取并导入。
