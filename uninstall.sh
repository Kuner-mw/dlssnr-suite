#!/usr/bin/env bash
# dlssnr-suite 卸载器
# 用法:
#   ./uninstall.sh            # 只卸本套件安装的面板与服务
#   ./uninstall.sh --all      # 连同上游 DLSS5VKLayer 与导入的 NGX dll 一并清除
set -euo pipefail

log()  { printf '\033[1;36m==>\033[0m %s\n' "$*"; }
ok()   { printf '\033[1;32m ✓ \033[0m%s\n' "$*"; }
warn() { printf '\033[1;33m ! \033[0m%s\n' "$*"; }

ALL=0
case "${1:-}" in
    "") ;;
    --all) ALL=1 ;;
    -h|--help)
        sed -n '3,5p' "$0"; exit 0 ;;
    *)
        echo "未知参数: $1（本脚本无参数=卸面板，--all=连上游组件一并卸）" >&2
        echo "查看用法: $0 --help" >&2
        exit 1 ;;
esac

if [[ ${ALL} -eq 1 && "${DLSSNR_UNINSTALL_CONFIRM:-}" != "yes" ]]; then
    echo "--all 会连同上游 DLSS5VKLayer 与 NGX dll 一并清除。"
    echo "确认请重跑: DLSSNR_UNINSTALL_CONFIRM=yes ./uninstall.sh --all"
    exit 1
fi

DATA_HOME="${XDG_DATA_HOME:-$HOME/.local/share}"
BIN_HOME="$HOME/.local/bin"
UNIT_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"

log "停止并移除面板服务…"
if command -v systemctl &>/dev/null; then
    systemctl --user disable --now dlssnr-panel 2>/dev/null || true
fi
rm -f "$UNIT_DIR/dlssnr-panel.service"
command -v systemctl &>/dev/null && systemctl --user daemon-reload 2>/dev/null || true
ok "面板服务已移除"

rm -f "$BIN_HOME/dlssnr-panel"
ok "面板二进制已移除"

if [[ $ALL -eq 1 ]]; then
    warn "将连同上游组件一并清除（helper/层/NGX dll，不影响游戏本体）"
    if [[ -f "$HOME/.local/lib/dlssnr/uninstall.sh" ]]; then
        bash "$HOME/.local/lib/dlssnr/uninstall.sh" || warn "上游卸载脚本报错，继续手工清理"
    fi
    rm -rf "$HOME/.local/lib/dlssnr"
    rm -f "$BIN_HOME"/dlssnr-helper "$BIN_HOME"/dlssnr-gui \
          "$BIN_HOME"/dlssnr-shmctl "$BIN_HOME"/dlssnr-runner-probe
    rm -rf "$DATA_HOME/dlssnr"          # 含 binaries/nvngx_dlssnr.dll 与 helper 前缀
    rm -f "$DATA_HOME"/vulkan/implicit_layer.d/VK_LAYER_NV_dlssnr.*.json
    ok "DLSS5VKLayer 与 NGX dll 已清除"
else
    log "上游 DLSS5VKLayer 与 NVIDIA dll 保留在原位（--all 可一并卸载）"
fi
ok "卸载完成"
