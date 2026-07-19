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
4. Call `snapshot` before interactions. Prefer its CSS selectors and coordinates over broad text matches. Use `element_info` for bounded locator counts, attributes, state, or geometry, and `evaluate_readonly` only for a focused page read that those tools cannot answer.
5. Use `move_cursor`, `click`, `double_click`, `drag`, `scroll`, `type_text`, `keypress`, `select_option`, and `set_checked` for observable interaction. Pointer tools route movement through the app so the user sees the Codex cursor. Use `selector_index` when a page repeats the same selector for several select fields.
6. For an inner scroll area, pass its selector to `scroll`, or pass a point inside it. Positive `scroll_y` scrolls down. Use `wait_for` or `wait_for_page` for a concrete post-action state rather than a fixed delay.
7. Use `upload_files` only for absolute paths the user authorized. Use `click_and_wait_for_download` for downloads so completion is verified on disk.
8. Use `screenshot` for viewport, full-page, or element captures. Use `export_page` for HTML/text/Markdown/PDF and `page_assets` for rendered asset inventory or bundles.
9. Use `console_logs` with `start` before reproducing a page error. For JavaScript dialogs, call `handle_dialog` with the trigger selector/text so the visible click and dialog response remain on one socket connection. Use `browser_viewport` only for requested responsive testing and reset temporary overrides afterward.
10. Use `cdp_command` or `cdp_events` only for a developer task that requires them. They are unavailable unless Codex Desktop was explicitly launched with `RUST_BROWSER_ENABLE_RAW_CDP=1`.

## Safety

- The server controls only the current Codex task's in-app browser and uses that pane's existing signed-in state.
- Never fall back to a separate Chrome, Chromium, Playwright, or WebDriver window when this skill is selected. Report a missing side-pane socket instead.
- Navigation is limited to the configured domain allowlist. The default `*` permits all HTTP(S) hosts; set `RUST_BROWSER_ALLOWED_HOSTS` to a comma-separated list for a tighter policy.
- Treat purchases, publishing, account/security changes, and destructive actions as confirmation-sensitive.
- Never enter or expose passwords, one-time codes, payment data, or unrelated personal data unless the user explicitly supplies and authorizes that exact action.
- Keep downloads in the configured download directory and report the verified absolute file path.
- Raw CDP is a developer-mode escape hatch, not a way around navigation, download, authentication, or confirmation policy.

## Sign-in

The user and Codex share the same visible side-pane tab. If a site is signed out, ask the user to complete sign-in there, then resume after `snapshot` confirms the authenticated page. Never request or transfer cookies from another browser.
