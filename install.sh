#!/usr/bin/env bash
# dlssnr-suite 一键安装器
# 装齐三件套：DLSS5VKLayer(helper+Vulkan层) + nvngx_dlssnr.dll + dlssnr-panel
#
# 用法:
#   ./install.sh                  # 全自动（dll 自动下载）
#   ./install.sh --dll <path>     # 使用自备的 nvngx_dlssnr.dll
#   ./install.sh --no-download    # 禁止联网下载 dll（须配合 --dll）
#   ./install.sh --panel-only     # 只装面板，不安装上游 DLSS5VKLayer 与 dll
#   ./install.sh --from-source    # 强制从源码编译面板（默认优先）
#   ./install.sh --no-service     # 不注册 systemd 用户服务
#   ./install.sh --force-dlssnr   # 检测到已装上游时仍强制重装用户级
set -euo pipefail

# ---------- 版本 pin（升级时只改这里） ----------
DLSSNR_VER="0.3.2-1"
DLSSNR_TARBALL_URL="https://github.com/bmitch87/DLSS5VKLayer/releases/download/${DLSSNR_VER}/dlssnr-${DLSSNR_VER}-linux-x86_64.tar.gz"
DLL_VER="310.9.16"
DLL_ZIP_URL="https://github.com/RankFTW/rhi-repo/releases/download/dlssnr-${DLL_VER}/nvngx_dlssnr_${DLL_VER}.zip"
# 上游 DLSS5VKLayer tarball 指纹（升版本时同步更新）
DLSSNR_TARBALL_SHA256="${DLSSNR_TARBALL_SHA256:-2a5df946ff7054c2c6cdfaffad5c6833bbaaf484c9991affd97a7abf08404ffd}"
# Ada(RTX 40) retarget 版 nvngx_dlssnr.dll 指纹
DLL_SHA256="cdfa6c1ec784867182164c7836f324f087b5c9f374d0c9fe46f54999feabf3ed"
# 发布前改成 "你的用户名/dlssnr-suite"，或运行时用 DLSSNR_SUITE_REPO=<owner/repo> 覆盖
GH_REPO="${DLSSNR_SUITE_REPO:-Kuner-mw/dlssnr-suite}"

# ---------- 路径 ----------
DATA_HOME="${XDG_DATA_HOME:-$HOME/.local/share}"
BIN_HOME="$HOME/.local/bin"
DLSSNR_LIB="$HOME/.local/lib/dlssnr"
BINARIES_DIR="$DATA_HOME/dlssnr/binaries"
PANEL_BIN="$BIN_HOME/dlssnr-panel"
UNIT_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
LAYER_JSON="$DATA_HOME/vulkan/implicit_layer.d/VK_LAYER_NV_dlssnr.x86_64.json"

# ---------- 参数 ----------
DLL_FROM=""          # --dll 路径
NO_DOWNLOAD=0
FROM_SOURCE=0
NO_SERVICE=0
FORCE_DLSSNR=0
PANEL_ONLY=0

# ---------- 工具 ----------
log()  { printf '\033[1;36m==>\033[0m %s\n' "$*"; }
ok()   { printf '\033[1;32m ✓ \033[0m%s\n' "$*"; }
warn() { printf '\033[1;33m ! \033[0m%s\n' "$*"; }
err()  { printf '\033[1;31m ✗ \033[0m%s\n' "$*" >&2; exit 1; }
have() { command -v "$1" &>/dev/null; }

while [[ $# -gt 0 ]]; do
    case "$1" in
        --dll)          DLL_FROM="$2"; shift 2 ;;
        --no-download)  NO_DOWNLOAD=1; shift ;;
        --panel-only)   PANEL_ONLY=1; shift ;;
        --from-source)  FROM_SOURCE=1; shift ;;
        --no-service)   NO_SERVICE=1; shift ;;
        --force-dlssnr) FORCE_DLSSNR=1; shift ;;
        -h|--help)      grep '^#' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) err "未知参数: $1（--help 查看用法）" ;;
    esac
done

# ---------- 工具（续） ----------
fetch() { # fetch <url> <out> —— 带重试与断点续传
    curl -fL --retry 3 --retry-delay 2 -C - --connect-timeout 15 -o "$2" "$1"
}

# ---------- 步骤 1：前置检查 ----------
log "检查依赖…"
for t in curl tar sha256sum; do have "$t" || err "缺少 $t，请先安装"; done
if [[ $PANEL_ONLY -ne 1 ]]; then
    have unzip || err "缺少 unzip（解压 dll 压缩包需要），请先安装"
fi
ok "依赖齐全"

[[ "$(uname -m)" == "x86_64" ]] || err "仅支持 x86_64（上游 dlssnr 与 NVIDIA NGX 均为 64 位 x86 构建）"

# ---------- 步骤 2：DLSS5VKLayer（helper + Vulkan 层） ----------
# 智能检测：用户级（上游 tarball --user）与系统级（AUR dlssnr-bin）均识别，
# 已装则自动复用，避免 Vulkan Loader 读到两份层清单冲突。
detect_dlssnr() { # 已装则输出形态描述，未装返回 1
    if   [[ -f "$HOME/.local/share/vulkan/implicit_layer.d/VK_LAYER_NV_dlssnr.x86_64.json" \
        && -f "$DLSSNR_LIB/layer/libVkLayer_NV_dlssnr.so" ]]; then
        echo "用户级 ~/.local/lib/dlssnr"
    elif [[ -f "/usr/share/vulkan/implicit_layer.d/VK_LAYER_NV_dlssnr.x86_64.json" \
        && ( -f "/usr/lib/dlssnr/layer/libVkLayer_NV_dlssnr.so" || -f "/usr/lib64/dlssnr/layer/libVkLayer_NV_dlssnr.so" ) ]]; then
        echo "系统级（如 AUR dlssnr-bin）"
    else
        return 1
    fi
}

install_dlssnr() {
    log "下载 DLSS5VKLayer ${DLSSNR_VER}…"
    local tmp; tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
    fetch "$DLSSNR_TARBALL_URL" "$tmp/dlssnr.tar.gz"
    echo "${DLSSNR_TARBALL_SHA256}  $tmp/dlssnr.tar.gz" | sha256sum -c - \
        || err "DLSS5VKLayer tarball SHA-256 校验失败（上游文件可能被篡改或版本不匹配）"
    tar xzf "$tmp/dlssnr.tar.gz" -C "$tmp"
    log "安装 DLSS5VKLayer（用户级 --user）…"
    (cd "$tmp"/dlssnr-*-linux-x86_64 && bash install.sh --user)
    rm -rf "$tmp"; trap - EXIT
    [[ -f "$LAYER_JSON" ]] || err "Vulkan 层清单未就位（$LAYER_JSON），上游安装异常"
    ok "DLSS5VKLayer 安装完成"
}

if [[ $PANEL_ONLY -eq 1 ]]; then
    warn "--panel-only：跳过 DLSS5VKLayer（面板 NR 功能需自行安装上游后才可用）"
elif FOUND="$(detect_dlssnr)" && [[ $FORCE_DLSSNR -eq 0 ]]; then
    ok "检测到 DLSS5VKLayer 已安装（$FOUND），自动复用不重装（--force-dlssnr 可强制重装用户级）"
else
    install_dlssnr
fi

# ---------- 步骤 3：nvngx_dlssnr.dll（NVIDIA 专有，不入库） ----------
if [[ $PANEL_ONLY -eq 1 ]]; then
    warn "--panel-only：跳过 dll（自装上游后可用 dlssnr-helper import-binaries 导入，或重跑本脚本不带 --panel-only）"
else
mkdir -p "$BINARIES_DIR"
DLL_PATH="$BINARIES_DIR/nvngx_dlssnr.dll"
sha_of() { sha256sum "$1" | awk '{print $1}'; }

if [[ -n "$DLL_FROM" ]]; then
    # 显式 --dll 优先于已存在文件（允许替换 dll）
    [[ -f "$DLL_FROM" ]] || err "自备 dll 不存在: $DLL_FROM"
    head -c 2 "$DLL_FROM" | grep -q "MZ" || err "$DLL_FROM 不是 Windows PE 文件"
    ACTUAL="$(sha_of "$DLL_FROM")"
    [[ "$ACTUAL" == "$DLL_SHA256" ]] || warn "指纹与 pin 版本不同（${ACTUAL:0:12}…），按你的要求继续使用"
    cp -f "$DLL_FROM" "$DLL_PATH"
    ok "自备 dll 已导入 $BINARIES_DIR"
elif [[ -f "$DLL_PATH" ]]; then
    if [[ "$(sha_of "$DLL_PATH")" == "$DLL_SHA256" ]]; then
        ok "nvngx_dlssnr.dll 已就位且校验一致，跳过"
    else
        warn "已存在 nvngx_dlssnr.dll 但指纹与 pin 版本不同（可能是更新版或不同来源），保留不动"
    fi
elif [[ $NO_DOWNLOAD -eq 1 ]]; then
    err "已禁止下载且未提供 --dll <路径>。请自备 nvngx_dlssnr.dll 后重跑：./install.sh --dll /路径/nvngx_dlssnr.dll"
else
    log "下载 nvngx_dlssnr ${DLL_VER}（约 183MB，RankFTW/rhi-repo release）…"
    TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
    fetch "$DLL_ZIP_URL" "$TMP/dll.zip"
    unzip -q -o "$TMP/dll.zip" -d "$TMP/unz"
    FOUND="$(find "$TMP/unz" -type f -name 'nvngx_dlssnr.dll' | head -1)"
    [[ -n "$FOUND" ]] || err "压缩包内未找到 nvngx_dlssnr.dll（上游打包结构可能已变更）"
    ACTUAL="$(sha_of "$FOUND")"
    [[ "$ACTUAL" == "$DLL_SHA256" ]] || err "下载的 dll 指纹不匹配（期望 ${DLL_SHA256:0:12}…，实际 ${ACTUAL:0:12}…），已中止以防供应链篡改"
    cp -f "$FOUND" "$DLL_PATH"
    ok "nvngx_dlssnr.dll 校验通过并导入 $BINARIES_DIR"
    rm -rf "$TMP"; trap - EXIT
fi
fi  # PANEL_ONLY 短路结束

# ---------- 步骤 4：dlssnr-panel（本项目面板） ----------
log "安装 dlssnr-panel…"
mkdir -p "$BIN_HOME"
PANEL_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/panel"
built=0
if [[ $FROM_SOURCE -eq 1 ]] || [[ -d "$PANEL_DIR" ]]; then
    if have cargo; then
        log "从源码编译面板（约 6-30s）…"
        (cd "$PANEL_DIR" && cargo build --release)
        cp -f "$PANEL_DIR/target/release/dlssnr-panel" "$PANEL_BIN"
        built=1
        ok "面板已编译安装到 $PANEL_BIN"
    elif [[ $FROM_SOURCE -eq 1 ]]; then
        err "指定了 --from-source 但未找到 cargo（请先安装 Rust: https://rustup.rs）"
    else
        warn "未找到 cargo，改为尝试下载预编译二进制…"
    fi
fi
if [[ $built -eq 0 ]] && { [[ ! -d "$PANEL_DIR" ]] || ! have cargo; }; then
    if [[ -z "$GH_REPO" || "$GH_REPO" == *your-name* ]]; then  # 防呆：未配置或仍是示例值
        err "预编译面板下载地址尚未配置（install.sh 顶部 GH_REPO 仍为占位符）。\
改用源码编译：安装 Rust（https://rustup.rs）后重跑本脚本，或设置 DLSSNR_SUITE_REPO=<owner>/dlssnr-suite"
    fi
    log "下载面板预编译二进制（$GH_REPO release）…"
    local_tmp="$(mktemp -d)"; trap 'rm -rf "$local_tmp"' EXIT
    fetch "https://github.com/${GH_REPO}/releases/latest/download/dlssnr-panel" "$local_tmp/dlssnr-panel"
    fetch "https://github.com/${GH_REPO}/releases/latest/download/dlssnr-panel.sha256" "$local_tmp/dlssnr-panel.sha256" \
        || err "未获取到 dlssnr-panel.sha256，拒绝安装未校验的二进制"
    (cd "$local_tmp" && sha256sum -c dlssnr-panel.sha256) \
        || err "面板预编译二进制 SHA-256 校验失败"
    install -m 0755 "$local_tmp/dlssnr-panel" "$PANEL_BIN"
    rm -rf "$local_tmp"; trap - EXIT
    ok "面板已安装到 $PANEL_BIN"
fi

# ---------- 步骤 5：systemd 用户服务 ----------
if [[ $NO_SERVICE -eq 1 ]]; then
    warn "按要求跳过 systemd 服务（手动运行: $PANEL_BIN serve）"
else
    if have systemctl && systemctl --user list-unit-files &>/dev/null; then
        if have ss && ss -tln 2>/dev/null | grep -q ':8099 '; then
            warn "端口 8099 已被占用——若非面板旧实例，服务将起不来（可编辑单元 ExecStart 追加 --port 换端口）"
        fi
        log "注册 systemd 用户服务…"
        mkdir -p "$UNIT_DIR"
        cat > "$UNIT_DIR/dlssnr-panel.service" <<'UNIT'
[Unit]
Description=DLSSNR Web Control Panel (dlssnr-suite)
After=graphical-session.target
PartOf=graphical-session.target

[Service]
Type=simple
ExecStart=%h/.local/bin/dlssnr-panel serve
Restart=on-failure
RestartSec=3
Environment=DISPLAY=:0

[Install]
WantedBy=graphical-session.target
UNIT
        systemctl --user daemon-reload
        systemctl --user enable --now dlssnr-panel
        ok "面板服务已启动（开机自启）：http://127.0.0.1:8099"
    else
        warn "无 systemd 用户会话，手动运行: $PANEL_BIN serve"
    fi
fi

# ---------- 完成 ----------
TOKEN_FILE="${XDG_STATE_HOME:-$HOME/.local/state}/dlssnr/webui.token"
# 全新安装时服务刚 fork，等 token 落盘（最多 1s）
if [[ $NO_SERVICE -eq 0 ]] && have systemctl; then
    for _ in {1..10}; do
        [[ -f "$TOKEN_FILE" ]] && break
        sleep 0.1
    done
fi
echo
echo "────────────────────────────────────────────────────"
echo " 安装完成。"
if [[ -f "$TOKEN_FILE" ]]; then
    echo
    echo " 1. 面板已由 systemd 启动（开机自启），浏览器打开："
    echo
    echo "      http://127.0.0.1:8099/?token=$(cat "$TOKEN_FILE")"
    echo
    echo "    点击即用，token 自动记住；手机/副屏同网段访问"
    echo "    http://<本机IP>:8099，首次粘贴同一 token 即可。"
else
    echo
    echo " 1. 面板二进制就绪，启动：$PANEL_BIN serve"
    echo "    → http://127.0.0.1:8099 （token 在 $TOKEN_FILE，"
    echo "      首次启动后生成；加 --no-token 则仅本机免密）"
fi
cat <<'DONE'

 2. 启用 DLSS5: 给游戏加 Steam 启动项
        VKLayer_DLSS5=1 %command%
    或写入 ~/.config/environment.d/dlssnr.conf:
        VKLayer_DLSS5=1
 3. 游戏启动后面板会自动联动 helper（auto_helper），
    无需手动起停。生效检查:
        ~/.local/bin/dlssnr-shmctl /tmp/dlssnr-$(id -u)/shm.bin status

 卸载: ./uninstall.sh
 免责: nvngx_dlssnr.dll 为 NVIDIA 专有库，仅供个人学习
       使用；本脚本只做下载校验，不存储不再分发。
────────────────────────────────────────────────────
DONE
