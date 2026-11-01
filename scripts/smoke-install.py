#!/usr/bin/python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Exercise archive install, saved-device switching, upgrade and removal as a fresh user."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import tarfile
import tempfile


def run(arguments, *, env=None):
    result = subprocess.run(list(map(str, arguments)), capture_output=True, text=True, timeout=30, env=env)
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
        runtime = root / 'runtime'
        runtime.mkdir(mode=0o700)
        management_env = dict(os.environ, XDG_RUNTIME_DIR=str(runtime))
        prefix = root / 'application files'
        install = ['python3', '-B', release / 'scripts/install.py', '--prefix', prefix, '--skip-systemd', '--no-restart']
        run(install)
        binary = prefix / 'bin/niri-bridge'
        version = json.loads((release / 'release.json').read_text())['version']
        assert run([binary, '--version']).strip() == 'niri-bridge ' + version
        run([prefix / 'bin/niri-bridge-ui', '--help'])
        assert (prefix / 'share/niri-bridge/docs/LICENSE').is_file()
        documents = prefix / 'share/niri-bridge/docs'
        for document in documents.rglob('*.md'):
            if 'licenses' in document.relative_to(documents).parts:
                continue
            for target in re.findall(r'\]\(([^)]+)\)', document.read_text()):
                if '://' not in target and not target.startswith('#'):
                    assert (document.parent / target.split('#')[0]).is_file(), 'Installed documentation has a missing local link'
        def manage(*arguments):
            reply = json.loads(run([binary, 'manage', *arguments], env=management_env))
            if not reply['ok']:
                raise RuntimeError('A saved-device management check failed; private details were not exported.')
            return reply['data']
        configurations = []
        for name, edge in [('test-laptop', 'top'), ('test-desktop', 'bottom'), ('test-studio', 'bottom')]:
            config = root / name / 'config.toml'
            request = root / (name + '.json')
            request.write_text(json.dumps({'settings': {'connection': {'mode': 'listen', 'address': '127.0.0.1:42420'},
                'activity_devices': ['/dev/input/by-path/test-keyboard'], 'native_touchpads': False},
                'edge': {'output': 'test-output', 'boundary': {'edge': edge, 'start': 0., 'end': 1.}}}))
            request.chmod(0o600)
            manage('initialize', '--config', config, '--name', name, '--request', request)
            assert (config.parent / 'identity.key.pem').stat().st_mode & 0o777 == 0o600
            configurations.append(config)
        identities = {path: hashlib.sha256(path.read_bytes()).digest() for config in configurations
                      for path in (config.parent / 'identity.pem', config.parent / 'identity.key.pem')}
        def import_peer(config, other):
            view = manage('show', '--config', config)
            certificate = manage('certificate', other.parent / 'identity.pem')
            manage('import-peer', '--config', config, '--candidate', other.parent / 'identity.pem',
                   '--revision', view['revision'], '--fingerprint', certificate['fingerprint'])
            run([binary, 'check-config', '--config', config])
            return certificate['fingerprint']
        def show(config):
            return manage('show', '--config', config)
        def selected(view):
            active = [peer['fingerprint'] for peer in view['paired_devices'] if peer['active']]
            assert len(active) == 1, 'Exactly one saved computer must be selected'
            return active[0]
        def select(config, fingerprint):
            view = show(config)
            manage('select-peer', '--config', config, '--revision', view['revision'], '--fingerprint', fingerprint)
            view = show(config)
            assert selected(view) == fingerprint
            run([binary, 'check-config', '--config', config])
            return view
        def save_connection(config, mode, address):
            view = show(config)
            view['settings']['connection'] = {'mode': mode, 'address': address}
            request = root / (config.parent.name + '-settings.json')
            request.write_text(json.dumps({'revision': view['revision'], 'settings': view['settings']}))
            request.chmod(0o600)
            manage('save', '--config', config, '--request', request)
            return show(config)
        laptop, desktop, studio = configurations
        desktop_id = import_peer(laptop, desktop)
        import_peer(desktop, laptop)
        original = save_connection(laptop, 'connect', 'test-desktop.local:42420')
        studio_id = import_peer(laptop, studio)
        import_peer(studio, laptop)
        added = show(laptop)
        assert selected(added) == desktop_id, 'Adding a computer must keep the current selection'
        assert len(added['paired_devices']) == 2
        for peer in (desktop, studio):
            import_peer(laptop, peer)
        repeated = show(laptop)
        assert len(repeated['paired_devices']) == 2, 'Re-importing must not duplicate saved computers'
        assert selected(repeated) == desktop_id
        assert repeated['settings'] == original['settings']
        assert repeated['edges'] == original['edges']
        other = select(laptop, studio_id)
        assert other['settings']['connection'] != original['settings']['connection']
        other = save_connection(laptop, 'listen', '127.0.0.1:42421')
        for fingerprint, expected in ((desktop_id, original), (studio_id, other), (desktop_id, original)):
            restored = select(laptop, fingerprint)
            assert restored['settings'] == expected['settings'], 'Saved connection and input settings were not restored'
            assert restored['edges'] == expected['edges'], 'Saved screen connections were not restored'
        assert all(hashlib.sha256(path.read_bytes()).digest() == value for path, value in identities.items())
        for config in configurations:
            peers = config.parent / 'peers'
            assert peers.stat().st_mode & 0o777 == 0o700
            assert all(path.stat().st_mode & 0o777 == 0o600 for path in peers.glob('*.pem'))
        def snapshot():
            return {path: (hashlib.sha256(path.read_bytes()).digest(), path.stat().st_mode & 0o777)
                    for config in configurations for path in config.parent.rglob('*') if path.is_file()}
        saved = snapshot()
        run(install)
        assert snapshot() == saved, 'Upgrade changed saved identities, certificates or device settings'
        assert len(show(laptop)['paired_devices']) == 2
        assert selected(show(laptop)) == desktop_id
        edited = prefix / 'share/niri-bridge/ui/model.py'
        edited.write_text(edited.read_text() + '\n# retained user customization\n')
        run([prefix / 'bin/niri-bridge-uninstall', '--dry-run', '--skip-systemd'])
        assert binary.is_file()
        run([prefix / 'bin/niri-bridge-uninstall', '--skip-systemd'])
        assert not binary.exists()
        assert edited.read_text().endswith('# retained user customization\n')
        assert snapshot() == saved, 'Removal changed retained identities, certificates or device settings'
    print('Non-root install, three-device pairing, saved-target switching, upgrade, preview and removal passed. No input devices or host services were used.')


if __name__ == '__main__':
    main()
