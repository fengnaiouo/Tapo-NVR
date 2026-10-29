#!/usr/bin/env bash
# Tapo NVR Linux 伺服器安裝腳本 / installer for the Linux server
#   sudo ./install.sh
# 安裝到 /opt/tapo-nvr，資料放在 /var/lib/tapo-nvr，並建立 systemd 服務（開機自動啟動）。
# 重新執行即可升級（資料會保留）。
set -euo pipefail

if [ "$(id -u)" -ne 0 ]; then
  echo "請用 sudo 執行 / Please run with sudo: sudo $0" >&2
  exit 1
fi

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PREFIX=/opt/tapo-nvr
DATA=/var/lib/tapo-nvr

echo "==> 檢查 ffmpeg / Checking ffmpeg"
if ! command -v ffmpeg >/dev/null 2>&1; then
  if command -v apt-get >/dev/null 2>&1; then
    apt-get update -qq && apt-get install -y -qq ffmpeg
  elif command -v dnf >/dev/null 2>&1; then
    dnf install -y ffmpeg || dnf install -y ffmpeg-free
  elif command -v pacman >/dev/null 2>&1; then
    pacman -Sy --noconfirm ffmpeg
  elif command -v zypper >/dev/null 2>&1; then
    zypper --non-interactive install ffmpeg
  else
    echo "找不到 ffmpeg，請先自行安裝 / ffmpeg not found, please install it first" >&2
    exit 1
  fi
fi

echo "==> 建立使用者 tapo-nvr / Creating user tapo-nvr"
if ! id tapo-nvr >/dev/null 2>&1; then
  useradd --system --home-dir "$DATA" --shell /usr/sbin/nologin tapo-nvr
fi

echo "==> 安裝程式到 $PREFIX / Installing to $PREFIX"
systemctl stop tapo-nvr 2>/dev/null || true
install -d "$PREFIX/bin"
install -m 0755 "$HERE/tapo-nvr" "$PREFIX/tapo-nvr"
install -m 0755 "$HERE/bin/go2rtc" "$PREFIX/bin/go2rtc"
install -m 0644 "$HERE/LICENSE" "$HERE/THIRD_PARTY_NOTICES.md" "$PREFIX/" 2>/dev/null || true
install -d -o tapo-nvr -g tapo-nvr -m 0750 "$DATA"

echo "==> 建立 systemd 服務 / Installing systemd service"
install -m 0644 "$HERE/tapo-nvr.service" /etc/systemd/system/tapo-nvr.service
systemctl daemon-reload
systemctl enable --now tapo-nvr

IP="$(hostname -I 2>/dev/null | awk '{print $1}')"
cat <<EOF

✅ Tapo NVR 已安裝並啟動 / installed and running

  網址 / URL:      http://${IP:-<this-host>}:8080
  資料 / Data:     $DATA（設定資料庫、事件截圖）
  記錄 / Logs:     journalctl -u tapo-nvr -f
  停止 / Stop:     sudo systemctl stop tapo-nvr

第一次開啟請在區網內用瀏覽器建立管理員帳號。
錄影資料夾請讓 tapo-nvr 使用者可寫入，例如：
  sudo mkdir -p /srv/recordings && sudo chown tapo-nvr:tapo-nvr /srv/recordings
防火牆（若有啟用）：開放 TCP 8080 與 TCP/UDP 8555（WebRTC）。
EOF
