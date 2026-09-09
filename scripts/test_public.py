# SPDX-License-Identifier: GPL-3.0-or-later
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest
from unittest import mock

spec = importlib.util.spec_from_file_location('public_check', Path(__file__).with_name('check-public.py'))
checker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(checker)
PUBLIC_EMAIL = 'contributor' + '@public.invalid'


class PublicationChecks(unittest.TestCase):
    def run_check(self, *args):
        with mock.patch('sys.argv', ['check-public.py', *map(str, args)]), mock.patch('sys.stdout', new=io.StringIO()) as output:
            with self.assertRaises(SystemExit) as result:
                checker.main()
        return result.exception.code, json.loads(output.getvalue())

    def git(self, root, *args):
        return subprocess.check_output(
            ['git', '-c', 'user.name=Public Contributor', '-c', 'user.email=' + PUBLIC_EMAIL, *args],
            cwd=root, env=dict(os.environ, GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM='1'),
            stderr=subprocess.PIPE)

    def test_private_values_fail_without_returning_the_matched_value(self):
        email = 'private-person' + '@private.invalid'
        path = '/home/' + 'private-person/project'
        token = 'ghp_' + 'x' * 40
        for data in (email, path, token):
            result = checker.findings('README.md', data.encode())
            self.assertTrue(result)
            self.assertNotIn(data, repr(result))
        self.assertTrue(checker.findings('licenses/rust/NOTICE.txt', b'private marker', [b'private marker']))
        self.assertEqual(checker.findings('licenses/rust/NOTICE.txt', email.encode()), [])

    def test_vendored_source_must_match_its_cargo_checksums(self):
        with tempfile.TemporaryDirectory() as directory:
            for tampered in (False, True):
                archive = Path(directory) / ('modified.tar.gz' if tampered else 'valid.tar.gz')
                content = b'upstream source'
                upstream_instructions = b'Public upstream development notes'
                data = {'files': {'src/target/platform.rs': hashlib.sha256(content).hexdigest(),
                                  'AGENTS.md': hashlib.sha256(upstream_instructions).hexdigest()}, 'package': None}
                with tarfile.open(archive, 'w:gz') as tar:
                    for name, value in [('src/target/platform.rs', content + b' changed' if tampered else content),
                                        ('AGENTS.md', upstream_instructions),
                                        ('.cargo-checksum.json', json.dumps(data).encode())]:
                        entry = tarfile.TarInfo('release/vendor/example-1.0.0/' + name)
                        entry.size = len(value)
                        tar.addfile(entry, io.BytesIO(value))
                with mock.patch('sys.argv', ['check-public.py', '--archive', str(archive)]), mock.patch('sys.stdout', new=io.StringIO()):
                    with self.assertRaises(SystemExit) as result:
                        checker.main()
                self.assertEqual(bool(result.exception.code), tampered)

    def test_archives_reject_local_instructions_including_nested_tool_config(self):
        with tempfile.TemporaryDirectory() as directory:
            archive = Path(directory) / 'release.tar.gz'
            for name in ['AGENTS.md', 'src/AGENTS.override.md', '.codex/config.toml',
                         '.github/instructions/review.instructions.md']:
                with self.subTest(path=name):
                    with tarfile.open(archive, 'w:gz') as tar:
                        content = b'Local development instructions'
                        entry = tarfile.TarInfo('release/' + name)
                        entry.size = len(content)
                        tar.addfile(entry, io.BytesIO(content))
                    code, report = self.run_check('--archive', archive)
                    self.assertTrue(code)
                    self.assertIn('local automation instructions or configuration',
                                  [issue['reason'] for issue in report['issues']])

    def test_untracking_preserves_local_instructions_and_reports_history(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / 'repo'
            root.mkdir()
            self.git(root, 'init', '-q', '--initial-branch=main')
            instructions = root / 'AGENTS.md'
            instructions.write_text('Local development instructions\n')
            self.git(root, 'add', 'AGENTS.md')
            self.git(root, 'commit', '-qm', 'Initial source')
            code, report = self.run_check('--tree', root)
            self.assertTrue(code)
            self.assertEqual(report['issues'][0]['file'], 'AGENTS.md')

            (root / '.gitignore').write_text('AGENTS.md\n')
            self.git(root, 'rm', '--cached', 'AGENTS.md')
            self.git(root, 'add', '.gitignore')
            self.git(root, 'commit', '-qm', 'Keep local instructions outside source')
            self.assertTrue(instructions.is_file())
            code, report = self.run_check('--tree', root, '--history')
            self.assertFalse(code)
            self.assertEqual(report['issues'], [])
            self.assertEqual([item['file'] for item in report['historical_local_instruction_files']], ['AGENTS.md'])

            denied = Path(directory) / 'private-audit.json'
            denied.write_text(json.dumps([PUBLIC_EMAIL]))
            code, report = self.run_check('--tree', root, '--history', '--deny-file', denied)
            self.assertTrue(code)
            self.assertIn('private value from local audit rules', [item['reason'] for item in report['issues']])
            self.assertNotIn(PUBLIC_EMAIL, json.dumps(report))

    def test_release_rejects_tracked_instructions_before_building_assets(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            scripts = root / 'scripts'
            scripts.mkdir()
            for name in ['release.py', 'check-public.py']:
                (scripts / name).write_bytes(Path(__file__).with_name(name).read_bytes())
            (root / 'AGENTS.md').write_text('Local development instructions\n')
            self.git(root, 'init', '-q', '--initial-branch=main')
            self.git(root, 'add', 'AGENTS.md', 'scripts')
            self.git(root, 'commit', '-qm', 'Source with local instructions')
            result = subprocess.run(['python3', '-B', str(scripts / 'release.py')], cwd=root,
                                    capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            report = json.loads(result.stdout)
            self.assertIn('local automation instructions or configuration', [item['reason'] for item in report['issues']])
            self.assertNotIn('FileNotFoundError', result.stderr)
            self.assertFalse((root / 'dist').exists())


if __name__ == '__main__':
    unittest.main()
