# Linux device parity

Verified on 2026-07-19 with one Arch Linux x86_64 workstation and one Arch
Linux ARM64 device.

## Compatibility contract

The following values should match on every device:

- Codex Desktop app build, or a build proven to expose the same in-app-browser
  (`iab`) socket protocol;
- plugin ID `rust-browser-control@codex-desktop-linux`;
- plugin manifest version;
- source digest reported by `codex-browser-control-doctor`;
- Git marketplace source `ASoldo/codex-desktop-linux` on `main`;
- one enabled Rust Browser Control installation, with legacy Personal copies
  disabled.

The following values may legitimately differ:

- CPU architecture and native Rust binary (`x86_64` versus `aarch64`);
- Electron runtime package and patch version;
- desktop packaging/update method;
- local side-pane login cookies, downloads, screenshots, tabs, and task IDs.

Browser login state is deliberately local to each Codex Desktop profile. The
plugin shares the current device's visible side-pane session with Codex; it
does not copy cookies or credentials between machines.

## Verified baseline

| Component | x86_64 workstation | ARM64 device |
| --- | --- | --- |
| Codex Desktop | `26.715.31925` | `26.715.31925` |
| Codex CLI | `0.144.6` | `0.144.6` |
| Electron | `42.6.1` system package | `42.2.0` bundled runtime |
| Plugin before migration | Personal `0.1.0` | Personal `0.2.0` |
| Plugin after feature-parity update | Git marketplace `0.3.0` | Git marketplace `0.3.0` |
| Browser transport | Codex side-pane `iab` socket | Codex side-pane `iab` socket |

The original `0.1.0` plugin defaulted to Mixamo and a small Adobe host
allowlist. Version `0.2.1` made the controller site-neutral and safer. Version
`0.3.0` aligns it with the practical bundled side-pane Browser feature set,
including tab lifecycle, complete pointer/keyboard gestures, bounded locator
inspection, waits, screenshots, clipboard, dialogs, uploads/downloads, logs,
exports, page assets, viewport/visibility capabilities, and opt-in raw CDP.

## Update and switch workflow

Run this on each device before switching work:

```bash
codex-browser-control-update
codex-browser-control-doctor
```

Confirm that the plugin version and full plugin source digest match. Then start
a new Codex thread, open its Browser side pane, and ask Codex to attach. If the
site needs authentication on that device, sign in yourself in the visible pane
and tell Codex to continue.

The update command performs the operations in this order:

1. add or refresh the GitHub marketplace;
2. reinstall the GitHub-backed plugin;
3. build the native Rust binary under the user's XDG cache;
4. install the stable updater and doctor commands;
5. disable an older Personal installation only after the build succeeds.

The launcher hashes Rust sources and serializes builds with a file lock. This
prevents copied timestamps or simultaneous MCP starts from triggering duplicate
release links on small ARM systems.

## Chrome independence

No Chrome or Chromium extension setup is part of this workflow. The plugin
rejects non-`iab` browser routes and requires the current Codex task ID, so an
unrelated extension socket cannot be selected. An official Codex Chrome plugin
may remain installed for other workflows, but Rust Browser Control does not
call it and does not need it.

## After a Codex Desktop update

Run the doctor, start a new test thread, open the side pane, and perform a
non-destructive smoke test such as opening `https://example.com`, taking a
snapshot, and moving the visible cursor without clicking. A missing or changed
socket protocol should fail closed with a request to open the side pane rather
than falling back to another browser.
