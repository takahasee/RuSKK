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

# 署名処理: RUSKKSERV_CODESIGN_IDENTITY が指定されていれば Keychain の証明書で厳密署名する。
# 未指定時は、再コンパイル後も macOS の TCC 権限を引き継ぐため、識別子固定の ad-hoc 署名にフォールバックする
# （※同一マシン上の他バイナリが同 Identifier で ad-hoc 署名すれば TCC 権限を継承できるトレードオフがある）。
if [ -n "${RUSKKSERV_CODESIGN_IDENTITY:-}" ]; then
  codesign --force --deep --sign "$RUSKKSERV_CODESIGN_IDENTITY" --identifier "com.ruskkserv.importer" "$APP_DIR"
else
  codesign --force --deep --sign - --identifier "com.ruskkserv.importer" -r='designated => identifier "com.ruskkserv.importer"' "$APP_DIR"
fi

echo "Successfully built RuSKKservImporter.app at ${APP_DIR}"

