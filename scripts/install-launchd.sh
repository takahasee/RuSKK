#!/bin/sh
set -eu

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CONFIG="${ROOT}/launchd/config.env"
LAUNCH_AGENTS="${HOME}/Library/LaunchAgents"
LOG_DIR="${HOME}/Library/Logs/ruskk"
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

  RUSKK_BIN="${RUSKK_BIN:-${SKK_PROXY_BIN:-}}"
  : "${RUSKK_BIN:?RUSKK_BIN (or SKK_PROXY_BIN) is required}"
  : "${YASKKSERV2_BIN:?YASKKSERV2_BIN is required}"
  : "${YASKKSERV2_DICTIONARY:?YASKKSERV2_DICTIONARY is required}"

  RUSKK_BIN="$(expand_path "$RUSKK_BIN")"
  YASKKSERV2_BIN="$(expand_path "$YASKKSERV2_BIN")"
  YASKKSERV2_DICTIONARY="$(expand_path "$YASKKSERV2_DICTIONARY")"
  LOG_DIR="$(expand_path "${LOG_DIR:-${HOME}/Library/Logs/ruskk}")"
  WAIT_SCRIPT="${ROOT}/scripts/wait-and-run-ruskk.sh"
  MACSKK_USER_DICT_PATH="${MACSKK_USER_DICT_PATH:-}"
  if [ -n "$MACSKK_USER_DICT_PATH" ]; then
    MACSKK_USER_DICT_PATH="$(expand_path "$MACSKK_USER_DICT_PATH")"
  fi
}

check_binaries() {
  for bin in "$RUSKK_BIN" "$YASKKSERV2_BIN"; do
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
    -e "s|@RUSKK_BIN@|${RUSKK_BIN}|g" \
    -e "s|@SKK_PROXY_BIN@|${RUSKK_BIN}|g" \
    -e "s|@YASKKSERV2_BIN@|${YASKKSERV2_BIN}|g" \
    -e "s|@YASKKSERV2_DICTIONARY@|${YASKKSERV2_DICTIONARY}|g" \
    -e "s|@WAIT_SCRIPT@|${WAIT_SCRIPT}|g" \
    -e "s|@LOG_DIR@|${LOG_DIR}|g" \
    -e "s|@MACSKK_USER_DICT_PATH@|${MACSKK_USER_DICT_PATH:-}|g" \
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

  # 旧 skk-proxy 関連の登録をクリーンアップ
  for old_label in com.skkproxy.azookey com.skkproxy.skkserv com.skkproxy.yaskkserv2; do
    bootout_if_loaded "$old_label"
    rm -f "${LAUNCH_AGENTS}/${old_label}.plist"
  done

  render_plist "${ROOT}/launchd/com.ruskk.yaskkserv2.plist" \
    "${LAUNCH_AGENTS}/com.ruskk.yaskkserv2.plist"
  render_plist "${ROOT}/launchd/com.ruskk.skkserv.plist" \
    "${LAUNCH_AGENTS}/com.ruskk.skkserv.plist"

  bootout_if_loaded "com.ruskk.yaskkserv2"
  bootout_if_loaded "com.ruskk.skkserv"

  launchctl bootstrap "$DOMAIN" "${LAUNCH_AGENTS}/com.ruskk.yaskkserv2.plist"
  launchctl bootstrap "$DOMAIN" "${LAUNCH_AGENTS}/com.ruskk.skkserv.plist"

  if [ -n "$MACSKK_USER_DICT_PATH" ]; then
    render_plist "${ROOT}/launchd/com.ruskk.import-user-dict.plist" \
      "${LAUNCH_AGENTS}/com.ruskk.import-user-dict.plist"
    bootout_if_loaded "com.ruskk.import-user-dict"
    launchctl bootstrap "$DOMAIN" "${LAUNCH_AGENTS}/com.ruskk.import-user-dict.plist"
  fi

  echo "installed. logs: ${LOG_DIR}"
  echo "  tail -f ${LOG_DIR}/ruskk.log"
}

do_uninstall() {
  for label in com.ruskk.skkserv com.ruskk.yaskkserv2 com.ruskk.import-user-dict com.skkproxy.skkserv com.skkproxy.yaskkserv2 com.skkproxy.azookey; do
    bootout_if_loaded "$label"
    rm -f "${LAUNCH_AGENTS}/${label}.plist"
  done

  echo "uninstalled."
}

do_status() {
  for label in com.ruskk.yaskkserv2 com.ruskk.skkserv com.ruskk.import-user-dict; do
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
