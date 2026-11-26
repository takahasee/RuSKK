#!/bin/sh
set -eu

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CONFIG="${ROOT}/launchd/config.env"
LAUNCH_AGENTS="${HOME}/Library/LaunchAgents"
LOG_DIR="${HOME}/Library/Logs/ruskkserv"
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

  RUSKKSERV_BIN="${RUSKKSERV_BIN:-${SKK_PROXY_BIN:-}}"
  : "${RUSKKSERV_BIN:?RUSKKSERV_BIN (or SKK_PROXY_BIN) is required}"
  : "${YASKKSERV2_BIN:?YASKKSERV2_BIN is required}"
  : "${YASKKSERV2_DICTIONARY:?YASKKSERV2_DICTIONARY is required}"

  RUSKKSERV_BIN="$(expand_path "$RUSKKSERV_BIN")"
  YASKKSERV2_BIN="$(expand_path "$YASKKSERV2_BIN")"
  YASKKSERV2_DICTIONARY="$(expand_path "$YASKKSERV2_DICTIONARY")"
  YASKKSERV2_GOOGLE_CACHE="$(expand_path "${YASKKSERV2_GOOGLE_CACHE:-${HOME}/Library/Caches/ruskkserv/yaskkserv2-google.cache}")"
  YASKKSERV2_GOOGLE_JAPANESE_INPUT="${YASKKSERV2_GOOGLE_JAPANESE_INPUT:-disable}"
  case "$YASKKSERV2_GOOGLE_JAPANESE_INPUT" in
    disable|notfound|last|first) ;;
    *)
      echo "error: YASKKSERV2_GOOGLE_JAPANESE_INPUT は disable|notfound|last|first のいずれかを指定してください: ${YASKKSERV2_GOOGLE_JAPANESE_INPUT}" >&2
      exit 1
      ;;
  esac
  LOG_DIR="$(expand_path "${LOG_DIR:-${HOME}/Library/Logs/ruskkserv}")"
  WAIT_SCRIPT="${ROOT}/scripts/wait-and-run-ruskkserv.sh"
  IMPORTER_APP="${HOME}/Applications/RuSKKservImporter.app"
  IMPORTER_APP_BIN="${IMPORTER_APP}/Contents/MacOS/RuSKKservImporter"
  RUSKKSERV_OKURI_EXPANSION="${RUSKKSERV_OKURI_EXPANSION:-1}"
  RUSKKSERV_CONTEXT_RANKING="${RUSKKSERV_CONTEXT_RANKING:-1}"
  RUSKKSERV_ANNOTATION_PRIORITY="${RUSKKSERV_ANNOTATION_PRIORITY:-1}"
  RUSKKSERV_STRICT_OKURI="${RUSKKSERV_STRICT_OKURI:-1}"
  RUSKKSERV_BACKEND_ORDER="${RUSKKSERV_BACKEND_ORDER:-yaskkserv2-first}"
  RUSKKSERV_YASKKSERV2_ENCODING="${RUSKKSERV_YASKKSERV2_ENCODING:-euc}"
  case "$RUSKKSERV_YASKKSERV2_ENCODING" in
    euc|utf8|euc-req-utf8-resp) ;;
    *)
      echo "error: RUSKKSERV_YASKKSERV2_ENCODING は euc|utf8|euc-req-utf8-resp のいずれかを指定してください: ${RUSKKSERV_YASKKSERV2_ENCODING}" >&2
      exit 1
      ;;
  esac
  RUSKKSERV_LOG_LEVEL="${RUSKKSERV_LOG_LEVEL:-info}"
  MACSKK_USER_DICT_PATH="${MACSKK_USER_DICT_PATH:-}"
  if [ -n "$MACSKK_USER_DICT_PATH" ]; then
    MACSKK_USER_DICT_PATH="$(expand_path "$MACSKK_USER_DICT_PATH")"
  fi
}

check_binaries() {
  for bin in "$RUSKKSERV_BIN" "$YASKKSERV2_BIN"; do
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

# XML および sed 置換文字列のメタ文字を安全な順序でエスケープする
escape_plist_sed() {
  printf '%s' "$1" | sed \
    -e 's/\\/\\\\/g' \
    -e 's/&/\&amp;/g' \
    -e 's/</\&lt;/g' \
    -e 's/>/\&gt;/g' \
    -e 's/"/\&quot;/g' \
    -e "s/'/\\&apos;/g" \
    -e 's/&/\\&/g' \
    -e 's/|/\\|/g'
}

render_plist() {
  template="$1"
  dest="$2"
  esc_ruskkserv_bin="$(escape_plist_sed "$RUSKKSERV_BIN")"
  esc_yaskkserv2_bin="$(escape_plist_sed "$YASKKSERV2_BIN")"
  esc_yaskkserv2_dict="$(escape_plist_sed "$YASKKSERV2_DICTIONARY")"
  esc_yaskkserv2_cache="$(escape_plist_sed "$YASKKSERV2_GOOGLE_CACHE")"
  esc_yaskkserv2_google_input="$(escape_plist_sed "$YASKKSERV2_GOOGLE_JAPANESE_INPUT")"
  esc_wait_script="$(escape_plist_sed "$WAIT_SCRIPT")"
  esc_importer_app_bin="$(escape_plist_sed "$IMPORTER_APP_BIN")"
  esc_log_dir="$(escape_plist_sed "$LOG_DIR")"
  esc_okuri="$(escape_plist_sed "$RUSKKSERV_OKURI_EXPANSION")"
  esc_context="$(escape_plist_sed "$RUSKKSERV_CONTEXT_RANKING")"
  esc_annotation="$(escape_plist_sed "$RUSKKSERV_ANNOTATION_PRIORITY")"
  esc_strict_okuri="$(escape_plist_sed "$RUSKKSERV_STRICT_OKURI")"
  esc_backend_order="$(escape_plist_sed "$RUSKKSERV_BACKEND_ORDER")"
  esc_yaskkserv2_encoding="$(escape_plist_sed "$RUSKKSERV_YASKKSERV2_ENCODING")"
  esc_log_level="$(escape_plist_sed "$RUSKKSERV_LOG_LEVEL")"
  esc_user_dict="$(escape_plist_sed "${MACSKK_USER_DICT_PATH:-}")"

  if [ "$RUSKKSERV_YASKKSERV2_ENCODING" = "utf8" ]; then
    midashi_utf8_sed='s|@YASKKSERV2_MIDASHI_UTF8_LINE@|        <string>--midashi-utf8</string>|g'
  else
    midashi_utf8_sed='/@YASKKSERV2_MIDASHI_UTF8_LINE@/d'
  fi

  sed \
    -e "s|@RUSKKSERV_BIN@|${esc_ruskkserv_bin}|g" \
    -e "s|@SKK_PROXY_BIN@|${esc_ruskkserv_bin}|g" \
    -e "s|@YASKKSERV2_BIN@|${esc_yaskkserv2_bin}|g" \
    -e "s|@YASKKSERV2_DICTIONARY@|${esc_yaskkserv2_dict}|g" \
    -e "s|@YASKKSERV2_GOOGLE_CACHE@|${esc_yaskkserv2_cache}|g" \
    -e "s|@YASKKSERV2_GOOGLE_JAPANESE_INPUT@|${esc_yaskkserv2_google_input}|g" \
    -e "$midashi_utf8_sed" \
    -e "s|@WAIT_SCRIPT@|${esc_wait_script}|g" \
    -e "s|@IMPORTER_APP_BIN@|${esc_importer_app_bin}|g" \
    -e "s|@LOG_DIR@|${esc_log_dir}|g" \
    -e "s|@RUSKKSERV_OKURI_EXPANSION@|${esc_okuri}|g" \
    -e "s|@RUSKKSERV_CONTEXT_RANKING@|${esc_context}|g" \
    -e "s|@RUSKKSERV_ANNOTATION_PRIORITY@|${esc_annotation}|g" \
    -e "s|@RUSKKSERV_STRICT_OKURI@|${esc_strict_okuri}|g" \
    -e "s|@RUSKKSERV_BACKEND_ORDER@|${esc_backend_order}|g" \
    -e "s|@RUSKKSERV_YASKKSERV2_ENCODING@|${esc_yaskkserv2_encoding}|g" \
    -e "s|@RUSKKSERV_LOG_LEVEL@|${esc_log_level}|g" \
    -e "s|@MACSKK_USER_DICT_PATH@|${esc_user_dict}|g" \
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

  mkdir -p "$LAUNCH_AGENTS"
  mkdir -p -m 700 "$LOG_DIR"
  chmod 700 "$LOG_DIR"
  mkdir -p -m 700 "$(dirname "$YASKKSERV2_GOOGLE_CACHE")"
  chmod 700 "$(dirname "$YASKKSERV2_GOOGLE_CACHE")"
  if [ -f "$YASKKSERV2_GOOGLE_CACHE" ]; then
    chmod 600 "$YASKKSERV2_GOOGLE_CACHE"
  fi
  chmod +x "$WAIT_SCRIPT" "${ROOT}/scripts/build-importer-app.sh"

  # 旧 skk-proxy 関連の登録をクリーンアップ
  for old_label in com.skkproxy.azookey com.skkproxy.skkserv com.skkproxy.yaskkserv2; do
    bootout_if_loaded "$old_label"
    rm -f "${LAUNCH_AGENTS}/${old_label}.plist"
  done

  render_plist "${ROOT}/launchd/com.ruskkserv.yaskkserv2.plist" \
    "${LAUNCH_AGENTS}/com.ruskkserv.yaskkserv2.plist"
  render_plist "${ROOT}/launchd/com.ruskkserv.skkserv.plist" \
    "${LAUNCH_AGENTS}/com.ruskkserv.skkserv.plist"

  bootout_if_loaded "com.ruskkserv.yaskkserv2"
  # 旧デーモン化プロセスが残っている場合のみ自 UID 限定で停止し、ポート解放を待機する
  /usr/bin/pkill -u "$(id -u)" -x yaskkserv2 2>/dev/null || true
  wait_tries=0
  while [ "$wait_tries" -lt 10 ]; do
    if ! /usr/bin/pgrep -u "$(id -u)" -x yaskkserv2 >/dev/null 2>&1; then
      break
    fi
    sleep 0.2
    wait_tries=$((wait_tries + 1))
  done
  bootout_if_loaded "com.ruskkserv.skkserv"

  launchctl enable "$DOMAIN/com.ruskkserv.yaskkserv2" 2>/dev/null || true
  launchctl bootstrap "$DOMAIN" "${LAUNCH_AGENTS}/com.ruskkserv.yaskkserv2.plist"
  launchctl enable "$DOMAIN/com.ruskkserv.skkserv" 2>/dev/null || true
  launchctl bootstrap "$DOMAIN" "${LAUNCH_AGENTS}/com.ruskkserv.skkserv.plist"

  if [ -n "$MACSKK_USER_DICT_PATH" ]; then
    "${ROOT}/scripts/build-importer-app.sh"
    render_plist "${ROOT}/launchd/com.ruskkserv.import-user-dict.plist" \
      "${LAUNCH_AGENTS}/com.ruskkserv.import-user-dict.plist"
    bootout_if_loaded "com.ruskkserv.import-user-dict"
    launchctl enable "$DOMAIN/com.ruskkserv.import-user-dict" 2>/dev/null || true
    launchctl bootstrap "$DOMAIN" "${LAUNCH_AGENTS}/com.ruskkserv.import-user-dict.plist"
  else
    bootout_if_loaded "com.ruskkserv.import-user-dict"
    rm -f "${LAUNCH_AGENTS}/com.ruskkserv.import-user-dict.plist"
  fi

  echo "installed. logs: ${LOG_DIR}"
  echo "  tail -f ${LOG_DIR}/ruskkserv.log"
}

do_uninstall() {
  for label in com.ruskkserv.skkserv com.ruskkserv.yaskkserv2 com.ruskkserv.import-user-dict com.skkproxy.skkserv com.skkproxy.yaskkserv2 com.skkproxy.azookey; do
    bootout_if_loaded "$label"
    rm -f "${LAUNCH_AGENTS}/${label}.plist"
  done

  echo "uninstalled."
}

do_status() {
  for label in com.ruskkserv.yaskkserv2 com.ruskkserv.skkserv com.ruskkserv.import-user-dict; do
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
