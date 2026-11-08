# dlssnr-suite

**Linux + Proton 下一键上 DLSS 5 神经渲染（NR）的完整方案**：Web 调参面板 + 自动安装器，把原本需要东拼西凑的三件套（helper、Vulkan 层、`nvngx_dlssnr.dll`）整理成一条命令的安装体验。

```bash
git clone https://github.com/Kuner-mw/dlssnr-suite
cd dlssnr-suite && ./install.sh
```

装完即得：**DLSS5VKLayer 截取管线**（开源，自动装）+ **NVIDIA NGX 神经渲染库**（自动下载并校验）+ **本项目的 Web 控制面板**（自动编译/下载并注册 systemd 服务）。

## 这是什么

DLSS 5 神经渲染（NGX Feature 18，`nvngx_dlssnr.dll`）官方只支持 RTX 50 系。本方案面向 **RTX 40 系（Ada）及兼容 GPU**，走社区验证过的路线：

```
┌─────────────┐   dma-buf    ┌──────────────────┐  IPC   ┌─────────────────────┐
│ Vulkan 游戏  │──(零拷贝)──▶ │ VK_LAYER_NV_dlssnr │──────▶ │ dlssnr_helper.exe    │
│ (Proton/Wine)│             │ (Linux 隐式层)      │ ◀────── │ (Wine 内调 NGX)      │
└─────────────┘              └──────────────────┘  写回   └─────────────────────┘
                                                            ▲
                                             /dev/shm 调参   │
                                     ┌──────────────────────┴───┐
                                     │  dlssnr-panel (本项目)     │
                                     │  Web 面板 :8099 + systemd  │
                                     └──────────────────────────┘
```

- **截取管线**（helper + 层）来自开源项目 [bmitch87/DLSS5VKLayer](https://github.com/bmitch87/DLSS5VKLayer)（AGPL-3.0），本仓库不改不捆绑，由安装脚本从其官方 release 装入（当前 pin `0.3.2-1`）。
- **神经渲染库**（`nvngx_dlssnr.dll`）是 NVIDIA 专有文件，**本仓库不存储、不再分发**：安装脚本运行时从 [RankFTW/rhi-repo](https://github.com/RankFTW/rhi-repo) release 下载 Ada retarget 版（当前 pin `310.9.16`，SHA-256 固化校验），也支持 `--dll` 导入自备文件。
- **控制面板**（`panel/`）是本项目的原创 Rust 代码（GPL-3.0）：单二进制 Web 面板，调参直写共享内存、按游戏配置档、游戏启动自动联动 helper、vkBasalt 滤镜管理。

## 面板功能

- **实时调参**：intensity / local_tone / structure / colour / guard / 三层 pass 覆盖等全部 shm 参数，改完即时生效，同时双写 `config.ini` 防重启丢失；
- **按游戏配置档**：`~/.config/dlssnr-panel/games/<appid>.json`，填启动目录与游戏主程序路径即可建档；游戏进程出现自动拉起 helper、写参数、游戏退出自动回收；启动器公告小窗按关键词自动关闭（niri）；
- **游戏内 HUD**：布局编辑器画好的监控面板直接嵌进 MangoHud 渲染，含每核心频率律动条、CPU/GPU 功耗与频率、帧率曲线（需配套 MangoHud 补丁，见 `panel/custom-hud/`）；
- **模型切换 / 抓帧对比 / 配置快照还原**；
- **vkBasalt 滤镜管理**：cas / LUT 热切换（与 DLSS5 管线独立，不装 dlssnr 也能用）；
- 面板与上游仅通过 `/dev/shm` 通信，无代码级链接；helper/层缺失时面板照常运行并显示引导文案。

## 系统要求

| 项目 | 要求 |
|---|---|
| GPU | 面向 RTX 40 系（Ada）；RTX 50 系可用 NVIDIA 原版 dll（`--dll` 导入）；AMD/NVIDIA 更早架构未验证 |
| 驱动 | NVIDIA 驱动 615+（无需 616.56+） |
| 运行时 | 带 DXVK-NVAPI 的 Proton（Proton-CachyOS / GE / DW-Proton 等；Valve 官方 Proton 不含 DXVK-NVAPI 栈） |
| 系统 | x86_64 Linux，systemd 用户会话（可选） |
| 依赖 | curl / tar（unzip 仅下载 NGX dll 时需要，`--panel-only` 不需要）；编译面板需 Rust（无 Rust 时自动回退下载预编译二进制） |

## 安装

```bash
./install.sh                # 全自动（默认从源码编译面板，需 rust；否则下载 release 二进制）
./install.sh --dll ~/下载/nvngx_dlssnr.dll   # 使用自备 dll
./install.sh --no-download  # 禁止任何 dll 下载
./install.sh --panel-only   # 只装面板，不动上游（已自行管理 DLSS5VKLayer 的高级用法）
./install.sh --no-service   # 不注册 systemd 服务（手动 dlssnr-panel serve）
./install.sh --force-dlssnr # 检测到已装时仍强制重装用户级
```

**智能检测**：安装脚本会同时识别**用户级**（上游 tarball `--user`，`~/.local/lib/dlssnr`）与**系统级**（如 AUR `dlssnr-bin`，`/usr/lib/dlssnr`）安装——任一已存在即自动复用，绝不重复安装（避免 Vulkan Loader 读到两份层清单冲突）。只想装面板本身用 `--panel-only`。

脚本做什么：

1. 从 DLSS5VKLayer 官方 release 下载并 `install.sh --user` 安装 helper + Vulkan 隐式层（用户级，不动系统目录）；
2. 获取 `nvngx_dlssnr.dll`（下载并 SHA-256 校验，或导入自备文件）到 `~/.local/share/dlssnr/binaries/`；
3. 编译并安装面板到 `~/.local/bin/dlssnr-panel`，注册 systemd 用户服务（`http://127.0.0.1:8099`，登录自启）；
4. 校验 Vulkan 层清单就位，打印启用说明。

卸载：

```bash
./uninstall.sh        # 只卸面板与服务
./uninstall.sh --all  # 连同 DLSS5VKLayer 与 NGX dll 一并清除
```

## 面板启动与访问

默认安装（systemd）下面板**装完即启动、登录自启**，无需手动命令：

1. 浏览器打开安装完成时打印的带 token 地址：`http://127.0.0.1:8099/?token=<你的token>`（点击即用，token 自动记住）；
2. 直接访问 `http://127.0.0.1:8099` 也可以——页面会弹窗要求粘贴访问令牌，token 在面板主机上查看：

```bash
cat ~/.local/state/dlssnr/webui.token   # 访问令牌（0600，首次启动自动生成）
systemctl --user status dlssnr-panel    # 服务状态
journalctl --user -u dlssnr-panel -f    # 运行日志
```

3. 手机/副屏（同一局域网）：访问 `http://<面板主机IP>:8099`，首次粘贴同一 token（面板默认绑定 `0.0.0.0` 以支持局域网调参，鉴权始终开启）；
4. 不想用 systemd / 调试：`dlssnr-panel serve`（前台）或 `dlssnr-panel serve --no-token`（仅本机 127.0.0.1、免密）。端口冲突时加 `--port` 换端口。

> 安全提示：token 是面板全部控制能力的唯一凭证，仅放在面板主机上；不要把带 token 的完整 URL 分享到不可信环境。

## 启用 DLSS5

给游戏加 Steam 启动项：

```
VKLayer_DLSS5=1 %command%
```

或写入 `~/.config/environment.d/dlssnr.conf` 全局启用（注意：非 systemd 拉起的 Steam 可能不继承该文件，启动项方式最可靠）。游戏内看到的面板「联动中」横幅即表示参数已接管。

生效检查：

```bash
~/.local/bin/dlssnr-shmctl /tmp/dlssnr-$(id -u)/shm.bin status   # model_up=1 且 helper_frames 自增
tail -f ~/.local/state/dlssnr/helper.log                     # 逐帧日志
```

## 版本 pin 与升级

`install.sh` 顶部集中管理三个 pin，升级时改这里即可：

| 组件 | 来源 | 当前 pin |
|---|---|---|
| DLSS5VKLayer | bmitch87/DLSS5VKLayer release | `0.3.2-1` |
| nvngx_dlssnr.dll | RankFTW/rhi-repo release `dlssnr-310.9.16` | SHA-256 `cdfa6c1e…`（见脚本） |
| 面板 | 本仓库 CI release / 本地编译 | 随 tag |

> 面板与上游的兼容性由共享内存 ABI 锁保证（magic `GNR2` / version 20，不匹配拒绝写入），pin 升级前请先确认上游未破坏 ABI。

## 许可与合规

- 本项目原创代码（`panel/`、安装脚本）：**GPL-3.0**，见 [LICENSE](LICENSE)。
- 第三方组件边界与免责见 [NOTICE](NOTICE.md)：DLSS5VKLayer（AGPL-3.0，运行时安装、不修改不捆绑）；`nvngx_dlssnr.dll`（NVIDIA 专有，本仓库不存储不再分发，仅供个人学习使用，风险自担）。
- DLSS、NGX 为 NVIDIA Corporation 的商标；本项目与 NVIDIA 无关联。
