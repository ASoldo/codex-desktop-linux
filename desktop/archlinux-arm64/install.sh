#!/usr/bin/env bash
set -euo pipefail

checkout_recipe_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
recipe_dir="${CODEX_DESKTOP_SOURCE_DIR:-${HOME}/.local/src/openai-codex-desktop-arm64}"
install_root="${XDG_DATA_HOME:-${HOME}/.local/share}/openai-codex-desktop"
applications_dir="${XDG_DATA_HOME:-${HOME}/.local/share}/applications"
icons_dir="${XDG_DATA_HOME:-${HOME}/.local/share}/icons/hicolor/512x512/apps"
bin_dir="${HOME}/.local/bin"
systemd_user_dir="${XDG_CONFIG_HOME:-${HOME}/.config}/systemd/user"

electron_version="42.2.0"
electron_archive="electron-v${electron_version}-linux-arm64.zip"
electron_url="https://github.com/electron/electron/releases/download/v${electron_version}/${electron_archive}"
electron_sha256="1f2037dbdcb8b1327b855ec15fbe3fb8a7f27786b331d17866e88377a0606ad8"

[[ "$(uname -s)" == "Linux" && "$(uname -m)" == "aarch64" ]] || {
  printf 'This installer is only for Linux aarch64.\n' >&2
  exit 1
}

[[ -f /etc/arch-release ]] || {
  printf 'This desktop recipe is tested only on Arch Linux ARM64.\n' >&2
  exit 1
}

required_commands=(
  awk bsdtar curl desktop-file-validate find flock install makepkg
  node npm npx pgrep python3 sed sha256sum systemctl update-desktop-database
)
missing_commands=()
for command_name in "${required_commands[@]}"; do
  command -v "${command_name}" >/dev/null 2>&1 || missing_commands+=("${command_name}")
done
if ((${#missing_commands[@]} > 0)); then
  printf 'Missing required commands: %s\n' "${missing_commands[*]}" >&2
  exit 1
fi

command -v codex >/dev/null 2>&1 || {
  printf 'Codex CLI must be installed before Codex Desktop.\n' >&2
  exit 1
}

mkdir -p "${install_root}" "${applications_dir}" "${icons_dir}" "${bin_dir}" "${systemd_user_dir}"

mkdir -p "${recipe_dir}"
recipe_files=(
  Codex.desktop LICENSE PKGBUILD REUSE.toml codex-app.sh
  codex-desktop-tray.py codex-desktop-tray.service codex-desktop-update.sh
  codex-desktop.sh openai-codex-desktop.png
  patch-linux-browser-persistence.mjs patch-linux-dictation.mjs
  patch-linux-opaque-bg.mjs patch-linux-open-targets.mjs
  patch-linux-terminal-font.mjs
)
for recipe_file in "${recipe_files[@]}"; do
  [[ -f "${checkout_recipe_dir}/${recipe_file}" ]] || {
    printf 'Desktop recipe is missing %s\n' "${recipe_file}" >&2
    exit 1
  }
  recipe_mode=644
  case "${recipe_file}" in
    *.sh|*.py) recipe_mode=755 ;;
  esac
  install -Dm"${recipe_mode}" "${checkout_recipe_dir}/${recipe_file}" \
    "${recipe_dir}/${recipe_file}"
done

runtime_version=""
[[ -f "${install_root}/runtime/version" ]] && runtime_version="$(<"${install_root}/runtime/version")"
if [[ ! -x "${install_root}/runtime/electron" || "${runtime_version}" != "v${electron_version}" ]]; then
  temp_dir="$(mktemp -d)"
  runtime_stage="$(mktemp -d "${install_root}/.runtime.new.XXXXXX")"
  cleanup_runtime() {
    rm -rf "${temp_dir}"
    [[ -d "${runtime_stage}" ]] && rm -rf "${runtime_stage}"
  }
  trap cleanup_runtime EXIT

  curl --fail --show-error --location --retry 3 \
    --output "${temp_dir}/${electron_archive}" "${electron_url}"
  actual_sha256="$(sha256sum "${temp_dir}/${electron_archive}" | awk '{print $1}')"
  [[ "${actual_sha256}" == "${electron_sha256}" ]] || {
    printf 'Electron archive SHA-256 mismatch.\n' >&2
    exit 1
  }
  bsdtar -xf "${temp_dir}/${electron_archive}" -C "${runtime_stage}"
  [[ -x "${runtime_stage}/electron" ]] || {
    printf 'Electron archive did not contain an executable runtime.\n' >&2
    exit 1
  }

  runtime_backup="${install_root}/runtime.previous"
  [[ -d "${runtime_backup}" ]] && rm -rf "${runtime_backup}"
  [[ -d "${install_root}/runtime" ]] && mv "${install_root}/runtime" "${runtime_backup}"
  mv "${runtime_stage}" "${install_root}/runtime"
  trap - EXIT
  rm -rf "${temp_dir}"
fi

install -Dm755 "${recipe_dir}/codex-desktop-update.sh" "${bin_dir}/codex-desktop-update"
install -Dm755 "${recipe_dir}/codex-desktop-tray.py" "${bin_dir}/codex-desktop-tray"
install -Dm755 "${recipe_dir}/codex-app.sh" "${bin_dir}/codex-app"
install -Dm644 "${recipe_dir}/Codex.desktop" "${applications_dir}/Codex.desktop"
install -Dm644 "${recipe_dir}/openai-codex-desktop.png" \
  "${icons_dir}/openai-codex-desktop.png"
install -Dm644 "${recipe_dir}/codex-desktop-tray.service" \
  "${systemd_user_dir}/codex-desktop-tray.service"

desktop-file-validate "${applications_dir}/Codex.desktop"
update-desktop-database "${applications_dir}"

"${bin_dir}/codex-desktop-update"

systemctl --user daemon-reload
if python3 -c 'import cairo, gi' >/dev/null 2>&1; then
  systemctl --user enable --now codex-desktop-tray.service
else
  printf 'GTK/PyGObject tray dependencies are unavailable; the app and command-line updater are installed without the tray.\n'
fi

printf '\nCodex Desktop is installed. Launch it with codex-app or your desktop menu.\n'
