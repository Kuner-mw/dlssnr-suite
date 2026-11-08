# NOTICE — 第三方组件与合规声明

本仓库（dlssnr-suite）的原创代码为 `panel/` 目录、`install.sh` 与 `uninstall.sh`，
以 GPL-3.0 授权（见 LICENSE）。除此之外的组件归属与授权边界如下：

## 1. DLSS5VKLayer（helper + Vulkan 隐式层）

- 上游：<https://github.com/bmitch87/DLSS5VKLayer>
- 许可：**AGPL-3.0**（版权归上游作者）
- 本仓库的使用方式：**不修改、不捆绑、不再分发**。`install.sh` 仅在安装时从上游
  官方 GitHub release 下载原始构建产物并执行其自带安装脚本（pin `0.3.2-1`）。
- 若你修改了该组件的代码，AGPL-3.0 的传染条款适用于你的修改版。

## 2. nvngx_dlssnr.dll（NVIDIA NGX 神经渲染库）

- 版权：**NVIDIA Corporation 专有闭源软件**，非开源。
- 本仓库**不存储、不捆绑、不再分发、不下载**该文件，安装器也不提供任何获取途径。
  使用者从面板【模型】页列出的来源自行获取合法副本（下载后请自行核对哈希），
  在面板内粘贴本地路径导入模型库。
- 该 dll 的 Ada（RTX 40 系）retarget 版本为社区修改产物，**未经 NVIDIA 认证**，
  可能违反 NVIDIA 软件许可协议的再分发条款；仅供个人学习研究，商用/再分发风险自负。
- 使用本方案（含任何 dll 来源）导致的账号风险（反作弊）、系统稳定性问题由使用者自行承担。

## 3. 运行时依赖（安装脚本不拉取，均不入库）

| 组件 | 来源 | 许可 |
|---|---|---|
| DXVK / DXVK-NVAPI | 由用户所选 Proton 发行版自带，或自行安装 | zlib/libpng 类（Zlib） |
| vkBasalt（可选滤镜） | 用户自装（AUR 等） | zlib 许可 |
| MangoHud（游戏内 HUD 载体） | 上游 [flightlessmango/MangoHud](https://github.com/flightlessmango/MangoHud) v0.8.4 | MIT |

`panel/custom-hud/` 内含针对 MangoHud v0.8.4 的接入补丁与派生代码（GPL-3.0，
与本项目同许可）；MangoHud 上游本身不入库、不改不分发，由使用者按其 README
自行构建。MangoHud 采用 MIT 许可，其版权与许可声明归上游作者所有；
完整许可正文见 [licenses/MangoHud-MIT.txt](licenses/MangoHud-MIT.txt)。

## 4. 商标

DLSS、NGX、GeForce、RTX 为 NVIDIA Corporation 的商标/注册商标。
本项目为独立社区项目，与 NVIDIA 无任何隶属或背书关系。
