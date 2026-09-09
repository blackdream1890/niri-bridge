# SPDX-License-Identifier: GPL-3.0-or-later
import importlib.util
import json
from pathlib import Path
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


if __name__ == '__main__':
    unittest.main()
