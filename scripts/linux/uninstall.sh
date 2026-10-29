#!/usr/bin/env bash
# Tapo NVR 解除安裝 / uninstaller
#   sudo ./uninstall.sh          保留資料 / keep data
#   sudo ./uninstall.sh --purge  一併刪除 /var/lib/tapo-nvr（錄影檔在自選資料夾，不會刪除）
set -euo pipefail
if [ "$(id -u)" -ne 0 ]; then
  echo "請用 sudo 執行 / Please run with sudo" >&2
  exit 1
fi
systemctl disable --now tapo-nvr 2>/dev/null || true
rm -f /etc/systemd/system/tapo-nvr.service
systemctl daemon-reload
rm -rf /opt/tapo-nvr
if [ "${1:-}" = "--purge" ]; then
  rm -rf /var/lib/tapo-nvr
  userdel tapo-nvr 2>/dev/null || true
  echo "已刪除程式與資料 / Removed program and data"
else
  echo "已刪除程式，資料保留在 /var/lib/tapo-nvr / Removed program; data kept in /var/lib/tapo-nvr"
fi
