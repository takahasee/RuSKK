#!/bin/sh
set -eu

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CONFIG="${ROOT}/launchd/config.env"

if [ ! -f "$CONFIG" ]; then
  echo "error: ${CONFIG} が見つかりません" >&2
  exit 1
fi

# shellcheck disable=SC1090
. "$CONFIG"

expand_path() {
  case "$1" in
    "~/"*) printf '%s\n' "${HOME}${1#\~}" ;;
    "~") printf '%s\n' "$HOME" ;;
    *) printf '%s\n' "$1" ;;
  esac
}

RUSKKSERV_BIN="$(expand_path "${RUSKKSERV_BIN:-${SKK_PROXY_BIN:-}}")"
MACSKK_USER_DICT_PATH="$(expand_path "${MACSKK_USER_DICT_PATH:-}")"

APP_DIR="${HOME}/Applications/RuSKKservImporter.app"
CONTENTS_DIR="${APP_DIR}/Contents"
MACOS_DIR="${CONTENTS_DIR}/MacOS"

mkdir -p "$MACOS_DIR"

# Info.plist
cat <<EOF >"${CONTENTS_DIR}/Info.plist"
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleIdentifier</key>
    <string>com.ruskkserv.importer</string>
    <key>CFBundleName</key>
    <string>RuSKKservImporter</string>
    <key>CFBundleExecutable</key>
    <string>RuSKKservImporter</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>1.0</string>
    <key>CFBundleVersion</key>
    <string>1</string>
    <key>LSUIElement</key>
    <true/>
</dict>
</plist>
EOF

# Binary copy
if [ ! -x "$RUSKKSERV_BIN" ]; then
  echo "error: ${RUSKKSERV_BIN} が見つかりません。先に cargo build --release を実行してください。" >&2
  exit 1
fi

cp -p "$RUSKKSERV_BIN" "${MACOS_DIR}/RuSKKservImporter"
chmod +x "${MACOS_DIR}/RuSKKservImporter"

# アプリバンドルとして固定署名 (Identifier を固定し、Designated Requirement を固定することで再コンパイル後も TCC 権限を維持)
codesign --force --deep --sign - --identifier "com.ruskkserv.importer" -r='designated => identifier "com.ruskkserv.importer"' "$APP_DIR"

echo "Successfully built RuSKKservImporter.app at ${APP_DIR}"

