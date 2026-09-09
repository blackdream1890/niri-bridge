# SPDX-License-Identifier: GPL-3.0-or-later
"""Package migration checks use only temporary user application files."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest import mock
import package
from install import MANIFEST, digest


class PackageMigrationTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.prefix = Path(self.directory.name)
        self.desktop = self.prefix / package.DESKTOP
        self.desktop.parent.mkdir(parents=True)
        self.original = '[Desktop Entry]\nType=Application\nExec="/synthetic/old/niri-bridge-ui"\n'
        self.desktop.write_text(self.original)
        self.manifest = self.prefix / MANIFEST
        self.manifest.parent.mkdir(parents=True)
        self.data = {'format': 1, 'application': 'niri-bridge', 'files': {package.DESKTOP: digest(self.desktop)}}
        self.manifest.write_text(json.dumps(self.data))

    def test_legacy_registration_and_package_removal_restore_portable_launcher(self):
        package.register_user(self.prefix)
        self.assertIn('Exec="/usr/bin/niri-bridge-ui"', self.desktop.read_text())
        registered = self.desktop.read_text()
        package.register_user(self.prefix)
        self.assertEqual(registered, self.desktop.read_text())
        self.assertEqual(json.loads(self.manifest.read_text())['files'][package.DESKTOP], digest(self.desktop))
        package.unregister_user(self.prefix)
        self.assertEqual(self.desktop.read_text(), self.original)
        self.assertEqual(json.loads(self.manifest.read_text())['files'][package.DESKTOP], digest(self.desktop))
        self.assertFalse((self.prefix / package.REGISTER).exists())

    def test_custom_launcher_is_preserved_during_registration(self):
        custom = self.original + 'X-User-Customized=true\n'
        self.desktop.write_text(custom)
        package.register_user(self.prefix)
        self.assertEqual(self.desktop.read_text(), custom)
        self.assertFalse((self.prefix / package.REGISTER).exists())

    def test_later_user_edit_is_preserved_when_package_is_removed(self):
        package.register_user(self.prefix)
        custom = self.desktop.read_text() + 'X-User-Customized=true\n'
        self.desktop.write_text(custom)
        package.unregister_user(self.prefix)
        self.assertEqual(self.desktop.read_text(), custom)

    def test_external_desktop_symlink_is_rejected_without_writing_its_target(self):
        target = self.prefix.parent / (self.prefix.name + '-external')
        target.write_text('user work')
        self.addCleanup(target.unlink, missing_ok=True)
        self.desktop.unlink()
        self.desktop.symlink_to(target)
        with self.assertRaises(ValueError):
            package.register_user(self.prefix)
        self.assertEqual(target.read_text(), 'user work')

    def test_package_hook_runs_bundled_code_only_as_the_owning_user(self):
        spec = importlib.util.spec_from_file_location('package_users', Path(package.__file__).with_name('package-users.py'))
        hooks = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(hooks)
        home = self.prefix
        user = mock.Mock(pw_uid=1234, pw_gid=1234, pw_name='test-user', pw_dir=str(home))
        with mock.patch.object(hooks.os, 'geteuid', return_value=0), mock.patch.object(hooks.pwd, 'getpwall', return_value=[user]), \
             mock.patch.object(Path, 'stat', return_value=mock.Mock(st_uid=1234, st_mode=0o40700)), \
             mock.patch.object(hooks.subprocess, 'run', return_value=mock.Mock(returncode=0)) as run, \
             mock.patch('sys.argv', ['package-users.py', 'register']):
            hooks.main()
        args = run.call_args.args[0]
        self.assertEqual(args[:6], ['runuser', '-u', 'test-user', '--', '/usr/bin/python3', '-I'])
        self.assertTrue(args[6].endswith('/scripts/package.py'))
        self.assertEqual(args[7], '--register-user')
        self.assertEqual(set(run.call_args.kwargs['env']), {'PATH', 'HOME', 'USER', 'LOGNAME', 'XDG_RUNTIME_DIR', 'DBUS_SESSION_BUS_ADDRESS'})


if __name__ == '__main__':
    unittest.main()
