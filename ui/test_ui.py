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
from app import Application, Window
from canvas import LayoutDraft, OPPOSITE
from model import Model, OperationError, endpoint, split_endpoint
from i18n import _


def fixture():
    edge_a = {'id': 'default', 'output': 'eDP-1', 'boundary': {'edge': 'top', 'start': 0., 'end': 1.}}
    edge_b = {'id': 'default', 'output': 'DP-1', 'boundary': {'edge': 'bottom', 'start': .05, 'end': .55}}
    local = {'edges': [edge_a], 'revision': 'a' * 64, 'outputs': {'eDP-1': {'x': 0, 'y': 0, 'width': 1920, 'height': 1200, 'scale': 1.6}}}
    peer = {'edges': [edge_b], 'revision': 'b' * 64, 'outputs': {
        'DP-1': {'x': 0, 'y': 0, 'width': 2648, 'height': 1489, 'scale': 1.45},
        'DP-2': {'x': -1490, 'y': -388, 'width': 1489, 'height': 2648, 'scale': 1.45}}}
    certificate = {'name': 'laptop', 'fingerprint': '0123456789abcdef' * 4, 'expires': '2027-09-08', 'pem': 'public-certificate-fixture'}
    config = {'revision': 'a' * 64, 'edges': [edge_a], 'peer_name': 'desktop',
              'identity': certificate, 'peer': dict(certificate, name='desktop', fingerprint='fedcba9876543210' * 4),
              'settings': {'connection': {'mode': 'connect', 'address': 'desktop.local:42420'},
                           'activity_devices': ['/dev/input/by-path/test-keyboard', '/dev/input/by-path/test-touchpad'], 'native_touchpads': True}}
    return {'version': '0.2.0-beta.3', 'config': config, 'outputs': local['outputs'], 'service': {'ActiveState': 'active', 'SubState': 'running', 'UnitFileState': 'enabled'},
            'status': {'connection': 'connected', 'role': 'local', 'local_unlocked': True, 'peer_unlocked': True,
                       'peer_name': 'desktop', 'latency_ms': 1.6, 'local': local, 'peer': peer, 'configuring': False},
            'devices': {'uinput_writable': True, 'devices': [
                {'name': 'Built-in keyboard', 'path': '/dev/input/by-path/test-keyboard', 'classes': ['ID_INPUT_KEYBOARD'], 'readable': True, 'writable': True, 'available': True, 'selected': True},
                {'name': 'Precision touchpad', 'path': '/dev/input/by-path/test-touchpad', 'classes': ['ID_INPUT_TOUCHPAD'], 'readable': True, 'writable': True, 'available': True, 'selected': True}]}}


class FakeModel:
    def __init__(self):
        self.directory = tempfile.TemporaryDirectory(prefix="niri-bridge-ui-test-")
        self.config = Path(self.directory.name) / "config.toml"
        self.data = fixture()
        self.actions = []
    def load(self):
        return copy.deepcopy(self.data)
    def poll(self):
        return {k: copy.deepcopy(self.data[k]) for k in ('service', 'status')} | {'autostart': self.data.get('autostart', True)}
    def set_running(self, running):
        self.actions.append(('running', running))
        self.data['service']['ActiveState'] = 'active' if running else 'inactive'
        return self.poll()
    def stop_for_exit(self):
        return self.set_running(False)
    def set_autostart(self, enabled):
        self.actions.append(('autostart', enabled))
        self.data['autostart'] = enabled
        return self.poll()
    def apply_layout(self, request):
        self.actions.append(('layout', copy.deepcopy(request)))
        self.data['status']['local']['edges'] = request['local_edges']
        self.data['status']['peer']['edges'] = request['peer_edges']
        self.data['config']['edges'] = request['local_edges']
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

    def test_second_connection_uses_the_portrait_edge_without_overwriting_the_first(self):
        f = fixture()
        draft = LayoutDraft(f['status']['local'], f['status']['peer'])
        original = copy.deepcopy(draft.request())
        identifier = draft.add_connection()
        self.assertEqual(draft.local['edge']['boundary']['edge'], 'left')
        self.assertEqual(draft.peer['edge']['output'], 'DP-2')
        self.assertEqual(draft.peer['edge']['boundary']['edge'], 'right')
        self.assertGreater(draft.peer['edge']['boundary']['start'], .5)
        self.assertEqual(draft.local['edges'][0], original['local_edges'][0])
        self.assertEqual(draft.peer['edges'][0], original['peer_edges'][0])
        self.assertEqual(draft.local['edge']['id'], draft.peer['edge']['id'])
        self.assertIsNone(draft.validation_error())
        draft.select('default')
        draft.select(identifier)
        draft.local['edge']['boundary'] = copy.deepcopy(draft.local['edges'][0]['boundary'])
        self.assertEqual(draft.validation_error(), 'layout_overlap')
        draft.remove_connection()
        self.assertEqual(draft.request(), original)

    def test_missing_output_is_reported_without_silently_replacing_it(self):
        f = fixture()
        local = f['status']['local']
        local['outputs']['replacement'] = local['outputs'].pop('eDP-1')
        draft = LayoutDraft(local, f['status']['peer'])
        self.assertEqual(draft.local['edge']['output'], 'eDP-1')
        self.assertEqual(draft.validation_error(), 'output_unavailable')

    def test_old_backend_keeps_single_connection_edits_and_rejects_multiple(self):
        f = fixture()
        for node in (f['status']['local'], f['status']['peer']):
            node['edge'] = node.pop('edges')[0]
            node['edge'].pop('id')
        draft = LayoutDraft(f['status']['local'], f['status']['peer'])
        self.assertFalse(draft.supports_multiple)
        model = Model(config=Path('/unused-test-config'))
        model.rpc = mock.Mock(side_effect=[f['status'], {}])
        request = copy.deepcopy(draft.request())
        model.apply_layout(request)
        sent = model.rpc.call_args_list[-1].args[0]['layout']
        self.assertIn('local_edge', sent)
        self.assertNotIn('id', sent['local_edge'])
        self.assertIn('local_edges', request)
        draft.add_connection()
        model.rpc = mock.Mock(return_value=f['status'])
        with self.assertRaises(OperationError) as result:
            model.apply_layout(draft.request())
        self.assertEqual(result.exception.code, 'backend_update_required')
        self.assertEqual(model.rpc.call_count, 1)


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
        self.win.boundary_combos['peer'].set_active_id('top')
        self.assertTrue(self.win.layout_dirty)
        self.win.layout_save.emit('clicked')
        pump(lambda: not self.win.busy)
        calls = [action for action in self.model.actions if action[0] == 'layout']
        request = calls[-1][1]
        self.assertEqual(request['local_edges'][0]['boundary']['edge'], 'bottom')
        self.assertEqual(request['peer_edges'][0]['boundary']['edge'], 'top')
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

    def test_09_multiple_connection_drafts_survive_selection_and_language_changes(self):
        self.model.data = fixture()
        self.app.window.settings_dirty = self.app.window.layout_dirty = False
        self.app.change_language('en')
        pump(lambda: self.app.window.info is not None and not self.app.window.busy)
        window = self.app.window
        window.navigate('layout')
        before = list(self.model.actions)
        first = copy.deepcopy(window.editor.draft.request())
        window.connection_add.emit('clicked')
        selected = window.editor.draft.selected_id
        self.assertEqual(len(window.editor.draft.connection_ids()), 2)
        window.boundary_combos['local'].set_active_id('top')
        self.assertFalse(window.layout_save.get_sensitive())
        window.boundary_combos['local'].set_active_id('left')
        window.range_spins['peer', 'start'].set_value(75)
        window.connection_combo.set_active_id('default')
        window.connection_combo.set_active_id(selected)
        self.assertEqual(window.range_spins['peer', 'start'].get_value(), 75)
        self.app.change_language('zh_CN')
        pump(lambda: self.app.window.info is not None and not self.app.window.busy)
        window = self.app.window
        self.assertEqual(window.editor.draft.selected_id, selected)
        self.assertEqual(window.range_spins['peer', 'start'].get_value(), 75)
        self.assertEqual(self.model.actions, before)
        self.assertEqual(window.editor.draft.local['edges'][0], first['local_edges'][0])
        self.assertEqual(window.editor.draft.peer['edges'][0], first['peer_edges'][0])
        self.assertTrue(window.layout_save.get_sensitive())
        window.layout_save.emit('clicked')
        pump(lambda: not window.busy)
        request = [a[1] for a in self.model.actions if a[0] == 'layout'][-1]
        self.assertEqual(len(request['local_edges']), 2)
        self.assertEqual({e['id'] for e in request['local_edges']}, {e['id'] for e in request['peer_edges']})
        window.connection_combo.set_active_id(selected)
        window.connection_remove.emit('clicked')
        self.assertEqual(window.editor.draft.request(), first)
        self.assertFalse(window.connection_remove.get_sensitive())
        self.assertTrue(window.layout_dirty)
        window.reset_layout()

    def test_10_reconnection_does_not_turn_old_peer_metadata_into_an_unsaved_draft(self):
        window = self.app.window
        window.layout_dirty = False
        complete = self.model.poll()
        identifier = complete['status']['local']['edges'][1]['id']
        window.connection_combo.set_active_id(identifier)
        before = list(self.model.actions)
        transient = copy.deepcopy(complete)
        transient['status']['peer']['edges'] = transient['status']['peer']['edges'][:1]
        transient['status']['peer_unlocked'] = None
        window.update_status(transient)
        self.assertFalse(window.layout_dirty)
        self.assertFalse(window.layout_save.get_sensitive())
        transient['status']['peer'] = None
        transient['status']['connection'] = 'connecting'
        window.update_status(transient)
        window.update_status(complete)
        self.assertFalse(window.layout_dirty)
        self.assertEqual(window.editor.draft.selected_id, identifier)
        self.assertEqual(window.editor.draft.peer['edge'], next(edge for edge in complete['status']['peer']['edges'] if edge['id'] == identifier))
        self.assertEqual(self.model.actions, before)

    def test_11_opening_an_idle_interface_does_not_start_sharing(self):
        model = FakeModel()
        model.data['service']['ActiveState'] = 'inactive'
        model.data['status'] = None
        window = Window(self.app, model)
        try:
            pump(lambda: window.info is not None and not window.busy)
            self.assertEqual(model.actions, [])
            self.assertEqual(window.toggle_button.get_label(), _('Start sharing'))
            window.toggle_button.emit('clicked')
            pump(lambda: not window.busy)
            self.assertEqual(model.actions, [('running', True)])
        finally:
            window.destroy()
            model.directory.cleanup()

    def test_12_tray_exit_stops_sharing_before_quitting_and_keeps_ui_on_failure(self):
        application = self.app
        window = application.window
        window.settings_dirty = window.layout_dirty = False
        before = len(self.model.actions)
        with mock.patch.object(application, 'quit') as quit_application:
            with mock.patch.object(self.model, 'stop_for_exit', side_effect=OperationError('stop_failed')):
                application.request_quit()
                pump(lambda: not window.busy)
            quit_application.assert_not_called()
            self.assertTrue(window.alive)
            self.assertFalse(application.quit_pending)
            self.assertEqual(len(self.model.actions), before)

            def quit_after_stop():
                self.assertEqual(self.model.actions[-1], ('running', False))
                self.assertEqual(self.model.data['service']['ActiveState'], 'inactive')

            quit_application.side_effect = quit_after_stop
            application.request_quit()
            pump(lambda: quit_application.called)
            self.assertEqual(quit_application.call_count, 1)
        application.quit_pending = application.backend_stopped = False

    def test_13_window_close_retains_the_tray_and_existing_sharing(self):
        window = self.app.window
        before = list(self.model.actions)
        with mock.patch.object(window, 'tray', object()), mock.patch.object(window, 'hide') as hide, mock.patch.object(self.app, 'request_quit') as quit_application:
            self.assertTrue(window.on_close())
            hide.assert_called_once()
            quit_application.assert_not_called()
        self.assertEqual(self.model.actions, before)



if __name__ == '__main__':
    unittest.main(verbosity=2)
