# Rust Browser Control

A Codex plugin for Linux that controls the visible browser inside the current
task's side pane. Its local Rust MCP server connects directly to Codex's
`browser-use` Unix socket and sends pointer input plus Chrome DevTools Protocol
commands through the app's own browser backend.

The user and Codex share one visible tab, cursor, and login state. The plugin
does not launch a separate Chrome profile, copy cookies, or collect credentials.
If a site needs sign-in, the user signs in directly in the side pane and Codex
continues afterward.

## Capabilities

- Attach to the current task's in-app Browser side pane.
- List, claim, create, and navigate tabs.
- Read compact page snapshots with reusable CSS selectors.
- Move the visible Codex cursor, click, scroll, type, select options, and set
  checkbox or radio state.
- Capture screenshots and verify completed downloads on disk.
- Restrict tool-requested navigation to a configurable host allowlist.

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
for the current Linux architecture.

## Safe sign-in flow

1. Open the Browser side pane in the Codex task.
2. Ask Codex to attach or navigate to the site.
3. If the site is signed out, complete the login yourself in the shared pane.
4. Tell Codex to continue. Passwords and one-time codes should stay under user
   control.

## License

MIT
