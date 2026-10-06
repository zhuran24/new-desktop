#!/usr/bin/env bash
# 仅由 owner 在真实 Wayland 桌面启动；不读取日常 HOME，不连接模型。
set -euo pipefail
binary=$(realpath "${1:?usage: composer-lab.sh /path/to/nd-composer-lab [--quit-after SECONDS]}")
shift
socket=${WAYLAND_DISPLAY:?需要在 Wayland 会话内运行}
if [[ "$socket" != /* ]]; then socket="${XDG_RUNTIME_DIR:?}/$socket"; fi
[[ -S "$socket" ]] || { echo 'Wayland socket 不存在' >&2; exit 1; }
lab_root=$(mktemp -d /tmp/nd-composer-lab-XXXXXX)
trap 'rm -rf -- "$lab_root"' EXIT
mkdir -m 700 "$lab_root"/{home,claude,config,data,state,cache,runtime}
cat > "$lab_root/dbus.conf" <<'DBUS'
<busconfig><type>session</type><listen>unix:tmpdir=/tmp</listen>
<policy context="default"><allow send_destination="*"/><allow receive_sender="*"/><allow own="*"/></policy></busconfig>
DBUS
bwrap --unshare-net --die-with-parent --new-session \
    --ro-bind /usr /usr --ro-bind /etc /etc \
    --symlink usr/bin /bin --symlink usr/lib /lib --symlink usr/lib /lib64 \
    --proc /proc --ro-bind /sys /sys --dev /dev --dev-bind /dev/dri /dev/dri --tmpfs /tmp \
    --bind "$lab_root" /sandbox --ro-bind "$binary" /nd-composer-lab \
    --bind "$socket" /sandbox/runtime/wayland-0 \
    --clearenv --setenv PATH /usr/bin --setenv LANG C.UTF-8 \
    --setenv WAYLAND_DEBUG "${ND_COMPOSER_TRACE:-}" \
    --setenv HOME /sandbox/home --setenv CLAUDE_CONFIG_DIR /sandbox/claude \
    --setenv XDG_RUNTIME_DIR /sandbox/runtime --setenv WAYLAND_DISPLAY wayland-0 \
    --setenv XDG_CONFIG_HOME /sandbox/config --setenv XDG_DATA_HOME /sandbox/data \
    --setenv XDG_STATE_HOME /sandbox/state --setenv XDG_CACHE_HOME /sandbox/cache \
    dbus-run-session --config-file /sandbox/dbus.conf -- /nd-composer-lab "$@"
