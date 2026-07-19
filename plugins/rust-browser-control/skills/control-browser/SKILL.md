---
name: control-browser
description: Control the visible browser inside the current Codex task's side pane through the local Rust Browser Control MCP server. Use when the user wants shared, observable cursor clicks and scrolling in the Codex in-app browser, including signed-in browsing and verified downloads.
---

# Control Browser

Use the `rust-browser-control` MCP tools only for operations the user requested.

## Workflow

1. Call `browser_status`. If there is no matching in-app browser socket, ask the user to open the Browser side pane for this Codex task, then retry.
2. Call `launch_browser` to claim or create a side-pane tab. Pass an initial URL only when the user requested navigation.
3. Call `list_tabs` when more than one side-pane tab may be open.
4. Call `snapshot` before interactions. Prefer its CSS selectors and coordinates over broad text matches.
5. Use `move_cursor`, `click`, `scroll`, `type_text`, `select_option`, and `set_checked` for observable interaction. These tools route pointer movement through the app so the user sees the Codex cursor. Use `selector_index` when a page repeats the same selector for several select fields.
6. For an inner scroll area, pass its selector to `scroll`, or pass a point inside it. Positive `scroll_y` scrolls down.
7. Use `click_and_wait_for_download` for downloads so completion is verified on disk.
8. Use `screenshot` when visual inspection is needed.

## Safety

- The server controls only the current Codex task's in-app browser and uses that pane's existing signed-in state.
- Never fall back to a separate Chrome, Chromium, Playwright, or WebDriver window when this skill is selected. Report a missing side-pane socket instead.
- Navigation is limited to the configured domain allowlist. The default `*` permits all HTTP(S) hosts; set `RUST_BROWSER_ALLOWED_HOSTS` to a comma-separated list for a tighter policy.
- Treat purchases, publishing, account/security changes, and destructive actions as confirmation-sensitive.
- Never enter or expose passwords, one-time codes, payment data, or unrelated personal data unless the user explicitly supplies and authorizes that exact action.
- Keep downloads in the configured download directory and report the verified absolute file path.

## Sign-in

The user and Codex share the same visible side-pane tab. If a site is signed out, ask the user to complete sign-in there, then resume after `snapshot` confirms the authenticated page. Never request or transfer cookies from another browser.
