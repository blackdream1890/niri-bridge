#!/usr/bin/python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Exercise a complete archive install, first pairing, upgrade and removal as a fresh user."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile


def run(arguments):
    result = subprocess.run(list(map(str, arguments)), capture_output=True, text=True, timeout=30)
    if result.returncode:
        raise RuntimeError('An installation smoke-check command failed; no private command output was exported.')
    return result.stdout


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--archive', type=Path, required=True)
    args = parser.parse_args()
    if os.geteuid() == 0:
        parser.exit(1, 'Run this check as a non-root test account.\n')
    with tempfile.TemporaryDirectory(prefix='niri-bridge-install-smoke-') as directory:
        root = Path(directory)
        with tarfile.open(args.archive, 'r:gz') as archive:
            members = archive.getmembers()
            if any(not m.isfile() or Path(m.name).is_absolute() or '..' in Path(m.name).parts for m in members):
                raise ValueError('Unexpected archive contents')
            archive.extractall(root / 'unpacked', filter='data')
        extracted = list((root / 'unpacked').iterdir())
        assert len(extracted) == 1
        release = extracted[0]
        prefix = root / 'application files'
        install = ['python3', '-B', release / 'scripts/install.py', '--prefix', prefix, '--skip-systemd', '--no-restart']
        run(install)
        binary = prefix / 'bin/niri-bridge'
        version = json.loads((release / 'release.json').read_text())['version']
        assert run([binary, '--version']).strip() == 'niri-bridge ' + version
        run([prefix / 'bin/niri-bridge-ui', '--help'])
        assert (prefix / 'share/niri-bridge/docs/LICENSE').is_file()
        def manage(*arguments):
            reply = json.loads(run([binary, 'manage', *arguments]))
            if not reply['ok']:
                raise RuntimeError('A first-pairing management check failed.')
            return reply['data']
        configurations = []
        for name, edge in [('test-laptop', 'top'), ('test-desktop', 'bottom')]:
            config = root / name / 'config.toml'
            request = root / (name + '.json')
            request.write_text(json.dumps({'settings': {'connection': {'mode': 'listen', 'address': '127.0.0.1:42420'},
                'activity_devices': ['/dev/input/by-path/test-keyboard'], 'native_touchpads': False},
                'edge': {'output': 'test-output', 'boundary': {'edge': edge, 'start': 0., 'end': 1.}}}))
            request.chmod(0o600)
            manage('initialize', '--config', config, '--name', name, '--request', request)
            assert (config.parent / 'identity.key.pem').stat().st_mode & 0o777 == 0o600
            configurations.append(config)
        for config, other in [configurations, configurations[::-1]]:
            view = manage('show', '--config', config)
            certificate = manage('certificate', other.parent / 'identity.pem')
            manage('import-peer', '--config', config, '--candidate', other.parent / 'identity.pem',
                   '--revision', view['revision'], '--fingerprint', certificate['fingerprint'])
            run([binary, 'check-config', '--config', config])
        saved = {path: hashlib.sha256(path.read_bytes()).digest() for config in configurations
                 for path in config.parent.iterdir() if path.is_file()}
        run(install)
        assert all(hashlib.sha256(path.read_bytes()).digest() == value for path, value in saved.items())
        edited = prefix / 'share/niri-bridge/ui/model.py'
        edited.write_text(edited.read_text() + '\n# retained user customization\n')
        run([prefix / 'bin/niri-bridge-uninstall', '--dry-run', '--skip-systemd'])
        assert binary.is_file()
        run([prefix / 'bin/niri-bridge-uninstall', '--skip-systemd'])
        assert not binary.exists()
        assert edited.read_text().endswith('# retained user customization\n')
        assert all(hashlib.sha256(path.read_bytes()).digest() == value for path, value in saved.items())
    print('Non-root install, first identity/pairing, upgrade, preview and removal passed. No input devices or host services were used.')


if __name__ == '__main__':
    main()
