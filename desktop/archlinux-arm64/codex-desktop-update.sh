#!/usr/bin/env bash
set -Eeuo pipefail

APPCAST_URL="${CODEX_DESKTOP_APPCAST_URL:-https://persistent.oaistatic.com/codex-app-prod/appcast.xml}"
SOURCE_DIR="${CODEX_DESKTOP_SOURCE_DIR:-${HOME}/.local/src/openai-codex-desktop-arm64}"
INSTALL_ROOT="${XDG_DATA_HOME:-${HOME}/.local/share}/openai-codex-desktop"
PACKAGE_DIR="${INSTALL_ROOT}/package"
BACKUP_DIR="${INSTALL_ROOT}/package.previous"
STATUS_FILE="${INSTALL_ROOT}/update-status"
DESKTOP_ENTRY="${XDG_DATA_HOME:-${HOME}/.local/share}/applications/Codex.desktop"
ICON_TARGET="${XDG_DATA_HOME:-${HOME}/.local/share}/icons/hicolor/512x512/apps/openai-codex-desktop.png"

mode=update
force=false
notify=false

usage() {
  cat <<'EOF'
Usage: codex-desktop-update [--check] [--force] [--rollback] [--notify]

  no arguments  Check OpenAI's feed, build the newest ARM64 app, and stage it.
  --check       Report whether an update is available without building it.
  --force       Rebuild and reinstall even when the installed version is current.
  --rollback    Swap the installed package with the single retained backup.
  --notify      With --check, show a desktop notification when an update exists.
EOF
}

log() {
  printf '[codex-desktop-update] %s\n' "$*"
}

die() {
  printf '[codex-desktop-update] ERROR: %s\n' "$*" >&2
  exit 1
}

for arg in "$@"; do
  case "${arg}" in
    --check) mode=check ;;
    --force) force=true ;;
    --rollback) mode=rollback ;;
    --notify) notify=true ;;
    -h|--help) usage; exit 0 ;;
    *) die "Unknown argument: ${arg}" ;;
  esac
done

require_commands() {
  local missing=()
  local command_name
  for command_name in "$@"; do
    command -v "${command_name}" >/dev/null 2>&1 || missing+=("${command_name}")
  done
  ((${#missing[@]} == 0)) || die "Missing commands: ${missing[*]}"
}

package_version() {
  local root="$1"
  local raw
  [[ -f "${root}/.PKGINFO" ]] || return 1
  raw="$(sed -n 's/^pkgver = //p' "${root}/.PKGINFO" | head -n1)"
  [[ -n "${raw}" ]] || return 1
  printf '%s\n' "${raw%-*}"
}

is_newer() {
  python3 - "$1" "$2" <<'PY'
import re
import sys

def key(value: str):
    return tuple(int(part) for part in re.findall(r"\d+", value))

raise SystemExit(0 if key(sys.argv[2]) > key(sys.argv[1]) else 1)
PY
}

refresh_user_assets() {
  local root="$1"
  local launcher="${root}/usr/bin/codex-desktop"
  local icon="${root}/usr/share/icons/hicolor/512x512/apps/openai-codex-desktop.png"

  [[ -x "${launcher}" ]] || die "Package does not contain the patched launcher"
  install -Dm755 "${launcher}" "${HOME}/.local/bin/codex-desktop"
  [[ -f "${icon}" ]] && install -Dm644 "${icon}" "${ICON_TARGET}"

  if [[ -f "${DESKTOP_ENTRY}" ]]; then
    desktop-file-validate "${DESKTOP_ENTRY}"
    update-desktop-database "$(dirname "${DESKTOP_ENTRY}")"
  fi
}

rollback_package() {
  [[ -d "${BACKUP_DIR}" ]] || die "No rollback package exists at ${BACKUP_DIR}"

  local swap_dir="${INSTALL_ROOT}/.package.rollback.$$"
  [[ ! -e "${swap_dir}" ]] || die "Rollback staging path already exists"

  if [[ -d "${PACKAGE_DIR}" ]]; then
    mv "${PACKAGE_DIR}" "${swap_dir}"
  fi
  mv "${BACKUP_DIR}" "${PACKAGE_DIR}"
  if [[ -d "${swap_dir}" ]]; then
    mv "${swap_dir}" "${BACKUP_DIR}"
  fi

  refresh_user_assets "${PACKAGE_DIR}"
  local rolled_version latest_version restart_pids restart_required
  rolled_version="$(package_version "${PACKAGE_DIR}")"
  latest_version="$(sed -n 's/^latest_version=//p' "${STATUS_FILE}" 2>/dev/null | tail -n1 || true)"
  [[ -n "${latest_version}" ]] || latest_version="${rolled_version}"
  restart_pids="$(
    pgrep -f "^${INSTALL_ROOT}/runtime/electron --enable-sandbox" 2>/dev/null |
      awk 'BEGIN { first=1 } { if (!first) printf ","; printf "%s", $1; first=0 }' || true
  )"
  restart_required=false
  [[ -n "${restart_pids}" ]] && restart_required=true
  {
    printf 'checked_at=%s\n' "$(date --iso-8601=seconds)"
    printf 'installed_version=%s\n' "${rolled_version}"
    printf 'latest_version=%s\n' "${latest_version}"
    printf 'restart_required=%s\n' "${restart_required}"
    printf 'restart_pids=%s\n' "${restart_pids}"
  } >"${STATUS_FILE}"
  log "Rolled back to Codex Desktop ${rolled_version}"
  log "Restart the desktop app to load the rolled-back build."
}

require_commands curl python3 sed head tail flock pgrep awk
mkdir -p "${INSTALL_ROOT}"
exec 9>"${INSTALL_ROOT}/update.lock"
flock -n 9 || die "Another Codex Desktop update is already running"

if [[ "${mode}" == rollback ]]; then
  require_commands desktop-file-validate update-desktop-database install
  rollback_package
  exit 0
fi

feed_file="$(mktemp "${INSTALL_ROOT}/.appcast.XXXXXX")"
cleanup_feed() {
  rm -f "${feed_file}"
}
trap cleanup_feed EXIT

log "Checking OpenAI release feed"
curl --fail --silent --show-error --location --retry 3 --max-time 60 \
  --output "${feed_file}" "${APPCAST_URL}"

mapfile -t release < <(python3 - "${feed_file}" <<'PY'
import sys
import urllib.parse
import xml.etree.ElementTree as ET

root = ET.parse(sys.argv[1]).getroot()
item = root.find("./channel/item")
if item is None:
    raise SystemExit("No release found in appcast")
version = (item.findtext("title") or "").strip()
enclosure = item.find("enclosure")
if not version or enclosure is None:
    raise SystemExit("Incomplete release in appcast")
url = enclosure.attrib.get("url", "")
length = enclosure.attrib.get("length", "")
parsed = urllib.parse.urlparse(url)
expected_name = f"ChatGPT-darwin-arm64-{version}.zip"
if parsed.scheme != "https" or parsed.netloc != "persistent.oaistatic.com":
    raise SystemExit(f"Refusing unexpected release host: {url}")
if not parsed.path.endswith("/codex-app-prod/" + expected_name):
    raise SystemExit(f"Refusing unexpected release path: {url}")
if not length.isdigit():
    raise SystemExit("Release length is missing")
print(version)
print(url)
print(length)
PY
)

((${#release[@]} == 3)) || die "Could not parse the OpenAI release feed"
latest_version="${release[0]}"
latest_url="${release[1]}"
latest_length="${release[2]}"
installed_version="$(package_version "${PACKAGE_DIR}" 2>/dev/null || printf '0')"
previous_restart_required="$(sed -n 's/^restart_required=//p' "${STATUS_FILE}" 2>/dev/null | tail -n1 || true)"
previous_restart_pids="$(sed -n 's/^restart_pids=//p' "${STATUS_FILE}" 2>/dev/null | tail -n1 || true)"

{
  printf 'checked_at=%s\n' "$(date --iso-8601=seconds)"
  printf 'installed_version=%s\n' "${installed_version}"
  printf 'latest_version=%s\n' "${latest_version}"
  printf 'release_url=%s\n' "${latest_url}"
  printf 'restart_required=%s\n' "${previous_restart_required:-false}"
  printf 'restart_pids=%s\n' "${previous_restart_pids}"
} >"${STATUS_FILE}"

if [[ "${latest_version}" == "${installed_version}" && "${force}" == false ]]; then
  log "Codex Desktop ${installed_version} is current"
  exit 0
fi

if [[ "${force}" == false ]] && ! is_newer "${installed_version}" "${latest_version}"; then
  log "Installed version ${installed_version} is newer than feed version ${latest_version}; no downgrade performed"
  exit 0
fi

log "Update available: ${installed_version} -> ${latest_version}"
if [[ "${notify}" == true ]] && command -v notify-send >/dev/null 2>&1; then
  notify-send --app-name='Codex Desktop' \
    'Codex Desktop update available' \
    "${installed_version} -> ${latest_version}. Run codex-desktop-update to install it."
fi

if [[ "${mode}" == check ]]; then
  exit 0
fi

require_commands makepkg bsdtar npx node npm sha256sum awk find install desktop-file-validate update-desktop-database pgrep
[[ -f "${SOURCE_DIR}/PKGBUILD" ]] || die "Missing ARM64 build recipe at ${SOURCE_DIR}/PKGBUILD"

archive="${SOURCE_DIR}/ChatGPT-${latest_version}.zip"
archive_part="${archive}.part"

if [[ -f "${archive}" ]] && [[ "$(stat -c '%s' "${archive}")" == "${latest_length}" ]]; then
  log "Using cached $(basename "${archive}")"
else
  if [[ -f "${archive}" ]]; then
    rm -f "${archive}"
  fi
  if [[ -f "${archive_part}" ]] && (( $(stat -c '%s' "${archive_part}") > latest_length )); then
    rm -f "${archive_part}"
  fi
  log "Downloading $(basename "${archive}")"
  curl --fail --show-error --location --retry 3 --continue-at - \
    --output "${archive_part}" "${latest_url}"
  [[ "$(stat -c '%s' "${archive_part}")" == "${latest_length}" ]] || \
    die "Downloaded archive length does not match the appcast"
  mv "${archive_part}" "${archive}"
fi

probe_dir="${SOURCE_DIR}/.update-probe"
rm -rf "${probe_dir}"
mkdir -p "${probe_dir}/bs3" "${probe_dir}/npty"
asar_member="$(bsdtar -tf "${archive}" | awk '/\/Contents\/Resources\/app\.asar$/ && $0 !~ /^__MACOSX\// {print; exit}')"
[[ -n "${asar_member}" ]] || die "Could not locate app.asar in $(basename "${archive}")"
bsdtar -xf "${archive}" -C "${probe_dir}" "${asar_member}"
asar_file="${probe_dir}/${asar_member}"

(
  cd "${probe_dir}/bs3"
  npx --yes asar extract-file "${asar_file}" node_modules/better-sqlite3/package.json >/dev/null
)
(
  cd "${probe_dir}/npty"
  npx --yes asar extract-file "${asar_file}" node_modules/node-pty/package.json >/dev/null
)

better_sqlite3_version="$(node -p "require('${probe_dir}/bs3/package.json').version")"
node_pty_version="$(node -p "require('${probe_dir}/npty/package.json').version")"
rm -rf "${probe_dir}"
log "Native modules: better-sqlite3 ${better_sqlite3_version}, node-pty ${node_pty_version}"

better_sqlite3_tgz="${SOURCE_DIR}/better-sqlite3.tgz"
node_pty_tgz="${SOURCE_DIR}/node-pty.tgz"
curl --fail --silent --show-error --location --retry 3 \
  --output "${better_sqlite3_tgz}.part" \
  "https://registry.npmjs.org/better-sqlite3/-/better-sqlite3-${better_sqlite3_version}.tgz"
mv "${better_sqlite3_tgz}.part" "${better_sqlite3_tgz}"
curl --fail --silent --show-error --location --retry 3 \
  --output "${node_pty_tgz}.part" \
  "https://registry.npmjs.org/node-pty/-/node-pty-${node_pty_version}.tgz"
mv "${node_pty_tgz}.part" "${node_pty_tgz}"

hash_of() {
  sha256sum "$1" | awk '{print $1}'
}

app_hash="$(hash_of "${archive}")"
bs3_hash="$(hash_of "${better_sqlite3_tgz}")"
npty_hash="$(hash_of "${node_pty_tgz}")"
launcher_hash="$(hash_of "${SOURCE_DIR}/codex-desktop.sh")"
desktop_hash="$(hash_of "${SOURCE_DIR}/Codex.desktop")"
open_targets_hash="$(hash_of "${SOURCE_DIR}/patch-linux-open-targets.mjs")"
opaque_bg_hash="$(hash_of "${SOURCE_DIR}/patch-linux-opaque-bg.mjs")"
browser_persistence_hash="$(hash_of "${SOURCE_DIR}/patch-linux-browser-persistence.mjs")"
terminal_font_hash="$(hash_of "${SOURCE_DIR}/patch-linux-terminal-font.mjs")"
dictation_hash="$(hash_of "${SOURCE_DIR}/patch-linux-dictation.mjs")"
icon_hash="$(hash_of "${SOURCE_DIR}/openai-codex-desktop.png")"

python3 - \
  "${SOURCE_DIR}/PKGBUILD" \
  "${latest_version}" \
  "${better_sqlite3_version}" \
  "${node_pty_version}" \
  "${app_hash}" "${bs3_hash}" "${npty_hash}" "${launcher_hash}" \
  "${desktop_hash}" "${open_targets_hash}" "${opaque_bg_hash}" \
  "${browser_persistence_hash}" "${terminal_font_hash}" \
  "${dictation_hash}" "${icon_hash}" <<'PY'
import pathlib
import re
import sys

(
    path,
    version,
    bs3_version,
    npty_version,
    *hashes,
) = sys.argv[1:]
pkgbuild = pathlib.Path(path)
text = pkgbuild.read_text()
text = re.sub(r"(?m)^pkgver=.*$", f"pkgver={version}", text, count=1)
text = re.sub(
    r"(?m)^_better_sqlite3_ver=.*$",
    f"_better_sqlite3_ver={bs3_version}",
    text,
    count=1,
)
text = re.sub(
    r"(?m)^_node_pty_ver=.*$",
    f"_node_pty_ver={npty_version}",
    text,
    count=1,
)
checksum_block = "sha256sums=(" + "\n".join(
    ("'" if index == 0 else "            '") + value + "'"
    for index, value in enumerate(hashes)
) + ")"
text, count = re.subn(
    r"sha256sums=\(.*?\)(?=\n\nprepare\(\))",
    checksum_block,
    text,
    count=1,
    flags=re.S,
)
if count != 1:
    raise SystemExit("Could not update PKGBUILD checksum block")
pkgbuild.write_text(text)
PY

(
  cd "${SOURCE_DIR}"
  makepkg --printsrcinfo >.SRCINFO
)

package_path="$(cd "${SOURCE_DIR}" && makepkg --packagelist | tail -n1)"
package_archive_version=""
if [[ -f "${package_path}" ]]; then
  package_archive_version="$(
    bsdtar -x -O -f "${package_path}" .PKGINFO 2>/dev/null |
      sed -n 's/^pkgver = //p' |
      head -n1
  )"
  package_archive_version="${package_archive_version%-*}"
fi

if [[ "${package_archive_version}" == "${latest_version}" ]]; then
  log "Using completed package $(basename "${package_path}")"
else
  log "Building Codex Desktop ${latest_version} for aarch64"
  (
    cd "${SOURCE_DIR}"
    export MAKEFLAGS="${MAKEFLAGS:--j2}"
    makepkg --nodeps --force --noconfirm --cleanbuild
  )
  package_path="$(cd "${SOURCE_DIR}" && makepkg --packagelist | tail -n1)"
fi

[[ -f "${package_path}" ]] || die "Build completed without the expected package artifact"

stage_dir="$(mktemp -d "${INSTALL_ROOT}/.package.new.XXXXXX")"
bsdtar -xf "${package_path}" -C "${stage_dir}"
staged_version="$(package_version "${stage_dir}")"
[[ "${staged_version}" == "${latest_version}" ]] || die "Staged package version is ${staged_version}, expected ${latest_version}"
[[ -s "${stage_dir}/usr/lib/openai-codex-desktop/resources/app.asar" ]] || die "Staged app.asar is missing"
[[ -x "${stage_dir}/usr/bin/codex-desktop" ]] || die "Staged launcher is missing"

rm -rf "${BACKUP_DIR}"
if [[ -d "${PACKAGE_DIR}" ]]; then
  mv "${PACKAGE_DIR}" "${BACKUP_DIR}"
fi

if ! mv "${stage_dir}" "${PACKAGE_DIR}"; then
  [[ -d "${BACKUP_DIR}" ]] && mv "${BACKUP_DIR}" "${PACKAGE_DIR}"
  die "Could not activate the staged package"
fi

if ! refresh_user_assets "${PACKAGE_DIR}"; then
  rm -rf "${PACKAGE_DIR}"
  [[ -d "${BACKUP_DIR}" ]] && mv "${BACKUP_DIR}" "${PACKAGE_DIR}"
  refresh_user_assets "${PACKAGE_DIR}" || true
  die "Post-install validation failed; restored the previous package"
fi

for old_archive in "${SOURCE_DIR}"/ChatGPT-*.zip; do
  [[ -e "${old_archive}" ]] || continue
  [[ "${old_archive}" == "${archive}" ]] || rm -f "${old_archive}"
done
for old_package in "${SOURCE_DIR}"/openai-codex-desktop-*.pkg.tar.*; do
  [[ -e "${old_package}" ]] || continue
  [[ "${old_package}" == "${package_path}" ]] || rm -f "${old_package}"
done

restart_pids="$(
  pgrep -f "^${INSTALL_ROOT}/runtime/electron --enable-sandbox" 2>/dev/null |
    awk 'BEGIN { first=1 } { if (!first) printf ","; printf "%s", $1; first=0 }' || true
)"
restart_required=false
[[ -n "${restart_pids}" ]] && restart_required=true

{
  printf 'checked_at=%s\n' "$(date --iso-8601=seconds)"
  printf 'installed_version=%s\n' "${latest_version}"
  printf 'latest_version=%s\n' "${latest_version}"
  printf 'release_url=%s\n' "${latest_url}"
  printf 'package_path=%s\n' "${package_path}"
  printf 'restart_required=%s\n' "${restart_required}"
  printf 'restart_pids=%s\n' "${restart_pids}"
} >"${STATUS_FILE}"

log "Installed Codex Desktop ${latest_version}"
if [[ "${restart_required}" == true ]]; then
  log "Codex Desktop is currently running; restart it when convenient to load the new build."
fi
log "Rollback is available with: codex-desktop-update --rollback"
