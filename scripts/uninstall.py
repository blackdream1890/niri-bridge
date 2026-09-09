#!/usr/bin/python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Remove unchanged NiriBridge application files; preserve configuration and input authorization."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess

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
    unit = Path.home() / '.config/systemd/user/niri-bridge.service'
    unit_record = data.get('unit') or {}
    remove_unit = bool(unit.is_file() and not unit.is_symlink() and unit_record.get('sha256') == digest(unit))
    return data, removed, preserved, unit, remove_unit


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--prefix', type=Path, default=Path.home() / '.local')
    parser.add_argument('--dry-run', action='store_true', help='List the removal scope without changing files or services.')
    parser.add_argument('--skip-systemd', action='store_true', help='Remove staging files without accessing user services.')
    parser.add_argument('--revoke-input-access', action='store_true', help='Request administrator authentication to revoke managed input authorization before removing the helper.')
    args = parser.parse_args()
    if os.geteuid() == 0 and not args.skip_systemd:
        parser.exit(1, 'Run application removal as your desktop user.\n')
    prefix = args.prefix.expanduser().resolve()
    try:
        data, removed, preserved, unit, remove_unit = removal_plan(prefix)
    except (OSError, ValueError, TypeError) as error:
        parser.exit(1, 'Application removal stopped: ' + str(error) + '\n')
    print(f'Unchanged application files: {len(removed)}. Modified files preserved: {len(preserved)}.')
    print('Settings and pairing identities will be preserved.')
    print('Managed input authorization will be revoked with administrator authentication.' if args.revoke_input_access else 'Device authorization will be preserved; add --revoke-input-access to revoke it.')
    if unit.exists() and not remove_unit:
        print('The existing custom user service definition will be preserved.')
    if args.dry_run:
        print('Preview only; no files or services were changed.')
        return
    if not args.skip_systemd:
        if remove_unit:
            subprocess.run(['systemctl', '--user', 'disable', '--now', 'niri-bridge.service'], check=True)
        elif subprocess.run(['systemctl', '--user', 'is-active', '--quiet', 'niri-bridge.service']).returncode == 0:
            parser.exit(1, 'Stop niri-bridge.service before removing an installation with a custom service definition.\n')
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
    if not args.skip_systemd and remove_unit and unit.is_file() and digest(unit) == data['unit']['sha256']:
        unit.unlink()
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
