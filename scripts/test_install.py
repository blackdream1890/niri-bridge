# SPDX-License-Identifier: GPL-3.0-or-later
"""Installation lifecycle checks use temporary users, files and simulated service calls."""
from contextlib import ExitStack
import io
import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest import mock

import install
import uninstall


class InstallationTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix='niri-bridge-install-test-')
        self.root = Path(self.directory.name)
        self.user = self.root / 'user'
        self.user.mkdir()
        self.source = self.root / 'source'
        self.prefix = self.user / 'application files %'
        for name in ['bin/niri-bridge', 'scripts/device-access.py', 'scripts/uninstall.py',
                     'LICENSE', 'COPYRIGHT', 'THIRD_PARTY_NOTICES.md', 'README.md',
                     *['ui/' + item for item in install.UI_FILES]]:
            path = self.source / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text('# synthetic installation fixture\n')
        self.unit = self.user / '.config/systemd/user/niri-bridge.service'
        template = self.source / 'service/niri-bridge.service'
        template.parent.mkdir(parents=True)
        template.write_text((install.ROOT / 'service/niri-bridge.service').read_text())
        self.calls = []
        actual_run = subprocess.run
        def run(args, **kwargs):
            self.calls.append(args)
            if args[0] == 'desktop-file-validate':
                return actual_run(args, **kwargs)
            code = 3 if 'is-active' in args else 0
            return subprocess.CompletedProcess(args, code, '', '')
        self.patches = ExitStack()
        self.patches.enter_context(mock.patch.object(install, 'ROOT', self.source))
        self.patches.enter_context(mock.patch('pathlib.Path.home', return_value=self.user))
        self.patches.enter_context(mock.patch('os.geteuid', return_value=1000))
        self.patches.enter_context(mock.patch('subprocess.run', side_effect=run))
        self.patches.enter_context(mock.patch('sys.stdout', new=io.StringIO()))

    def tearDown(self):
        self.patches.close()
        self.directory.cleanup()

    def install(self):
        with mock.patch('sys.argv', ['install.py', '--prefix', str(self.prefix), '--no-restart']):
            install.main()

    def remove(self, *arguments):
        with mock.patch('sys.argv', ['uninstall.py', '--prefix', str(self.prefix), *arguments]):
            uninstall.main()

    def test_install_upgrade_and_uninstall_preserve_identity_and_configuration(self):
        config = self.user / '.config/niri-bridge/config.toml'
        config.parent.mkdir(parents=True)
        config.write_text('user-owned configuration')
        identity = config.parent / 'identity.key.pem'
        identity.write_text('synthetic private fixture; not a usable key')
        self.install()
        manifest = self.prefix / install.MANIFEST
        self.assertEqual(manifest.stat().st_mode & 0o777, 0o600)
        self.assertTrue(self.unit.is_file())
        self.assertFalse(any('enable' in call for call in self.calls))
        (self.source / 'bin/niri-bridge').write_text('# newer synthetic backend\n')
        self.install()
        self.assertEqual((self.prefix / 'bin/niri-bridge').read_text(), '# newer synthetic backend\n')
        self.remove('--dry-run')
        self.assertTrue((self.prefix / 'bin/niri-bridge').exists())
        self.remove()
        self.assertFalse((self.prefix / 'bin/niri-bridge').exists())
        self.assertFalse(self.unit.exists())
        self.assertEqual(config.read_text(), 'user-owned configuration')
        self.assertEqual(identity.read_text(), 'synthetic private fixture; not a usable key')
        self.assertFalse(any(call[0] == 'pkexec' for call in self.calls))

    def test_custom_service_and_edited_application_file_survive_removal(self):
        self.unit.parent.mkdir(parents=True)
        self.unit.write_text('# user-customized service\n')
        self.install()
        edited = self.prefix / 'share/niri-bridge/ui/model.py'
        edited.write_text('# a user edit\n')
        self.remove()
        self.assertEqual(self.unit.read_text(), '# user-customized service\n')
        self.assertEqual(edited.read_text(), '# a user edit\n')
        self.assertTrue((self.prefix / install.MANIFEST).is_file())
        self.assertFalse(any('disable' in call for call in self.calls))

    def test_unexpected_manifest_path_rejects_removal_before_any_delete(self):
        self.install()
        manifest = self.prefix / install.MANIFEST
        data = json.loads(manifest.read_text())
        data['files']['../../unrelated-file'] = '0' * 64
        manifest.write_text(json.dumps(data))
        with self.assertRaises(SystemExit):
            self.remove()
        self.assertTrue((self.prefix / 'bin/niri-bridge').is_file())
        self.assertTrue(self.unit.is_file())

    def test_external_symlink_rejects_removal_and_preserves_target(self):
        self.install()
        external = self.root / 'external'
        external.write_text('private user work')
        installed = self.prefix / 'share/niri-bridge/ui/model.py'
        installed.unlink()
        installed.symlink_to(external)
        with self.assertRaises(SystemExit):
            self.remove()
        self.assertEqual(external.read_text(), 'private user work')
        self.assertTrue((self.prefix / 'bin/niri-bridge').is_file())


if __name__ == '__main__':
    unittest.main()
