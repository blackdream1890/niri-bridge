# SPDX-License-Identifier: GPL-3.0-or-later
"""Sharing lifecycle tests simulate services and sockets; they never use input devices."""
import importlib.util
from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock

UI = Path(__file__).resolve().parent.parent / 'ui'
sys.path.insert(0, str(UI))
spec = importlib.util.spec_from_file_location('lifecycle_test_model', UI / 'model.py')
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class SharingLifecycleTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.config = Path(self.directory.name) / 'config.toml'
        self.config.write_text('# synthetic config')
        self.model = module.Model(config=self.config, binary='/synthetic/niri-bridge')
        self.active = True
        self.safe = True
        self.exit_status = '0'
        self.pid = 55
        self.commands = []
        def service(unit='niri-bridge.service'):
            return {'ActiveState': 'active' if self.active else 'inactive', 'MainPID': '55',
                    'ExecMainStatus': self.exit_status, 'UnitFileState': 'disabled'}
        def rpc(*_args, **_kwargs):
            if not self.active:
                raise module.OperationError('peer_offline')
            return {'pid': self.pid, 'config_path': str(self.config), 'safe_touchpad_release': self.safe}
        def command(args, **_kwargs):
            self.commands.append(args)
            if args[:3] == ['systemctl', '--user', 'stop']:
                self.active = False
        self.addCleanup(mock.patch.stopall)
        mock.patch.object(self.model, 'service', side_effect=service).start()
        mock.patch.object(self.model, 'rpc', side_effect=rpc).start()
        mock.patch.object(self.model, 'command', side_effect=command).start()

    def test_clean_stop_does_not_reset_physical_devices_twice(self):
        result = self.model.stop_for_exit()
        self.assertEqual(result['service']['ActiveState'], 'inactive')
        self.assertEqual(self.commands, [['systemctl', '--user', 'stop', 'niri-bridge.service']])

    def test_legacy_backend_recovers_only_after_service_stopped(self):
        self.safe = False
        self.model.stop_for_exit()
        self.assertEqual(self.commands, [['systemctl', '--user', 'stop', 'niri-bridge.service'],
                                        ['/synthetic/niri-bridge', 'restore-input', '--config', str(self.config)]])

    def test_abnormal_exit_recovers_even_when_backend_supports_safe_release(self):
        self.exit_status = '9'
        self.model.stop_for_exit()
        self.assertEqual(self.commands[-1][1], 'restore-input')

    def test_fresh_install_or_already_stopped_service_needs_no_stop_command(self):
        self.active = False
        self.model.config = Path.home() / '.config/niri-bridge/config.toml'
        self.model.stop_for_exit()
        self.assertEqual(self.commands, [])

    def test_a_previously_killed_backend_is_recovered_before_exit(self):
        self.active = False
        self.model.service.side_effect = lambda unit='': {'ActiveState': 'failed', 'MainPID': '0',
                                                        'ExecMainStatus': '9', 'ExecMainCode': '2'}
        with mock.patch.object(self.model, 'controls_current_config', return_value=True):
            self.model.stop_for_exit()
        self.assertEqual(self.commands, [['/synthetic/niri-bridge', 'restore-input', '--config', str(self.config)]])

    def test_manual_instance_is_preserved_and_blocks_exit_acknowledgment(self):
        self.pid = 999
        with self.assertRaises(module.OperationError) as error:
            self.model.stop_for_exit()
        self.assertEqual(error.exception.code, 'manual_instance')
        self.assertEqual(self.commands, [])

    def test_failed_stop_cannot_invoke_recovery_or_acknowledge_exit(self):
        self.model.command.side_effect = lambda args, **kwargs: self.commands.append(args)
        with self.assertRaises(module.OperationError) as error:
            self.model.stop_for_exit()
        self.assertEqual(error.exception.code, 'stop_failed')
        self.assertFalse(any('restore-input' in command for command in self.commands))


if __name__ == '__main__':
    unittest.main()
