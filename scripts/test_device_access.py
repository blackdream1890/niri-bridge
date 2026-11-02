# SPDX-License-Identifier: GPL-3.0-or-later
import importlib.util
from contextlib import ExitStack, redirect_stdout
import hashlib
import io
import json
import os
from pathlib import Path
import shlex
import stat
import sys
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('device_access', Path(__file__).with_name('device-access.py'))
access = importlib.util.module_from_spec(spec)
spec.loader.exec_module(access)


class DeviceAccessTests(unittest.TestCase):
    def test_rule_identifiers_cannot_inject_udev_assignments(self):
        with self.assertRaises(ValueError):
            access.render_rules([{'id_path': 'bad", RUN+="command', 'classes': ['ID_INPUT_KEYBOARD']}])
        with self.assertRaises(ValueError):
            access.render_rules([{'id_path': 'platform-kbd', 'classes': ['RUN']}])

    def test_rules_grant_active_session_access_without_world_writable_modes(self):
        rule = access.render_rules([{'id_path': 'platform-kbd', 'classes': ['ID_INPUT_KEYBOARD']}])
        self.assertIn('ENV{ID_PATH}=="platform-kbd"', rule)
        self.assertIn('TAG+="uaccess"', rule)
        self.assertNotIn('MODE=', rule)
        self.assertNotIn('GROUP=', rule)

    def test_firewall_scope_is_one_private_peer(self):
        self.assertEqual(access.lan_address('192.168.1.20'), '192.168.1.20')
        for value in ('0.0.0.0', '127.0.0.1', '8.8.8.8', '192.168.1.0/24'):
            with self.assertRaises(ValueError):
                access.lan_address(value)

    def test_changed_device_identity_invalidates_plan(self):
        device = {'path': '/dev/input/by-path/example', 'id_path': 'platform-kbd', 'classes': ['ID_INPUT_KEYBOARD']}
        data = {'version': 1, 'devices': [device], 'rules': access.render_rules([device]), 'allow_from': None}
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / 'plan.json'
            path.write_text(json.dumps(data))
            with patch.object(access, 'device_info', return_value=dict(device, id_path='other-device', sys_path='/sys/example')):
                with self.assertRaises(ValueError):
                    access.verify_plan(path)
            with patch.object(access, 'device_info', return_value=dict(device, sys_path='/sys/new-event-number')):
                self.assertEqual(access.verify_plan(path), data)


class FirewallAccessTests(unittest.TestCase):
    def setUp(self):
        self.stack = ExitStack()
        self.addCleanup(self.stack.close)
        self.folder = Path(self.stack.enter_context(tempfile.TemporaryDirectory()))
        self.state = self.folder / 'root/firewall-access.json'
        self.config = self.folder / 'config.toml'
        self.config.write_text('[connection]\nmode = "listen"\naddress = "0.0.0.0:43111"\n')
        self.interface = 'lan0'
        self.local = '192.168.1.10'
        self.addresses = [self.local]
        self.active = True
        self.rules = []
        self.calls = []
        self.fail_add = False
        self.fail_delete = False
        self.stack.enter_context(patch.object(access, 'FIREWALL_STATE', self.state))
        # Exercise root-owned file checks against private test files without root.
        self.stack.enter_context(patch.object(access, 'ROOT_UID', os.geteuid()))
        self.stack.enter_context(patch.object(access, 'physical_interface', side_effect=lambda name: name in ('lan0', 'lan1')))
        self.stack.enter_context(patch.object(access, 'command', side_effect=self.command))

    def command(self, arguments):
        self.calls.append(arguments)
        if arguments[:4] == ['ip', '-j', 'route', 'get']:
            return json.dumps([{'dev': self.interface, 'prefsrc': self.local}])
        if arguments[:5] == ['ip', '-j', 'address', 'show', 'dev']:
            return json.dumps([{'ifname': self.interface, 'flags': ['UP', 'LOWER_UP'],
                                'addr_info': [{'family': 'inet', 'local': value} for value in self.addresses]}])
        if arguments == ['ufw', 'status']:
            return 'Status: active\n' if self.active else 'Status: inactive\n'
        if arguments == ['ufw', 'show', 'added']:
            return 'Added user rules:\n' + '\n'.join(self.rules)
        if arguments[:2] == ['ufw', 'allow']:
            if self.fail_add:
                raise RuntimeError('synthetic private failure details')
            self.rules.append(shlex.join(arguments))
            return 'Rule added\n'
        if arguments[:2] == ['ufw', 'delete']:
            if self.fail_delete:
                raise RuntimeError('synthetic private failure details')
            self.rules.remove(shlex.join([arguments[0], *arguments[2:]]))
            return 'Rule deleted\n'
        raise AssertionError(f'Unexpected command: {arguments[0]}')

    def prepare(self, peer='192.168.1.20'):
        path = self.folder / f'plan-{len(list(self.folder.glob("plan-*")))}.json'
        response = access.firewall_plan(self.config, path, peer)
        return path, response['sha256'], response['plan']

    def install(self, peer='192.168.1.20'):
        path, digest, plan = self.prepare(peer)
        return access.firewall_install(path, digest), plan

    def assert_code(self, code, function, *args):
        with self.assertRaises(access.FirewallError) as error:
            function(*args)
        self.assertEqual(error.exception.code, code)

    def test_reviewed_plan_uses_actual_port_route_and_private_permissions(self):
        path, digest, plan = self.prepare()
        self.assertEqual(plan, {'version': 1, 'kind': 'lan-firewall', 'peer': '192.168.1.20',
                                'local': self.local, 'interface': self.interface, 'port': 43111})
        self.assertEqual(hashlib.sha256(path.read_bytes()).hexdigest(), digest)
        self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)
        self.assertFalse(any(arguments[0] == 'ufw' for arguments in self.calls))
        self.assert_code('firewall_plan_invalid', access.verify_firewall_plan, path, '0' * 64)

    def test_existing_plan_is_never_overwritten(self):
        path, _digest, _plan = self.prepare()
        original = path.read_bytes()
        with self.assertRaises(FileExistsError):
            access.firewall_plan(self.config, path, '192.168.1.21')
        self.assertEqual(path.read_bytes(), original)

    def test_plan_requires_waiting_and_private_peer(self):
        for peer in ('127.0.0.1', '169.254.1.2', '100.64.1.2', '198.18.0.2',
                     '8.8.8.8', '::1', '192.168.1.0/24', 'desktop.local'):
            with self.subTest(peer=peer):
                self.assert_code('firewall_peer_invalid', self.prepare, peer)
        self.config.write_text('[connection]\nmode = "connect"\naddress = "192.168.1.20:43111"\n')
        self.assert_code('firewall_listen_required', self.prepare)

    def test_explicit_secondary_listening_address_is_retained(self):
        self.addresses.append('192.168.1.11')
        self.config.write_text('[connection]\nmode = "listen"\naddress = "192.168.1.11:43112"\n')
        path, digest, plan = self.prepare()
        self.assertEqual((plan['local'], plan['port']), ('192.168.1.11', 43112))
        self.assertEqual(access.verify_firewall_plan(path, digest), plan)

    def test_route_and_interface_are_rechecked_after_review(self):
        path, digest, _plan = self.prepare()
        self.interface = 'lan1'
        self.assert_code('firewall_route_changed', access.firewall_install, path, digest)
        self.assertFalse(self.state.exists())
        self.assertFalse(any(arguments[0] == 'ufw' for arguments in self.calls))

    def test_tunnel_source_and_same_machine_are_rejected(self):
        self.local, self.addresses = '198.18.0.1', ['198.18.0.1']
        self.assert_code('firewall_route_changed', self.prepare)
        self.local, self.addresses = '192.168.1.20', ['192.168.1.20']
        self.assert_code('firewall_route_changed', self.prepare)

    def test_digest_file_type_and_schema_are_enforced(self):
        path, _digest, plan = self.prepare()
        link = self.folder / 'link.json'
        link.symlink_to(path)
        self.assert_code('firewall_plan_invalid', access.verify_firewall_plan,
                         link, hashlib.sha256(path.read_bytes()).hexdigest())
        path.write_bytes(b' ' * 8193)
        self.assert_code('firewall_plan_invalid', access.verify_firewall_plan,
                         path, hashlib.sha256(path.read_bytes()).hexdigest())
        for change in ({'port': True}, {'port': 0}, {'port': 65536}, {'interface': 'lan0; command'},
                       {'local': plan['peer']}, {'unexpected': 'field'}):
            altered = dict(plan, **change)
            path.write_text(json.dumps(altered))
            digest = hashlib.sha256(path.read_bytes()).hexdigest()
            with self.subTest(change=change), self.assertRaises(access.FirewallError):
                access.verify_firewall_plan(path, digest)

    def test_non_root_and_inactive_firewall_never_change_permissions(self):
        path, digest, _plan = self.prepare()
        with patch.object(access.os, 'geteuid', return_value=os.geteuid() + 1):
            self.assert_code('firewall_failed', access.firewall_install, path, digest)
        self.active = False
        self.assert_code('firewall_inactive', access.firewall_install, path, digest)
        self.assertFalse(self.state.parent.exists())
        self.assertFalse(any('enable' in arguments or arguments[:2] == ['ufw', 'allow'] for arguments in self.calls))

    def test_install_and_repeated_authorization_keep_input_scope_unchanged(self):
        self.state.parent.mkdir(mode=0o700)
        old = self.state.with_name('device-access.json')
        old.write_text('{"plan":{"allow_from":null},"existing_input_scope":true}')
        before = old.read_bytes()
        result, plan = self.install()
        self.assertEqual(result, {'ok': True, 'already_allowed': False, 'managed': True})
        self.assertEqual(old.read_bytes(), before)
        self.assertEqual(stat.S_IMODE(self.state.stat().st_mode), 0o600)
        parsed = access.parse_firewall_rule(self.rules[0])
        self.assertEqual(access.firewall_key(parsed), access.firewall_key(plan))
        second, _plan = self.install()
        self.assertEqual(second, {'ok': True, 'already_allowed': True, 'managed': True})
        self.assertEqual(len(self.rules), 1)
        self.assertFalse(any(arguments[0] in ('modprobe', 'udevadm', 'setfacl') for arguments in self.calls))

    def test_equivalent_external_rule_is_preserved_and_never_owned(self):
        _path, _digest, plan = self.prepare()
        self.rules = [shlex.join(['ufw', *access.firewall_arguments(plan, 'external administrator comment')])]
        before = list(self.rules)
        result, _plan = self.install()
        self.assertEqual(result, {'ok': True, 'already_allowed': True, 'managed': False})
        self.assertEqual(self.rules, before)
        self.assertFalse(self.state.exists())
        access.firewall_remove()
        self.assertEqual(self.rules, before)

    def test_external_comments_with_quotes_do_not_hide_an_equivalent_rule(self):
        _path, _digest, plan = self.prepare()
        scope = ' '.join(['ufw', *access.firewall_arguments(plan, 'unused')[:-2]])
        for comment in ("'administrator's existing rule'", "'first line\nsecond line'"):
            with self.subTest(comment=comment):
                self.rules = [scope + ' comment ' + comment]
                before = list(self.rules)
                result, _plan = self.install()
                self.assertFalse(result['managed'])
                self.assertEqual(self.rules, before)
                self.assertFalse(self.state.exists())

    def test_external_deny_is_not_replaced(self):
        _path, _digest, plan = self.prepare()
        arguments = access.firewall_arguments(plan, 'administrator policy')
        arguments[0] = 'deny'
        self.rules = [shlex.join(['ufw', *arguments])]
        before = list(self.rules)
        self.assert_code('firewall_changed', self.install)
        self.assertEqual(self.rules, before)

    def test_multiple_peers_coexist_and_only_selected_owned_rule_is_removed(self):
        _result, first = self.install()
        self.install('192.168.1.21')
        self.assertEqual(len(json.loads(self.state.read_text())['rules']), 2)
        self.assertEqual(access.firewall_remove(first), {'ok': True, 'removed': 1})
        self.assertEqual(len(self.rules), 1)
        self.assertIn('192.168.1.21', self.rules[0])
        self.assertEqual(access.firewall_remove(), {'ok': True, 'removed': 1})
        self.assertFalse(self.state.exists())

    def test_failed_add_remains_recoverable_without_duplicate_rules(self):
        path, digest, _plan = self.prepare()
        self.fail_add = True
        self.assert_code('firewall_failed', access.firewall_install, path, digest)
        state = json.loads(self.state.read_text())
        self.assertEqual(state['rules'][0]['status'], 'pending')
        tag = state['rules'][0]['tag']
        self.fail_add = False
        access.firewall_install(path, digest)
        self.assertEqual(len(self.rules), 1)
        self.assertIn(tag, self.rules[0])
        self.assertEqual(json.loads(self.state.read_text())['rules'][0]['status'], 'installed')

    def test_add_that_commits_before_command_failure_is_recovered_by_readback(self):
        path, digest, _plan = self.prepare()
        def interrupted(arguments):
            output = self.command(arguments)
            if arguments[:2] == ['ufw', 'allow']:
                raise RuntimeError('reply lost after committing')
            return output
        with patch.object(access, 'command', side_effect=interrupted):
            self.assert_code('firewall_failed', access.firewall_install, path, digest)
        self.assertEqual(len(self.rules), 1)
        self.assertEqual(json.loads(self.state.read_text())['rules'][0]['status'], 'pending')
        result = access.firewall_install(path, digest)
        self.assertTrue(result['already_allowed'])
        self.assertEqual(len(self.rules), 1)

    def test_failed_delete_preserves_recovery_state(self):
        self.install()
        before = self.state.read_bytes()
        self.fail_delete = True
        self.assert_code('firewall_failed', access.firewall_remove)
        self.assertEqual(self.state.read_bytes(), before)
        self.fail_delete = False
        self.assertEqual(access.firewall_remove()['removed'], 1)

    def test_modified_owned_rule_is_preserved(self):
        self.install()
        original = self.state.read_bytes()
        self.rules[0] = self.rules[0].replace('43111', '43112')
        before = list(self.rules)
        self.assert_code('firewall_changed', access.firewall_remove)
        self.assertEqual(self.rules, before)
        self.assertEqual(self.state.read_bytes(), original)

    def test_unsafe_state_and_simultaneous_operations_are_rejected(self):
        path, digest, _plan = self.prepare()
        with access.firewall_lock():
            self.assert_code('firewall_busy', access.firewall_install, path, digest)
        self.state.write_text('{"version":1,"rules":[]}')
        self.state.chmod(0o644)
        self.assert_code('firewall_changed', access.firewall_install, path, digest)
        self.assertEqual(self.rules, [])

    def test_state_symlinks_and_non_private_directories_are_preserved(self):
        path, digest, _plan = self.prepare()
        self.state.parent.mkdir(mode=0o755)
        self.state.parent.chmod(0o755)
        self.assert_code('firewall_changed', access.firewall_install, path, digest)
        self.state.parent.chmod(0o700)
        external = self.folder / 'external.json'
        external.write_text('{"version":1,"rules":[]}')
        external.chmod(0o600)
        before = external.read_bytes()
        self.state.symlink_to(external)
        self.assert_code('firewall_changed', access.firewall_install, path, digest)
        self.assertEqual(external.read_bytes(), before)
        self.assertTrue(self.state.is_symlink())

    def test_uninstall_cleans_new_firewall_grants_and_keeps_legacy_input_cleanup(self):
        self.install()
        input_state = self.state.with_name('device-access.json')
        rule = self.folder / 'input.rules'
        rule.write_text('example managed rule\n')
        input_state.write_text(json.dumps({'plan': {'devices': [], 'allow_from': None},
                                          'snapshots': [], 'boot': 'same-test-boot',
                                          'rule_sha256': hashlib.sha256(rule.read_bytes()).hexdigest(),
                                          'firewall_tag': 'NiriBridge-legacy'}))
        with patch.object(access, 'STATE', input_state), patch.object(access, 'RULE', rule), \
                patch.object(access, 'require_root_state'), patch.object(access, 'refresh') as refresh, \
                patch.object(access, 'boot_marker', return_value='same-test-boot'), redirect_stdout(io.StringIO()):
            access.uninstall()
        self.assertFalse(input_state.exists())
        self.assertFalse(self.state.exists())
        self.assertFalse(rule.exists())
        self.assertEqual(self.rules, [])
        refresh.assert_called_once_with([])

    def test_network_info_filters_virtual_down_and_non_private_addresses(self):
        interfaces = [{'ifname': name, 'flags': flags,
                       'addr_info': [{'family': 'inet', 'local': address} for address in addresses]}
                      for name, flags, addresses in [
                          ('lan0', ['UP', 'LOWER_UP'], ['192.168.1.10', '169.254.1.1', '8.8.8.8']),
                          ('docker0', ['UP', 'LOWER_UP'], ['172.17.0.1']),
                          ('tun0', ['UP', 'LOWER_UP'], ['10.0.0.2']),
                          ('lan1', ['UP'], ['192.168.2.10'])]]
        with patch.object(access, 'command', return_value=json.dumps(interfaces)):
            self.assertEqual(access.network_info(self.config),
                             {'ok': True, 'addresses': [{'address': self.local, 'interface': 'lan0'}], 'port': 43111})
        with patch.object(access, 'command', side_effect=FileNotFoundError):
            self.assertEqual(access.network_info(self.folder / 'missing'),
                             {'ok': True, 'addresses': [], 'port': None})
        self.config.write_text('[connection]\naddress = 12\n')
        with patch.object(access, 'command', return_value='[null]'):
            self.assertEqual(access.network_info(self.config),
                             {'ok': True, 'addresses': [], 'port': None})

    def test_firewall_cli_errors_do_not_expose_raw_failures(self):
        arguments = ['device-access.py', 'firewall-plan', '--config', str(self.config),
                     '--peer', '8.8.8.8', '--output', str(self.folder / 'new.json')]
        output = io.StringIO()
        with patch.object(sys, 'argv', arguments), redirect_stdout(output), self.assertRaises(SystemExit):
            access.main()
        self.assertEqual(json.loads(output.getvalue()), {'ok': False, 'error': 'firewall_peer_invalid'})


if __name__ == '__main__':
    unittest.main()
