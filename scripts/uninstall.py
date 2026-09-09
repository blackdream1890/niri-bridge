#!/usr/bin/python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Remove unchanged NiriBridge application files; preserve configuration and input authorization."""
import argparse
from contextlib import nullcontext
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import lifecycle

MANIFEST = Path('share/niri-bridge/install-manifest.json')
FIXED_FILES = {'bin/niri-bridge', 'bin/niri-bridge-ui', 'bin/niri-bridge-setup-input',
               'bin/niri-bridge-uninstall', 'share/applications/org.niribridge.NiriBridge.desktop',
               'share/icons/hicolor/scalable/apps/niri-bridge.svg'}


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def safe_file(prefix, name):
    path = Path(name)
    if path.is_absolute() or '..' in path.parts or not (name in FIXED_FILES or name.startswith('share/niri-bridge/')):
        raise ValueError('The installation record contains an unexpected path. No files were removed.')
    destination = prefix / path
    if not destination.resolve().is_relative_to(prefix):
        raise ValueError('An installation path points outside its prefix. No files were removed.')
    return destination


def removal_plan(prefix):
    record = prefix / MANIFEST
    data = json.loads(record.read_text())
    if data.get('format') != 1 or data.get('application') != 'niri-bridge' or not isinstance(data.get('files'), dict):
        raise ValueError('The installation record is not recognized.')
    removed, preserved = [], []
    for name, expected in data['files'].items():
        if not isinstance(expected, str) or not re.fullmatch('[0-9a-f]{64}', expected):
            raise ValueError('The installation record has an invalid checksum.')
        path = safe_file(prefix, name)
        if path.is_symlink() or (path.exists() and (not path.is_file() or digest(path) != expected)):
            preserved.append(name)
        elif path.exists():
            removed.append(path)
    units = []
    for name, key in (('niri-bridge.service', 'unit'), ('niri-bridge-ui.service', 'ui_unit')):
        unit = Path.home() / '.config/systemd/user' / name
        record = data.get(key) or {}
        owned = bool(unit.is_file() and not unit.is_symlink() and record.get('sha256') == digest(unit))
        units.append((unit, key, owned))
    return data, removed, preserved, units


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--prefix', type=Path, default=Path.home() / '.local')
    parser.add_argument('--dry-run', action='store_true', help='List the removal scope without changing files or services.')
    parser.add_argument('--skip-systemd', action='store_true', help='Remove staging files without accessing user services.')
    parser.add_argument('--revoke-input-access', action='store_true', help='Request administrator authentication to revoke managed input authorization before removing the helper.')
    parser.add_argument('--offline-package', action='store_true', help=argparse.SUPPRESS)
    args = parser.parse_args()
    if os.geteuid() == 0 and not args.skip_systemd:
        parser.exit(1, 'Run application removal as your desktop user.\n')
    prefix = args.prefix.expanduser().resolve()
    with nullcontext() if args.dry_run else lifecycle.installation_lock(prefix):
        try:
            data, removed, preserved, units = removal_plan(prefix)
        except (OSError, ValueError, TypeError) as error:
            parser.exit(1, 'Application removal stopped: ' + str(error) + '\n')
        print(f'Unchanged application files: {len(removed)}. Modified files preserved: {len(preserved)}.')
        print('Settings and pairing identities will be preserved.')
        print('Managed input authorization will be revoked with administrator authentication.' if args.revoke_input_access else 'Device authorization will be preserved; add --revoke-input-access to revoke it.')
        if any(unit.exists() and not owned for unit, _, owned in units):
            print('Existing custom user service definitions will be preserved.')
        if args.dry_run:
            print('Preview only; no files or services were changed.')
            return
        if args.offline_package:
            if not data.get('package'):
                parser.exit(1, 'Offline removal requires a system-package installation record.\n')
            lifecycle.ensure_offline()
        if not args.skip_systemd and not args.offline_package:
            try:
                lifecycle.close_interface()
                lifecycle.stop_sharing(prefix / 'bin/niri-bridge')
            except Exception as error:
                parser.exit(1, 'Application removal paused: ' + str(error) + '\n')
            for unit, _, owned in units:
                if owned:
                    subprocess.run(['systemctl', '--user', 'disable', unit.name], check=True)
        if args.revoke_input_access:
            if args.skip_systemd:
                parser.exit(1, 'Input authorization cannot be changed during staging removal.\n')
            helper = prefix / 'bin/niri-bridge-setup-input'
            if helper.is_symlink() or not helper.is_file() or digest(helper) != data['files'].get('bin/niri-bridge-setup-input'):
                parser.exit(1, 'The administrator helper was modified or is missing. Application files were preserved.\n')
            result = subprocess.run(['pkexec', '/usr/bin/python3', str(helper), 'uninstall'], capture_output=True, text=True)
            if result.returncode:
                parser.exit(1, 'Input authorization could not be revoked. Application files were preserved; sharing may have been stopped.\n')
        # Recheck every candidate after service shutdown, retaining any intervening edits.
        for path in removed:
            name = str(path.relative_to(prefix))
            if path.is_file() and not path.is_symlink() and digest(path) == data['files'][name]:
                path.unlink()
            else:
                preserved.append(name)
        if not args.skip_systemd:
            for unit, key, owned in units:
                if owned and unit.is_file() and not unit.is_symlink() and digest(unit) == data[key]['sha256']:
                    if args.offline_package:
                        for target in ('graphical-session.target.wants', 'default.target.wants'):
                            link = unit.parent / target / unit.name
                            if link.is_symlink() and link.resolve() == unit.resolve():
                                link.unlink()
                    unit.unlink()
            if not args.offline_package:
                subprocess.run(['systemctl', '--user', 'daemon-reload'], check=True)
        if preserved:
            print('Modified application files remain, together with the installation record.')
        else:
            (prefix / MANIFEST).unlink()
        directories = {parent for path in removed + [prefix / MANIFEST] for parent in path.parents
                       if parent != prefix and parent.is_relative_to(prefix)}
        for path in sorted(directories, key=lambda p: len(p.parts), reverse=True):
            try:
                path.rmdir()
            except OSError:
                pass
        print('Application removal completed. Configuration and pairing files were retained.')
        if not args.revoke_input_access:
            print('Device authorization is unchanged. The release archive includes the administrator helper for later recovery.')


if __name__ == '__main__':
    main()
