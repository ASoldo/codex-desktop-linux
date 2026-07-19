# Browser feature parity

This matrix was audited against the bundled Codex Browser API in Codex Desktop
build `26.715.31925`. Rust Browser Control `0.3.0` targets the same practical
feature set for the in-app side pane on Linux. It is an MCP API, not a byte-for-
byte reimplementation of the bundled JavaScript client, so tool names differ.

## Side-pane feature map

| Bundled Browser surface | Rust Browser Control | Coverage |
| --- | --- | --- |
| Browser discovery, selected browser, status | `browser_status`, `launch_browser` | Current-task `iab` socket only; extension/CDP browser routes are rejected. |
| Session naming | `name_session` | Native `nameSession` socket request. |
| Show, hide, query visibility | `browser_visibility` | Native advertised `visibility` capability. |
| Set and reset viewport | `browser_viewport` | Native advertised `viewport` capability. |
| List, select, create, close tabs | `list_tabs`, `open_tab`, `tab_action` | The active tab is reported by `list_tabs`; target IDs select another tab. |
| Navigate, back, forward, reload | `navigate`, `tab_action` | HTTP(S) navigation remains subject to the host allowlist. |
| DOM snapshot | `snapshot` | Bounded visible text plus up to 300 reusable interactive selectors. |
| CSS, text, test-id, role/label-derived locators | `snapshot`, `element_info`, interaction tools | Snapshot-derived selectors and bounded text matching replace the bundled locator object syntax. |
| Locator count, text, attributes, state, bounds | `element_info` | Returns up to 100 matches with password values masked. |
| Read-only page evaluation | `evaluate_readonly` | Uses V8 `throwOnSideEffect`; arbitrary mutation is rejected. |
| Click, double-click, move, scroll | `click`, `double_click`, `move_cursor`, `scroll` | Pointer movement is routed through the visible Codex cursor. |
| Drag | `drag` | Visible interpolated cursor path between selectors or coordinates. |
| Type, fill, keypress | `type_text`, `keypress` | Supports clearing, submission, and modifier combinations such as `Ctrl+A`. |
| Select option, check, uncheck | `select_option`, `set_checked` | State is verified after interaction. |
| Wait for locator, URL, load state, timeout | `wait_for`, `wait_for_page` | Polling is bounded and capped at 60 seconds. A fixed delay can be represented by a page wait only when a concrete state is available. |
| Viewport, full-page, element screenshots | `screenshot` | PNGs are written to the configured screenshot directory. |
| Clipboard text read/write | `clipboard` | Uses the current side-pane page permission and focus context. |
| JavaScript dialog handling | `handle_dialog` | Accepts or dismisses alerts, confirms, prompts, and before-unload dialogs. Pass the trigger selector/text so click and response share one socket connection. |
| File chooser/set files | `upload_files` | Verifies explicit absolute paths and targets `input[type=file]` directly. |
| Download trigger, wait, download path | `click_and_wait_for_download`, `browser_status` | Source URLs are allowlist-checked and completion is verified on disk. |
| Console logs and runtime errors | `console_logs` | Start before reproducing, then read or clear a bounded in-page buffer. |
| Content export | `export_page` | HTML, text, Markdown, and PDF. Google Workspace conversion is left to the app/platform. |
| Page asset inventory and bundle | `page_assets` | Implements the advertised `pageAssets` feature over CDP/page fetch; CORS or large-asset failures are reported per asset. |
| Raw CDP commands and events | `cdp_command`, `cdp_events` | Present but disabled by default. Launch Codex with `RUST_BROWSER_ENABLE_RAW_CDP=1` for explicit developer mode. |

## Deliberate platform boundaries

The following are not missing Linux controller features:

- `browserAuth` and bot-detection reporting are app-owned optional capabilities.
  This side-pane backend does not advertise either one. Users sign in directly
  in the visible pane; the plugin never reads password-manager secrets, cookies,
  or one-time codes.
- Chrome-profile history, claiming regular Chrome tabs, and Chrome extension
  setup apply to the extension surface. They are not required for the in-app
  side pane.
- Tab handoff/deliverable/finalize and background `tabs.content` are marked
  unsupported on the bundled in-app-browser surface.
- CUA media download is also marked unsupported for the in-app browser. The
  verified download tool covers ordinary page download triggers instead.
- Google Workspace conversion and secure native browser-auth elicitation depend
  on app services outside the public `iab` socket. The plugin does not emulate
  them by copying credentials or bypassing app security.

## Compatibility rule

After a Codex Desktop update, compare `getInfo` capabilities and the bundled
Browser API with this matrix. A protocol mismatch must fail closed. Do not fall
back to Chrome, WebDriver, a copied profile, or a second browser window.
