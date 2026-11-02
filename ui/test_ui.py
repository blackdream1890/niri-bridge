# SPDX-License-Identifier: GPL-3.0-or-later
"""UI behavior tests use simulated state and never call the live input service."""
import copy
import fcntl
import gc
import hashlib
import weakref
import json
import os
import subprocess
from pathlib import Path
import tempfile
import threading
import time
import unittest
from unittest import mock
import gi
gi.require_version('Gtk', '3.0')
from gi.repository import Gtk, Gdk, GLib
from app import Application, Window
from canvas import LayoutDraft, OPPOSITE
from model import Model, OperationError, endpoint, lan_peer_address, split_endpoint
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
              'paired_devices': [
                  {'name': 'desktop', 'fingerprint': 'fedcba9876543210' * 4, 'expires': '2027-09-08', 'active': True},
                  {'name': 'office', 'fingerprint': '1234567890abcdef' * 4, 'expires': '2027-09-08', 'active': False}],
              'settings': {'connection': {'mode': 'connect', 'address': 'desktop.local:42420'},
                           'activity_devices': ['/dev/input/by-path/test-keyboard', '/dev/input/by-path/test-touchpad'], 'native_touchpads': True}}
    return {'version': '1.0.0-rc.1', 'config': config, 'outputs': local['outputs'],
            'network': {'addresses': [{'address': '192.168.50.10', 'interface': 'wlan-test'}], 'port': 42420},
            'service': {'ActiveState': 'active', 'SubState': 'running', 'UnitFileState': 'enabled'},
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

    def test_connect_rejects_unspecified_addresses_but_listen_keeps_them(self):
        for host in ('0.0.0.0', '::', '0:0:0:0:0:0:0:0', '[::]'):
            with self.subTest(host=host):
                with self.assertRaises(OperationError) as error:
                    endpoint(host, 42420, 'connect')
                self.assertEqual(error.exception.code, 'peer_address_required')
                self.assertTrue(endpoint(host, 42420, 'listen').endswith(':42420'))
        with self.assertRaises(OperationError) as error:
            endpoint('  ', 42420, 'connect')
        self.assertEqual(error.exception.code, 'peer_address_required')

    def test_connect_preserves_loopback_dns_and_other_ip_addresses(self):
        for host in ('127.0.0.1', '::1', '192.0.2.10', '2001:db8::1', 'desktop.local'):
            with self.subTest(host=host):
                self.assertEqual(split_endpoint(endpoint(host, 42420, 'connect')), (host, 42420))

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


class FileChooserTests(unittest.TestCase):
    def setUp(self):
        if not Gtk.init_check()[0]:
            self.skipTest('A GTK display is required')
        self.model = FakeModel()
        with mock.patch('app.Window.setup_tray'):
            self.window = Window(None, self.model)
        pump(lambda: self.window.info is not None and not self.window.busy)
        self.window.navigate('pair')
        self.references = []
        create = Gtk.FileChooserNative.new
        def track(*args):
            dialog = create(*args)
            self.references.append(weakref.ref(dialog))
            return dialog
        self.factory = mock.patch('app.Gtk.FileChooserNative.new', side_effect=track)
        self.factory.start()

    def tearDown(self):
        self.window.destroy()
        self.factory.stop()
        self.model.directory.cleanup()

    def open_chooser(self, button):
        button.emit('clicked')
        gc.collect()
        dialog = self.references[-1]()
        self.assertIsNotNone(dialog, 'The native chooser was collected before the user could respond')
        self.assertTrue(dialog.get_visible())
        return dialog

    def test_export_stays_open_and_writes_only_the_public_pairing_file(self):
        dialog = self.open_chooser(self.window.export_button)
        destination = Path(self.model.directory.name) / 'export.pem'
        dialog.set_current_folder(str(destination.parent))
        dialog.set_current_name(destination.name)
        pump(lambda: dialog.get_filename() == str(destination))
        dialog.emit('response', Gtk.ResponseType.ACCEPT)
        pump(lambda: destination.exists() and not self.window.busy)
        self.assertEqual(destination.read_text(), self.model.data['config']['identity']['pem'])
        self.assertIsNone(self.window.file_chooser)
        self.assertEqual(self.model.actions, [])

    def test_import_cancel_and_reopen_then_review_without_changing_pairing(self):
        self.model.inspect_certificate = mock.Mock(return_value=self.model.data['config']['peer'])
        with mock.patch.object(self.window, 'review_peer') as review:
            dialog = self.open_chooser(self.window.import_button)
            self.window.import_button.emit('clicked')
            self.window.export_button.emit('clicked')
            self.assertEqual(len(self.references), 1)
            dialog.emit('response', Gtk.ResponseType.CANCEL)
            self.assertIsNone(self.window.file_chooser)
            self.model.inspect_certificate.assert_not_called()
            review.assert_not_called()
            candidate = Path(self.model.directory.name) / 'peer.pem'
            candidate.write_text('synthetic public pairing file')
            dialog = self.open_chooser(self.window.import_button)
            dialog.set_filename(str(candidate))
            pump(lambda: dialog.get_filename() == str(candidate))
            dialog.emit('response', Gtk.ResponseType.ACCEPT)
            pump(lambda: review.called)
            self.model.inspect_certificate.assert_called_once_with(candidate)
            review.assert_called_once_with(candidate, self.model.data['config']['peer'])
            self.assertIsNone(self.window.file_chooser)
            self.assertEqual(self.model.actions, [])

    def test_destroying_parent_closes_pending_chooser_without_exporting(self):
        dialog = self.open_chooser(self.window.export_button)
        destination = Path(self.model.directory.name) / 'cancelled.pem'
        dialog.set_current_folder(str(destination.parent))
        dialog.set_current_name(destination.name)
        pump(lambda: dialog.get_filename() == str(destination))
        self.window.destroy()
        self.assertFalse(dialog.get_visible())
        self.assertIsNone(self.window.file_chooser)
        dialog.emit('response', Gtk.ResponseType.ACCEPT)
        self.assertFalse(destination.exists())


class ConnectionAddressWidgetTests(unittest.TestCase):
    def setUp(self):
        if not Gtk.init_check()[0]:
            self.skipTest('A GTK display is required')
        self.model = FakeModel()
        with mock.patch('app.Window.setup_tray'):
            self.window = Window(None, self.model)
        pump(lambda: self.window.info is not None and not self.window.busy)
        self.window.navigate('pair')

    def tearDown(self):
        self.window.destroy()
        self.model.directory.cleanup()

    def test_user_switch_to_connect_clears_only_the_listening_wildcard(self):
        window = self.window
        for host in ('0.0.0.0', '::', '0:0:0:0:0:0:0:0'):
            with self.subTest(host=host):
                window.connection_mode.set_active_id('listen')
                window.host_entry.set_text(host)
                self.assertEqual(window.current_settings()['connection']['mode'], 'listen')
                window.connection_mode.set_active_id('connect')
                self.assertEqual(window.host_entry.get_text(), '')
                self.assertEqual(window.notice_label.get_text(), str(OperationError('peer_address_required')))
        for host in ('desktop.local', '192.0.2.10', '::1'):
            with self.subTest(host=host):
                window.connection_mode.set_active_id('listen')
                window.host_entry.set_text(host)
                window.connection_mode.set_active_id('connect')
                self.assertEqual(window.host_entry.get_text(), host)
        self.assertEqual(self.model.actions, [])

    def test_loading_an_old_invalid_connect_address_keeps_it_visible_for_repair(self):
        window = self.window
        for address, host in (('0.0.0.0:42420', '0.0.0.0'), ('[::]:42420', '::')):
            with self.subTest(address=address):
                self.model.data['config']['settings']['connection'] = {'mode': 'connect', 'address': address}
                window.settings_dirty = False
                window.populate(self.model.load())
                self.assertEqual(window.host_entry.get_text(), host)
                self.assertEqual(window.connection_mode.get_active_id(), 'connect')
                self.assertFalse(window.settings_dirty)
                self.assertEqual(self.model.data['config']['settings']['connection']['address'], address)
        self.assertEqual(self.model.actions, [])

    def test_invalid_connect_save_never_calls_model_or_restarts_sharing(self):
        window = self.window
        self.model.save_settings = mock.Mock()
        for host in ('0.0.0.0', '::', ''):
            with self.subTest(host=host):
                window.host_entry.set_text(host)
                window.network_save.emit('clicked')
                self.assertFalse(window.busy)
                self.assertEqual(window.notice_label.get_text(), str(OperationError('peer_address_required')))
        self.model.save_settings.assert_not_called()
        self.assertEqual(self.model.actions, [])

    def test_invalid_and_valid_address_drafts_survive_language_rebuild_state(self):
        for host in ('0.0.0.0', '::', 'new-desktop.local'):
            with self.subTest(host=host):
                self.window.host_entry.set_text(host)
                state = self.window.editing_state()
                with mock.patch('app.Window.setup_tray'):
                    restored = Window(None, self.model, restore_state=state)
                try:
                    pump(lambda: restored.info is not None and not restored.busy)
                    self.assertEqual(restored.host_entry.get_text(), host)
                    self.assertEqual(restored.connection_mode.get_active_id(), 'connect')
                    self.assertTrue(restored.settings_dirty)
                finally:
                    restored.destroy()
        self.assertEqual(self.model.actions, [])

    def test_backend_invalid_peer_address_explains_the_required_action(self):
        data = self.model.poll()
        data['status']['reason'] = 'peer_address_required'
        data['status']['connection'] = 'connecting'
        data['status']['local_unlocked'] = False
        self.window.update_status(data)
        self.assertEqual(self.window.hero_title.get_text(), _("Enter the other computer's address"))
        self.assertEqual(self.window.hero_description.get_text(), str(OperationError('peer_address_required')))
        self.assertEqual(self.window.header_badge.get_text(), _('Address required'))
        self.assertEqual(self.model.actions, [])


class LanFirewallModelTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.model = Model(config=Path(self.directory.name) / 'config.toml', helper='/synthetic/input-helper')
        self.model.config.write_text('# synthetic configuration')
        self.view = fixture()['config']
        self.view['settings']['connection'] = {'mode': 'listen', 'address': '0.0.0.0:43111'}
        self.model.manage = mock.Mock(side_effect=lambda *_args: copy.deepcopy(self.view))
        self.plan = {'version': 1, 'kind': 'lan-firewall', 'peer': '192.168.50.20',
                     'local': '192.168.50.10', 'interface': 'wlan-test', 'port': 43111}
        self.raw = json.dumps(self.plan, indent=1).encode() + b'\n'
        self.digest = hashlib.sha256(self.raw).hexdigest()
        self.calls = []
        self.privileged_result = (0, {'ok': True, 'already_allowed': False})
        self.plan_error = None
        self.plan_mode = 0o600
        self.during_authorization = None
        def run(arguments, **_kwargs):
            self.calls.append(arguments)
            if arguments[0] == 'pkexec':
                self.assertEqual(arguments[:4], ['pkexec', '/usr/bin/python3', self.model.helper, 'firewall-install'])
                path = Path(arguments[arguments.index('--plan') + 1])
                self.assertEqual(path.read_bytes(), self.raw)
                self.assertEqual(path.stat().st_mode & 0o777, 0o600)
                self.assertEqual(arguments[-2:], ['--sha256', self.digest])
                if self.during_authorization:
                    self.during_authorization()
                code, reply = self.privileged_result
                return subprocess.CompletedProcess(arguments, code, json.dumps(reply), 'untrusted raw stderr')
            self.assertEqual(arguments[:3], ['/usr/bin/python3', self.model.helper, 'firewall-plan'])
            self.assertEqual(arguments[arguments.index('--config') + 1], str(self.model.config))
            self.assertEqual(arguments[arguments.index('--peer') + 1], self.plan['peer'])
            if self.plan_error:
                return subprocess.CompletedProcess(arguments, 1, json.dumps({'ok': False, 'error': self.plan_error}), 'untrusted raw stderr')
            path = Path(arguments[arguments.index('--output') + 1])
            path.write_bytes(self.raw)
            path.chmod(self.plan_mode)
            return subprocess.CompletedProcess(arguments, 0,
                json.dumps({'ok': True, 'plan': self.plan, 'sha256': self.digest}), '')
        self.runner = mock.patch('model.subprocess.run', side_effect=run).start()
        self.addCleanup(mock.patch.stopall)

    def prepare(self):
        return self.model.prepare_firewall(self.plan['peer'], self.view['revision'], self.view['peer']['fingerprint'])

    def test_plan_and_privilege_bind_exact_bytes_port_and_digest_without_service_changes(self):
        prepared = self.prepare()
        self.assertEqual(len(self.calls), 1)
        self.assertEqual(prepared['plan']['port'], 43111)
        self.assertEqual(prepared['bytes'], self.raw)
        self.assertEqual(prepared['sha256'], self.digest)
        result = self.model.grant_firewall(prepared)
        self.assertTrue(result['ok'])
        self.assertEqual(len(self.calls), 2)
        self.assertEqual(self.calls[-1][0], 'pkexec')

    def test_invalid_peer_addresses_never_prepare_or_authorize(self):
        for peer in ('', '8.8.8.8', '172.32.0.1', '127.0.0.1', '169.254.1.2', '::1', '192.168.1.2/24', '192.168.1.2:43111'):
            with self.subTest(peer=peer), self.assertRaises(OperationError) as error:
                self.model.prepare_firewall(peer, self.view['revision'], self.view['peer']['fingerprint'])
            self.assertEqual(error.exception.code, 'firewall_peer_invalid')
        self.assertEqual(self.calls, [])
        self.model.manage.assert_not_called()
        for peer in ('10.0.0.2', '172.16.0.2', '192.168.1.2'):
            self.assertEqual(lan_peer_address(peer), peer)

    def test_changed_revision_or_peer_before_prepare_has_no_helper_call(self):
        for revision, fingerprint in (('changed', self.view['peer']['fingerprint']), (self.view['revision'], 'changed')):
            with self.subTest(revision=revision), self.assertRaises(OperationError) as error:
                self.model.prepare_firewall(self.plan['peer'], revision, fingerprint)
            self.assertEqual(error.exception.code, 'settings_conflict')
        self.assertEqual(self.calls, [])

    def test_changed_revision_after_prepare_cannot_authorize(self):
        prepared = self.prepare()
        self.view['revision'] = 'b' * 64
        with self.assertRaises(OperationError) as error:
            self.model.grant_firewall(prepared)
        self.assertEqual(error.exception.code, 'settings_conflict')
        self.assertEqual(len(self.calls), 1)

    def test_changed_selection_during_preparation_does_not_return_a_plan(self):
        before = copy.deepcopy(self.view)
        after = copy.deepcopy(before)
        after['peer']['fingerprint'] = 'c' * 64
        self.model.manage.side_effect = [before, after]
        with self.assertRaises(OperationError) as error:
            self.prepare()
        self.assertEqual(error.exception.code, 'settings_conflict')
        self.assertFalse(any(call[0] == 'pkexec' for call in self.calls))

    def test_only_saved_listening_configuration_can_prepare(self):
        self.view['settings']['connection']['mode'] = 'connect'
        with self.assertRaises(OperationError) as error:
            self.prepare()
        self.assertEqual(error.exception.code, 'firewall_listen_required')
        self.assertEqual(self.calls, [])

    def test_pkexec_cancellation_has_a_friendly_error_and_does_not_retry(self):
        prepared = self.prepare()
        for code in (126, 127):
            self.privileged_result = (code, None)
            with self.subTest(code=code), self.assertRaises(OperationError) as error:
                self.model.grant_firewall(prepared)
            self.assertEqual(error.exception.code, 'firewall_cancelled')
            self.assertNotIn('untrusted raw stderr', str(error.exception))
        self.assertEqual(len(self.calls), 3)

    def test_nonzero_helper_errors_are_decoded_without_raw_stderr(self):
        self.plan_error = 'firewall_inactive'
        with self.assertRaises(OperationError) as error:
            self.prepare()
        self.assertEqual(error.exception.code, 'firewall_inactive')
        self.assertNotIn('untrusted raw stderr', str(error.exception))
        self.assertFalse(any(call[0] == 'pkexec' for call in self.calls))

    def test_unfrozen_or_nonprivate_plan_cannot_authorize(self):
        self.plan_mode = 0o644
        with self.assertRaises(OperationError) as error:
            self.prepare()
        self.assertEqual(error.exception.code, 'firewall_plan_invalid')
        self.plan_mode = 0o600
        prepared = self.prepare()
        prepared['plan']['port'] = 42420
        with self.assertRaises(OperationError) as error:
            self.model.grant_firewall(prepared)
        self.assertEqual(error.exception.code, 'firewall_plan_invalid')
        self.assertFalse(any(call[0] == 'pkexec' for call in self.calls))

    def test_old_helper_network_info_failure_does_not_block_loading(self):
        self.runner.side_effect = None
        self.runner.return_value = subprocess.CompletedProcess([], 2, '', 'old helper usage')
        self.model.command = mock.Mock(side_effect=[json.dumps({'devices': []}), '{}', 'niri-bridge test'])
        self.model.poll = mock.Mock(return_value={})
        loaded = self.model.load()
        self.assertEqual(loaded['config'], self.view)
        self.assertEqual(loaded['network'], {'addresses': [], 'port': None})

    def test_configuration_edit_lock_covers_authorization_and_is_released_on_every_outcome(self):
        prepared = self.prepare()
        lock_path = self.model.config.with_suffix('.edit.lock')
        def verify_locked():
            with lock_path.open('r+b') as other:
                with self.assertRaises(BlockingIOError):
                    fcntl.flock(other, fcntl.LOCK_EX | fcntl.LOCK_NB)
            inode = lock_path.stat().st_ino
            descriptors = [int(path.name) for path in Path('/proc/self/fd').iterdir() if path.name.isdigit()]
            held = []
            for descriptor in descriptors:
                try:
                    if os.fstat(descriptor).st_ino == inode:
                        held.append(descriptor)
                except OSError:
                    pass
            self.assertTrue(held)
            self.assertTrue(all(not os.get_inheritable(descriptor) for descriptor in held))
        for outcome in ('success', 'canceled', 'exception'):
            with self.subTest(outcome=outcome):
                def authorize():
                    verify_locked()
                    if outcome == 'exception':
                        raise subprocess.TimeoutExpired('synthetic pkexec', 180)
                self.during_authorization = authorize
                self.privileged_result = (126, None) if outcome == 'canceled' else (0, {'ok': True})
                if outcome == 'success':
                    self.model.grant_firewall(prepared)
                else:
                    with self.assertRaises(OperationError):
                        self.model.grant_firewall(prepared)
                with lock_path.open('r+b') as other:
                    fcntl.flock(other, fcntl.LOCK_EX | fcntl.LOCK_NB)
                    fcntl.flock(other, fcntl.LOCK_UN)

    def test_busy_or_unsafe_configuration_lock_prevents_privileged_call(self):
        prepared = self.prepare()
        lock_path = self.model.config.with_suffix('.edit.lock')
        descriptor = os.open(lock_path, os.O_RDWR | os.O_CREAT, 0o600)
        try:
            fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
            with self.assertRaises(OperationError) as error:
                self.model.grant_firewall(prepared)
            self.assertEqual(error.exception.code, 'settings_busy')
        finally:
            os.close(descriptor)
        lock_path.chmod(0o644)
        with self.assertRaises(OperationError) as error:
            self.model.grant_firewall(prepared)
        self.assertEqual(error.exception.code, 'settings_permissions')
        self.assertFalse(any(call[0] == 'pkexec' for call in self.calls))


class LanFirewallWidgetTests(unittest.TestCase):
    def setUp(self):
        if not Gtk.init_check()[0]:
            self.skipTest('A GTK display is required')
        self.model = FakeModel()
        self.model.data['config']['settings']['connection'] = {'mode': 'listen', 'address': '0.0.0.0:43111'}
        self.model.data['network'] = {'addresses': [
            {'address': '192.168.50.10', 'interface': 'wlan-test'},
            {'address': '10.0.0.10', 'interface': 'eth-test'}], 'port': 43111}
        self.prepared = {'plan': {'version': 1, 'kind': 'lan-firewall', 'peer': '192.168.50.20',
            'local': '192.168.50.10', 'interface': 'wlan-test', 'port': 43111}, 'name': 'desktop'}
        self.model.prepare_firewall = mock.Mock(return_value=self.prepared)
        self.model.grant_firewall = mock.Mock(return_value={'ok': True})
        self.model.grant_devices = mock.Mock()
        with mock.patch('app.Window.setup_tray'):
            self.window = Window(None, self.model)
        pump(lambda: self.window.info is not None and not self.window.busy)
        self.window.navigate('pair')

    def tearDown(self):
        self.window.destroy()
        self.model.directory.cleanup()

    def test_listening_preparation_copies_plain_addresses_and_shows_the_saved_port(self):
        window = self.window
        self.assertTrue(window.firewall_box.get_visible())
        self.assertTrue(window.firewall_button.get_sensitive())
        self.assertIn('43111', window.local_port_label.get_text())
        clipboard = mock.Mock()
        with mock.patch('app.Gtk.Clipboard.get', return_value=clipboard):
            for row in window.local_addresses_box.get_children():
                next(child for child in row.get_children() if isinstance(child, Gtk.Button)).emit('clicked')
        self.assertEqual(clipboard.set_text.call_args_list, [mock.call('192.168.50.10', -1), mock.call('10.0.0.10', -1)])
        self.assertEqual(self.model.actions, [])

    def test_connect_role_unpaired_and_unsaved_settings_do_not_offer_authorization(self):
        window = self.window
        window.host_entry.set_text('127.0.0.1')
        self.assertFalse(window.firewall_button.get_sensitive())
        with mock.patch.object(window, 'ask_lan_peer') as ask:
            window.allow_lan_connection()
        ask.assert_not_called()
        window.connection_mode.set_active_id('connect')
        self.assertFalse(window.firewall_box.get_visible())
        self.model.data['config']['peer'] = None
        window.settings_dirty = False
        window.populate(self.model.load())
        self.assertFalse(window.firewall_button.get_sensitive())
        self.model.prepare_firewall.assert_not_called()
        self.model.grant_firewall.assert_not_called()

    def test_canceling_address_or_scope_review_never_authorizes(self):
        window = self.window
        with mock.patch.object(window, 'ask_lan_peer', return_value=None):
            window.firewall_button.emit('clicked')
        self.model.prepare_firewall.assert_not_called()
        with mock.patch.object(window, 'ask_lan_peer', return_value='192.168.50.20'), mock.patch.object(window, 'confirm', return_value=False):
            window.firewall_button.emit('clicked')
            pump(lambda: not window.busy)
        self.model.grant_firewall.assert_not_called()
        self.assertEqual(self.model.actions, [])

    def test_address_dialog_requires_one_private_ipv4_and_names_the_paired_computer(self):
        observed = []
        def descendants(widget):
            yield widget
            if isinstance(widget, Gtk.Container):
                for child in widget.get_children():
                    yield from descendants(child)
        def respond():
            dialog = next(widget for widget in Gtk.Window.list_toplevels()
                          if isinstance(widget, Gtk.Dialog) and widget.get_transient_for() == self.window)
            entry = next(widget for widget in descendants(dialog) if isinstance(widget, Gtk.Entry))
            accept = dialog.get_widget_for_response(Gtk.ResponseType.ACCEPT)
            for value in ('', '8.8.8.8', '::1', '192.168.1.20/24', '192.168.1.20:43111'):
                entry.set_text(value)
                observed.append(not accept.get_sensitive())
            entry.set_text('192.168.50.20')
            observed.append(accept.get_sensitive())
            observed.append(any('desktop' in widget.get_text() for widget in descendants(dialog) if isinstance(widget, Gtk.Label)))
            dialog.response(Gtk.ResponseType.CANCEL)
            return False
        GLib.idle_add(respond)
        self.assertIsNone(self.window.ask_lan_peer('desktop'))
        self.assertTrue(all(observed))
        self.model.prepare_firewall.assert_not_called()
        self.assertEqual(self.model.actions, [])

    def test_confirmation_contains_exact_scope_and_success_keeps_drafts_and_sharing(self):
        window = self.window
        window.layout_dirty = True
        draft = window.editor.draft
        generation = window.poll_generation
        before = copy.deepcopy(self.model.data)
        with mock.patch.object(window, 'ask_lan_peer', return_value='192.168.50.20'), mock.patch.object(window, 'confirm', return_value=True) as confirm:
            window.firewall_button.emit('clicked')
            pump(lambda: not window.busy)
        self.model.prepare_firewall.assert_called_once_with('192.168.50.20', before['config']['revision'], before['config']['peer']['fingerprint'])
        scope = confirm.call_args.args[1]
        for value in ('192.168.50.20', '192.168.50.10', '43111', 'wlan-test'):
            self.assertIn(value, scope)
        self.model.grant_firewall.assert_called_once_with(self.prepared)
        self.model.grant_devices.assert_not_called()
        self.assertEqual(self.model.data, before)
        self.assertEqual(self.model.actions, [])
        self.assertTrue(window.layout_dirty)
        self.assertIs(window.editor.draft, draft)
        self.assertEqual(window.poll_generation, generation)
        self.assertEqual(window.notice_label.get_text(), _('LAN access is allowed.'))

    def test_inactive_firewall_and_canceled_authentication_keep_state_and_drafts(self):
        window = self.window
        window.layout_dirty = True
        before = copy.deepcopy(self.model.data)
        self.model.prepare_firewall.side_effect = OperationError('firewall_inactive')
        with mock.patch.object(window, 'ask_lan_peer', return_value='192.168.50.20'):
            window.firewall_button.emit('clicked')
            pump(lambda: not window.busy)
        self.assertEqual(window.notice_label.get_text(), str(OperationError('firewall_inactive')))
        self.assertFalse(window.notice_box.get_style_context().has_class('error'))
        self.model.grant_firewall.assert_not_called()
        self.model.prepare_firewall.side_effect = None
        self.model.grant_firewall.side_effect = OperationError('firewall_cancelled')
        with mock.patch.object(window, 'ask_lan_peer', return_value='192.168.50.20'), mock.patch.object(window, 'confirm', return_value=True):
            window.firewall_button.emit('clicked')
            pump(lambda: not window.busy)
        self.assertEqual(self.model.data, before)
        self.assertTrue(window.layout_dirty)
        self.assertEqual(self.model.actions, [])

    def test_start_checks_saved_settings_and_old_invalid_address_before_any_service_action(self):
        window = self.window
        self.model.data['service']['ActiveState'] = 'inactive'
        self.model.data['status'] = None
        window.populate(self.model.load())
        window.port_spin.set_value(43112)
        window.toggle()
        self.assertEqual(self.model.actions, [])
        self.assertEqual(window.notice_label.get_text(), _('Save local settings before starting sharing.'))
        self.model.data['config']['settings']['connection'] = {'mode': 'connect', 'address': '0.0.0.0:43111'}
        window.settings_dirty = False
        window.populate(self.model.load())
        window.toggle()
        self.assertEqual(self.model.actions, [])
        self.assertEqual(window.notice_label.get_text(), str(OperationError('peer_address_required')))

    def test_waiting_status_explains_each_role_and_links_to_preparation(self):
        window = self.window
        state = self.model.poll()
        state['status']['connection'] = 'connecting'
        for mode in ('listen', 'connect'):
            self.model.data['config']['settings']['connection']['mode'] = mode
            window.info['config']['settings']['connection']['mode'] = mode
            window.update_status(state)
            self.assertIn('desktop', window.hero_description.get_text())
            self.assertTrue(window.connection_button.get_visible())
            window.navigate('overview')
            window.connection_button.emit('clicked')
            self.assertEqual(window.stack.get_visible_child_name(), 'pair')
        self.assertEqual(self.model.actions, [])

    def test_connection_action_reveals_addresses_and_authorization_in_a_normal_window(self):
        window = self.window
        window.resize(1128, 864)
        window.navigate('overview')
        window.layout_dirty = True
        draft = window.editor.draft
        state = self.model.poll()
        state['status']['connection'] = 'connecting'
        window.update_status(state)
        page = window.stack.get_child_by_name('pair')
        page.get_vadjustment().set_value(0)
        window.connection_button.emit('clicked')
        pump(lambda: page.get_vadjustment().get_value() > 0)
        viewport = page.get_child()
        for widget in (window.host_entry, window.local_addresses_box, window.firewall_button):
            translated = widget.translate_coordinates(viewport, 0, 0)
            self.assertIsNotNone(translated)
            self.assertGreaterEqual(translated[1], 0)
            self.assertLess(translated[1], viewport.get_allocated_height())
        self.assertTrue(window.layout_dirty)
        self.assertIs(window.editor.draft, draft)
        self.assertEqual(self.model.actions, [])

    def test_start_still_requires_pairing_and_input_permissions(self):
        window = self.window
        self.model.data['service']['ActiveState'] = 'inactive'
        self.model.data['status'] = None
        peer = self.model.data['config'].pop('peer')
        window.populate(self.model.load())
        window.toggle()
        self.assertEqual(window.stack.get_visible_child_name(), 'pair')
        self.assertEqual(self.model.actions, [])
        self.model.data['config']['peer'] = peer
        self.model.data['devices']['uinput_writable'] = False
        window.populate(self.model.load())
        window.toggle()
        self.assertEqual(window.stack.get_visible_child_name(), 'preferences')
        self.assertEqual(self.model.actions, [])

    def test_start_does_not_discard_or_block_an_unsaved_layout_draft(self):
        window = self.window
        draft = window.editor.draft
        self.model.data['service']['ActiveState'] = 'inactive'
        self.model.data['status'] = None
        window.populate(self.model.load())
        window.editor.draft = draft
        window.layout_dirty = True
        window.toggle()
        pump(lambda: not window.busy)
        self.assertEqual(self.model.actions, [('running', True)])
        self.assertTrue(window.layout_dirty)
        self.assertIs(window.editor.draft, draft)


class SavedDeviceModelTests(unittest.TestCase):
    def setUp(self):
        self.model = Model(config=Path('/synthetic/config.toml'), binary='/synthetic/niri-bridge')
        self.model.ensure_managed = mock.Mock()
        self.model.controls_current_config = mock.Mock(return_value=True)
        self.model.set_running = mock.Mock()
        self.model.manage = mock.Mock(return_value={'revision': 'b' * 64})

    def test_switch_stops_before_writing_and_does_not_restart(self):
        actions = []
        self.model.set_running.side_effect = lambda running: actions.append(('running', running))
        self.model.manage.side_effect = lambda *args: actions.append(('manage', *args)) or {}
        self.model.select_peer('a' * 64, 'c' * 64)
        self.assertEqual(actions, [
            ('running', False),
            ('manage', 'select-peer', '--config', self.model.config,
             '--revision', 'a' * 64, '--fingerprint', 'c' * 64)])

    def test_failed_stop_preserves_pairing_without_management_write(self):
        self.model.set_running.side_effect = OperationError('stop_failed')
        with self.assertRaises(OperationError):
            self.model.select_peer('a' * 64, 'c' * 64)
        self.model.manage.assert_not_called()
        self.model.set_running.assert_called_once_with(False)

    def test_failed_switch_leaves_sharing_stopped(self):
        self.model.manage.side_effect = OperationError('settings_conflict')
        with self.assertRaises(OperationError) as error:
            self.model.select_peer('a' * 64, 'c' * 64)
        self.assertEqual(error.exception.code, 'settings_conflict')
        self.model.set_running.assert_called_once_with(False)

    def test_manual_instance_and_different_config_block_all_pairing_writes(self):
        for code in ('manual_instance', 'config_changed'):
            for operation in (
                    lambda: self.model.select_peer('a' * 64, 'c' * 64),
                    lambda: self.model.import_peer(Path('/synthetic/peer.pem'), 'a' * 64, 'c' * 64),
                    lambda: self.model.save_settings('a' * 64, {})):
                with self.subTest(code=code):
                    self.model.ensure_managed.side_effect = OperationError(code) if code == 'manual_instance' else None
                    self.model.controls_current_config.return_value = code != 'config_changed'
                    with self.assertRaises(OperationError) as error:
                        operation()
                    self.assertEqual(error.exception.code, code)
        self.model.set_running.assert_not_called()
        self.model.manage.assert_not_called()


class SavedDeviceWidgetTests(unittest.TestCase):
    def setUp(self):
        if not Gtk.init_check()[0]:
            self.skipTest('A GTK display is required')
        self.model = FakeModel()
        self.model.data['config']['paired_devices'][1]['name'] = 'desktop'
        self.model.select_peer = mock.Mock(return_value={})
        with mock.patch('app.Window.setup_tray'):
            self.window = Window(None, self.model)
        pump(lambda: self.window.info is not None and not self.window.busy)
        self.window.navigate('pair')
        self.other = self.model.data['config']['paired_devices'][1]['fingerprint']

    def tearDown(self):
        self.window.destroy()
        self.model.directory.cleanup()

    def test_browsing_same_name_devices_is_distinct_and_has_no_backend_effect(self):
        window = self.window
        active = window.peer_combo.get_active_id()
        self.assertFalse(window.peer_switch.get_sensitive())
        window.peer_combo.set_active_id(self.other)
        self.assertTrue(window.peer_switch.get_sensitive())
        labels = [row[0] for row in window.peer_combo.get_model()]
        self.assertEqual(len(set(labels)), 2)
        self.assertTrue(all('desktop' in label for label in labels))
        self.assertIn(self.other[:12], window.peer_combo.get_active_text())
        self.assertIn(self.other, window.peer_combo.get_tooltip_text())
        self.model.select_peer.assert_not_called()
        self.assertEqual(self.model.actions, [])
        window.peer_combo.set_active_id(active)
        window.switch_peer()
        self.model.select_peer.assert_not_called()

    def test_cancelled_switch_preserves_unsaved_settings_and_layout(self):
        window = self.window
        window.host_entry.set_text('unsaved.local')
        window.layout_dirty = True
        draft = window.editor.draft
        window.peer_combo.set_active_id(self.other)
        with mock.patch.object(window, 'confirm', return_value=False) as confirm:
            window.peer_switch.emit('clicked')
        confirm.assert_called_once()
        self.model.select_peer.assert_not_called()
        self.assertTrue(window.settings_dirty)
        self.assertTrue(window.layout_dirty)
        self.assertIs(window.editor.draft, draft)
        self.assertEqual(window.host_entry.get_text(), 'unsaved.local')
        self.assertEqual(self.model.actions, [])

    def test_successful_switch_discards_confirmed_drafts_and_loads_the_new_target(self):
        window = self.window
        window.host_entry.set_text('unsaved.local')
        window.layout_dirty = True
        window.peer_combo.set_active_id(self.other)
        def switched(_revision, fingerprint):
            config = self.model.data['config']
            for device in config['paired_devices']:
                device['active'] = device['fingerprint'] == fingerprint
            config['peer']['fingerprint'] = fingerprint
            config['settings']['connection']['address'] = 'office.local:42421'
            self.model.data['service']['ActiveState'] = 'inactive'
            self.model.data['status'] = None
        self.model.select_peer.side_effect = switched
        with mock.patch.object(window, 'confirm', return_value=True):
            window.peer_switch.emit('clicked')
            pump(lambda: not window.busy)
        self.model.select_peer.assert_called_once_with('a' * 64, self.other)
        self.assertFalse(window.settings_dirty)
        self.assertFalse(window.layout_dirty)
        self.assertIsNone(window.editor.draft)
        self.assertEqual(window.host_entry.get_text(), 'office.local')
        self.assertEqual(window.port_spin.get_value_as_int(), 42421)
        self.assertFalse(window.peer_switch.get_sensitive())
        self.assertEqual(window.toggle_button.get_label(), _('Start sharing'))

    def test_failed_switch_keeps_drafts_and_original_target(self):
        window = self.window
        original = window.info['config']['peer']['fingerprint']
        window.host_entry.set_text('unsaved.local')
        window.layout_dirty = True
        draft = window.editor.draft
        window.peer_combo.set_active_id(self.other)
        self.model.select_peer.side_effect = OperationError('settings_conflict')
        with mock.patch.object(window, 'confirm', return_value=True):
            window.peer_switch.emit('clicked')
            pump(lambda: not window.busy)
        self.assertEqual(window.info['config']['peer']['fingerprint'], original)
        self.assertTrue(window.settings_dirty)
        self.assertTrue(window.layout_dirty)
        self.assertIs(window.editor.draft, draft)
        self.assertEqual(window.host_entry.get_text(), 'unsaved.local')
        self.assertEqual(self.model.actions, [])

    def test_browsed_device_and_drafts_survive_language_rebuild_state(self):
        window = self.window
        window.peer_combo.set_active_id(self.other)
        window.host_entry.set_text('unsaved.local')
        state = window.editing_state()
        with mock.patch('app.Window.setup_tray'):
            restored = Window(None, self.model, restore_state=state)
        try:
            pump(lambda: restored.info is not None and not restored.busy)
            self.assertEqual(restored.peer_combo.get_active_id(), self.other)
            self.assertEqual(restored.host_entry.get_text(), 'unsaved.local')
            self.assertTrue(restored.settings_dirty)
            self.model.select_peer.assert_not_called()
        finally:
            restored.destroy()

    def test_old_in_flight_poll_cannot_restore_the_previous_computers_layout(self):
        window = self.window
        old_status = self.model.poll()
        normal_poll = self.model.poll
        started, release = threading.Event(), threading.Event()
        def delayed_poll():
            if not started.is_set():
                started.set()
                release.wait(3)
                return old_status
            return normal_poll()
        def switched(_revision, fingerprint):
            config = self.model.data['config']
            for device in config['paired_devices']:
                device['active'] = device['fingerprint'] == fingerprint
            config['peer']['fingerprint'] = fingerprint
            self.model.data['service']['ActiveState'] = 'inactive'
            self.model.data['status'] = None
        self.model.select_peer.side_effect = switched
        with mock.patch.object(self.model, 'poll', side_effect=delayed_poll):
            try:
                window.poll()
                pump(started.is_set)
                window.peer_combo.set_active_id(self.other)
                window.peer_switch.emit('clicked')
                pump(lambda: not window.busy)
            finally:
                release.set()
            pump(lambda: not window.poll_pending)
        self.assertEqual(window.info['config']['peer']['fingerprint'], self.other)
        self.assertIsNone(window.poll_data['status'])
        self.assertIsNone(window.editor.draft)
        self.assertEqual(window.toggle_button.get_label(), _('Start sharing'))
        self.assertEqual(self.model.actions, [])


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
