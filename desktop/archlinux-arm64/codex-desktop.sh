#!/bin/bash
# SPDX-FileCopyrightText: 2026 Arch Linux Contributors
# SPDX-License-Identifier: 0BSD
set -euo pipefail

if [[ -n "${SteamAppId:-}" ]]; then
  unset LD_PRELOAD
fi

install_root="${XDG_DATA_HOME:-${HOME}/.local/share}/openai-codex-desktop"
appdir="${install_root}/package/usr/lib/openai-codex-desktop"
electron="${install_root}/runtime/electron"
resources_dir="${appdir}/resources"
webview_dir="${appdir}/content/webview"
user_flags=()

[[ -x "${electron}" ]] || {
  echo "Missing Electron runtime: ${electron}" >&2
  exit 1
}

config_home="${XDG_CONFIG_HOME:-}"
if [[ -z "${config_home}" && -n "${HOME:-}" ]]; then
  config_home="${HOME}/.config"
fi

if [[ -n "${config_home}" && -f "${config_home}/codex-flags.conf" ]]; then
  while IFS= read -r flag_line || [[ -n "${flag_line}" ]]; do
    flag_line="${flag_line%%#*}"
    read -r -a flag_parts <<<"${flag_line}"
    user_flags+=("${flag_parts[@]}")
  done <"${config_home}/codex-flags.conf"
fi

resolve_codex_cli() {
  local candidate default_alias default_pattern

  if [[ -n "${CODEX_CLI_PATH:-}" && -x "${CODEX_CLI_PATH}" ]]; then
    printf '%s\n' "${CODEX_CLI_PATH}"
    return 0
  fi

  candidate="$(command -v codex 2>/dev/null || true)"
  if [[ -n "${candidate}" && -x "${candidate}" ]]; then
    printf '%s\n' "${candidate}"
    return 0
  fi

  # Graphical launchers do not inherit NVM's shell PATH. Prefer the active
  # default NVM line, then fall back to the newest installed NVM Codex CLI.
  default_alias=""
  [[ -f "${HOME}/.nvm/alias/default" ]] && default_alias="$(<"${HOME}/.nvm/alias/default")"
  default_alias="${default_alias#v}"
  case "${default_alias}" in
    [0-9]*) default_pattern="v${default_alias}*" ;;
    *) default_pattern="v*" ;;
  esac

  candidate="$(compgen -G "${HOME}/.nvm/versions/node/${default_pattern}/bin/codex" | sort -V | tail -n1 || true)"
  if [[ -n "${candidate}" && -x "${candidate}" ]]; then
    printf '%s\n' "${candidate}"
    return 0
  fi

  candidate="$(compgen -G "${HOME}/.nvm/versions/node/v*/bin/codex" | sort -V | tail -n1 || true)"
  if [[ -n "${candidate}" && -x "${candidate}" ]]; then
    printf '%s\n' "${candidate}"
    return 0
  fi

  for candidate in \
    "${HOME}/.local/bin/codex" \
    "${HOME}/.volta/bin/codex" \
    "${HOME}/.asdf/shims/codex" \
    "${HOME}/.local/share/mise/shims/codex"; do
    if [[ -x "${candidate}" ]]; then
      printf '%s\n' "${candidate}"
      return 0
    fi
  done

  return 1
}

CODEX_CLI_PATH="$(resolve_codex_cli || true)"
[[ -n "${CODEX_CLI_PATH}" ]] || {
  echo "Unable to find an installed Codex CLI. Install it or set CODEX_CLI_PATH." >&2
  exit 1
}
export CODEX_CLI_PATH

codex_cli_bin_dir="$(dirname "${CODEX_CLI_PATH}")"
case ":${PATH}:" in
  *":${codex_cli_bin_dir}:"*) ;;
  *) export PATH="${codex_cli_bin_dir}:${PATH}" ;;
esac
export BUILD_FLAVOR="${BUILD_FLAVOR:-prod}"
export NODE_ENV="${NODE_ENV:-production}"
export GTK_THEME="${GTK_THEME:-Adwaita:dark}"
export CODEX_ELECTRON_RESOURCES_PATH="${CODEX_ELECTRON_RESOURCES_PATH:-${resources_dir}}"
export CODEX_ELECTRON_BUNDLED_PLUGINS_RESOURCES_PATH="${CODEX_ELECTRON_BUNDLED_PLUGINS_RESOURCES_PATH:-${resources_dir}}"

webview_cache_version=""
if [[ -f "${appdir}/resources/app.asar" ]]; then
  webview_cache_version="$(stat -c '%Y-%s' "${appdir}/resources/app.asar" 2>/dev/null || true)"
fi

if [[ -n "${config_home}" && -n "${webview_cache_version}" ]]; then
  state_dir="${config_home}/Codex"
  version_file="${state_dir}/aur-webview-cache-version"
  previous_version=""
  [[ -f "${version_file}" ]] && previous_version="$(<"${version_file}")"

  if [[ "${previous_version}" != "${webview_cache_version}" ]]; then
    rm -rf "${state_dir}/Cache" "${state_dir}/Code Cache"
    mkdir -p "${state_dir}"
    printf '%s\n' "${webview_cache_version}" >"${version_file}"
  fi
fi

renderer_url="http://localhost:5175/"
if [[ -n "${webview_cache_version}" ]]; then
  renderer_url="http://localhost:5175/?aurWebviewVersion=${webview_cache_version}"
fi
export ELECTRON_RENDERER_URL="${ELECTRON_RENDERER_URL:-${renderer_url}}"

http_pid=""
electron_pid=""
tmpdir=""

cleanup() {
  [[ -n "${electron_pid}" ]] && wait "${electron_pid}" 2>/dev/null || true
  [[ -n "${http_pid}" ]] && kill "${http_pid}" 2>/dev/null || true
  [[ -n "${http_pid}" ]] && wait "${http_pid}" 2>/dev/null || true
  [[ -n "${tmpdir}" ]] && rm -rf "${tmpdir}"
}

forward_signal() {
  local sig="$1"

  if [[ -n "${electron_pid}" ]] && kill -0 "${electron_pid}" 2>/dev/null; then
    kill -"${sig}" "${electron_pid}" 2>/dev/null || true
    wait "${electron_pid}" 2>/dev/null || true
  fi

  exit 0
}

trap cleanup EXIT
trap 'forward_signal HUP' HUP
trap 'forward_signal INT' INT
trap 'forward_signal TERM' TERM

if [[ -d "${webview_dir}" ]] && find "${webview_dir}" -mindepth 1 -maxdepth 1 -print -quit | grep -q .; then
  tmpdir="$(mktemp -d)"
  ready_file="${tmpdir}/ready"
  fail_file="${tmpdir}/fail"

  python3 - 5175 "${webview_dir}" "${ready_file}" "${fail_file}" >/dev/null 2>&1 <<'PY' &
import http.server
import os
import socketserver
import sys

port = int(sys.argv[1])
root = sys.argv[2]
ready_file = sys.argv[3]
fail_file = sys.argv[4]

os.chdir(root)

class Handler(http.server.SimpleHTTPRequestHandler):
    def end_headers(self):
        self.send_header("Cache-Control", "no-store, no-cache, must-revalidate, max-age=0")
        self.send_header("Pragma", "no-cache")
        self.send_header("Expires", "0")
        super().end_headers()

    def log_message(self, fmt, *args):
        pass

class TCPServer(socketserver.TCPServer):
    allow_reuse_address = True

try:
    with TCPServer(("127.0.0.1", port), Handler) as httpd:
        with open(ready_file, "w") as f:
            f.write("ok")
        httpd.serve_forever()
except Exception as e:
    with open(fail_file, "w") as f:
        f.write(str(e))
    raise
PY
  http_pid=$!

  for _ in {1..50}; do
    [[ -f "${ready_file}" ]] && break
    if [[ -f "${fail_file}" ]]; then
      echo "Failed to start local webview server on 127.0.0.1:5175" >&2
      cat "${fail_file}" >&2
      exit 1
    fi
    kill -0 "${http_pid}" 2>/dev/null || {
      echo "Local webview server exited before becoming ready" >&2
      exit 1
    }
    sleep 0.1
  done

  [[ -f "${ready_file}" ]] || {
    echo "Timed out waiting for local webview server on 127.0.0.1:5175" >&2
    exit 1
  }
fi

"${electron}" \
  --enable-sandbox \
  --ozone-platform-hint=auto \
  --class=codex \
  "${user_flags[@]}" \
  "${appdir}/resources/app.asar" \
  "$@" &
electron_pid=$!

wait "${electron_pid}"
