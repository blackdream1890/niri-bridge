# SPDX-License-Identifier: GPL-3.0-or-later
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest import mock

spec = importlib.util.spec_from_file_location('public_check', Path(__file__).with_name('check-public.py'))
checker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(checker)


class PublicationChecks(unittest.TestCase):
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
                data = {'files': {'src/target/platform.rs': hashlib.sha256(content).hexdigest()}, 'package': None}
                with tarfile.open(archive, 'w:gz') as tar:
                    for name, value in [('src/target/platform.rs', content + b' changed' if tampered else content),
                                        ('.cargo-checksum.json', json.dumps(data).encode())]:
                        entry = tarfile.TarInfo('release/vendor/example-1.0.0/' + name)
                        entry.size = len(value)
                        tar.addfile(entry, io.BytesIO(value))
                with mock.patch('sys.argv', ['check-public.py', '--archive', str(archive)]), mock.patch('sys.stdout', new=io.StringIO()):
                    with self.assertRaises(SystemExit) as result:
                        checker.main()
                self.assertEqual(bool(result.exception.code), tampered)


if __name__ == '__main__':
    unittest.main()
