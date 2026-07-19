#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
plugin_root="${repo_root}/plugins/rust-browser-control"
target_root="${XDG_CACHE_HOME:-${HOME}/.cache}/rust-browser-control/target"

for command_name in codex cargo python3; do
  command -v "${command_name}" >/dev/null 2>&1 || {
    printf 'Missing required command: %s\n' "${command_name}" >&2
    exit 1
  }
done

[[ -f "${repo_root}/.agents/plugins/marketplace.json" ]] || {
  printf 'Marketplace manifest is missing from %s\n' "${repo_root}" >&2
  exit 1
}

[[ -f "${plugin_root}/Cargo.lock" ]] || {
  printf 'Plugin source is incomplete at %s\n' "${plugin_root}" >&2
  exit 1
}

codex plugin marketplace add "${repo_root}" --json
codex plugin add rust-browser-control@codex-desktop-linux --json

CARGO_TARGET_DIR="${target_root}" \
  cargo build --release --locked --manifest-path "${plugin_root}/Cargo.toml"

printf '\nRust Browser Control is installed. Start a new Codex Desktop thread, open the Browser side pane, and ask Codex to attach.\n'
