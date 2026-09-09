# SPDX-License-Identifier: GPL-3.0-or-later
"""UI behavior tests use simulated state and never call the live input service."""
import copy
import json
import os
from pathlib import Path
import tempfile
import time
import unittest
from unittest import mock
import gi
gi.require_version('Gtk', '3.0')
from gi.repository import Gtk, Gdk, GLib
from app import Application
from canvas import LayoutDraft, OPPOSITE
from model import endpoint, split_endpoint
from i18n import _


def fixture():
    edge_a = {'output': 'eDP-1', 'boundary': {'edge': 'top', 'start': 0., 'end': 1.}}
    edge_b = {'output': 'DP-1', 'boundary': {'edge': 'bottom', 'start': .05, 'end': .55}}
    local = {'edge': edge_a, 'revision': 'a' * 64, 'outputs': {'eDP-1': {'x': 0, 'y': 0, 'width': 1920, 'height': 1200, 'scale': 1.6}}}
    peer = {'edge': edge_b, 'revision': 'b' * 64, 'outputs': {
        'DP-1': {'x': 0, 'y': 0, 'width': 2648, 'height': 1489, 'scale': 1.45},
        'DP-2': {'x': -1490, 'y': -388, 'width': 1489, 'height': 2648, 'scale': 1.45}}}
    certificate = {'name': 'laptop', 'fingerprint': '0123456789abcdef' * 4, 'expires': '2027-09-08', 'pem': 'public-certificate-fixture'}
    config = {'revision': 'a' * 64, 'edge': edge_a, 'peer_name': 'desktop',
              'identity': certificate, 'peer': dict(certificate, name='desktop', fingerprint='fedcba9876543210' * 4),
              'settings': {'connection': {'mode': 'connect', 'address': 'desktop.local:42420'},
                           'activity_devices': ['/dev/input/by-path/test-keyboard', '/dev/input/by-path/test-touchpad'], 'native_touchpads': True}}
    return {'version': '0.2.0-beta.1', 'config': config, 'outputs': local['outputs'], 'service': {'ActiveState': 'active', 'SubState': 'running', 'UnitFileState': 'enabled'},
            'status': {'connection': 'connected', 'role': 'local', 'local_unlocked': True, 'peer_unlocked': True,
                       'peer_name': 'desktop', 'latency_ms': 1.6, 'local': local, 'peer': peer, 'configuring': False},
            'devices': {'uinput_writable': True, 'devices': [
                {'name': 'Built-in keyboard', 'path': '/dev/input/by-path/test-keyboard', 'classes': ['ID_INPUT_KEYBOARD'], 'readable': True, 'available': True, 'selected': True},
                {'name': 'Precision touchpad', 'path': '/dev/input/by-path/test-touchpad', 'classes': ['ID_INPUT_TOUCHPAD'], 'readable': True, 'available': True, 'selected': True}]}}


class FakeModel:
    def __init__(self):
        self.directory = tempfile.TemporaryDirectory(prefix="niri-bridge-ui-test-")
        self.config = Path(self.directory.name) / "config.toml"
        self.data = fixture()
        self.actions = []
    def load(self):
        return copy.deepcopy(self.data)
    def poll(self):
        return {k: copy.deepcopy(self.data[k]) for k in ('service', 'status')}
    def set_running(self, running):
        self.actions.append(('running', running))
        self.data['service']['ActiveState'] = 'active' if running else 'inactive'
        return self.poll()
    def set_autostart(self, enabled):
        self.actions.append(('autostart', enabled))
        self.data['service']['UnitFileState'] = 'enabled' if enabled else 'disabled'
        return self.poll()
    def apply_layout(self, request):
        self.actions.append(('layout', copy.deepcopy(request)))
        self.data['status']['local']['edge'] = request['local_edge']
        self.data['status']['peer']['edge'] = request['peer_edge']
        return {'reconnecting': True}
    def release(self):
        self.actions.append(('release',))
        self.data['status']['role'] = 'local'
        return {}
    def save_settings(self, revision, settings):
        self.actions.append(('settings', revision, settings))
        self.data['config']['settings'] = settings
        return {}


def pump(until=lambda: False, timeout=4):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        while Gtk.events_pending():
            Gtk.main_iteration_do(False)
        if until():
            return
        time.sleep(.01)
    if not until():
        raise AssertionError('UI condition not reached')


class GeometryTests(unittest.TestCase):
    def test_dragging_changes_both_ends_and_preserves_existing_monitor_topology(self):
        f = fixture()
        d = LayoutDraft(f['status']['local'], f['status']['peer'])
        topology = copy.deepcopy(d.peer['outputs'])
        for x, y, edge in [(0, 2000, 'bottom'), (-4000, 0, 'left'), (4000, 0, 'right'), (0, -3000, 'top')]:
            d.finish_drag(x, y)
            self.assertEqual(d.local['edge']['boundary']['edge'], edge)
            self.assertEqual(d.peer['edge']['boundary']['edge'], OPPOSITE[edge])
            for node in (d.local, d.peer):
                b = node['edge']['boundary']
                self.assertTrue(0 <= b['start'] < b['end'] <= 1)
            self.assertEqual(d.peer['outputs'], topology)
    def test_addresses_round_trip_ipv4_dns_and_ipv6(self):
        for host in ['192.0.2.10', 'desktop.local', '2001:db8::1']:
            self.assertEqual(split_endpoint(endpoint(host, 42420)), (host, 42420))


class WidgetTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if not Gtk.init_check()[0]:
            raise unittest.SkipTest('A GTK display is required')
        cls.model = FakeModel()
        cls.app = Application(cls.model, language=os.environ.get("NIRI_BRIDGE_UI_TEST_LANGUAGE", "en"))
        cls.app.set_application_id('org.niribridge.NiriBridge.Tests')
        cls.app.register(None)
        cls.tray_patch = mock.patch('app.Window.setup_tray')
        cls.tray_patch.start()
        cls.app.activate()
        cls.win = cls.app.window
        cls.win.get_titlebar().set_title('NiriBridge · UI test')
        pump(lambda: cls.win.info is not None and not cls.win.busy)
    @classmethod
    def tearDownClass(cls):
        cls.app.window.destroy()
        cls.app.quit()
        cls.model.directory.cleanup()
        cls.tray_patch.stop()
    def test_01_initial_load_does_not_modify_settings_or_startup(self):
        self.assertFalse(self.win.settings_dirty)
        self.assertFalse(self.win.layout_dirty)
        self.assertEqual(self.model.actions, [])
        self.assertEqual(self.win.hero_title.get_text(), _('Your computers are connected'))
    def test_02_pause_and_resume_use_the_real_button_handlers(self):
        self.win.toggle_button.emit('clicked')
        pump(lambda: not self.win.busy)
        self.assertEqual(self.model.actions[-1], ('running', False))
        self.assertEqual(self.win.toggle_button.get_label(), _('Start sharing'))
        self.win.toggle_button.emit('clicked')
        pump(lambda: not self.win.busy)
        self.assertEqual(self.model.actions[-1], ('running', True))
    def test_03_screen_edit_submits_both_revisions_and_edges(self):
        self.win.navigate('layout')
        self.win.edge_combo.set_active_id('bottom')
        self.assertTrue(self.win.layout_dirty)
        self.win.layout_save.emit('clicked')
        pump(lambda: not self.win.busy)
        calls = [action for action in self.model.actions if action[0] == 'layout']
        request = calls[-1][1]
        self.assertEqual(request['local_edge']['boundary']['edge'], 'bottom')
        self.assertEqual(request['peer_edge']['boundary']['edge'], 'top')
        self.assertEqual(request['local_revision'], 'a' * 64)
        self.assertEqual(request['peer_revision'], 'b' * 64)
    def test_04_layout_and_status_render_at_laptop_size(self):
        self.win.resize(1000, 800)
        self.win.render_dir = Path(tempfile.mkdtemp(prefix='niri-bridge-ui-qa-'))
        self.win.render_pages()
        pump(lambda: (self.win.render_dir / 'preferences.png').exists(), timeout=5)
        for name in ['overview', 'layout', 'pair', 'preferences']:
            self.assertGreater((self.win.render_dir / f'{name}.png').stat().st_size, 5000)
        self.assertEqual(len({(self.win.render_dir / f'{name}.png').read_bytes()
                              for name in ['overview', 'layout', 'pair', 'preferences']}), 4)
        print('UI_QA_DIRECTORY=' + str(self.win.render_dir))


    def test_05_language_change_preserves_unsaved_settings_without_backend_changes(self):
        self.win.host_entry.set_text('edited-desktop.local')
        self.assertTrue(self.win.settings_dirty)
        before = list(self.model.actions)
        self.app.change_language('zh_CN')
        pump(lambda: self.app.window.info is not None and not self.app.window.busy)
        new = self.app.window
        self.assertEqual(new.host_entry.get_text(), 'edited-desktop.local')
        self.assertTrue(new.settings_dirty)
        self.assertEqual(new.nav['overview'].get_label(), '总览')
        self.assertEqual(self.model.actions, before)
        self.app.change_language('en')
        pump(lambda: self.app.window.info is not None and not self.app.window.busy)
        self.assertEqual(self.app.window.nav['overview'].get_label(), 'Overview')
        self.assertEqual(self.app.window.host_entry.get_text(), 'edited-desktop.local')
        self.assertEqual(self.model.actions, before)

    def test_06_pairing_requires_checked_fingerprint_and_binds_import_to_it(self):
        window = self.app.window
        certificate = self.model.data['config']['peer']
        self.model.import_peer = mock.Mock(return_value={})
        observed = []
        def review(checked, response):
            def respond():
                dialog = next(w for w in Gtk.Window.list_toplevels()
                              if isinstance(w, Gtk.Dialog) and w.get_transient_for() == window)
                accept = dialog.get_widget_for_response(Gtk.ResponseType.ACCEPT)
                observed.append(not accept.get_sensitive())
                def descendants(widget):
                    yield widget
                    if isinstance(widget, Gtk.Container):
                        for child in widget.get_children():
                            yield from descendants(child)
                checkbox = next(w for w in descendants(dialog) if isinstance(w, Gtk.CheckButton))
                checkbox.set_active(checked)
                dialog.response(response)
                return False
            GLib.idle_add(respond)
            window.review_peer(Path('/unused-public-pairing.pem'), certificate)
        review(False, Gtk.ResponseType.ACCEPT)
        review(True, Gtk.ResponseType.CANCEL)
        self.model.import_peer.assert_not_called()
        review(True, Gtk.ResponseType.ACCEPT)
        pump(lambda: not window.busy)
        self.assertTrue(all(observed))
        self.model.import_peer.assert_called_once_with(Path('/unused-public-pairing.pem'),
            self.model.data['config']['revision'], certificate['fingerprint'])


    def test_07_first_setup_preserves_selected_devices_and_name_across_language_change(self):
        before = list(self.model.actions)
        self.model.data['config'] = None
        self.model.data['status'] = None
        self.model.data['service']['ActiveState'] = 'inactive'
        self.app.change_language('en')
        pump(lambda: self.app.window.info is not None and not self.app.window.busy)
        window = self.app.window
        self.assertEqual(window.stack.get_visible_child_name(), 'setup')
        self.assertEqual(self.model.actions, before)

        window.setup_name.set_text('test-laptop')
        for index, (check, device) in enumerate(window.setup_device_checks):
            check.set_active(index == 0)
        selected = window.editing_state()['setup_devices']
        self.app.change_language('zh_CN')
        pump(lambda: self.app.window.info is not None and not self.app.window.busy)
        self.assertEqual(self.app.window.setup_name.get_text(), 'test-laptop')
        self.assertEqual(self.app.window.editing_state()['setup_devices'], selected)
        self.assertEqual(self.model.actions, before)

    def test_08_about_dialog_exposes_license_and_public_authorship(self):
        observed = {}
        def close():
            dialog = next(w for w in Gtk.Window.list_toplevels() if isinstance(w, Gtk.AboutDialog))
            observed['license'] = dialog.get_license()
            observed['copyright'] = dialog.get_copyright()
            dialog.response(Gtk.ResponseType.CLOSE)
            return False
        GLib.idle_add(close)
        self.app.window.about_dialog()
        self.assertIn('GNU GENERAL PUBLIC LICENSE', observed['license'])
        self.assertIn('blackdream1890', observed['copyright'])



if __name__ == '__main__':
    unittest.main(verbosity=2)
