#!/usr/bin/env python3
"""Codex Desktop update tray and visual updater for the local ARM64 port."""

from __future__ import annotations

import html
import os
import re
import signal
import subprocess
import threading
import time
import warnings
from pathlib import Path

import cairo
import gi

warnings.filterwarnings("ignore", category=DeprecationWarning)

gi.require_version("Gtk", "3.0")
gi.require_version("Gdk", "3.0")
gi.require_version("GdkPixbuf", "2.0")
from gi.repository import Gdk, GdkPixbuf, GLib, Gtk  # noqa: E402


HOME = Path.home()
INSTALL_ROOT = HOME / ".local/share/openai-codex-desktop"
PACKAGE_DIR = INSTALL_ROOT / "package"
STATUS_FILE = INSTALL_ROOT / "update-status"
UPDATER = HOME / ".local/bin/codex-desktop-update"
CODEX_APP = HOME / ".local/bin/codex-app"
APP_ICON = HOME / ".local/share/icons/hicolor/512x512/apps/openai-codex-desktop.png"
RUNTIME_ELECTRON = INSTALL_ROOT / "runtime/electron"
APP_ASAR = PACKAGE_DIR / "usr/lib/openai-codex-desktop/resources/app.asar"

CHECK_RESPONSE = 101
INSTALL_RESPONSE = 102
RESTART_RESPONSE = 103
ANSI_RE = re.compile(r"\x1b\[[0-9;]*m")


def package_version() -> str:
    metadata = PACKAGE_DIR / ".PKGINFO"
    if not metadata.exists():
        return "unknown"
    for line in metadata.read_text(errors="replace").splitlines():
        if line.startswith("pkgver = "):
            return line.removeprefix("pkgver = ").rsplit("-", 1)[0]
    return "unknown"


def version_key(value: str) -> tuple[int, ...]:
    return tuple(int(part) for part in re.findall(r"\d+", value))


def load_status() -> dict[str, str]:
    values: dict[str, str] = {}
    if STATUS_FILE.exists():
        for line in STATUS_FILE.read_text(errors="replace").splitlines():
            key, separator, value = line.partition("=")
            if separator:
                values[key] = value
    values.setdefault("installed_version", package_version())
    values.setdefault("latest_version", values["installed_version"])
    values.setdefault("restart_required", "false")
    values.setdefault("restart_pids", "")
    if values["restart_required"] == "true":
        tracked = [pid for pid in values["restart_pids"].split(",") if pid]
        if tracked and not any(codex_process_alive(pid) for pid in tracked):
            values["restart_required"] = "false"
            values["restart_pids"] = ""
            write_status(values)
    return values


def write_status(values: dict[str, str]) -> None:
    STATUS_FILE.parent.mkdir(parents=True, exist_ok=True)
    temporary = STATUS_FILE.with_name(STATUS_FILE.name + ".tray")
    temporary.write_text("".join(f"{key}={value}\n" for key, value in values.items()))
    temporary.replace(STATUS_FILE)


def codex_process_alive(pid_text: str) -> bool:
    try:
        command_line = (
            (Path("/proc") / pid_text / "cmdline")
            .read_bytes()
            .replace(b"\0", b" ")
            .decode(errors="replace")
            .strip()
        )
    except (FileNotFoundError, PermissionError, ProcessLookupError, ValueError):
        return False
    return bool(
        command_line.startswith(str(RUNTIME_ELECTRON))
        and "--enable-sandbox" in command_line
        and "--type=" not in command_line
        and str(APP_ASAR) in command_line
    )


def release_available(status: dict[str, str]) -> bool:
    try:
        return version_key(status["latest_version"]) > version_key(status["installed_version"])
    except (KeyError, ValueError):
        return False


def tray_pixbuf(state: str, size: int = 32) -> GdkPixbuf.Pixbuf:
    base = GdkPixbuf.Pixbuf.new_from_file_at_scale(str(APP_ICON), size, size, True)
    surface = cairo.ImageSurface(cairo.FORMAT_ARGB32, size, size)
    context = cairo.Context(surface)
    Gdk.cairo_set_source_pixbuf(context, base, 0, 0)
    context.paint()

    radius = size * 0.29
    center_x = size - radius - 1
    center_y = size - radius - 1
    colors = {
        "current": (0.10, 0.72, 0.32),
        "available": (0.13, 0.48, 0.95),
        "checking": (0.13, 0.48, 0.95),
        "updating": (0.13, 0.48, 0.95),
        "error": (0.92, 0.48, 0.12),
    }
    red, green, blue = colors.get(state, colors["checking"])
    context.arc(center_x, center_y, radius, 0, 2 * 3.141592653589793)
    context.set_source_rgb(red, green, blue)
    context.fill_preserve()
    context.set_source_rgba(1, 1, 1, 0.95)
    context.set_line_width(max(1.2, size * 0.055))
    context.stroke()

    context.set_source_rgb(1, 1, 1)
    context.set_line_width(max(1.6, size * 0.075))
    context.set_line_cap(cairo.LINE_CAP_ROUND)
    context.set_line_join(cairo.LINE_JOIN_ROUND)

    if state == "current":
        context.move_to(center_x - radius * 0.52, center_y)
        context.line_to(center_x - radius * 0.12, center_y + radius * 0.38)
        context.line_to(center_x + radius * 0.56, center_y - radius * 0.42)
        context.stroke()
    elif state in {"available", "updating"}:
        context.move_to(center_x, center_y - radius * 0.55)
        context.line_to(center_x, center_y + radius * 0.18)
        context.move_to(center_x - radius * 0.34, center_y - radius * 0.02)
        context.line_to(center_x, center_y + radius * 0.34)
        context.line_to(center_x + radius * 0.34, center_y - radius * 0.02)
        context.move_to(center_x - radius * 0.48, center_y + radius * 0.54)
        context.line_to(center_x + radius * 0.48, center_y + radius * 0.54)
        context.stroke()
    elif state == "checking":
        context.arc(center_x, center_y, radius * 0.48, -1.4, 1.75)
        context.stroke()
    else:
        context.move_to(center_x, center_y - radius * 0.48)
        context.line_to(center_x, center_y + radius * 0.16)
        context.stroke()
        context.arc(center_x, center_y + radius * 0.52, radius * 0.07, 0, 6.3)
        context.fill()

    surface.flush()
    return Gdk.pixbuf_get_from_surface(surface, 0, 0, size, size)


class UpdateDialog:
    def __init__(self, controller: "TrayController") -> None:
        self.controller = controller
        self.updating = False
        self.pulse_source: int | None = None

        self.dialog = Gtk.Dialog(title="Codex Desktop Update")
        self.dialog.set_name("codex-update-dialog")
        self.dialog.set_default_size(560, 390)
        self.dialog.set_position(Gtk.WindowPosition.CENTER)
        self.dialog.set_resizable(True)
        self.dialog.connect("delete-event", self.on_delete)
        self.dialog.connect("response", self.on_response)

        content = self.dialog.get_content_area()
        content.set_border_width(18)
        content.set_spacing(14)

        header = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=14)
        header_pixbuf = GdkPixbuf.Pixbuf.new_from_file_at_scale(str(APP_ICON), 64, 64, True)
        self.icon = Gtk.Image.new_from_pixbuf(header_pixbuf)
        header.pack_start(self.icon, False, False, 0)

        heading = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=4)
        title = Gtk.Label()
        title.set_xalign(0)
        title.set_markup("<span size='x-large' weight='bold'>Codex Desktop</span>")
        heading.pack_start(title, False, False, 0)
        self.version_label = Gtk.Label(xalign=0)
        self.version_label.set_selectable(True)
        heading.pack_start(self.version_label, False, False, 0)
        header.pack_start(heading, True, True, 0)
        content.pack_start(header, False, False, 0)

        self.status_label = Gtk.Label(xalign=0)
        self.status_label.set_line_wrap(True)
        content.pack_start(self.status_label, False, False, 0)

        self.progress = Gtk.ProgressBar()
        self.progress.set_show_text(True)
        self.progress.set_no_show_all(True)
        content.pack_start(self.progress, False, False, 0)

        self.expander = Gtk.Expander(label="Update details")
        scroller = Gtk.ScrolledWindow()
        scroller.set_min_content_height(150)
        scroller.set_policy(Gtk.PolicyType.AUTOMATIC, Gtk.PolicyType.AUTOMATIC)
        self.log_view = Gtk.TextView()
        self.log_view.set_editable(False)
        self.log_view.set_cursor_visible(False)
        self.log_view.set_monospace(True)
        self.log_view.set_wrap_mode(Gtk.WrapMode.WORD_CHAR)
        scroller.add(self.log_view)
        self.expander.add(scroller)
        content.pack_start(self.expander, True, True, 0)

        self.check_button = self.dialog.add_button("Check now", CHECK_RESPONSE)
        self.close_button = self.dialog.add_button("Close", Gtk.ResponseType.CLOSE)
        self.install_button = self.dialog.add_button("Install update", INSTALL_RESPONSE)
        self.install_button.get_style_context().add_class("suggested-action")
        self.restart_button = self.dialog.add_button("Restart Codex", RESTART_RESPONSE)
        self.restart_button.get_style_context().add_class("suggested-action")
        self.restart_button.set_no_show_all(True)

        self.refresh()

    def on_delete(self, _dialog: Gtk.Dialog, _event: object) -> bool:
        self.dialog.hide()
        return True

    def present(self) -> None:
        self.dialog.show_all()
        self.refresh()
        if not self.restart_button.get_visible():
            self.restart_button.hide()
        if not self.progress.get_visible():
            self.progress.hide()
        self.dialog.present()

    def refresh(self) -> None:
        status = self.controller.status
        installed = status.get("installed_version", "unknown")
        latest = status.get("latest_version", installed)
        self.version_label.set_markup(
            f"Installed <b>{html.escape(installed)}</b>   ·   Latest <b>{html.escape(latest)}</b>"
        )
        if self.updating:
            return
        if status.get("restart_required") == "true":
            self.status_label.set_markup(
                "<span foreground='#19a84b' weight='bold'>Update installed.</span> "
                "Restart Codex when you are ready to load the new build."
            )
            self.install_button.set_sensitive(False)
            self.install_button.hide()
            self.restart_button.show()
        elif release_available(status):
            self.status_label.set_markup(
                "<span foreground='#2479e8' weight='bold'>An update is ready.</span> "
                "The ARM64 package will be rebuilt locally, staged, and kept with one rollback copy."
            )
            self.install_button.set_sensitive(True)
            self.install_button.show()
        else:
            self.status_label.set_markup(
                "<span foreground='#19a84b' weight='bold'>Codex Desktop is up to date.</span> "
                "The green tray badge confirms this build matches OpenAI's release feed."
            )
            self.install_button.set_sensitive(False)
            self.install_button.hide()
        self.check_button.set_sensitive(True)
        if status.get("restart_required") != "true":
            self.restart_button.hide()
        self.dialog.queue_draw()

    def on_response(self, _dialog: Gtk.Dialog, response: int) -> None:
        if response == CHECK_RESPONSE:
            self.controller.check_now(show_dialog=True)
        elif response == INSTALL_RESPONSE:
            self.start_update()
        elif response == RESTART_RESPONSE:
            self.confirm_restart()
        else:
            self.dialog.hide()

    def start_update(self) -> None:
        if self.updating:
            return
        self.updating = True
        self.controller.set_state("updating")
        self.check_button.set_sensitive(False)
        self.install_button.set_sensitive(False)
        self.close_button.set_label("Hide")
        self.progress.set_fraction(0.02)
        self.progress.set_text("Preparing update…")
        self.progress.show()
        self.expander.set_expanded(True)
        self.log_view.get_buffer().set_text("")
        self.status_label.set_markup(
            "<span foreground='#2479e8' weight='bold'>Installing update…</span> "
            "You can hide this window; the tray icon will remain active."
        )
        self.pulse_source = GLib.timeout_add(350, self.pulse)
        threading.Thread(target=self.run_update, daemon=True).start()

    def pulse(self) -> bool:
        if not self.updating:
            return False
        self.progress.pulse()
        return True

    def run_update(self) -> None:
        process = subprocess.Popen(
            [str(UPDATER)],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            bufsize=1,
            env={**os.environ, "NO_COLOR": "1"},
        )
        assert process.stdout is not None
        for raw in process.stdout:
            for fragment in raw.replace("\r", "\n").splitlines():
                line = ANSI_RE.sub("", fragment).strip()
                if line:
                    GLib.idle_add(self.append_output, line)
        return_code = process.wait()
        GLib.idle_add(self.finish_update, return_code)

    def append_output(self, line: str) -> bool:
        buffer = self.log_view.get_buffer()
        buffer.insert(buffer.get_end_iter(), line + "\n")
        mark = buffer.create_mark(None, buffer.get_end_iter(), False)
        self.log_view.scroll_to_mark(mark, 0.0, True, 0.0, 1.0)

        phase_fraction = (
            ("Downloading", 0.08, "Downloading official release…"),
            ("Native modules:", 0.28, "Inspecting native modules…"),
            ("Building Codex Desktop", 0.34, "Applying Linux patches…"),
            ("Starting prepare()", 0.42, "Preparing application…"),
            ("Building modules:", 0.55, "Compiling native modules…"),
            ("Starting package()", 0.78, "Packaging ARM64 build…"),
            ("Creating package", 0.88, "Creating install package…"),
            ("Installed Codex Desktop", 1.0, "Update installed"),
        )
        for marker, fraction, label in phase_fraction:
            if marker in line:
                self.progress.set_fraction(fraction)
                self.progress.set_text(label)
                break
        return False

    def finish_update(self, return_code: int) -> bool:
        self.updating = False
        if self.pulse_source is not None:
            GLib.source_remove(self.pulse_source)
            self.pulse_source = None
        self.close_button.set_label("Close")
        self.check_button.set_sensitive(True)
        if return_code == 0:
            self.controller.reload_status()
            self.controller.set_state("current")
            self.progress.set_fraction(1.0)
            self.progress.set_text("Update installed")
            self.status_label.set_markup(
                "<span foreground='#19a84b' weight='bold'>Update installed successfully.</span> "
                "Restart Codex when you are ready; your settings and tasks are preserved."
            )
            self.install_button.hide()
            self.restart_button.show()
            self.version_label.set_markup(
                f"Installed <b>{html.escape(self.controller.status.get('installed_version', 'unknown'))}</b>"
            )
        else:
            self.controller.set_state("error")
            self.progress.set_text("Update failed")
            self.status_label.set_markup(
                "<span foreground='#d56a12' weight='bold'>The update did not complete.</span> "
                "The previous package is still active. Expand the details for the exact error."
            )
            self.install_button.set_sensitive(True)
            self.install_button.show()
        return False

    def confirm_restart(self) -> None:
        prompt = Gtk.MessageDialog(
            transient_for=self.dialog,
            modal=True,
            message_type=Gtk.MessageType.QUESTION,
            buttons=Gtk.ButtonsType.CANCEL,
            text="Restart Codex now?",
        )
        prompt.format_secondary_text(
            "This closes the current Codex window and opens the newly installed build. "
            "Running work should be allowed to finish first."
        )
        prompt.add_button("Restart Codex", Gtk.ResponseType.OK).get_style_context().add_class(
            "suggested-action"
        )
        response = prompt.run()
        prompt.destroy()
        if response == Gtk.ResponseType.OK:
            threading.Thread(target=self.restart_codex, daemon=True).start()
            self.dialog.hide()

    def restart_codex(self) -> None:
        main_pids: list[int] = []
        for proc_dir in Path("/proc").glob("[0-9]*"):
            try:
                command_line = (
                    (proc_dir / "cmdline")
                    .read_bytes()
                    .replace(b"\0", b" ")
                    .decode(errors="replace")
                    .strip()
                )
            except (FileNotFoundError, PermissionError, ProcessLookupError):
                continue
            if not command_line.startswith(str(RUNTIME_ELECTRON)):
                continue
            if "--type=" in command_line:
                continue
            if str(APP_ASAR) in command_line:
                main_pids.append(int(proc_dir.name))
        for pid in main_pids:
            try:
                os.kill(pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
        deadline = time.monotonic() + 12
        while main_pids and time.monotonic() < deadline:
            main_pids = [pid for pid in main_pids if Path(f"/proc/{pid}").exists()]
            time.sleep(0.25)
        subprocess.Popen(
            [str(CODEX_APP)],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            start_new_session=True,
        )
        self.controller.status["restart_required"] = "false"
        self.controller.status["restart_pids"] = ""
        write_status(self.controller.status)


class TrayController:
    def __init__(self) -> None:
        self.status = load_status()
        self.state = "checking"
        self.dialog: UpdateDialog | None = None
        self.checking = False

        self.icon = Gtk.StatusIcon()
        self.icon.set_title("Codex Desktop Updates")
        self.icon.connect("activate", self.on_activate)
        self.icon.connect("popup-menu", self.on_popup_menu)
        self.icon.set_visible(True)
        self.set_state("checking")

        GLib.timeout_add_seconds(6 * 60 * 60, self.periodic_check)
        GLib.timeout_add_seconds(30, self.watch_status)
        GLib.idle_add(self.check_now)

    def set_state(self, state: str) -> None:
        self.state = state
        self.icon.set_from_pixbuf(tray_pixbuf(state))
        installed = self.status.get("installed_version", "unknown")
        latest = self.status.get("latest_version", installed)
        tooltips = {
            "current": f"Codex Desktop {installed} is up to date",
            "available": f"Codex Desktop {latest} is ready to download",
            "checking": "Checking Codex Desktop updates…",
            "updating": f"Updating Codex Desktop to {latest}…",
            "error": "Codex Desktop update check needs attention",
        }
        self.icon.set_tooltip_text(tooltips.get(state, "Codex Desktop Updates"))

    def reload_status(self) -> None:
        self.status = load_status()

    def check_now(self, show_dialog: bool = False) -> bool:
        if self.checking:
            if show_dialog:
                self.show_dialog()
            return False
        self.checking = True
        self.set_state("checking")
        if show_dialog:
            self.show_dialog()
            assert self.dialog is not None
            self.dialog.status_label.set_text("Checking OpenAI's release feed…")
            self.dialog.check_button.set_sensitive(False)
        threading.Thread(target=self.run_check, daemon=True).start()
        return False

    def run_check(self) -> None:
        result = subprocess.run(
            [str(UPDATER), "--check"],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
        )
        GLib.idle_add(self.finish_check, result.returncode, result.stdout)

    def finish_check(self, return_code: int, output: str) -> bool:
        self.checking = False
        self.reload_status()
        if return_code == 0:
            self.set_state("available" if release_available(self.status) else "current")
        else:
            self.set_state("error")
        if self.dialog is not None:
            self.dialog.check_button.set_sensitive(True)
            self.dialog.refresh()
            if return_code != 0:
                self.dialog.status_label.set_markup(
                    "<span foreground='#d56a12' weight='bold'>Update check failed.</span> "
                    + html.escape(output.strip())
                )
        return False

    def periodic_check(self) -> bool:
        self.check_now()
        return True

    def watch_status(self) -> bool:
        if self.state not in {"updating", "checking"}:
            previous = dict(self.status)
            self.reload_status()
            if self.status != previous:
                self.set_state("available" if release_available(self.status) else "current")
                if self.dialog is not None:
                    self.dialog.refresh()
        return True

    def show_dialog(self) -> None:
        if self.dialog is None:
            self.dialog = UpdateDialog(self)
        self.dialog.present()

    def on_activate(self, _icon: Gtk.StatusIcon) -> None:
        self.show_dialog()

    def on_popup_menu(self, icon: Gtk.StatusIcon, button: int, activate_time: int) -> None:
        menu = Gtk.Menu()
        open_item = Gtk.MenuItem(label="Open update status")
        open_item.connect("activate", lambda _item: self.show_dialog())
        menu.append(open_item)
        check_item = Gtk.MenuItem(label="Check now")
        check_item.connect("activate", lambda _item: self.check_now(show_dialog=True))
        menu.append(check_item)
        menu.append(Gtk.SeparatorMenuItem())
        quit_item = Gtk.MenuItem(label="Quit update tray")
        quit_item.connect("activate", lambda _item: Gtk.main_quit())
        menu.append(quit_item)
        menu.show_all()
        menu.popup(None, None, Gtk.StatusIcon.position_menu, icon, button, activate_time)


def main() -> None:
    if not UPDATER.exists() or not APP_ICON.exists():
        raise SystemExit("Codex Desktop updater or icon is missing")
    css = Gtk.CssProvider()
    css.load_from_data(
        b"""
        window#codex-update-dialog {
          background-color: #f7f7f8;
          color: #202124;
        }
        window#codex-update-dialog textview,
        window#codex-update-dialog textview text {
          background-color: #17191c;
          color: #e8eaed;
        }
        """
    )
    Gtk.StyleContext.add_provider_for_screen(
        Gdk.Screen.get_default(), css, Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION
    )
    TrayController()
    Gtk.main()


if __name__ == "__main__":
    main()
