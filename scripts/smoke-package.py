#!/usr/bin/python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Exercise Debian package setup, reopen, upgrade and removal in the isolated CI container."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parent.parent


def desktop_check():
    if os.geteuid() == 0:
        raise SystemExit('The package interface test requires the non-root test account.')
    from gi.repository import Gio, GLib
    bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
    prefix = Path.home() / '.local'
    manifest = prefix / 'share/niri-bridge/install-manifest.json'
    with tempfile.TemporaryDirectory(prefix='niri-bridge-package-desktop-') as temporary:
        directory = Path(temporary)
        stub = directory / 'systemctl'
        calls = directory / 'service-calls.jsonl'
        stub.write_text('''#!/usr/bin/python3
import json, os, sys
from pathlib import Path
with Path(os.environ['NIRI_BRIDGE_TEST_CALLS']).open('a') as log:
    log.write(json.dumps(sys.argv[1:]) + '\\n')
if 'show' in sys.argv:
    print('ActiveState=inactive\\nMainPID=0\\nUnitFileState=disabled\\nExecMainStatus=0')
''')
        stub.chmod(0o755)
        env = dict(os.environ, PATH=str(directory) + ':/usr/bin:/bin', NIRI_BRIDGE_TEST_CALLS=str(calls),
                   GSETTINGS_BACKEND='memory', GTK_USE_PORTAL='0', NO_AT_BRIDGE='1')
        def owner():
            return bus.call_sync('org.freedesktop.DBus', '/org/freedesktop/DBus', 'org.freedesktop.DBus',
                'NameHasOwner', GLib.Variant('(s)', ('org.niribridge.NiriBridge',)), None,
                Gio.DBusCallFlags.NO_AUTO_START, 3000, None).unpack()[0]
        for _ in range(2):
            with (directory / 'interface.log').open('w') as log:
                process = subprocess.Popen(['/usr/bin/niri-bridge-ui'], env=env, stdout=log, stderr=log)
                try:
                    deadline = time.monotonic() + 30
                    while not (manifest.is_file() and owner()):
                        if process.poll() is not None or time.monotonic() >= deadline:
                            raise RuntimeError('The packaged interface did not open: ' + (directory / 'interface.log').read_text())
                        time.sleep(0.1)
                    data = json.loads(manifest.read_text())
                    assert data['package'] and data['safe_touchpad_release'] and data['package_fingerprint']
                    bus.call_sync('org.niribridge.NiriBridge', '/org/niribridge/NiriBridge', 'org.gtk.Actions',
                        'Activate', GLib.Variant('(sava{sv})', ('quit', [], {})), None,
                        Gio.DBusCallFlags.NO_AUTO_START, 3000, None)
                    assert process.wait(timeout=20) == 0
                    assert not owner()
                finally:
                    if process.poll() is None:
                        process.terminate()
                        try:
                            process.wait(timeout=5)
                        except subprocess.TimeoutExpired:
                            process.kill()
                            process.wait()
        commands = [json.loads(line) for line in calls.read_text().splitlines()]
        assert not any(action in command for command in commands for action in ('start', 'restart', 'try-restart', '--now'))
        desktop = (prefix / 'share/applications/org.niribridge.NiriBridge.desktop').read_text()
        assert 'Exec="/usr/bin/niri-bridge-ui"' in desktop
        assert 'ExecStart="/usr/bin/niri-bridge-ui"' in (Path.home() / '.config/systemd/user/niri-bridge-ui.service').read_text()
        config = Path.home() / '.config/niri-bridge'
        config.mkdir(parents=True, exist_ok=True)
        (config / 'retained-settings.txt').write_text('synthetic user setting')
        (config / 'identity.key.pem').write_text('synthetic private fixture, not a usable identity')
        (config / 'identity.key.pem').chmod(0o600)
    print('Packaged first open, idle sharing, quit and reopen passed with simulated services.')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--package', type=Path)
    parser.add_argument('--desktop', action='store_true')
    args = parser.parse_args()
    if os.environ.get('NIRI_BRIDGE_CI') != '1' or not Path('/.dockerenv').is_file():
        parser.exit(1, 'This test only runs in the isolated build container.\n')
    if args.desktop:
        desktop_check()
        return
    if os.geteuid() != 0 or not args.package:
        parser.error('The package test requires container root and --package.')
    for _ in range(2):
        subprocess.run(['dpkg', '--install', str(args.package)], check=True)
        subprocess.run(['runuser', '-u', 'tester', '--', 'env', 'NIRI_BRIDGE_CI=1', 'GDK_BACKEND=x11',
                        'xvfb-run', '-a', 'dbus-run-session', '--', 'python3', '-B', str(__file__), '--desktop'], check=True)
    subprocess.run(['dpkg', '--remove', 'niri-bridge'], check=True)
    subprocess.run(['runuser', '-u', 'tester', '--', 'python3', '-B', '-c', '''
from pathlib import Path
home = Path.home()
assert not (home / '.local/bin/niri-bridge').exists()
assert not (home / '.config/systemd/user/niri-bridge-ui.service').exists()
assert (home / '.config/niri-bridge/retained-settings.txt').read_text() == 'synthetic user setting'
assert (home / '.config/niri-bridge/identity.key.pem').is_file()
'''], check=True)
    print('Debian install, desktop setup, repeated package upgrade and removal passed; no physical inputs or real sharing services were used.')


if __name__ == '__main__':
    main()
