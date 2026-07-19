# Codex Desktop Linux

Linux support files for a practical Codex Desktop setup with a shared,
agent-controlled in-app browser.

This repository combines two pieces:

1. `rust-browser-control`, a portable Codex plugin that connects to the
   current task's visible Browser side pane and uses the same cursor and login
   state as the user.
2. The tested Arch Linux ARM64 desktop packaging used on a small ARM device, built
   from OpenAI's official ARM64 app archive with native modules rebuilt for
   Electron on Linux.

## Tested platforms

| Platform | Desktop path | Browser control |
| --- | --- | --- |
| Arch Linux `x86_64` | AUR `openai-codex-desktop` | Rust plugin, verified on a reference workstation |
| Arch Linux ARM64 | `desktop/archlinux-arm64` | Rust plugin built natively for ARM64 |
| Other Linux distributions | Bring an in-app-Browser-capable Codex Desktop build | Plugin source is portable but the desktop package is not yet verified |

## Install the browser plugin

From GitHub:

```bash
codex plugin marketplace add ASoldo/codex-desktop-linux
codex plugin add rust-browser-control@codex-desktop-linux
```

Or clone the repository and install the local checkout:

```bash
git clone https://github.com/ASoldo/codex-desktop-linux.git
cd codex-desktop-linux
./install-plugin.sh
```

Start a new Codex thread after installation. Plugin skills and MCP tools are
resolved when the thread starts; an already-running thread will not gain them
retroactively.

## Shared login and control flow

1. Open the Browser side pane in the Codex task.
2. Ask Codex to attach to the visible browser or navigate to a site.
3. Complete sign-in yourself in that same pane when needed.
4. Tell Codex to continue. It can inspect the authenticated page and move the
   visible cursor, click, scroll, and type without copying cookies to another
   profile.

Passwords, one-time codes, account-security changes, purchases, publishing,
and destructive actions remain user-controlled or confirmation-sensitive.

## Navigation policy

The plugin permits all HTTP(S) hosts by default. To restrict it, launch Codex
Desktop with a comma-separated allowlist:

```bash
export RUST_BROWSER_ALLOWED_HOSTS="example.com,openai.com"
```

For desktop launchers, set the variable through your session environment (for
example `~/.config/environment.d/`) so the Codex app and its MCP server inherit
it.

## Arch Linux ARM64 desktop

The ARM64 recipe is intentionally separate from the portable plugin. It:

- reads OpenAI's official Codex app feed;
- verifies the official archive host, path, length, and checksums;
- extracts the official ARM64 app;
- rebuilds `better-sqlite3` and `node-pty` for Electron/Linux ARM64;
- restores the bundled plugins and skills beside `app.asar`;
- applies guarded Linux patches that fail closed when the upstream bundle
  shape changes;
- installs atomically and retains one rollback package.

On an Arch Linux ARM64 machine with the listed build dependencies:

```bash
./desktop/archlinux-arm64/install.sh
```

The installer downloads Electron `42.2.0` from the official Electron GitHub
release, verifies its SHA-256 digest, stages Codex Desktop in the user's home
directory, and installs the update tray. It does not replace a running Codex
window.

The ARM64 build is CPU-intensive and may take 15-30 minutes on a small device.
Use `codex-desktop-update --rollback` to swap back to the retained prior app
package.

## Arch Linux x86_64 desktop

Install the AUR package and then install this repository's plugin:

```bash
yay -S openai-codex-desktop
codex plugin marketplace add ASoldo/codex-desktop-linux
codex plugin add rust-browser-control@codex-desktop-linux
```

The custom ARM64 repackaging is not needed on `x86_64`.

## Development checks

```bash
cargo fmt --check --manifest-path plugins/rust-browser-control/Cargo.toml
cargo test --locked --manifest-path plugins/rust-browser-control/Cargo.toml
python3 ~/.codex/skills/.system/plugin-creator/scripts/validate_plugin.py \
  plugins/rust-browser-control
```

## Repository layout

```text
.agents/plugins/marketplace.json       Codex marketplace manifest
plugins/rust-browser-control/          Portable Rust MCP plugin
desktop/archlinux-arm64/               ARM64 package, updater, and Linux patches
install-plugin.sh                      Local-checkout plugin installer
```

## License

MIT for the Rust plugin and repository scripts. The ARM64 recipe retains the
license headers and metadata from its source files. Large OpenAI application
archives and Electron runtimes are downloaded from their official release
locations rather than stored in this repository.
