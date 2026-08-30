#!/bin/sh
set -eu

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CONFIG="${ROOT}/launchd/config.env"
LAUNCH_AGENTS="${HOME}/Library/LaunchAgents"
LOG_DIR="${HOME}/Library/Logs/skk-proxy"
DOMAIN="gui/$(id -u)"

usage() {
  cat <<EOF
Usage: $0 [install|uninstall|status]

  install   LaunchAgent を登録して起動
  uninstall LaunchAgent を停止・削除
  status    登録状態を表示

事前に ${ROOT}/launchd/config.env.example を config.env にコピーし、
パスを編集してください。

azooKey SKKServ は GUI アプリで自動起動する想定です。
EOF
}

expand_path() {
  case "$1" in
    "~/"*) printf '%s\n' "${HOME}${1#\~}" ;;
    "~") printf '%s\n' "$HOME" ;;
    *) printf '%s\n' "$1" ;;
  esac
}

load_config() {
  if [ ! -f "$CONFIG" ]; then
    echo "error: ${CONFIG} が見つかりません" >&2
    echo "  cp launchd/config.env.example launchd/config.env" >&2
    exit 1
  fi
  # shellcheck disable=SC1090
  . "$CONFIG"

  : "${SKK_PROXY_BIN:?SKK_PROXY_BIN is required}"
  : "${YASKKSERV2_BIN:?YASKKSERV2_BIN is required}"
  : "${YASKKSERV2_DICTIONARY:?YASKKSERV2_DICTIONARY is required}"

  SKK_PROXY_BIN="$(expand_path "$SKK_PROXY_BIN")"
  YASKKSERV2_BIN="$(expand_path "$YASKKSERV2_BIN")"
  YASKKSERV2_DICTIONARY="$(expand_path "$YASKKSERV2_DICTIONARY")"
  LOG_DIR="$(expand_path "${LOG_DIR:-${HOME}/Library/Logs/skk-proxy}")"
  WAIT_SCRIPT="${ROOT}/scripts/wait-and-run-skk-proxy.sh"
}

check_binaries() {
  for bin in "$SKK_PROXY_BIN" "$YASKKSERV2_BIN"; do
    if [ ! -x "$bin" ]; then
      echo "error: 実行ファイルが見つかりません: $bin" >&2
      exit 1
    fi
  done
  if [ ! -f "$YASKKSERV2_DICTIONARY" ]; then
    echo "error: 辞書ファイルが見つかりません: $YASKKSERV2_DICTIONARY" >&2
    exit 1
  fi
}

render_plist() {
  template="$1"
  dest="$2"
  sed \
    -e "s|@SKK_PROXY_BIN@|${SKK_PROXY_BIN}|g" \
    -e "s|@YASKKSERV2_BIN@|${YASKKSERV2_BIN}|g" \
    -e "s|@YASKKSERV2_DICTIONARY@|${YASKKSERV2_DICTIONARY}|g" \
    -e "s|@WAIT_SCRIPT@|${WAIT_SCRIPT}|g" \
    -e "s|@LOG_DIR@|${LOG_DIR}|g" \
    "$template" >"$dest"
}

bootout_if_loaded() {
  label="$1"
  plist="${LAUNCH_AGENTS}/${label}.plist"
  if launchctl print "${DOMAIN}/${label}" >/dev/null 2>&1; then
    launchctl bootout "$DOMAIN" "$plist" 2>/dev/null || true
  fi
}

do_install() {
  load_config
  check_binaries

  mkdir -p "$LAUNCH_AGENTS" "$LOG_DIR"
  chmod +x "$WAIT_SCRIPT"

  # 旧バージョンで登録されていた azookey を削除
  bootout_if_loaded "com.skkproxy.azookey"
  rm -f "${LAUNCH_AGENTS}/com.skkproxy.azookey.plist"

  render_plist "${ROOT}/launchd/com.skkproxy.yaskkserv2.plist" \
    "${LAUNCH_AGENTS}/com.skkproxy.yaskkserv2.plist"
  render_plist "${ROOT}/launchd/com.skkproxy.skkserv.plist" \
    "${LAUNCH_AGENTS}/com.skkproxy.skkserv.plist"

  bootout_if_loaded "com.skkproxy.yaskkserv2"
  bootout_if_loaded "com.skkproxy.skkserv"

  launchctl bootstrap "$DOMAIN" "${LAUNCH_AGENTS}/com.skkproxy.yaskkserv2.plist"
  launchctl bootstrap "$DOMAIN" "${LAUNCH_AGENTS}/com.skkproxy.skkserv.plist"

  echo "installed. logs: ${LOG_DIR}"
  echo "  tail -f ${LOG_DIR}/skk-proxy.log"
}

do_uninstall() {
  bootout_if_loaded "com.skkproxy.skkserv"
  bootout_if_loaded "com.skkproxy.yaskkserv2"
  bootout_if_loaded "com.skkproxy.azookey"

  rm -f \
    "${LAUNCH_AGENTS}/com.skkproxy.skkserv.plist" \
    "${LAUNCH_AGENTS}/com.skkproxy.yaskkserv2.plist" \
    "${LAUNCH_AGENTS}/com.skkproxy.azookey.plist"

  echo "uninstalled."
}

do_status() {
  for label in com.skkproxy.yaskkserv2 com.skkproxy.skkserv; do
    if launchctl print "${DOMAIN}/${label}" >/dev/null 2>&1; then
      echo "${label}: loaded"
    else
      echo "${label}: not loaded"
    fi
  done
}

cmd="${1:-install}"
case "$cmd" in
  install) do_install ;;
  uninstall) do_uninstall ;;
  status) do_status ;;
  -h|--help|help) usage ;;
  *)
    echo "unknown command: $cmd" >&2
    usage
    exit 1
    ;;
esac
