#!/usr/bin/python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""NiriBridge native desktop interface. Closing this window does not stop sharing."""
from __future__ import annotations
import argparse
from concurrent.futures import ThreadPoolExecutor
import copy
import json
import os
from pathlib import Path
import re
import socket
import sys
import gi
gi.require_version('Gtk', '3.0')
gi.require_version('GdkPixbuf', '2.0')
from gi.repository import Gtk, Gdk, Gio, GLib, Pango, GdkPixbuf
from i18n import _, N_, configure, get_selection, save_language
from model import Model, OperationError, endpoint, split_endpoint
from canvas import ScreenCanvas, EDGE_NAMES, OPPOSITE
from tray import Tray

HERE = Path(__file__).resolve().parent


def css(widget, *names):
    for name in names:
        widget.get_style_context().add_class(name)
    return widget


def box(vertical=True, spacing=10, *classes):
    return css(Gtk.Box(orientation=Gtk.Orientation.VERTICAL if vertical else Gtk.Orientation.HORIZONTAL, spacing=spacing), *classes)


def text(value='', style=None, wrap=True):
    label = Gtk.Label(label=value, xalign=0)
    label.set_line_wrap(wrap)
    label.set_line_wrap_mode(Pango.WrapMode.WORD_CHAR)
    if style:
        css(label, style)
    return label


def button(label, callback, primary=False, icon=None):
    widget = Gtk.Button(label=label)
    if icon:
        widget.set_image(Gtk.Image.new_from_icon_name(icon, Gtk.IconSize.BUTTON))
        widget.set_always_show_image(True)
    if primary:
        css(widget, 'suggested-action')
    widget.connect('clicked', callback)
    return widget


def card(title=None, subtitle=None):
    widget = box(True, 11, 'card')
    if title:
        widget.pack_start(text(title, 'section-title'), False, False, 0)
    if subtitle:
        widget.pack_start(text(subtitle, 'muted'), False, False, 0)
    return widget


def horizontal(*children):
    row = box(False, 12)
    for child in children:
        row.pack_start(child, True, True, 0)
    return row


class Window(Gtk.ApplicationWindow):
    def __init__(self, application, model, render_dir=None, restore_state=None):
        super().__init__(application=application, title='NiriBridge')
        css(self, 'niri-bridge')
        self.set_default_size(1100, 820)
        self.set_size_request(880, 650)
        self.model = model
        self.restore_state = restore_state
        self.language_combos = []
        self.executor = ThreadPoolExecutor(max_workers=3, thread_name_prefix='niri-bridge-ui')
        self.info = None
        self.poll_data = {'service': {}, 'status': None}
        self.busy = False
        self.poll_pending = False
        self.updating = False
        self.settings_dirty = False
        self.layout_dirty = False
        self.settings_revision = None
        self.layout_key = None
        self.device_checks = []
        self.tray = None
        self.render_dir = Path(render_dir) if render_dir else None
        self.rendered = False
        self.alive = True
        self.build()
        self.connect('delete-event', self.on_close)
        self.connect('destroy', self.on_destroy)
        self.show_all()
        self.notice_revealer.set_reveal_child(False)
        self.reload_all()
        self.poll_source = GLib.timeout_add(650, self.poll)
        self.setup_tray()

    def build(self):
        header = Gtk.HeaderBar(show_close_button=True)
        header.set_title('NiriBridge')
        header.set_subtitle(_("Keyboard, mouse & gestures"))
        self.header_badge = css(text(_("Loading status"), wrap=False), 'badge', 'waiting')
        self.header_badge.set_valign(Gtk.Align.CENTER)
        header.pack_start(self.header_badge)
        refresh = button('', lambda *_unused: self.reload_clicked(), icon='view-refresh-symbolic')
        refresh.set_tooltip_text(_("Reload status and settings"))
        css(refresh, 'flat')
        header.pack_end(refresh)
        self.set_titlebar(header)
        root = box(False, 0)
        self.add(root)
        sidebar = box(True, 8, 'sidebar')
        sidebar.set_size_request(184, -1)
        brand = box(True, 6)
        image = Gtk.Image.new_from_file(str(HERE / 'assets/niri-bridge.svg'))
        image.set_pixel_size(54)
        brand.pack_start(image, False, False, 10)
        title = text('NiriBridge', 'brand', False)
        title.set_xalign(.5)
        brand.pack_start(title, False, False, 0)
        sub = text(_("Connect your desktops"), 'muted', False)
        sub.set_xalign(.5)
        brand.pack_start(sub, False, False, 0)
        sidebar.pack_start(brand, False, False, 12)
        self.stack = Gtk.Stack(transition_type=Gtk.StackTransitionType.CROSSFADE, transition_duration=120)
        self.stack.set_hexpand(True)
        self.stack.set_vexpand(True)
        self.nav = {}
        for name, title, icon in [('overview', _("Overview"), 'view-grid-symbolic'), ('layout', _("Screen connections"), 'video-display-symbolic'), ('pair', _("Devices & pairing"), 'network-wired-symbolic'), ('preferences', _("Preferences"), 'preferences-system-symbolic')]:
            nav = button(title, lambda _unused, n=name: self.navigate(n), icon=icon)
            nav.set_halign(Gtk.Align.FILL)
            css(nav, 'nav')
            self.nav[name] = nav
            sidebar.pack_start(nav, False, False, 0)
        sidebar.pack_start(Gtk.Box(), True, True, 0)
        sidebar.pack_end(text(_("Sharing continues in the\nbackground when you close this window"), 'footer'), False, False, 10)
        sidebar.pack_end(button(_("Help"), self.help_dialog, icon='help-browser-symbolic'), False, False, 0)
        sidebar.pack_end(button(_("About and license"), self.about_dialog, icon='help-about-symbolic'), False, False, 0)
        root.pack_start(sidebar, False, False, 0)
        main = box(True, 0)
        root.pack_start(main, True, True, 0)
        self.notice_revealer = Gtk.Revealer(transition_type=Gtk.RevealerTransitionType.SLIDE_DOWN)
        notice_outer = box(True, 0)
        notice_outer.set_margin_start(28)
        notice_outer.set_margin_end(28)
        notice_outer.set_margin_top(14)
        self.notice_box = box(False, 8, 'notice')
        self.notice_label = text()
        self.notice_box.pack_start(self.notice_label, True, True, 0)
        close_notice = button('×', lambda *_unused: self.notice_revealer.set_reveal_child(False))
        css(close_notice, 'flat')
        self.notice_box.pack_end(close_notice, False, False, 0)
        notice_outer.pack_start(self.notice_box, False, False, 0)
        self.notice_revealer.add(notice_outer)
        main.pack_start(self.notice_revealer, False, False, 0)
        main.pack_start(self.stack, True, True, 0)
        self.build_overview()
        self.build_layout()
        self.build_pair()
        self.build_preferences()
        self.build_setup()
        footer = box(False, 10)
        footer.set_margin_start(28)
        footer.set_margin_end(28)
        footer.set_margin_top(8)
        footer.set_margin_bottom(15)
        footer.pack_start(text(_("Emergency return"), 'footer'), False, False, 0)
        footer.pack_start(text('Ctrl + Alt + Shift + Escape', 'footer-key'), False, False, 0)
        self.version_label = text('NiriBridge', 'footer')
        footer.pack_end(self.version_label, False, False, 0)
        main.pack_end(footer, False, False, 0)
        self.navigate('overview')

    def page(self, name, title, subtitle):
        area = Gtk.ScrolledWindow()
        area.set_policy(Gtk.PolicyType.NEVER, Gtk.PolicyType.AUTOMATIC)
        content = box(True, 19, 'page')
        area.add(content)
        content.pack_start(text(title, 'page-title'), False, False, 0)
        content.pack_start(text(subtitle, 'subtitle'), False, False, 0)
        self.stack.add_named(area, name)
        return content

    def build_overview(self):
        page = self.page('overview', _("Move naturally between screens"), _("Move your pointer across. Your keyboard and touchpad gestures follow."))
        hero = box(True, 14, 'hero')
        self.hero_title = text(_("Loading sharing status"), 'hero-title')
        self.hero_description = text(_("Connecting to the local background service…"), 'muted')
        hero.pack_start(self.hero_title, False, False, 0)
        hero.pack_start(self.hero_description, False, False, 0)
        actions = box(False, 12)
        self.toggle_button = button(_("Start sharing"), self.toggle, True, 'media-playback-start-symbolic')
        self.release_button = button(_("Return to this computer"), self.release_control, icon='go-home-symbolic')
        actions.pack_start(self.toggle_button, False, False, 0)
        actions.pack_start(self.release_button, False, False, 0)
        self.spinner = Gtk.Spinner()
        actions.pack_end(self.spinner, False, False, 4)
        hero.pack_start(actions, False, False, 0)
        page.pack_start(hero, False, False, 0)
        metrics = []
        self.metric_labels = {}
        for key, title, initial in [('peer', _("Paired computer"), _("Not loaded")), ('latency', _("Connection round-trip"), '—'), ('mode', _("Control target"), _("This computer"))]:
            tile = card()
            css(tile, 'compact-card')
            tile.pack_start(text(title, 'muted'), False, False, 0)
            label = text(initial, 'metric', False)
            label.set_ellipsize(Pango.EllipsizeMode.END)
            tile.pack_start(label, False, False, 0)
            self.metric_labels[key] = label
            metrics.append(tile)
        self.metric_labels['latency'].set_tooltip_text(_("Round-trip time over the encrypted connection, excluding display latency."))
        page.pack_start(horizontal(*metrics), False, False, 0)
        self.overview_canvas = ScreenCanvas()
        self.overview_canvas.set_size_request(360, 305)
        page.pack_start(self.overview_canvas, True, True, 0)
        row = box(False, 12)
        row.pack_start(text(_("Green lines mark the entry edges"), 'muted'), True, True, 0)
        row.pack_end(button(_("Arrange screens"), lambda *_unused: self.navigate('layout')), False, False, 0)
        page.pack_start(row, False, False, 0)

    def build_layout(self):
        page = self.page('layout', _("Screen connections"), _("Drag the other computer's screens to arrange the connection, then save on both computers."))
        self.editor = ScreenCanvas(editable=True)
        self.editor.set_size_request(360, 335)
        self.editor.connect('layout-changed', self.canvas_changed)
        page.pack_start(self.editor, True, True, 0)
        self.edge_combo = Gtk.ComboBoxText()
        for value, label in EDGE_NAMES.items():
            self.edge_combo.append(value, _(label))
        self.edge_combo.connect('changed', self.direction_changed)
        direction = box(False, 12)
        direction.pack_start(text(_("The other computer is")), False, False, 0)
        direction.pack_start(self.edge_combo, False, False, 0)
        direction.pack_end(text(_("Local monitor arrangement follows your desktop settings"), 'muted'), False, False, 0)
        page.pack_start(direction, False, False, 0)
        self.output_combos, self.range_spins = {}, {}
        cards = []
        for role, title in [('local', _("This computer's entry edge")), ('peer', _("Other computer's entry edge"))]:
            panel = card(title)
            combo = Gtk.ComboBoxText()
            combo.connect('changed', self.output_changed, role)
            self.output_combos[role] = combo
            panel.pack_start(combo, False, False, 0)
            row = box(False, 6)
            for key, prefix in [('start', _("From")), ('end', _("to"))]:
                row.pack_start(text(prefix, 'muted', False), False, False, 0)
                spin = Gtk.SpinButton.new_with_range(0, 100, 1)
                spin.set_digits(1)
                spin.set_numeric(True)
                spin.set_width_chars(5)
                spin.connect('value-changed', self.range_changed, role, key)
                self.range_spins[role, key] = spin
                row.pack_start(spin, True, True, 0)
                row.pack_start(text('%', 'muted', False), False, False, 0)
            panel.pack_start(row, False, False, 0)
            cards.append(panel)
        page.pack_start(horizontal(*cards), False, False, 0)
        self.layout_hint = text(_("Once connected, configure both entry edges here. Saving briefly restores local control."), 'muted')
        page.pack_start(self.layout_hint, False, False, 0)
        actions = box(False, 10)
        self.layout_save = button(_("Save on both computers"), self.save_layout, True)
        self.layout_reset = button(_("Discard changes"), self.reset_layout)
        actions.pack_end(self.layout_save, False, False, 0)
        actions.pack_end(self.layout_reset, False, False, 0)
        page.pack_start(actions, False, False, 0)

    def build_pair(self):
        page = self.page('pair', _("Devices & pairing"), _("Exchange pairing files to establish trust. Each computer keeps its own private key."))
        self.cert_labels = {}
        panels = []
        for role, title in [('identity', _("This computer's pairing file")), ('peer', _("Paired computer"))]:
            panel = card(title)
            name = text(_("Not set up"), 'section-title')
            fingerprint = text(_("Fingerprint not loaded"), 'mono')
            fingerprint.set_selectable(True)
            panel.pack_start(name, False, False, 0)
            panel.pack_start(text(_("SHA-256 fingerprint"), 'muted'), False, False, 0)
            panel.pack_start(fingerprint, False, False, 0)
            self.cert_labels[role] = (name, fingerprint)
            action = button(_("Export this computer's file") if role == 'identity' else _("Import the other computer's file"), self.export_certificate if role == 'identity' else self.import_certificate, icon='document-save-symbolic' if role == 'identity' else 'document-open-symbolic')
            panel.pack_end(action, False, False, 0)
            if role == 'identity':
                self.export_button = action
            else:
                self.import_button = action
            panels.append(panel)
        page.pack_start(horizontal(*panels), False, False, 0)
        page.pack_start(text(_("Export a pairing file on each computer and import it on the other. Verify matching fingerprints before enabling shared input and screen-setting synchronization."), 'muted'), False, False, 0)
        network = card(_("Connection method"), _("One computer waits for a connection. The other connects to its address."))
        self.connection_mode = Gtk.ComboBoxText()
        self.connection_mode.append('connect', _("Connect to the other computer"))
        self.connection_mode.append('listen', _("Wait for the other computer to connect"))
        self.connection_mode.connect('changed', self.connection_mode_changed)
        network.pack_start(self.connection_mode, False, False, 0)
        address_row = box(False, 12)
        address_column = box(True, 6)
        self.address_label = text(_("Other computer's address"), 'muted')
        address_column.pack_start(self.address_label, False, False, 0)
        self.host_entry = Gtk.Entry()
        self.host_entry.set_placeholder_text(_("LAN address or hostname"))
        self.host_entry.connect('changed', self.settings_changed)
        address_column.pack_start(self.host_entry, False, False, 0)
        address_row.pack_start(address_column, True, True, 0)
        port_column = box(True, 6)
        port_column.pack_start(text(_("Port"), 'muted'), False, False, 0)
        self.port_spin = Gtk.SpinButton.new_with_range(1, 65535, 1)
        self.port_spin.set_numeric(True)
        self.port_spin.set_width_chars(6)
        self.port_spin.connect('value-changed', self.settings_changed)
        port_column.pack_start(self.port_spin, False, False, 0)
        address_row.pack_end(port_column, False, False, 0)
        network.pack_start(address_row, False, False, 0)
        self.address_hint = text('', 'muted')
        network.pack_start(self.address_hint, False, False, 0)
        self.network_save = button(_("Save local settings"), self.save_settings, True)
        network.pack_end(self.network_save, False, False, 0)
        page.pack_start(network, False, False, 0)

    def build_preferences(self):
        page = self.page('preferences', _("Preferences"), _("Manage this computer's input devices and startup preferences."))
        behavior = card(_("Sharing behavior"))
        self.gesture_check = Gtk.CheckButton(label=_("Share native gestures from this computer's touchpad"))
        self.gesture_check.connect('toggled', self.settings_changed)
        behavior.pack_start(self.gesture_check, False, False, 0)
        behavior.pack_start(text(_("Three-finger switching and four-finger overview follow the pointer."), 'muted'), False, False, 0)
        row = box(False, 15)
        row.pack_start(text(_("Start sharing when I log in to Niri")), True, True, 0)
        self.autostart = Gtk.Switch()
        self.autostart.set_valign(Gtk.Align.CENTER)
        self.autostart.connect('notify::active', self.autostart_changed)
        row.pack_end(self.autostart, False, False, 0)
        behavior.pack_start(row, False, False, 9)
        language_row = box(False, 15)
        language_row.pack_start(text(_('Interface language')), True, True, 0)
        language_row.pack_end(self.language_selector(), False, False, 0)
        behavior.pack_start(language_row, False, False, 0)
        behavior.pack_start(Gtk.Separator(), False, False, 0)
        behavior.pack_start(text(_("Sharing pauses when either computer locks. Physical input on the receiving computer restores local control."), 'muted'), False, False, 0)
        behavior.pack_start(text(_("Your existing KDE Connect setup continues to handle the clipboard."), 'muted'), False, False, 0)
        page.pack_start(behavior, False, False, 0)
        devices = card(_("Shared local devices"), _("Choose devices for keyboard forwarding, touchpad gestures and taking back local control."))
        self.devices_box = box(True, 0)
        devices.pack_start(self.devices_box, False, False, 0)
        self.permission_label = text(_("Checking device permissions"), 'muted')
        devices.pack_start(self.permission_label, False, False, 0)
        device_actions = box(False, 10)
        self.permission_button = button(_("Allow device access"), self.grant_permissions, icon='security-medium-symbolic')
        self.preferences_save = button(_("Save local settings"), self.save_settings, True)
        device_actions.pack_start(self.permission_button, False, False, 0)
        device_actions.pack_end(self.preferences_save, False, False, 0)
        devices.pack_start(device_actions, False, False, 0)
        page.pack_start(devices, False, False, 0)
        support = box(False, 12)
        support.pack_start(text(_("For troubleshooting, copy a diagnostic report without input contents."), 'muted'), True, True, 0)
        support.pack_end(button(_("Copy diagnostics"), self.copy_diagnostics, icon='edit-copy-symbolic'), False, False, 0)
        page.pack_start(support, False, False, 0)

    def build_setup(self):
        page = self.page('setup', _("Welcome to NiriBridge"), _("Create an identity for this computer, then exchange pairing files."))
        form = card(_("Set up this computer"))
        language_row = box(False, 15)
        language_row.pack_start(text(_('Interface language')), True, True, 0)
        language_row.pack_end(self.language_selector(), False, False, 0)
        form.pack_start(language_row, False, False, 0)
        form.pack_start(text(_("Device name"), 'muted'), False, False, 0)
        self.setup_name = Gtk.Entry()
        default = re.sub('[^a-z0-9-]', '-', socket.gethostname().lower()).strip('-')[:63] or 'my-computer'
        self.setup_name.set_text(default)
        form.pack_start(self.setup_name, False, False, 0)
        form.pack_start(text(_("Use lowercase letters, numbers and hyphens, such as my-laptop."), 'muted'), False, False, 0)
        form.pack_start(text(_("Display for sharing"), 'muted'), False, False, 0)
        self.setup_output = Gtk.ComboBoxText()
        form.pack_start(self.setup_output, False, False, 0)
        form.pack_start(text(_("This computer will initially wait for a connection. You can change this under Devices & pairing."), 'muted'), False, False, 0)
        form.pack_start(text(_('Input devices'), 'section-title'), False, False, 0)
        form.pack_start(text(_('Choose the physical devices this computer will share.'), 'muted'), False, False, 0)
        self.setup_devices = box(True, 4)
        self.setup_device_checks = []
        form.pack_start(self.setup_devices, False, False, 0)
        form.pack_start(button(_("Create this computer's identity"), self.initialize, True), False, False, 0)
        page.pack_start(form, False, False, 0)
        page.pack_start(text(_("Next: export your pairing file, import each other's file, verify fingerprints, turn on sharing, then arrange the screens."), 'muted'), False, False, 0)

    def language_selector(self):
        combo = Gtk.ComboBoxText()
        combo.append('auto', _('System default'))
        combo.append('en', 'English')
        combo.append('zh_CN', '简体中文')
        combo.set_active_id(get_selection())
        combo.connect('changed', self.language_changed)
        self.language_combos.append(combo)
        return combo

    def language_changed(self, combo):
        language = combo.get_active_id()
        if self.updating or self.busy or not language or language == get_selection():
            return
        try:
            self.get_application().change_language(language)
        except (OSError, ValueError):
            self.notice(_('The language preference could not be saved.'), True)

    def editing_state(self):
        return {'page': self.stack.get_visible_child_name(), 'settings_dirty': self.settings_dirty,
                'settings_revision': self.settings_revision,
                'settings': {'mode': self.connection_mode.get_active_id(), 'host': self.host_entry.get_text(),
                             'port': self.port_spin.get_value(), 'gestures': self.gesture_check.get_active(),
                             'devices': [d['path'] for check, d in self.device_checks if check.get_active()]},
                'layout_dirty': self.layout_dirty, 'layout': copy.deepcopy(self.editor.draft),
                'setup_name': self.setup_name.get_text(), 'setup_output': self.setup_output.get_active_id(),
                'setup_devices': [d['path'] for check, d in self.setup_device_checks if check.get_active()]}

    def restore_editing_state(self, state):
        self.updating = True
        if state['settings_dirty']:
            values = state['settings']
            self.connection_mode.set_active_id(values['mode'])
            self.host_entry.set_text(values['host'])
            self.port_spin.set_value(values['port'])
            self.gesture_check.set_active(values['gestures'])
            for check, device in self.device_checks:
                check.set_active(device['path'] in values['devices'])
            self.settings_revision = state['settings_revision']
            self.settings_dirty = True
        for check, device in self.setup_device_checks:
            check.set_active(device['path'] in state['setup_devices'])
        self.setup_name.set_text(state['setup_name'])
        if state['setup_output']:
            self.setup_output.set_active_id(state['setup_output'])
        self.updating = False
        if state['layout_dirty'] and state['layout']:
            self.editor.draft = state['layout']
            self.layout_dirty = True
            self.fill_layout_controls()
            self.editor.queue_draw()
        self.navigate(state['page'])
        self.update_controls()
        self.notice(_('Interface language updated. Unsaved edits were kept.'))

    def navigate(self, name):
        if self.info and self.info.get('config') is None and name != 'setup':
            name = 'setup'
        self.stack.set_visible_child_name(name)
        for key, nav in self.nav.items():
            context = nav.get_style_context()
            (context.add_class if key == name else context.remove_class)('selected')

    def notice(self, message, error=False):
        self.notice_label.set_text(str(message))
        context = self.notice_box.get_style_context()
        (context.add_class if error else context.remove_class)('error')
        self.notice_revealer.set_reveal_child(True)

    def work(self, caption, function, success=None):
        if self.busy:
            return
        self.busy = True
        self.spinner.start()
        self.notice(caption)
        self.update_controls()
        future = self.executor.submit(function)
        def complete(future):
            try:
                result, error = future.result(), None
            except Exception as exc:
                result, error = None, exc if isinstance(exc, OperationError) else OperationError()
            def finish():
                if not self.alive:
                    return False
                self.busy = False
                self.spinner.stop()
                if error:
                    self.notice(str(error), True)
                elif success:
                    success(result)
                else:
                    self.notice(_("Done."))
                self.update_controls()
                return False
            GLib.idle_add(finish)
        future.add_done_callback(complete)

    def reload_all(self, message=None):
        def loaded(info):
            self.populate(info)
            if message:
                self.notice(message)
            else:
                self.notice_revealer.set_reveal_child(False)
        self.work(_("Loading devices and settings…"), self.model.load, loaded)

    def reload_clicked(self):
        if (self.settings_dirty or self.layout_dirty) and not self.confirm(_("Discard unsaved changes?"), _("Reloading discards unsaved edits in this window."), _("Reload")):
            return
        self.settings_dirty = self.layout_dirty = False
        self.layout_key = None
        self.reload_all()

    def poll(self):
        if not self.alive:
            return False
        if self.poll_pending:
            return True
        self.poll_pending = True
        future = self.executor.submit(self.model.poll)
        def done(future):
            try:
                result = future.result()
            except Exception:
                result = {'service': {}, 'status': None}
            def update():
                self.poll_pending = False
                if self.alive:
                    self.update_status(result)
                return False
            GLib.idle_add(update)
        future.add_done_callback(done)
        return True

    def populate(self, info):
        self.info = info
        self.version_label.set_text('NiriBridge ' + info.get('version', ''))
        config = info.get('config')
        self.updating = True
        if config and not self.settings_dirty:
            self.settings_revision = config['revision']
            settings = config['settings']
            mode = settings['connection']['mode']
            host, port = split_endpoint(settings['connection']['address'])
            self.connection_mode.set_active_id(mode)
            self.host_entry.set_text(host)
            self.port_spin.set_value(port)
            self.gesture_check.set_active(settings['native_touchpads'])
            for child in self.devices_box.get_children():
                self.devices_box.remove(child)
            self.device_checks = []
            for device in info['devices']['devices']:
                row = box(True, 0, 'device-row')
                check = Gtk.CheckButton(label=device['name'] if device.get('available', True) else _('Unavailable device'))
                check.get_child().set_line_wrap(True)
                check.get_child().set_line_wrap_mode(Pango.WrapMode.WORD_CHAR)
                check.set_active(device['selected'])
                check.set_tooltip_text(device['path'])
                check.connect('toggled', self.settings_changed)
                row.pack_start(check, False, False, 0)
                kinds = []
                for kind, label in [('ID_INPUT_KEYBOARD', _("Keyboard")), ('ID_INPUT_MOUSE', _("Mouse")), ('ID_INPUT_TOUCHPAD', _("Touchpad"))]:
                    if kind in device.get('classes', []):
                        kinds.append(label)
                note = ' · '.join(kinds) or _("Currently disconnected")
                note += _(" · Access allowed") if device['readable'] else _(" · Disconnected") if not device.get('available', True) else _(" · Permission required")
                details = text(note, 'muted')
                details.set_margin_start(27)
                row.pack_start(details, False, False, 0)
                self.devices_box.pack_start(row, False, False, 0)
                self.device_checks.append((check, device))
            if not self.device_checks:
                self.devices_box.pack_start(text(_("No available keyboard, mouse or touchpad was detected."), 'muted'), False, False, 0)
            self.devices_box.show_all()
        for role in ('identity', 'peer'):
            cert = config.get(role) if config else None
            name, fp = self.cert_labels[role]
            name.set_text(cert['name'] if cert else _("Not set up"))
            fingerprint = cert['fingerprint'] if cert else ''
            fp.set_text('\n'.join(' '.join(fingerprint[i:i + 8] for i in range(start, min(start + 32, len(fingerprint)), 8)) for start in range(0, len(fingerprint), 32)) if cert else _("Import a pairing file to see its fingerprint"))
            fp.set_tooltip_text(_("Valid until ") + cert['expires'] if cert else None)
        self.connection_mode_changed()
        self.updating = False
        self.update_permissions()
        self.update_status(info)
        if not config:
            for child in self.setup_devices.get_children():
                self.setup_devices.remove(child)
            self.setup_device_checks = []
            for device in info['devices']['devices']:
                if not device.get('available', True):
                    continue
                check = Gtk.CheckButton(label=device['name'])
                check.get_child().set_line_wrap(True)
                check.set_active(True)
                self.setup_devices.pack_start(check, False, False, 0)
                self.setup_device_checks.append((check, device))
            self.setup_devices.show_all()
            self.setup_output.remove_all()
            for name in info.get('outputs', {}):
                self.setup_output.append(name, name)
            self.setup_output.set_active(0)
            self.navigate('setup')
        elif self.stack.get_visible_child_name() == 'setup':
            self.navigate('pair')
        if self.restore_state:
            self.restore_editing_state(self.restore_state)
            self.restore_state = None
        if self.render_dir and not self.rendered and config:
            self.rendered = True
            GLib.timeout_add(800, self.render_pages)

    def update_status(self, data):
        self.poll_data = {'service': data.get('service', {}), 'status': data.get('status'), 'unmanaged': data.get('unmanaged', False)}
        service, status = self.poll_data['service'], self.poll_data['status']
        active = service.get('ActiveState') == 'active'
        config = self.info.get('config') if self.info else None
        peer_name = status.get('peer_name') if status else config.get('peer_name', _("Not paired")) if config else _("Not paired")
        if not (config and config.get('peer')) and not (status and status.get('connection') == 'connected'):
            peer_name = _("Not paired")
        role = status.get('role', 'local') if status else 'local'
        connected = bool(status and status.get('connection') == 'connected')
        badge_class = 'waiting'
        if self.poll_data.get('unmanaged'):
            title, description, badge = _('A manually started instance is running'), _('Stop the manually started instance before managing sharing through the user service.'), _('Manual instance')
        elif not active:
            title = _("Sharing is paused") if service.get('ActiveState') != 'failed' else _("Sharing could not start")
            description = _("Turn sharing on to move the pointer between computers.") if title == _("Sharing is paused") else _("Check the pairing files, connection address and input device permissions.")
            badge, badge_class = _("Paused"), 'paused'
        elif not status:
            title, description, badge = _("Starting sharing"), _("Waiting for the background service…"), _("Starting")
        elif status.get('connection') == 'starting':
            title, description, badge = _("Starting sharing"), _("Checking the local session and input devices…"), _("Starting")
        elif status.get('reason') == 'config_changed':
            title, description, badge = _('A different configuration is running'), _('Reload the active configuration before managing sharing.'), _('Configuration changed')
        elif status.get('configuring'):
            title, description, badge = _("Synchronizing screen connections"), _("Checking and saving entry edges on both computers."), _("Synchronizing")
        elif not status.get('local_unlocked', False):
            title, description, badge = _("Sharing is paused for this session"), _("Unlock this computer and return to your Niri session to continue."), _("Session paused")
        elif connected and status.get('peer_unlocked') is False:
            title, description, badge = _("The other computer is unavailable"), _("The other computer is locked or its session is inactive. Sharing can resume when it is available."), _("Other side paused")
        elif status.get('reason') == 'output_unavailable':
            title, description, badge = _('Choose an entry display'), _('An entry display is unavailable. Open Screen connections to select an active display.'), _('Display unavailable')
        elif status.get('reason') == 'protocol_mismatch':
            title, description, badge = _('Update both computers'), _('Install the same NiriBridge version on both computers.'), _('Version mismatch')
        elif connected:
            title = {'sending': _("Controlling the other computer"), 'receiving': _("This computer is being controlled")}.get(role, _("Your computers are connected"))
            description = {'sending': _("Your keyboard and touchpad gestures are following the pointer to the other computer."), 'receiving': _("Use this computer's physical keyboard or pointer to take back control.")}.get(role, _("Move the pointer across the green edge to start sharing."))
            badge, badge_class = _("Connected"), ''
        elif status.get('reason') == 'authentication_failed':
            title, description, badge = _("Pairing verification failed"), _("Check the pairing files and device names on both computers."), _("Verify pairing")
        else:
            title, description, badge = _("Waiting for the other computer"), _("Check that sharing is on at the other computer and that its address is reachable."), _("Waiting for connection")
        self.hero_title.set_text(title)
        self.hero_description.set_text(description)
        self.header_badge.set_text(badge)
        for name in ('waiting', 'paused'):
            self.header_badge.get_style_context().remove_class(name)
        if badge_class:
            css(self.header_badge, badge_class)
        self.metric_labels['peer'].set_text(peer_name)
        self.metric_labels['peer'].set_tooltip_text(peer_name)
        latency = status.get('latency_ms') if connected else None
        self.metric_labels['latency'].set_text(f'{latency:.1f} ms' if isinstance(latency, (int, float)) else '—')
        self.metric_labels['mode'].set_text({'sending': _("Other computer"), 'receiving': _("Remote control")}.get(role, _("This computer")))
        self.updating = True
        self.autostart.set_active(service.get('UnitFileState') in ('enabled', 'enabled-runtime'))
        self.updating = False
        local = status.get('local') if status else None
        peer = status.get('peer') if status else None
        if local is None and config:
            local = {'edge': config['edge'], 'revision': config['revision'], 'outputs': self.info.get('outputs', {})}
        key = json.dumps([local, peer], sort_keys=True)
        if key != getattr(self, 'overview_key', None):
            self.overview_key = key
            self.overview_canvas.set_desktops(local, peer, peer_name)
        self.overview_canvas.control_role = role
        self.overview_canvas.queue_draw()
        if not self.layout_dirty and key != self.layout_key:
            self.layout_key = key
            self.editor.set_desktops(local, peer, peer_name)
            if self.editor.draft and self.editor.draft.repaired:
                self.layout_dirty = True
            self.fill_layout_controls()
        if self.tray:
            self.tray.update(f'NiriBridge · {badge}', (_('Open NiriBridge'), _('Pause sharing') if active else _('Start sharing'), _('Quit the interface')), not self.busy)
        self.update_controls()

    def update_controls(self):
        service, status = self.poll_data.get('service', {}), self.poll_data.get('status') or {}
        active = service.get('ActiveState') == 'active'
        config = self.info.get('config') if self.info else None
        configuring = bool(status.get('configuring'))
        unmanaged = self.poll_data.get('unmanaged', False)
        self.toggle_button.set_label(_("Pause sharing") if active else _("Start sharing"))
        self.toggle_button.set_image(Gtk.Image.new_from_icon_name('media-playback-pause-symbolic' if active else 'media-playback-start-symbolic', Gtk.IconSize.BUTTON))
        self.toggle_button.set_sensitive(bool(config) and not unmanaged and not self.busy and not configuring and status.get('reason') != 'config_changed')
        self.release_button.set_sensitive(not self.busy and status.get('connection') == 'connected' and status.get('role') != 'local')
        valid_layout = self.editor.draft is not None and all(n['edge']['boundary']['start'] < n['edge']['boundary']['end'] for n in (self.editor.draft.local, self.editor.draft.peer))
        connected = status.get('connection') == 'connected'
        self.layout_save.set_sensitive(bool(valid_layout and self.layout_dirty and connected and not unmanaged and status.get('local_unlocked') and status.get('peer_unlocked') and not self.busy and not configuring))
        self.layout_reset.set_sensitive(self.layout_dirty and not self.busy)
        self.editor.set_sensitive(not self.busy and not configuring)
        self.network_save.set_sensitive(bool(config and self.settings_dirty and not self.busy and not unmanaged))
        self.preferences_save.set_sensitive(bool(config and self.settings_dirty and not self.busy and not unmanaged))
        self.export_button.set_sensitive(bool(config and config.get('identity') and not self.busy))
        self.import_button.set_sensitive(bool(config and not self.busy and not unmanaged))
        self.permission_button.set_sensitive(bool(config and not self.busy and not unmanaged))
        self.autostart.set_sensitive(bool(config and not unmanaged and not self.busy and status.get('reason') != 'config_changed'))
        for combo in self.language_combos:
            combo.set_sensitive(not self.busy)
        if not connected:
            self.layout_hint.set_text(_("Connect both computers to synchronize their entry edges. The last known layout is shown for now."))
        elif not status.get('local_unlocked') or not status.get('peer_unlocked'):
            self.layout_hint.set_text(_('Unlock both computers to save screen connections.'))
        elif self.editor.draft and self.editor.draft.repaired:
            self.layout_hint.set_text(_('An entry display is unavailable. Confirm its replacement and save on both computers.'))
        elif self.layout_dirty:
            self.layout_hint.set_text(_("You have unsaved changes. Saving briefly restores local control and synchronizes both computers."))
        else:
            self.layout_hint.set_text(_("Current connections loaded. Changes can be saved to both computers."))

    def toggle(self, *_unused):
        active = self.poll_data.get('service', {}).get('ActiveState') == 'active'
        if not active:
            config = self.info.get('config') if self.info else None
            if not config or not config.get('peer'):
                self.navigate('pair')
                self.notice(_("Import the other computer's pairing file first."))
                return
            if not self.permissions_ready():
                self.navigate('preferences')
                self.notice(_("Allow access to the selected input devices first."))
                return
        def done(result):
            self.update_status(result)
            self.notice(_("Sharing is paused. Both computers now use their own input devices.") if active else _("Sharing is on. Connecting to the paired computer."))
        self.work(_("Pausing sharing…") if active else _("Starting sharing…"), lambda: self.model.set_running(not active), done)

    def release_control(self, *_unused):
        self.work(_("Restoring local control…"), self.model.release, lambda _unused: self.notice(_("Local control has been restored.")))

    def fill_layout_controls(self):
        draft = self.editor.draft
        self.updating = True
        for role, combo in self.output_combos.items():
            combo.remove_all()
            if draft:
                node = draft.local if role == 'local' else draft.peer
                for name in node['outputs']:
                    combo.append(name, name)
                combo.set_active_id(node['edge']['output'])
                for key in ('start', 'end'):
                    self.range_spins[role, key].set_value(node['edge']['boundary'][key] * 100)
        if draft:
            self.edge_combo.set_active_id(draft.local['edge']['boundary']['edge'])
        self.updating = False

    def canvas_changed(self, *_unused):
        self.layout_dirty = True
        self.fill_layout_controls()
        self.update_controls()

    def direction_changed(self, *_unused):
        if self.updating or not self.editor.draft:
            return
        edge = self.edge_combo.get_active_id()
        if edge:
            self.editor.draft.local['edge']['boundary']['edge'] = edge
            self.editor.draft.peer['edge']['boundary']['edge'] = OPPOSITE[edge]
            self.editor.draft.free_origin = None
            self.editor.queue_draw()
            self.layout_dirty = True
            self.update_controls()

    def output_changed(self, combo, role):
        if self.updating or not self.editor.draft or not combo.get_active_id():
            return
        node = self.editor.draft.local if role == 'local' else self.editor.draft.peer
        node['edge']['output'] = combo.get_active_id()
        self.editor.draft.reset_scale()
        self.layout_dirty = True
        self.editor.queue_draw()
        self.update_controls()

    def range_changed(self, spin, role, key):
        if self.updating or not self.editor.draft:
            return
        node = self.editor.draft.local if role == 'local' else self.editor.draft.peer
        node['edge']['boundary'][key] = spin.get_value() / 100
        self.layout_dirty = True
        self.editor.queue_draw()
        self.update_controls()

    def reset_layout(self, *_unused):
        self.layout_dirty = False
        self.layout_key = None
        self.update_status(self.poll_data)

    def save_layout(self, *_unused):
        if not self.editor.draft:
            return
        request = copy.deepcopy(self.editor.draft.request())
        def done(result):
            self.layout_dirty = False
            self.layout_key = None
            self.notice(_("Screen connections saved on both computers. Reconnecting now."))
            self.reload_all(_("Screen connections saved on both computers. The new edges will appear after reconnection."))
        self.work(_("Checking and synchronizing screen connections…"), lambda: self.model.apply_layout(request), done)

    def settings_changed(self, *_unused):
        if self.updating:
            return
        self.settings_dirty = True
        self.update_permissions()
        self.update_controls()

    def connection_mode_changed(self, *_unused):
        mode = self.connection_mode.get_active_id()
        self.address_label.set_text(_("Local listening address") if mode == 'listen' else _("Other computer's address"))
        self.address_hint.set_text(_("Normally use 0.0.0.0 here. The other computer connects using this computer's LAN address.") if mode == 'listen' else _("Enter the other computer's LAN address or hostname. Both computers must use the same port."))
        self.settings_changed()

    def current_settings(self):
        return {'connection': {'mode': self.connection_mode.get_active_id(), 'address': endpoint(self.host_entry.get_text(), self.port_spin.get_value_as_int())},
                'native_touchpads': self.gesture_check.get_active(),
                'activity_devices': [d['path'] for check, d in self.device_checks if check.get_active()]}

    def save_settings(self, *_unused):
        try:
            settings = self.current_settings()
        except OperationError as error:
            self.notice(str(error), True)
            return
        if not settings['activity_devices']:
            self.notice(str(OperationError('input_selection_empty')), True)
            return
        def done(_unused):
            self.settings_dirty = False
            self.reload_all(_("Local settings saved. If sharing is on, it will reconnect automatically."))
        self.work(_("Saving local settings…"), lambda: self.model.save_settings(self.settings_revision, settings), done)

    def autostart_changed(self, switch, *_unused):
        if self.updating:
            return
        enabled = switch.get_active()
        self.work(_("Updating startup preferences…"), lambda: self.model.set_autostart(enabled), lambda data: (self.update_status(data), self.notice(_("Sharing will start automatically with your Niri session.") if enabled else _("Automatic startup is off. The current sharing session is unchanged."))))

    def permissions_ready(self):
        if not self.info:
            return False
        selected = [d for check, d in self.device_checks if check.get_active()]
        return bool(selected and self.info['devices'].get('uinput_writable') and all(d.get('readable') or not d.get('available', True) for d in selected))

    def update_permissions(self):
        if not self.info:
            return
        selected = [d for check, d in self.device_checks if check.get_active()]
        ready = sum(d['readable'] for d in selected)
        self.permission_label.set_text(_("Selected: {selected}. Available now: {ready}.").format(selected=len(selected), ready=ready) + (_(" Virtual input access is allowed.") if self.info['devices'].get('uinput_writable') else _(" Virtual input permission is required.")))

    def grant_permissions(self, *_unused):
        if self.settings_dirty:
            self.notice(_("Save your device selection before granting access."), True)
            return
        if self.permissions_ready():
            self.notice(_("Access to the selected devices and virtual input is already allowed."))
            return
        if not self.confirm(_("Allow access to the selected input devices?"), _("The system will ask for administrator authentication. Access is limited to the selected input devices and virtual input interface."), _("Continue")):
            return
        self.work(_("Waiting for administrator authentication…"), self.model.grant_devices, lambda info: (self.populate(info), self.notice(_("Device permissions updated."))))

    def initialize(self, *_unused):
        name = self.setup_name.get_text().strip()
        if not re.fullmatch('[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?', name):
            self.notice(_("Use lowercase letters, numbers and hyphens. Do not begin or end the name with a hyphen."), True)
            return
        output = self.setup_output.get_active_id()
        paths = [d['path'] for check, d in self.setup_device_checks if check.get_active()]
        if not output or not paths:
            self.notice(_("Connect a display and at least one input device first."), True)
            return
        settings = {'connection': {'mode': 'listen', 'address': '0.0.0.0:42420'}, 'activity_devices': paths, 'native_touchpads': True}
        edge = {'output': output, 'boundary': {'edge': 'top', 'start': 0., 'end': 1.}}
        self.work(_("Creating this computer's identity…"), lambda: self.model.initialize(name, settings, edge), lambda _unused: self.reload_all(_("This computer's identity is ready. Exchange pairing files with the other computer.")))

    def choose_file(self, title, save, callback, name=None):
        chooser = Gtk.FileChooserNative.new(title, self, Gtk.FileChooserAction.SAVE if save else Gtk.FileChooserAction.OPEN, _("Save") if save else _("Choose"), _("Cancel"))
        file_filter = Gtk.FileFilter()
        file_filter.set_name(_("NiriBridge pairing files (*.pem)"))
        file_filter.add_pattern('*.pem')
        chooser.add_filter(file_filter)
        chooser.set_do_overwrite_confirmation(True)
        if name:
            chooser.set_current_name(name)
        def response(dialog, result):
            path = dialog.get_filename()
            dialog.destroy()
            if result == Gtk.ResponseType.ACCEPT and path:
                callback(Path(path))
        chooser.connect('response', response)
        chooser.show()

    def export_certificate(self, *_unused):
        cert = self.info['config'].get('identity')
        if not cert:
            return
        def selected(path):
            def export():
                path.write_text(cert['pem'])
                return None
            self.work(_("Exporting the pairing file…"), export, lambda _unused: self.notice(_("Pairing file exported. Import it on the other computer and verify the fingerprint.")))
        self.choose_file(_("Export this computer's file"), True, selected, f'niri-bridge-{cert["name"]}.pem')

    def import_certificate(self, *_unused):
        def selected(path):
            self.work(_("Reading the pairing file…"), lambda: self.model.inspect_certificate(path), lambda info: self.review_peer(path, info))
        self.choose_file(_("Choose the other computer's pairing file"), False, selected)

    def review_peer(self, path, certificate):
        dialog = Gtk.Dialog(title=_("Verify the paired computer"), transient_for=self, modal=True)
        css(dialog, 'niri-bridge')
        dialog.add_button(_("Cancel"), Gtk.ResponseType.CANCEL)
        accept = dialog.add_button(_("Trust and pair"), Gtk.ResponseType.ACCEPT)
        css(accept, 'suggested-action')
        accept.set_sensitive(False)
        content = box(True, 16, 'page')
        content.set_size_request(490, -1)
        content.pack_start(text(certificate['name'], 'page-title'), False, False, 0)
        content.pack_start(text(_("Compare every group with the SHA-256 fingerprint shown on the other computer."), 'muted'), False, False, 0)
        fp = certificate['fingerprint']
        fingerprint = text('\n'.join(' '.join(fp[i:i + 8] for i in range(start, start + 32, 8)) for start in (0, 32)), 'mono')
        fingerprint.set_selectable(True)
        content.pack_start(fingerprint, False, False, 0)
        content.pack_start(text(_("Pairing allows shared keyboard, pointer and gesture control, plus synchronization of screen entry settings."), 'muted'), False, False, 0)
        checked = Gtk.CheckButton(label=_("I verified that this fingerprint matches the other computer"))
        checked.connect('toggled', lambda check: accept.set_sensitive(check.get_active()))
        content.pack_start(checked, False, False, 0)
        dialog.get_content_area().add(content)
        dialog.show_all()
        result = dialog.run()
        confirmed = checked.get_active()
        dialog.destroy()
        if result == Gtk.ResponseType.ACCEPT and confirmed:
            revision = self.info['config']['revision']
            self.work(_("Saving pairing information…"), lambda: self.model.import_peer(path, revision, certificate['fingerprint']), lambda _unused: self.reload_all(_("Pairing file imported. Make sure the other computer has imported your file, then start sharing.")))

    def copy_diagnostics(self, *_unused):
        def done(report):
            Gtk.Clipboard.get(Gdk.SELECTION_CLIPBOARD).set_text(report, -1)
            self.notice(_("Diagnostics copied. The report excludes input contents, pairing addresses and private keys."))
        self.work(_("Generating diagnostics…"), self.model.diagnostics, done)

    def confirm(self, title, description, accept=N_("Confirm")):
        dialog = Gtk.MessageDialog(transient_for=self, modal=True, message_type=Gtk.MessageType.QUESTION, buttons=Gtk.ButtonsType.NONE, text=title)
        css(dialog, 'niri-bridge')
        dialog.format_secondary_text(description)
        dialog.add_button(_("Cancel"), Gtk.ResponseType.CANCEL)
        dialog.add_button(_(accept), Gtk.ResponseType.ACCEPT)
        result = dialog.run()
        dialog.destroy()
        return result == Gtk.ResponseType.ACCEPT

    def about_dialog(self, *_unused):
        dialog = Gtk.AboutDialog(transient_for=self, modal=True)
        dialog.set_program_name('NiriBridge')
        dialog.set_version((self.info or {}).get('version', ''))
        dialog.set_comments(_("Keyboard, mouse and native touchpad sharing for Niri.\nThis software comes with no warranty. You may use, modify and redistribute it under the GNU GPL version 3 or later."))
        dialog.set_copyright('Copyright © 2026 blackdream1890 and NiriBridge contributors')
        dialog.set_authors(['blackdream1890 and NiriBridge contributors'])
        dialog.set_website('https://github.com/blackdream1890/niri-bridge')
        dialog.set_license_type(Gtk.License.GPL_3_0)
        dialog.set_logo(GdkPixbuf.Pixbuf.new_from_file_at_scale(str(HERE / 'assets/niri-bridge.svg'), 96, 96, True))
        for location in (HERE.parent / 'LICENSE', HERE.parent / 'docs/LICENSE'):
            if location.is_file():
                dialog.set_license(location.read_text())
                dialog.set_wrap_license(True)
                break
        dialog.run()
        dialog.destroy()

    def help_dialog(self, *_unused):
        dialog = Gtk.MessageDialog(transient_for=self, modal=True, message_type=Gtk.MessageType.INFO, buttons=Gtk.ButtonsType.CLOSE, text=_("Using NiriBridge"))
        css(dialog, 'niri-bridge')
        dialog.format_secondary_text(_("1. Export a pairing file on each computer and import it on the other.\n2. One computer waits for a connection; the other connects to its LAN address.\n3. Start sharing, then drag the other computer in Screen connections and save.\n4. Cross the green edge to move the keyboard and gestures with the pointer.\n\nEmergency return: Ctrl + Alt + Shift + Escape\nSharing pauses when either computer locks. Physical input on the receiving computer restores local control.\n\nClosing the interface does not stop background sharing."))
        dialog.run()
        dialog.destroy()

    def setup_tray(self):
        application = self.get_application()
        if application.tray is not None:
            self.tray = application.tray
            self.tray.update('NiriBridge', (_('Open NiriBridge'), _('Pause sharing'), _('Quit the interface')), False)
            return
        try:
            if Tray.available():
                application.tray = Tray(HERE / 'assets/niri-bridge.svg',
                    lambda: application.window.present_from_user(),
                    lambda: application.window.toggle(), application.quit)
                application.hold()
                self.tray = application.tray
                self.tray.update('NiriBridge', (_('Open NiriBridge'), _('Pause sharing'), _('Quit the interface')), False)
        except (GLib.Error, OSError):
            self.tray = None

    def present_from_user(self):
        self.present()
        if hasattr(self.model, 'focus_interface'):
            self.executor.submit(self.model.focus_interface)

    def on_close(self, *_unused):
        if self.tray:
            self.hide()
            return True
        self.quit_ui()
        return True

    def quit_ui(self):
        self.get_application().quit()

    def on_destroy(self, *_unused):
        self.alive = False
        if hasattr(self, 'poll_source'):
            GLib.source_remove(self.poll_source)
        self.executor.shutdown(wait=False, cancel_futures=True)

    def render_pages(self):
        # Renders this application's widgets only; it does not touch the shared clipboard.
        import cairo
        self.render_dir.mkdir(parents=True, exist_ok=True)
        settings = Gtk.Settings.get_default()
        animations = settings.get_property('gtk-enable-animations')
        settings.set_property('gtk-enable-animations', False)
        transition = self.stack.get_transition_type()
        self.stack.set_transition_type(Gtk.StackTransitionType.NONE)
        names = iter(['overview', 'layout', 'pair', 'preferences'])
        def next_page():
            if not self.alive:
                return False
            name = next(names, None)
            if name is None:
                self.navigate('overview')
                self.stack.set_transition_type(transition)
                settings.set_property('gtk-enable-animations', animations)
                return False
            self.navigate(name)
            def capture():
                if not self.alive:
                    return False
                # A locked compositor may stop frame callbacks; allocate pending
                # widget changes explicitly before this offscreen QA drawing.
                self.check_resize()
                self.size_allocate(self.get_allocation())
                surface = cairo.ImageSurface(cairo.FORMAT_ARGB32, self.get_allocated_width(), self.get_allocated_height())
                self.draw(cairo.Context(surface))
                surface.write_to_png(str(self.render_dir / f'{name}.png'))
                GLib.timeout_add(120, next_page)
                return False
            GLib.timeout_add(350, capture)
            return False
        next_page()
        return False


class Application(Gtk.Application):
    def __init__(self, model, render_dir=None, language=None):
        super().__init__(application_id='org.niribridge.NiriBridge', flags=Gio.ApplicationFlags.FLAGS_NONE)
        self.model = model
        self.preferences_directory = Path(getattr(model, 'config', Path.home() / '.config/niri-bridge/config.toml')).parent
        configure(self.preferences_directory, language)
        self.render_dir = render_dir
        self.window = None
        self.tray = None
        self.connect('activate', self.activate_window)
        self.connect('shutdown', self.shutdown_ui)

    def change_language(self, language):
        state = self.window.editing_state()
        save_language(language, self.preferences_directory)
        old = self.window
        self.window = Window(self, self.model, restore_state=state)
        old.destroy()
        self.window.present_from_user()

    def activate_window(self, *_unused):
        if self.window is None:
            provider = Gtk.CssProvider()
            provider.load_from_path(str(HERE / 'style.css'))
            Gtk.StyleContext.add_provider_for_screen(Gdk.Screen.get_default(), provider, Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION)
            self.window = Window(self, self.model, self.render_dir)
        self.window.present_from_user()

    def shutdown_ui(self, *_unused):
        if self.tray:
            self.tray.close()
            self.tray = None
        if self.window and self.window.alive:
            self.window.on_destroy()


def main():
    parser = argparse.ArgumentParser(description=_("NiriBridge desktop settings"))
    parser.add_argument('--config', type=Path)
    parser.add_argument('--language', choices=('auto', 'en', 'zh_CN'), help='Interface language; the default follows desktop settings.')
    parser.add_argument('--render-dir', type=Path, help=argparse.SUPPRESS)
    args = parser.parse_args()
    application = Application(Model(config=args.config), args.render_dir, args.language)
    return application.run([sys.argv[0]])


if __name__ == '__main__':
    sys.exit(main())
