#!/bin/sh
set -eu

AZOOKEY_HOST="${AZOOKEY_HOST:-127.0.0.1}"
AZOOKEY_PORT="${AZOOKEY_PORT:-1180}"
YASKKSERV2_HOST="${YASKKSERV2_HOST:-127.0.0.1}"
YASKKSERV2_PORT="${YASKKSERV2_PORT:-1179}"
WAIT_SECONDS="${WAIT_SECONDS:-120}"

wait_port() {
  host="$1"
  port="$2"
  name="$3"
  elapsed=0

  while [ "$elapsed" -lt "$WAIT_SECONDS" ]; do
    if nc -z "$host" "$port" 2>/dev/null; then
      return 0
    fi
    sleep 1
    elapsed=$((elapsed + 1))
  done

  echo "timeout waiting for ${name} at ${host}:${port}" >&2
  return 1
}

# azooKey SKKServ (GUI) on :1180, yaskkserv2 (LaunchAgent) on :1179
wait_port "$AZOOKEY_HOST" "$AZOOKEY_PORT" "azooKey SKKServ"
wait_port "$YASKKSERV2_HOST" "$YASKKSERV2_PORT" "yaskkserv2"

exec "${RUSKK_BIN:-${SKK_PROXY_BIN}}" "$@"
