# Rust Browser Control

A Codex plugin for Linux that controls the visible browser inside the current
task's side pane. Its local Rust MCP server connects directly to Codex's
`browser-use` Unix socket and sends pointer input plus Chrome DevTools Protocol
commands through the app's own browser backend.

The user and Codex share one visible tab, cursor, and login state. The plugin
does not launch a separate Chrome profile, copy cookies, or collect credentials.
If a site needs sign-in, the user signs in directly in the side pane and Codex
continues afterward.

This is deliberately side-pane-only. It does not use the Codex Chrome plugin,
a Chrome/Chromium extension, WebDriver, or a separate Playwright browser.
Extension-owned sockets are ignored because the server accepts only Codex
in-app-browser (`iab`) routes matching the current task ID.

## Capabilities

- Attach to the current task's in-app Browser side pane.
- Name the session; list, create, navigate, reload, and close tabs.
- Read compact snapshots; inspect locator counts, text, attributes, state, and
  geometry; run side-effect-blocked page reads.
- Move the visible Codex cursor, click, double-click, drag, scroll, type, press
  key combinations, select options, and set checkbox or radio state.
- Wait on elements, URLs, and load state; handle JavaScript dialogs and the
  side-pane clipboard.
- Capture viewport, full-page, and element screenshots; upload explicit local
  files; verify completed downloads on disk.
- Capture page console errors, export HTML/text/Markdown/PDF, and inventory or
  bundle rendered page assets.
- Show/hide the browser and set/reset responsive viewport overrides.
- Offer raw CDP commands and events only behind an explicit developer flag.
- Restrict tool-requested navigation to a configurable host allowlist.

See [the audited feature-parity matrix](../../docs/browser-feature-parity.md)
for the mapping to Codex Desktop's bundled Browser API and the deliberate
platform-owned boundaries.

## Requirements

- Linux with a Codex Desktop build that exposes the in-app Browser side pane
  and `/tmp/codex-browser-use/*.sock`.
- Codex CLI with plugin and MCP support.
- Rust and Cargo for the first local build.

## Configuration

The MCP launcher passes these optional environment variables through:

| Variable | Default | Purpose |
| --- | --- | --- |
| `RUST_BROWSER_ALLOWED_HOSTS` | `*` | Comma-separated domains; `*` allows every HTTP(S) host. |
| `RUST_BROWSER_DOWNLOAD_DIR` | `$HOME/Downloads/RustBrowserControl` | Verified download destination. |
| `RUST_BROWSER_SOCKET_DIR` | `/tmp/codex-browser-use` | Codex browser socket directory. |
| `RUST_BROWSER_SESSIONS_ROOT` | `$HOME/.codex/sessions` | Rollout metadata used to resolve the current turn. |
| `RUST_BROWSER_SESSION_ID` | `CODEX_THREAD_ID` | Explicit task/session override. |
| `RUST_BROWSER_TURN_ID` | `CODEX_TURN_ID` or latest rollout turn | Explicit turn override. |
| `RUST_BROWSER_ENABLE_RAW_CDP` | unset/false | Enable guarded developer-mode `cdp_command` and `cdp_events` tools. |

Use a narrow allowlist on machines where Codex should only control selected
sites, for example:

```bash
export RUST_BROWSER_ALLOWED_HOSTS="example.com,openai.com"
```

## Build and test

```bash
cargo test --locked
./scripts/build-release
```

Generated binaries live under the XDG cache directory rather than in the
plugin source tree. On first use, the MCP launcher builds the release binary
for the current Linux architecture. A source digest and file lock prevent
copied timestamps or concurrent tool calls from triggering duplicate builds.

## Install and update across devices

Run the repository installer once. It configures the GitHub marketplace,
builds for the current architecture, removes an older enabled Personal copy,
and installs two stable commands:

```bash
./install-plugin.sh
codex-browser-control-update
codex-browser-control-doctor
```

The updater refreshes the Git marketplace from `main`, reinstalls the plugin,
and prebuilds it before a new Codex thread can call it. The doctor prints the
plugin version and a platform-independent digest of the full plugin tree;
matching digests mean the ARM64 and x86_64 devices are running the same plugin.

## Safe sign-in flow

1. Open the Browser side pane in the Codex task.
2. Ask Codex to attach or navigate to the site.
3. If the site is signed out, complete the login yourself in the shared pane.
4. Tell Codex to continue. Passwords and one-time codes should stay under user
   control.

No Chrome extension or password-manager bridge is needed for this flow.

## License

MIT
