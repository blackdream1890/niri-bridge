#!/usr/bin/python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Apply the system package to the current desktop user's private installation."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import threading

ROOT = Path(__file__).resolve().parent.parent
# The system launcher uses Python isolated mode; only its bundled modules belong here.
sys.path.insert(0, str(ROOT / 'scripts'))
from install import MANIFEST, digest, write_file

DESKTOP = 'share/applications/org.niribridge.NiriBridge.desktop'
REGISTER = Path('share/niri-bridge/package-registration.json')


def fingerprint():
    from install import installation_sources
    result = hashlib.sha256()
    for source, name, mode in sorted(installation_sources(), key=lambda item: item[1]):
        result.update(name.encode() + b'\0' + str(mode).encode() + b'\0' + source.read_bytes())
    for name in ('niri-bridge.service', 'niri-bridge-ui.service'):
        result.update((ROOT / 'service' / name).read_bytes())
    return result.hexdigest()


def record(prefix):
    path = prefix / MANIFEST
    if not path.is_file() or path.is_symlink():
        return {}
    data = json.loads(path.read_text())
    if data.get('format') != 1 or data.get('application') != 'niri-bridge':
        raise ValueError('The existing installation record is not recognized.')
    return data


def register_user(prefix):
    """Redirect only an unchanged, installer-owned legacy desktop launcher."""
    from uninstall import safe_file
    data = record(prefix)
    if not data or data.get('package'):
        return
    desktop = safe_file(prefix, DESKTOP)
    if not desktop.is_file() or desktop.is_symlink() or digest(desktop) != data.get('files', {}).get(DESKTOP):
        return
    registration = safe_file(prefix, str(REGISTER))
    if registration.exists():
        return
    before = desktop.read_text()
    after = '\n'.join('Exec="/usr/bin/niri-bridge-ui"' if line.startswith('Exec=') else line for line in before.splitlines()) + '\n'
    if after == before:
        return
    write_file(registration, json.dumps({'desktop': before, 'sha256': hashlib.sha256(after.encode()).hexdigest()}) + '\n', 0o600)
    write_file(desktop, after)
    data['files'][DESKTOP] = digest(desktop)
    write_file(prefix / MANIFEST, json.dumps(data, indent=2, sort_keys=True) + '\n', 0o600)


def unregister_user(prefix):
    from uninstall import safe_file
    data = record(prefix)
    if not data:
        return
    registration = safe_file(prefix, str(REGISTER))
    if not data.get('package'):
        if registration.is_file() and not registration.is_symlink():
            saved = json.loads(registration.read_text())
            desktop = safe_file(prefix, DESKTOP)
            if desktop.is_file() and not desktop.is_symlink() and digest(desktop) == saved['sha256']:
                write_file(desktop, saved['desktop'])
                data['files'][DESKTOP] = digest(desktop)
                write_file(prefix / MANIFEST, json.dumps(data, indent=2, sort_keys=True) + '\n', 0o600)
            registration.unlink()
        return
    args = ['/usr/bin/python3', '-B', str(ROOT / 'scripts/uninstall.py'), '--prefix', str(prefix)]
    bus = Path(os.environ.get('XDG_RUNTIME_DIR', '/nonexistent')) / 'bus'
    if not bus.exists():
        args.append('--offline-package')
    subprocess.run(args, check=True)
    registration.unlink(missing_ok=True)


def launch():
    if '--help' in sys.argv[1:]:
        os.execv('/usr/bin/python3', ['python3', '-B', str(ROOT / 'ui/app.py'), '--help'])
    prefix = Path.home() / '.local'
    data = record(prefix)
    if data.get('package') and data.get('package_fingerprint') == fingerprint():
        os.execv(str(prefix / 'bin/niri-bridge-ui'), ['niri-bridge-ui', *sys.argv[1:]])
    import gi
    gi.require_version('Gtk', '3.0')
    from gi.repository import Gtk, GLib
    sys.path.insert(0, str(ROOT / 'ui'))
    from i18n import _, configure
    configure(Path.home() / '.config/niri-bridge')
    window = Gtk.Window(title=_('NiriBridge setup'))
    window.set_default_size(500, 160)
    window.set_border_width(24)
    box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=18)
    window.add(box)
    label = Gtk.Label(label=_('Preparing NiriBridge…'))
    label.set_line_wrap(True)
    box.pack_start(label, True, True, 0)
    retry = Gtk.Button.new_with_label(_('Try again'))
    retry.set_no_show_all(True)
    box.pack_end(retry, False, False, 0)
    spinner = Gtk.Spinner()
    box.pack_end(spinner, False, False, 0)
    state = {'running': False, 'done': False}

    def close(*_args):
        if state['running']:
            return True
        Gtk.main_quit()
        return False

    def complete(ok):
        state['running'] = False
        spinner.stop()
        if ok:
            state['done'] = True
            Gtk.main_quit()
        else:
            label.set_text(_('Setup could not finish. Save any open NiriBridge edits and quit from its tray menu. Lift your fingers from the touchpad, then try again. Also check free disk space and access to your application directory.'))
            retry.show()

    def work():
        state['running'] = True
        retry.hide()
        spinner.start()
        label.set_text(_('Preparing NiriBridge… Save any open edits and confirm quitting if asked. Sharing will stay stopped after setup.'))
        def install():
            ok = False
            try:
                result = subprocess.run(['/usr/bin/python3', '-B', str(ROOT / 'scripts/install.py'), '--package'],
                                        capture_output=True, text=True)
                log = Path.home() / '.local/state/niri-bridge/last-install.log'
                write_file(log, result.stdout + result.stderr, 0o600)
                ok = result.returncode == 0
            finally:
                GLib.idle_add(complete, ok)
        threading.Thread(target=install, daemon=True).start()

    window.connect('delete-event', close)
    retry.connect('clicked', lambda *_args: work())
    window.show_all()
    GLib.idle_add(work)
    Gtk.main()
    if state['done']:
        os.execv(str(prefix / 'bin/niri-bridge-ui'), ['niri-bridge-ui', *sys.argv[1:]])


if __name__ == '__main__':
    if os.geteuid() == 0:
        raise SystemExit('Open NiriBridge as your desktop user.')
    action = sys.argv[1] if len(sys.argv) == 2 else None
    if action == '--register-user':
        register_user(Path.home() / '.local')
    elif action == '--unregister-user':
        unregister_user(Path.home() / '.local')
    else:
        launch()
